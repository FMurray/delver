//! DocQL editor (CodeMirror + LSP over a server-fn websocket) plus the
//! template execution panel: runs the editor's template against the
//! currently-open stored document via the same hydrate + `process_parsed`
//! path the CLI `query --doc` uses (DV-006).
//!
//! Slice V2 (DV-012) additions: consumes the [`InsertBus`] (discover-mode
//! "insert into query" actions render their snippet against the live buffer
//! and land at the CodeMirror cursor), binds Ctrl/Cmd-Enter inside the
//! editor to execute, and surfaces LSP completions through the CodeMirror
//! show-hint addon (Ctrl-Space).

use futures::channel::mpsc;
#[cfg(feature = "hydrate")]
use leptos::logging::log;
use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_query_map};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(feature = "hydrate")]
use serde_json::Value;
use server_fn::{codec::JsonEncoding, BoxedStream, ServerFnError, Websocket};

use crate::components::insert::InsertBus;
use crate::snippets::render_snippet;
use crate::store::TemplateRun;

#[cfg(feature = "hydrate")]
use codemirror::{DocApi, Editor, EditorOptions};
#[cfg(feature = "hydrate")]
use wasm_bindgen::prelude::*;
#[cfg(feature = "hydrate")]
use web_sys::HtmlTextAreaElement;

/// Default editor content: a VALID runnable starter (the previous `//`
/// comment placeholder was a DocQL syntax error — the grammar has no
/// comments — so the editor opened with a red diagnostic).
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
const STARTER_TEMPLATE: &str = "TextChunk(chunkSize=500, chunkOverlap=150)";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub severity: String,
}

/// Execute DocQL template source against a stored document. Template
/// failures (parse errors, fail-loud matchers, missing embedder …) come back
/// as a structured `TemplateRun` so the UI can render a readable banner
/// instead of a transport error.
#[server]
pub async fn run_doc_template(
    doc_id: String,
    template: String,
) -> Result<TemplateRun, ServerFnError> {
    match crate::store::execute_template(&doc_id, &template).await {
        Ok(output) => Ok(TemplateRun {
            ok: true,
            output: Some(output),
            error: None,
        }),
        Err(e) => Ok(TemplateRun {
            ok: false,
            output: None,
            error: Some(format!("{e:#}")),
        }),
    }
}

#[server(protocol = Websocket<JsonEncoding, JsonEncoding>)]
async fn lsp_websocket(
    input: BoxedStream<String, ServerFnError>,
) -> Result<BoxedStream<String, ServerFnError>, ServerFnError> {
    use futures::{channel::mpsc, SinkExt, StreamExt};

    #[cfg(feature = "ssr")]
    {
        use crate::language_server::{DocQLLanguageServer, MockClient};

        let mut input = input;
        let (mut tx, rx) = mpsc::channel(100);

        tokio::spawn(async move {
            // Create language server instance
            let (response_sender, mut _response_receiver) = tokio::sync::mpsc::channel(100);
            let server = DocQLLanguageServer::new(MockClient {
                sender: response_sender,
            });

            // Process incoming LSP requests using the server
            while let Some(msg) = input.next().await {
                if let Ok(msg_str) = msg {
                    let responses = server.process_lsp_message(&msg_str).await;
                    for response in responses {
                        let _ = tx.send(Ok(response)).await;
                    }
                }
            }
        });

        Ok(rx.into())
    }

    #[cfg(not(feature = "ssr"))]
    {
        // Client-side fallback (though this should never be called)
        let (_, rx) = mpsc::channel(1);
        Ok(rx.into())
    }
}

/// Current document id parsed from the route path (`/viewer/<uuid>[/<page>]`).
/// Components outside the `<Routes>` tree (this panel, the palette) cannot
/// use `use_params_map`; the location pathname is always available.
pub fn doc_id_from_path(pathname: &str) -> Option<String> {
    let rest = pathname.strip_prefix("/viewer/")?;
    let id = rest.split('/').next()?;
    uuid::Uuid::parse_str(id).ok().map(|u| u.to_string())
}

