use leptos::html::*;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;

use crate::store::DocumentStore;

#[component]
pub fn PdfViewer() -> impl IntoView {
    let params = use_params_map();
    let store = expect_context::<DocumentStore>();

    let doc_id = Memo::new(move |_| {
        params.with(|params| {
            params
                .get("doc_id")
                .and_then(|id| Uuid::parse_str(&id).ok())
        })
    });

    let page_id = Memo::new(move |_| {
        params.with(|params| {
            params
                .get("page_id")
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or(0)
        })
    });

    let document = Memo::new(move |_| {
        if let Some(id) = doc_id.get() {
            store.get_document(id)
        } else {
            None
        }
    });

    div()
        .class("h-full flex flex-col bg-gray-50")
        .child(move || {
            if let Some(doc) = document.get() {
                div()
                    .class("h-full flex flex-col")
                    .child((
                        // Header with document info
                        header()
                            .class("bg-white border-b border-gray-200 px-6 py-4")
                            .child(
                                div().class("flex items-center justify-between").child((
                                    div().child((
                                        h1().class("text-xl font-semibold text-gray-900")
                                            .child(doc.name.clone()),
                                        p().class("text-sm text-gray-600").child(format!(
                                            "Page {} of {}",
                                            page_id.get() + 1,
                                            doc.total_pages
                                        )),
                                    )),
                                    div().class("flex items-center space-x-4").child(
                                        span().class("text-sm text-gray-500").child(move || {
                                            format!("{} / {}", page_id.get() + 1, doc.total_pages)
                                        }),
                                    ),
                                )),
                            ),
                        // PDF page content
                        main().class("flex-1 p-6 overflow-auto").child(
                            div().class("flex justify-center").child(
                                div().class("bg-white rounded-lg shadow-lg p-4").child(
                                    div().class("text-center").child((
                                        h3().class("text-lg font-medium text-gray-900 mb-2")
                                            .child(format!("Page {}", page_id.get() + 1)),
                                        p().class("text-gray-600")
                                            .child("PDF page rendering will be implemented here."),
                                        p().class("text-sm text-gray-500 mt-2")
                                            .child(format!("Document ID: {}", doc.id)),
                                    )),
                                ),
                            ),
                        ),
                    ))
                    .into_any()
            } else {
                div()
                    .class("flex-1 flex items-center justify-center")
                    .child(
                        div().class("text-center").child((
                            h2().class("text-xl font-semibold text-gray-900 mb-2")
                                .child("Document Not Found"),
                            p().class("text-gray-600")
                                .child("The requested document could not be found."),
                        )),
                    )
                    .into_any()
            }
        })
}
