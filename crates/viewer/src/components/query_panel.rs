use leptos::logging::log;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use server_fn::{codec::JsonEncoding, BoxedStream, ServerFnError, Websocket};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub severity: String,
}

#[server(protocol = Websocket<JsonEncoding, JsonEncoding>)]
async fn lsp_websocket(
    input: BoxedStream<String, ServerFnError>,
) -> Result<BoxedStream<String, ServerFnError>, ServerFnError> {
    use futures::{channel::mpsc, SinkExt, StreamExt};

    let mut input = input;
    let (mut tx, rx) = mpsc::channel(100);

    tokio::spawn(async move {
        // Process incoming LSP requests
        while let Some(msg) = input.next().await {
            if let Ok(msg_str) = msg {
                println!("Received LSP message: {}", msg_str);

                // Parse as basic JSON to extract method and params
                if let Ok(request) = serde_json::from_str::<Value>(&msg_str) {
                    if let Some(method) = request.get("method").and_then(|m| m.as_str()) {
                        println!("Processing LSP method: {}", method);

                        let response = match method {
                            "initialize" => {
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": request.get("id"),
                                    "result": {
                                        "capabilities": {
                                            "textDocumentSync": 1,
                                            "completionProvider": {
                                                "resolveProvider": false,
                                                "triggerCharacters": ["<", "(", "\"", " "]
                                            },
                                            "hoverProvider": true
                                        }
                                    }
                                })
                            }
                            "textDocument/didChange" => {
                                // Parse the text content for validation
                                if let Some(params) = request.get("params") {
                                    if let Some(changes) =
                                        params.get("contentChanges").and_then(|c| c.as_array())
                                    {
                                        if let Some(first_change) = changes.first() {
                                            if let Some(text) =
                                                first_change.get("text").and_then(|t| t.as_str())
                                            {
                                                // Simple validation using basic string matching
                                                let has_error = !text.trim().is_empty()
                                                    && !text.contains("Section")
                                                    && !text.contains("TextChunk")
                                                    && !text.contains("Image");

                                                if has_error {
                                                    let diagnostic_response = json!({
                                                        "jsonrpc": "2.0",
                                                        "method": "textDocument/publishDiagnostics",
                                                        "params": {
                                                            "uri": "file:///query.docql",
                                                            "diagnostics": [{
                                                                "range": {
                                                                    "start": {"line": 0, "character": 0},
                                                                    "end": {"line": 0, "character": 1}
                                                                },
                                                                "severity": 1,
                                                                "message": "Expected DocQL element (Section, TextChunk, Image, etc.)"
                                                            }]
                                                        }
                                                    });

                                                    let _ = tx
                                                        .send(Ok(diagnostic_response.to_string()))
                                                        .await;
                                                } else {
                                                    // Clear diagnostics
                                                    let clear_diagnostics = json!({
                                                        "jsonrpc": "2.0",
                                                        "method": "textDocument/publishDiagnostics",
                                                        "params": {
                                                            "uri": "file:///query.docql",
                                                            "diagnostics": []
                                                        }
                                                    });

                                                    let _ = tx
                                                        .send(Ok(clear_diagnostics.to_string()))
                                                        .await;
                                                }
                                            }
                                        }
                                    }
                                }
                                continue; // No direct response for didChange
                            }
                            _ => {
                                // Generic acknowledgment for other methods
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": request.get("id"),
                                    "result": null
                                })
                            }
                        };

                        let _ = tx.send(Ok(response.to_string())).await;
                    }
                }
            }
        }
    });

    Ok(rx.into())
}