// ───────────────────── CodeMirror JS interop (hydrate) ─────────────────────
//
// The `codemirror` crate wraps only value get/set + change events; cursor
// insertion, key bindings, and the show-hint addon need the underlying CM5
// instance. It is recovered the standard CM5 way — the wrapper div that
// `fromTextArea` inserts right after the textarea carries the instance as a
// `CodeMirror` property — and held in a thread-local (wasm is single-
// threaded; the discover-mode panels live in other component trees).

#[cfg(feature = "hydrate")]
mod cm {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[wasm_bindgen]
    extern "C" {
        /// Duck-typed CodeMirror 5 editor instance.
        pub type CmInstance;

        #[wasm_bindgen(method, js_name = getCursor)]
        pub fn get_cursor(this: &CmInstance) -> JsValue;
        #[wasm_bindgen(method, js_name = getLine)]
        pub fn get_line(this: &CmInstance, line: u32) -> Option<String>;
        #[wasm_bindgen(method, js_name = getValue)]
        pub fn get_value(this: &CmInstance) -> String;
        #[wasm_bindgen(method, js_name = setValue)]
        pub fn set_value(this: &CmInstance, value: &str);
        #[wasm_bindgen(method, js_name = replaceRange)]
        pub fn replace_range(this: &CmInstance, text: &str, from: &JsValue);
        #[wasm_bindgen(method, js_name = setOption)]
        pub fn set_option(this: &CmInstance, key: &str, value: &JsValue);
        #[wasm_bindgen(method)]
        pub fn focus(this: &CmInstance);
    }

    thread_local! {
        pub static INSTANCE: RefCell<Option<CmInstance>> = const { RefCell::new(None) };
        pub static PENDING_HINT: RefCell<Option<PendingHint>> = const { RefCell::new(None) };
        pub static NEXT_LSP_ID: Cell<u64> = const { Cell::new(1000) };
    }

    /// An in-flight completion request: the CM callback plus the word range
    /// the hints replace.
    pub struct PendingHint {
        pub id: u64,
        pub callback: js_sys::Function,
        pub line: u32,
        pub word_start: u32,
        pub cursor_ch: u32,
        pub prefix: String,
    }

    /// Capture the CM instance for a freshly initialized editor.
    pub fn capture_instance(textarea: &HtmlTextAreaElement) {
        let wrapper = js_sys::Reflect::get(textarea.as_ref(), &"nextSibling".into())
            .ok()
            .filter(|v| !v.is_null() && !v.is_undefined());
        let instance = wrapper
            .and_then(|w| js_sys::Reflect::get(&w, &"CodeMirror".into()).ok())
            .filter(|v| !v.is_null() && !v.is_undefined());
        match instance {
            Some(inst) => INSTANCE.with(|c| *c.borrow_mut() = Some(inst.unchecked_into())),
            None => leptos::logging::warn!("could not capture CodeMirror instance"),
        }
    }

    pub fn with_instance<R>(f: impl FnOnce(&CmInstance) -> R) -> Option<R> {
        INSTANCE.with(|c| c.borrow().as_ref().map(f))
    }

    fn cursor_parts(cursor: &JsValue) -> (u32, u32) {
        let get = |k: &str| {
            js_sys::Reflect::get(cursor, &k.into())
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as u32
        };
        (get("line"), get("ch"))
    }

    /// Insert `text` at the cursor (on its own line when the cursor line has
    /// content), or replace the whole buffer when it still holds the pristine
    /// starter. Returns false when no editor is mounted.
    pub fn insert_at_cursor(text: &str) -> bool {
        with_instance(|cm| {
            if cm.get_value().trim() == STARTER_TEMPLATE.trim() {
                cm.set_value(&format!("{text}\n"));
            } else {
                let cursor = cm.get_cursor();
                let (line, ch) = cursor_parts(&cursor);
                let line_text = cm.get_line(line).unwrap_or_default();
                let before: String = line_text.chars().take(ch as usize).collect();
                let mut snippet = String::new();
                if !before.trim().is_empty() {
                    snippet.push_str("\n\n");
                }
                snippet.push_str(text);
                snippet.push('\n');
                cm.replace_range(&snippet, &cursor);
            }
            cm.focus();
        })
        .is_some()
    }

