//! DocQL editor (CodeMirror + LSP over a server-fn websocket) plus the
//! template execution panel: runs the editor's template against the
//! currently-open stored document via the same hydrate + `process_parsed`
//! path the CLI `query --doc` uses (DV-006).

use futures::channel::mpsc;
use leptos::logging::log;
use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_query_map};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use server_fn::{codec::JsonEncoding, BoxedStream, ServerFnError, Websocket};

use crate::store::TemplateRun;

#[cfg(feature = "hydrate")]
use codemirror::{DocApi, Editor, EditorOptions};
#[cfg(feature = "hydrate")]
use wasm_bindgen::prelude::*;
#[cfg(feature = "hydrate")]
use web_sys::HtmlTextAreaElement;

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
                    println!("Received LSP message: {}", msg_str);

                    // Process message through the language server
                    let responses = server.process_lsp_message(&msg_str).await;

                    // Send all responses
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
/// The panel lives outside the `<Routes>` tree, so `use_params_map` is not
/// available; the location pathname is.
fn doc_id_from_path(pathname: &str) -> Option<String> {
    let rest = pathname.strip_prefix("/viewer/")?;
    let id = rest.split('/').next()?;
    uuid::Uuid::parse_str(id).ok().map(|u| u.to_string())
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

    // We'll use a simple approach - just initialize CodeMirror in an effect
    // without storing the editor instance in reactive state

    use futures::channel::mpsc;
    let (tx, rx) = mpsc::channel::<Result<String, ServerFnError>>(1);
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

                    // Send initialize request
                    send_lsp_initialize(&tx).await;

                    // Handle incoming messages
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

                // Initial content: URL-provided template, else an example.
                let initial = query.get_untracked();
                if initial.trim().is_empty() {
                    editor.set_value("// Enter your DocQL query here...\n\n// Example:\n// Section(match=\"Introduction\") {\n//     TextChunk(chunkSize=500)\n// }");
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

                log!("CodeMirror editor initialized successfully");
            }
        });
    }

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
                            "Press Ctrl+Enter to execute • "
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
                "version": "0.1.0"
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
                    },
                    "hover": {}
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
    log!("Received LSP message: {}", response);

    if let Ok(parsed) = serde_json::from_str::<Value>(response) {
        if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
            match method {
                "textDocument/publishDiagnostics" => {
                    parse_and_set_diagnostics(&parsed, set_diagnostics);
                }
                _ => {}
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