#[component]
pub fn query_panel() -> impl IntoView {
    let (query, set_query) = signal(String::new());
    let (diagnostics, set_diagnostics) = signal(Vec::<LspDiagnostic>::new());

    use futures::channel::mpsc;
    let (tx, rx) = mpsc::channel::<Result<String, ServerFnError>>(1);
    let (connected, set_connected) = signal(false);
    let tx = StoredValue::new(tx);

    // we'll only listen for websocket messages on the client
    #[cfg(feature = "hydrate")]
    {
        use futures::StreamExt;

        wasm_bindgen_futures::spawn_local(async move {
            match lsp_websocket(rx.into()).await {
                Ok(mut messages) => {
                    set_connected.set(true);
                    log!("Connected to DocQL Language Server");

                    // Send initialize request
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

                    while let Some(msg) = messages.next().await {
                        match msg {
                            Ok(response) => {
                                log!("Received LSP message: {}", response);

                                if let Ok(parsed) = serde_json::from_str::<Value>(&response) {
                                    if let Some(method) =
                                        parsed.get("method").and_then(|m| m.as_str())
                                    {
                                        match method {
                                            "textDocument/publishDiagnostics" => {
                                                if let Some(params) = parsed.get("params") {
                                                    if let Some(diags) = params
                                                        .get("diagnostics")
                                                        .and_then(|d| d.as_array())
                                                    {
                                                        let parsed_diagnostics: Vec<LspDiagnostic> =
                                                            diags
                                                                .iter()
                                                                .filter_map(|d| {
                                                                    let range = d.get("range")?;
                                                                    let start =
                                                                        range.get("start")?;
                                                                    let line = start
                                                                        .get("line")?
                                                                        .as_u64()?
                                                                        as u32;
                                                                    let character = start
                                                                        .get("character")?
                                                                        .as_u64()?
                                                                        as u32;
                                                                    let message = d
                                                                        .get("message")?
                                                                        .as_str()?
                                                                        .to_string();
                                                                    let severity = match d
                                                                        .get("severity")?
                                                                        .as_u64()?
                                                                    {
                                                                        1 => "error",
                                                                        2 => "warning",
                                                                        3 => "info",
                                                                        _ => "hint",
                                                                    };
                                                                    Some(LspDiagnostic {
                                                                        line,
                                                                        column: character,
                                                                        message,
                                                                        severity: severity
                                                                            .to_string(),
                                                                    })
                                                                })
                                                                .collect();
                                                        set_diagnostics.set(parsed_diagnostics);
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
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
    let on_query_change = {
        let tx = tx.clone();
        move |ev: web_sys::Event| {
            let value = event_target_value(&ev);
            set_query.set(value.clone());

            if connected.get() {
                let did_change = json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": "file:///query.docql",
                            "version": 1
                        },
                        "contentChanges": [{
                            "text": value
                        }]
                    }
                });

                if let Ok(msg) = serde_json::to_string(&did_change) {
                    let _ = tx.with_value(|tx| tx.clone().try_send(Ok(msg)));
                }
            }
        }
    };

    view! {
        <div class="fixed bottom-0 left-0 right-0 bg-white border-t border-gray-200 shadow-lg transition-all duration-300 ease-in-out">
            <div class="h-64 flex flex-col">
                <div class="p-4 border-b border-gray-200">
                    <div class="flex justify-between items-center">
                        <div>
                            <h3 class="text-lg font-semibold text-gray-900">DocQL Query Editor</h3>
                            <p class="text-sm text-gray-600 mt-1">Write queries to search and analyze your documents</p>
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
                            <button
                                class="text-xs px-2 py-1 bg-blue-100 text-blue-600 rounded hover:bg-blue-200"
                                on:click=move |_| {
                                    // Connection control will be handled by the server function
                                    log!("Connection toggle clicked - functionality to be implemented");
                                }
                            >
                                {move || if connected.get() { "Disconnect" } else { "Connect" }}
                            </button>
                        </div>
                    </div>
                </div>
                <div class="flex-1 p-4 relative">
                    <textarea
                        class="w-full h-full resize-none border border-gray-300 rounded-md p-3 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                        placeholder="Enter your DocQL query here...\n\nExample:\nSection(match=\"Introduction\") {\n    TextChunk(chunkSize=500)\n}"
                        prop:value=move || query.get()
                        on:input=on_query_change
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            if ev.ctrl_key() && ev.key() == "Enter" {
                                // Execute query logic here
                                log!("Executing query...");
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
                <div class="p-4 border-t border-gray-200 bg-gray-50">
                    <div class="flex justify-between items-center">
                        <div class="text-xs text-gray-500">
                            "Press Ctrl+Enter to execute query • "
                            {move || format!("{} diagnostics", diagnostics.get().len())}
                        </div>
                        <button
                            class="px-4 py-2 bg-blue-600 text-white text-sm font-medium rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 transition-colors duration-200 disabled:opacity-50"
                            disabled=move || query.get().trim().is_empty()
                            on:click=move |_| {
                                log!("Executing query...");
                                // Add query execution logic here
                            }
                        >
                            "Execute Query"
                        </button>
                    </div>
                </div>
            </div>
        </div>
    }
}