    /// The async show-hint `hint` function: looks up the word prefix at the
    /// cursor, sends `textDocument/completion`, and parks the CM callback
    /// until the matching LSP response arrives.
    pub fn request_completion(
        cm_js: &JsValue,
        callback: js_sys::Function,
        tx: &StoredValue<mpsc::Sender<Result<String, ServerFnError>>>,
    ) {
        let editor: &CmInstance = cm_js.unchecked_ref();
        let cursor = editor.get_cursor();
        let (line, ch) = cursor_parts(&cursor);
        let line_text = editor.get_line(line).unwrap_or_default();
        let chars: Vec<char> = line_text.chars().collect();
        let mut word_start = ch.min(chars.len() as u32);
        while word_start > 0 {
            let c = chars[(word_start - 1) as usize];
            if c.is_ascii_alphanumeric() || c == '_' {
                word_start -= 1;
            } else {
                break;
            }
        }
        let prefix: String = chars[(word_start as usize)..(ch.min(chars.len() as u32) as usize)]
            .iter()
            .collect();

        let id = NEXT_LSP_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        PENDING_HINT.with(|p| {
            *p.borrow_mut() = Some(PendingHint {
                id,
                callback,
                line,
                word_start,
                cursor_ch: ch,
                prefix,
            })
        });

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": "file:///query.docql"},
                "position": {"line": line, "character": ch}
            }
        });
        if let Ok(msg) = serde_json::to_string(&request) {
            let _ = tx.with_value(|tx| tx.clone().try_send(Ok(msg)));
        }
    }

    /// Completion response → CodeMirror hint object → parked callback.
    /// Returns true when the message was a completion response we own.
    pub fn try_complete_hint(parsed: &Value) -> bool {
        let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) else {
            return false;
        };
        let pending = PENDING_HINT.with(|p| {
            let matches = p.borrow().as_ref().is_some_and(|h| h.id == id);
            if matches {
                p.borrow_mut().take()
            } else {
                None
            }
        });
        let Some(hint) = pending else { return false };
        let Some(items) = parsed.get("result").and_then(|r| r.as_array()) else {
            return true; // ours, but unusable — swallow it
        };

        let list = js_sys::Array::new();
        for item in items {
            let label = item.get("label").and_then(|l| l.as_str()).unwrap_or("");
            if !hint.prefix.is_empty()
                && !label.to_lowercase().starts_with(&hint.prefix.to_lowercase())
            {
                continue;
            }
            let insert = item
                .get("insertText")
                .and_then(|t| t.as_str())
                .unwrap_or(label);
            let entry = js_sys::Object::new();
            let _ = js_sys::Reflect::set(
                &entry,
                &"text".into(),
                &crate::snippets::strip_snippet_placeholders(insert).into(),
            );
            let _ = js_sys::Reflect::set(&entry, &"displayText".into(), &label.into());
            list.push(&entry);
        }

        let pos = |line: u32, ch: u32| {
            let o = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&o, &"line".into(), &(line as f64).into());
            let _ = js_sys::Reflect::set(&o, &"ch".into(), &(ch as f64).into());
            o
        };
        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"list".into(), &list);
        let _ = js_sys::Reflect::set(&result, &"from".into(), &pos(hint.line, hint.word_start));
        let _ = js_sys::Reflect::set(&result, &"to".into(), &pos(hint.line, hint.cursor_ch));
        let _ = hint.callback.call1(&JsValue::NULL, &result);
        true
    }
}

