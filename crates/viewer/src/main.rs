#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use viewer::app::*;

    let conf = get_configuration(None).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;
    // Generate the list of routes in your Leptos App
    let routes = generate_route_list(App);

    let app = Router::new()
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(leptos_options);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

// async fn docql_lsp_handler(ws: WebSocketUpgrade) -> Response {
//     ws.on_upgrade(handle_docql_lsp_socket)
// }

// async fn handle_docql_lsp_socket(socket: WebSocket) {
//     log!("New DocQL LSP WebSocket connection established");

//     let (mut sender, mut receiver) = socket.split();

//     // Create channels for communication between WebSocket and LSP
//     let (lsp_sender, mut lsp_receiver) = tokio::sync::mpsc::channel(100);
//     let (response_sender, mut response_receiver) = tokio::sync::mpsc::channel(100);

//     // Task to handle WebSocket messages and forward to LSP
//     let ws_to_lsp = tokio::spawn(async move {
//         while let Some(msg) = receiver.next().await {
//             match msg {
//                 Ok(Message::Text(text)) => {
//                     log!("Received from WebSocket: {}", text);
//                     if let Ok(request) = serde_json::from_str::<jsonrpc::Request>(&text) {
//                         if let Err(e) = lsp_sender.send(request).await {
//                             log!("Failed to send to LSP: {}", e);
//                             break;
//                         }
//                     }
//                 }
//                 Ok(Message::Close(_)) => {
//                     log!("WebSocket connection closed");
//                     break;
//                 }
//                 Err(e) => {
//                     log!("WebSocket error: {}", e);
//                     break;
//                 }
//                 _ => {}
//             }
//         }
//     });

//     // Task to handle LSP responses and forward to WebSocket
//     let lsp_to_ws = tokio::spawn(async move {
//         while let Some(response) = response_receiver.recv().await {
//             let json_str = serde_json::to_string(&response).unwrap_or_default();
//             log!("Sending to WebSocket: {}", json_str);
//             if let Err(e) = sender.send(Message::Text(json_str)).await {
//                 log!("Failed to send WebSocket message: {}", e);
//                 break;
//             }
//         }
//     });

//     // Simple LSP handler that processes requests manually
//     tokio::spawn(async move {
//         let server = DocQLLanguageServer::new(MockClient {
//             sender: response_sender,
//         });

//         while let Some(request) = lsp_receiver.recv().await {
//             log!("Processing LSP request: {}", request.method);

//             let response = match request.method.as_str() {
//                 "initialize" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         match server.initialize(params).await {
//                             Ok(result) => Some(jsonrpc::Response::ok(request.id, result)),
//                             Err(e) => Some(jsonrpc::Response::error(request.id, e)),
//                         }
//                     } else {
//                         Some(jsonrpc::Response::error(
//                             request.id,
//                             jsonrpc::Error::invalid_params(),
//                         ))
//                     }
//                 }
//                 "initialized" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         server.initialized(params).await;
//                     }
//                     None // No response for notification
//                 }
//                 "textDocument/didOpen" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         server.did_open(params).await;
//                     }
//                     None // No response for notification
//                 }
//                 "textDocument/didChange" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         server.did_change(params).await;
//                     }
//                     None // No response for notification
//                 }
//                 "textDocument/completion" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         match server.completion(params).await {
//                             Ok(result) => Some(jsonrpc::Response::ok(request.id, result)),
//                             Err(e) => Some(jsonrpc::Response::error(request.id, e)),
//                         }
//                     } else {
//                         Some(jsonrpc::Response::error(
//                             request.id,
//                             jsonrpc::Error::invalid_params(),
//                         ))
//                     }
//                 }
//                 "textDocument/hover" => {
//                     if let Ok(params) = serde_json::from_value(request.params.unwrap_or_default()) {
//                         match server.hover(params).await {
//                             Ok(result) => Some(jsonrpc::Response::ok(request.id, result)),
//                             Err(e) => Some(jsonrpc::Response::error(request.id, e)),
//                         }
//                     } else {
//                         Some(jsonrpc::Response::error(
//                             request.id,
//                             jsonrpc::Error::invalid_params(),
//                         ))
//                     }
//                 }
//                 "shutdown" => match server.shutdown().await {
//                     Ok(result) => Some(jsonrpc::Response::ok(request.id, result)),
//                     Err(e) => Some(jsonrpc::Response::error(request.id, e)),
//                 },
//                 _ => {
//                     log!("Unhandled LSP method: {}", request.method);
//                     Some(jsonrpc::Response::error(
//                         request.id,
//                         jsonrpc::Error::method_not_found(),
//                     ))
//                 }
//             };

//             if let Some(resp) = response {
//                 if let Err(e) = server.client.sender.send(resp).await {
//                     log!("Failed to send response: {}", e);
//                     break;
//                 }
//             }
//         }
//     });

//     // Wait for either task to complete
//     tokio::select! {
//         _ = ws_to_lsp => {},
//         _ = lsp_to_ws => {},
//     }
// }

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // no client-side main function
}

// When compiling to web using trunk:
// #[cfg(target_arch = "wasm32")]
// fn main() {
// use eframe::wasm_bindgen::JsCast as _;

// // Redirect `log` message to `console.log` and friends:
// eframe::WebLogger::init(log::LevelFilter::Debug).ok();

// let web_options = eframe::WebOptions::default();

// wasm_bindgen_futures::spawn_local(async {
//     let document = web_sys::window()
//         .expect("No window")
//         .document()
//         .expect("No document");

//     let canvas = document
//         .get_element_by_id("the_canvas_id")
//         .expect("Failed to find the_canvas_id")
//         .dyn_into::<web_sys::HtmlCanvasElement>()
//         .expect("the_canvas_id was not a HtmlCanvasElement");

//     let start_result = eframe::WebRunner::new()
//         .start(
//             canvas,
//             web_options,
//             Box::new(|_cc| Ok(Box::new(viewer::app::AppWrapper::new()))),
//         )
//         .await;

//     // Remove the loading text and spinner:
//     if let Some(loading_text) = document.get_element_by_id("loading_text") {
//         match start_result {
//             Ok(_) => {
//                 loading_text.remove();
//             }
//             Err(e) => {
//                 loading_text.set_inner_html(
//                     "<p> The app has crashed. See the developer console for details. </p>",
//                 );
//                 panic!("Failed to start eframe: {e:?}");
//             }
//         }
//     }
// });
// }
