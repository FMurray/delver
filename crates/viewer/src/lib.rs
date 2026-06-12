pub mod app;
pub mod components;
// pub mod event_panel;
// pub mod match_panel;
// pub mod rendering;
pub mod query_tree;
pub mod snippets;
pub mod store;
// pub mod stubs;
// pub mod ui_controls;
// pub mod utils;

#[cfg(feature = "ssr")]
pub mod language_server;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    use crate::app::App;
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