#[component]
pub fn query_panel() -> impl IntoView {
    let location = use_location();
    let query_params = use_query_map();

    // Pre-fill the editor from ?template=… (urlencoded DocQL source).
    let initial_template = query_params
        .get_untracked()
        .get("template")
        .filter(|t| !t.trim().is_empty());
    let autorun = query_params
        .get_untracked()
        .get("run")
        .map(|r| r == "1" || r == "true")
        .unwrap_or(false);

    let (query, set_query) = signal(initial_template.clone().unwrap_or_default());
    let (diagnostics, set_diagnostics) = signal(Vec::<LspDiagnostic>::new());

    let current_doc = Memo::new(move |_| doc_id_from_path(&location.pathname.get()));

    // Template execution: bumping `run_request` (re)runs the resource.
    type RunKey = (String, String, u32);
    let run_request: RwSignal<Option<RunKey>> = RwSignal::new(
        match (autorun, initial_template, current_doc.get_untracked()) {
            (true, Some(template), Some(doc)) => Some((doc, template, 0)),
            _ => None,
        },
    );
    let run_result = Resource::new(
        move || run_request.get(),
        |request| async move {
            match request {
                Some((doc, template, _nonce)) => Some(run_doc_template(doc, template).await),
                None => None,
            }
        },
    );

    let execute = move || {
        let template = query.get_untracked();
        if template.trim().is_empty() {
            return;
        }
        if let Some(doc) = current_doc.get_untracked() {
            let nonce = run_request
                .get_untracked()
                .map(|(_, _, n)| n.wrapping_add(1))
                .unwrap_or(0);
            run_request.set(Some((doc, template, nonce)));
        }
    };

    let (tx, rx) = mpsc::channel::<Result<String, ServerFnError>>(8);
    let (connected, set_connected) = signal(false);
    let tx = StoredValue::new(tx);

    // Handle WebSocket connection on client side
    #[cfg(feature = "hydrate")]
    {
        use futures::StreamExt;

        wasm_bindgen_futures::spawn_local(async move {
            match lsp_websocket(rx.into()).await {
                Ok(mut messages) => {
                    set_connected.set(true);
                    log!("Connected to DocQL Language Server");

                    send_lsp_initialize(&tx).await;
                    // Seed the server's document state with the current
                    // buffer so position-aware completions work before the
                    // first edit.
                    send_lsp_did_change(&tx, &query.get_untracked());

                    while let Some(msg) = messages.next().await {
                        match msg {
                            Ok(response) => {
                                handle_lsp_response(&response, &set_diagnostics).await;
                            }
                            Err(e) => log!("LSP Error: {:?}", e),
                        }
                    }

                    set_connected.set(false);
                    log!("Disconnected from DocQL Language Server");
                }
                Err(e) => {
                    leptos::logging::warn!("Failed to connect: {e}");
                    set_connected.set(false);
                }
            }
        });
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = (rx, set_diagnostics, set_connected);

    // Handle query changes and send to LSP
    #[cfg(feature = "hydrate")]
    let on_query_change = {
        let tx = tx.clone();
        move |text: String| {
            set_query.set(text.clone());

            if connected.get_untracked() {
                send_lsp_did_change(&tx, &text);
            }
        }
    };

    // Fallback handler for non-hydrate mode
    #[cfg(not(feature = "hydrate"))]
    let on_textarea_input = {
        let tx = tx.clone();
        move |ev: web_sys::Event| {
            let value = event_target_value(&ev);
            set_query.set(value.clone());

            if connected.get_untracked() {
                send_lsp_did_change(&tx, &value);
            }
        }
    };

    // Node ref for the textarea that will be converted to CodeMirror
    let textarea_ref = NodeRef::<leptos::html::Textarea>::new();

    // Initialize CodeMirror editor after mount
    #[cfg(feature = "hydrate")]
    {
        let on_change_clone = on_query_change.clone();
        let textarea_ref_clone = textarea_ref.clone();

        Effect::new(move |_| {
            if let Some(textarea_element) = textarea_ref_clone.get() {
                let textarea_element = textarea_element
                    .clone()
                    .unchecked_into::<HtmlTextAreaElement>();

                let options = EditorOptions::default().line_numbers(true);

                let editor = Editor::from_text_area(&textarea_element, &options);

                // Initial content: URL-provided template, else a valid
                // runnable starter.
                let initial = query.get_untracked();
                if initial.trim().is_empty() {
                    editor.set_value(STARTER_TEMPLATE);
                } else {
                    editor.set_value(&initial);
                }

                // Set up change handler
                let on_change_effect = on_change_clone.clone();
                editor.on_change(move |editor, _change| {
                    if let Some(value) = editor.value() {
                        on_change_effect(value);
                    }
                });

                // Recover the raw CM5 instance for cursor insertion, key
                // bindings, and show-hint (DV-012).
                cm::capture_instance(&textarea_element);

                // Async hint function backed by the LSP completion request.
                let tx_for_hint = tx;
                let hint_fn = Closure::wrap(Box::new(
                    move |cm_js: JsValue, callback: js_sys::Function| {
                        cm::request_completion(&cm_js, callback, &tx_for_hint);
                    },
                )
                    as Box<dyn FnMut(JsValue, js_sys::Function)>);
                let hint_js: JsValue = hint_fn.as_ref().clone();
                let _ = js_sys::Reflect::set(&hint_js, &"async".into(), &JsValue::TRUE);
                hint_fn.forget();

                // Ctrl-Space → showHint (no-op when the addon is missing).
                let hint_for_trigger = hint_js.clone();
                let trigger_hint = Closure::wrap(Box::new(move |cm_js: JsValue| {
                    let show = js_sys::Reflect::get(&cm_js, &"showHint".into()).ok();
                    let Some(show) = show.filter(|f| f.is_function()) else {
                        leptos::logging::warn!("CodeMirror show-hint addon not loaded");
                        return;
                    };
                    let opts = js_sys::Object::new();
                    let _ = js_sys::Reflect::set(&opts, &"hint".into(), &hint_for_trigger);
                    let _ = js_sys::Reflect::set(
                        &opts,
                        &"completeSingle".into(),
                        &JsValue::FALSE,
                    );
                    let _ = show
                        .unchecked_into::<js_sys::Function>()
                        .call1(&cm_js, &opts);
                }) as Box<dyn FnMut(JsValue)>);

                // Ctrl/Cmd-Enter inside the editor → execute (the original
                // textarea's keydown never fires once CM owns input, so the
                // one-keystroke run flow lives here).
                let run = execute;
                let run_key = Closure::wrap(Box::new(move |_cm_js: JsValue| {
                    run();
                }) as Box<dyn FnMut(JsValue)>);

                let keys = js_sys::Object::new();
                let _ = js_sys::Reflect::set(&keys, &"Ctrl-Enter".into(), run_key.as_ref());
                let _ = js_sys::Reflect::set(&keys, &"Cmd-Enter".into(), run_key.as_ref());
                let _ =
                    js_sys::Reflect::set(&keys, &"Ctrl-Space".into(), trigger_hint.as_ref());
                cm::with_instance(|inst| inst.set_option("extraKeys", &keys));
                run_key.forget();
                trigger_hint.forget();

                log!("CodeMirror editor initialized successfully");
            }
        });
    }

    // Consume the insert bus (DV-012): render the snippet spec against the
    // live buffer (names stay unique) and insert at the cursor. Created
    // AFTER the editor-init effect so a panel opened by an insert action has
    // its editor by the time this first runs.
    if let Some(bus) = use_context::<InsertBus>() {
        Effect::new(move |_| {
            let Some((spec, _nonce)) = bus.0.get() else {
                return;
            };
            #[cfg(feature = "hydrate")]
            {
                let pristine = cm::with_instance(|cm| {
                    cm.get_value().trim() == STARTER_TEMPLATE.trim()
                })
                .unwrap_or(false);
                let buffer = if pristine {
                    String::new()
                } else {
                    cm::with_instance(|cm| cm.get_value())
                        .unwrap_or_else(|| query.get_untracked())
                };
                let text = render_snippet(&spec, &buffer);
                if !cm::insert_at_cursor(&text) {
                    set_query.update(|q| {
                        if !q.trim().is_empty() {
                            q.push_str("\n\n");
                        }
                        q.push_str(&text);
                        q.push('\n');
                    });
                }
            }
            #[cfg(not(feature = "hydrate"))]
            {
                let text = render_snippet(&spec, &query.get_untracked());
                set_query.update(|q| {
                    if !q.trim().is_empty() {
                        q.push_str("\n\n");
                    }
                    q.push_str(&text);
                    q.push('\n');
                });
            }
            bus.0.set(None); // consumed — re-mounts must not re-insert
        });
    }

    view! {
        <div class="fixed bottom-0 left-0 right-0 bg-white border-t border-gray-200 shadow-lg transition-all duration-300 ease-in-out z-20">
            <div class="flex flex-col" style="max-height:70vh">
                <div class="p-3 border-b border-gray-200">
                    <div class="flex justify-between items-center">
                        <div>
                            <h3 class="text-lg font-semibold text-gray-900">DocQL Query Editor</h3>
                            <p class="text-sm text-gray-600">
                                {move || match current_doc.get() {
                                    Some(doc) => format!("Runs against the open document {doc}"),
                                    None => "Open a document to run templates against it".to_string(),
                                }}
                            </p>
                        </div>
                        <div class="flex items-center space-x-2">
                            <div class={move || {
                                if connected.get() {
                                    "w-2 h-2 bg-green-400 rounded-full"
                                } else {
                                    "w-2 h-2 bg-red-400 rounded-full"
                                }
                            }}></div>
                            <span class="text-xs text-gray-500">
                                {move || {
                                    if connected.get() {
                                        "LSP Connected"
                                    } else {
                                        "LSP Disconnected"
                                    }
                                }}
                            </span>
                        </div>
                    </div>
                </div>
                <div class="h-48 p-3 relative shrink-0">
                    <textarea
                        node_ref=textarea_ref
                        class="w-full h-full resize-none border border-gray-300 rounded-md p-3 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                        placeholder="Enter your DocQL query here...\n\nExample:\nSection(match=\"Introduction\") {\n    TextChunk(chunkSize=500)\n}"
                        prop:value=move || query.get()
                        on:input={
                            #[cfg(feature = "hydrate")]
                            {
                                move |_| {} // CodeMirror handles this
                            }
                            #[cfg(not(feature = "hydrate"))]
                            {
                                on_textarea_input
                            }
                        }
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            if ev.ctrl_key() && ev.key() == "Enter" {
                                execute();
                            }
                        }
                    />

                    // Show diagnostics overlay
                    {move || {
                        let diags = diagnostics.get();
                        if !diags.is_empty() {
                            view! {
                                <div class="absolute top-4 right-4 max-w-xs">
                                    {diags.into_iter().map(|diag| {
                                        let color_class = match diag.severity.as_str() {
                                            "error" => "bg-red-100 border-red-400 text-red-700",
                                            "warning" => "bg-yellow-100 border-yellow-400 text-yellow-700",
                                            _ => "bg-blue-100 border-blue-400 text-blue-700"
                                        };
                                        view! {
                                            <div class={format!("mb-2 p-2 border rounded text-xs {}", color_class)}>
                                                <div class="font-medium">Line {diag.line + 1}:{diag.column + 1}</div>
                                                <div>{diag.message}</div>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            }.into_any()
                        } else {
                            view! { <div></div> }.into_any()
                        }
                    }}
                </div>
                // Template execution results
                <div class="px-3 pb-2" style="flex:1 1 auto;min-height:0;overflow-y:auto">
                    <Suspense fallback=move || view! {
                        <div class="flex items-center text-sm text-blue-600 py-2">
                            <div class="animate-spin rounded-full h-4 w-4 border-b-2 border-blue-600 mr-2"></div>
                            "Executing template..."
                        </div>
                    }>
                        {move || run_result.get().flatten().map(|result| match result {
                            Ok(TemplateRun { ok: true, output, .. }) => view! {
                                <div class="border border-gray-200 rounded-md">
                                    <div class="px-3 py-1.5 bg-gray-50 border-b border-gray-200 text-xs font-medium text-gray-600">
                                        "Template outputs"
                                    </div>
                                    <pre class="p-3 text-xs font-mono text-gray-800" style="max-height:18rem;overflow:auto">
                                        {output.unwrap_or_default()}
                                    </pre>
                                </div>
                            }.into_any(),
                            Ok(TemplateRun { error, .. }) => view! {
                                <div class="border border-red-300 bg-red-50 rounded-md p-3">
                                    <div class="text-xs font-semibold text-red-700 mb-1">"Template failed"</div>
                                    <div class="text-xs text-red-700 font-mono whitespace-pre-wrap">
                                        {error.unwrap_or_else(|| "unknown error".to_string())}
                                    </div>
                                </div>
                            }.into_any(),
                            Err(e) => view! {
                                <div class="border border-red-300 bg-red-50 rounded-md p-3">
                                    <div class="text-xs font-semibold text-red-700 mb-1">"Request failed"</div>
                                    <div class="text-xs text-red-700 font-mono whitespace-pre-wrap">{e.to_string()}</div>
                                </div>
                            }.into_any(),
                        })}
                    </Suspense>
                </div>
                <div class="p-3 border-t border-gray-200 bg-gray-50">
                    <div class="flex justify-between items-center">
                        <div class="text-xs text-gray-500">
                            "Ctrl+Enter runs • Ctrl+Space completes • "
                            {move || format!("{} diagnostics", diagnostics.get().len())}
                        </div>
                        <button
                            class="px-4 py-2 bg-blue-600 text-white text-sm font-medium rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 transition-colors duration-200 disabled:opacity-50"
                            disabled=move || query.get().trim().is_empty() || current_doc.get().is_none()
                            on:click=move |_| execute()
                        >
                            "Execute Query"
                        </button>
                    </div>
                </div>
            </div>
        </div>
    }
}

// Helper functions for LSP communication
#[cfg(feature = "hydrate")]
async fn send_lsp_initialize(tx: &StoredValue<mpsc::Sender<Result<String, ServerFnError>>>) {
    let init_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "clientInfo": {
                "name": "DocQL Query Editor",
                "version": "0.2.0"
            },
            "capabilities": {
                "textDocument": {
                    "synchronization": {
                        "didOpen": true,
                        "didChange": true
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true
                        }
                    }
                }
            }
        }
    });

    if let Ok(msg) = serde_json::to_string(&init_request) {
        let _ = tx.with_value(|tx| tx.clone().try_send(Ok(msg)));
    }
}

#[cfg(feature = "hydrate")]
async fn handle_lsp_response(response: &str, set_diagnostics: &WriteSignal<Vec<LspDiagnostic>>) {
    if let Ok(parsed) = serde_json::from_str::<Value>(response) {
        match parsed.get("method").and_then(|m| m.as_str()) {
            Some("textDocument/publishDiagnostics") => {
                parse_and_set_diagnostics(&parsed, set_diagnostics);
            }
            Some(_) => {}
            None => {
                // Response to a request we sent — completion ids are owned
                // by the hint machinery.
                cm::try_complete_hint(&parsed);
            }
        }
    }
}

#[cfg(feature = "hydrate")]
fn parse_and_set_diagnostics(parsed: &Value, set_diagnostics: &WriteSignal<Vec<LspDiagnostic>>) {
    if let Some(params) = parsed.get("params") {
        if let Some(diags) = params.get("diagnostics").and_then(|d| d.as_array()) {
            let parsed_diagnostics: Vec<LspDiagnostic> = diags
                .iter()
                .filter_map(|d| {
                    let range = d.get("range")?;
                    let start = range.get("start")?;
                    let line = start.get("line")?.as_u64()? as u32;
                    let character = start.get("character")?.as_u64()? as u32;
                    let message = d.get("message")?.as_str()?.to_string();
                    let severity = match d.get("severity")?.as_u64()? {
                        1 => "error",
                        2 => "warning",
                        3 => "info",
                        _ => "hint",
                    };
                    Some(LspDiagnostic {
                        line,
                        column: character,
                        message,
                        severity: severity.to_string(),
                    })
                })
                .collect();
            set_diagnostics.set(parsed_diagnostics);
        }
    }
}

fn send_lsp_did_change(tx: &StoredValue<mpsc::Sender<Result<String, ServerFnError>>>, text: &str) {
    let did_change = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {
                "uri": "file:///query.docql",
                "version": 1
            },
            "contentChanges": [{
                "text": text
            }]
        }
    });

    if let Ok(msg) = serde_json::to_string(&did_change) {
        let _ = tx.with_value(|tx| tx.clone().try_send(Ok(msg)));
    }
}
