use leptos::html::*;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use leptos::ev;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

use crate::store::DocumentPage;
use crate::components::file_upload::{get_pdf_page, get_documents};

// Helper function to render RGBA data to a canvas
fn render_rgba_to_canvas(canvas: &HtmlCanvasElement, rgba_data: &[u8], width: u32, height: u32) {
    canvas.set_width(width);
    canvas.set_height(height);
    
    let context = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()
        .unwrap();
    
    // Create ImageData from RGBA bytes
    let image_data = ImageData::new_with_u8_clamped_array_and_sh(
        wasm_bindgen::Clamped(rgba_data),
        width,
        height,
    ).unwrap();
    
    context.put_image_data(&image_data, 0.0, 0.0).unwrap();
}



#[component]
pub fn PdfViewer() -> impl IntoView {
    use leptos::logging::log;
    
    log!("PdfViewer component initializing...");
    let params = use_params_map();

    let doc_id = Memo::new(move |_| {
        let result = params.with(|params| {
            params
                .get("doc_id")
                .and_then(|id| Uuid::parse_str(&id).ok())
        });
        log!("Doc ID memo updated: {:?}", result);
        result
    });

    let page_id = Memo::new(move |_| {
        let result = params.with(|params| {
            params
                .get("page_id")
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or(0)
        });
        log!("Page ID memo updated: {}", result);
        result
    });

    // Resource to load the document metadata
    let documents = Resource::new(
        move || (),
        |_| async move {
            get_documents().await.unwrap_or_default()
        }
    );

    let document = Memo::new(move |_| {
        if let Some(id) = doc_id.get() {
            documents.get().and_then(|docs| {
                docs.into_iter().find(|doc| doc.id == id)
            })
        } else {
            None
        }
    });

    // Resource for loading page images on demand
    let page_resource = Resource::new(
        move || (doc_id.get().map(|id| id.to_string()), page_id.get()),
        move |(doc_id_opt, page_idx)| async move {
            if let Some(doc_id) = doc_id_opt {
                log!("Loading page {} for document {}", page_idx, doc_id);
                get_pdf_page(doc_id, page_idx).await
            } else {
                Err(server_fn::ServerFnError::new(anyhow::anyhow!("No document ID")))
            }
        }
    );
    
    // Current page data from server action result
    let current_page = Memo::new(move |_| {
        page_resource.get().and_then(|result| {
            result.ok().map(|page_data| DocumentPage {
                page_index: page_data.page_index,
                image_data: page_data.image_data,
                width: page_data.width,
                height: page_data.height,
            })
        })
    });

    // Use a simple effect that runs when current_page changes
    let canvas_ref = NodeRef::<Canvas>::new();
    
    // Single effect that handles canvas rendering when page data is available
    Effect::new(move |_| {
        log!("Canvas effect triggered - checking if page and canvas are ready");
        
        // This effect will re-run whenever current_page changes
        if let Some(page) = current_page.get() {
            // Try to get the canvas element
            if let Some(canvas_element) = canvas_ref.get() {
                if let Ok(canvas) = canvas_element.dyn_into::<HtmlCanvasElement>() {
                    log!("Rendering page {} to canvas ({}x{}, {} bytes)", 
                        page.page_index, page.width as u32, page.height as u32, page.image_data.len());
                    
                    render_rgba_to_canvas(
                        &canvas,
                        &page.image_data,
                        page.width as u32,
                        page.height as u32,
                    );
                    
                    log!("Canvas render complete for page {}", page.page_index);
                } else {
                    log!("Canvas element not ready for casting");
                }
            } else {
                log!("Canvas element not mounted yet");
            }
        } else {
            log!("No page data available for canvas");
        }
    });

    log!("PdfViewer component rendering");
    div()
        .class("h-full flex flex-col bg-gray-50")
        .child(move || {
            log!("PdfViewer child function called");
            
            // Show loading state while documents are loading
            view! {
                <Suspense fallback=move || view! {
                    <div class="flex-1 flex items-center justify-center">
                        <div class="text-center">
                            <div class="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-600 mx-auto mb-4"></div>
                            <p class="text-gray-600">"Loading document..."</p>
                        </div>
                    </div>
                }>
                    {move || {
                if let Some(doc) = document.get() {
                    log!("Document available for rendering: {} ({} pages)", doc.name, doc.total_pages);
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
                                        div().class("flex items-center space-x-4").child((
                                            // Navigation buttons
                                            button()
                                                .class("px-3 py-1 bg-gray-100 text-gray-700 rounded hover:bg-gray-200 disabled:opacity-50")
                                                .prop("disabled", move || page_id.get() == 0)
                                                .on(ev::click, move |_| {
                                                    if page_id.get() > 0 {
                                                        let new_page = page_id.get() - 1;
                                                        let doc_id_str = doc_id.get().unwrap().to_string();
                                                        use leptos_router::hooks::use_navigate;
                                                        let navigate = use_navigate();
                                                        navigate(&format!("/viewer/{}/{}", doc_id_str, new_page), Default::default());
                                                    }
                                                })
                                                .child("← Previous"),
                                            span().class("text-sm text-gray-500").child(move || {
                                                format!("{} / {}", page_id.get() + 1, doc.total_pages)
                                            }),
                                            button()
                                                .class("px-3 py-1 bg-gray-100 text-gray-700 rounded hover:bg-gray-200 disabled:opacity-50")
                                                .prop("disabled", move || page_id.get() >= doc.total_pages - 1)
                                                .on(ev::click, move |_| {
                                                    if page_id.get() < doc.total_pages - 1 {
                                                        let new_page = page_id.get() + 1;
                                                        let doc_id_str = doc_id.get().unwrap().to_string();
                                                        use leptos_router::hooks::use_navigate;
                                                        let navigate = use_navigate();
                                                        navigate(&format!("/viewer/{}/{}", doc_id_str, new_page), Default::default());
                                                    }
                                                })
                                                .child("Next →")
                                        )),
                                    )),
                                ),
                            // PDF page content
                            main().class("flex-1 p-6 overflow-auto").child(
                                div().class("flex justify-center").child(
                                    div().class("bg-white rounded-lg shadow-lg p-4").child(
                                        move || {
                                            log!("PDF page content render function called");
                                            
                                            view! {
                                                <Suspense fallback=move || view! {
                                                    <div class="text-center">
                                                        <h3 class="text-lg font-medium text-gray-900 mb-4">
                                                            {format!("Page {}", page_id.get() + 1)}
                                                        </h3>
                                                        <div class="flex items-center justify-center space-x-2 mb-4">
                                                            <div class="animate-spin rounded-full h-6 w-6 border-b-2 border-blue-600"></div>
                                                            <p class="text-blue-600">"Loading page..."</p>
                                                        </div>
                                                    </div>
                                                }>
                                                    {move || {
                                                if let Some(page) = current_page.get() {
                                                    log!("Page data available for page {}", page.page_index);
                                                    // Use the actual rendered dimensions
                                                    let display_width = page.width as u32;
                                                    let display_height = page.height as u32;
                                                    log!("Page display dimensions: {}x{}", display_width, display_height);
                                                    
                                                    div().class("text-center").child((
                                                        h3().class("text-lg font-medium text-gray-900 mb-4")
                                                            .child(format!("Page {}", page_id.get() + 1)),
                                                        div().class("flex justify-center mb-2").child(
                                                            canvas()
                                                                .attr("width", display_width.to_string())
                                                                .attr("height", display_height.to_string())
                                                                .class("border border-gray-200 max-w-full h-auto")
                                                                .node_ref(canvas_ref)
                                                        ),
                                                        p().class("text-sm text-gray-500")
                                                            .child(format!("{}x{} pixels", display_width, display_height)),
                                                    )).into_any()
                                                } else {
                                                    log!("No page data available for page {}", page_id.get());
                                                    div().class("text-center").child((
                                                        h3().class("text-lg font-medium text-gray-900 mb-2")
                                                            .child(format!("Page {}", page_id.get() + 1)),
                                                        p().class("text-gray-600")
                                                            .child("Failed to load page. Please try again."),
                                                        p().class("text-sm text-gray-500 mt-2")
                                                            .child(format!("Document ID: {}", doc.id)),
                                                    )).into_any()
                                                }
                                                    }}
                                                </Suspense>
                                            }
                                        }
                                    ),
                                ),
                            ),
                        ))
                        .into_any()
                } else {
                    log!("No document available");
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
                    }}
                </Suspense>
            }
        })
}