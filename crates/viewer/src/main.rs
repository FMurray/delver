#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use leptos::logging::log;
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

    // run our app with hyper
    // `axum::Server` is a re-export of `hyper::Server`
    log!("listening on http://{}", &addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

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
