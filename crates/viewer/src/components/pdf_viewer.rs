use leptos::html::*;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use uuid::Uuid;
use base64::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

use crate::store::DocumentStore;
use crate::components::file_upload::PageImageData;
use server_fn::ServerFnError;

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

// Helper function to convert RGBA data to a PNG data URL for img src (fallback)
fn rgba_to_png_data_url(rgba_data: &[u8], width: u32, height: u32) -> Option<String> {
    use image::{ImageBuffer, Rgba, DynamicImage};
    
    // Create an image buffer from RGBA data - clone the data to get owned Vec<u8>
    let img_buffer = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, rgba_data.to_vec())?;
    let dynamic_img = DynamicImage::ImageRgba8(img_buffer);
    
    // Encode to PNG bytes
    let mut png_bytes = Vec::new();
    if dynamic_img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png).is_ok() {
        let base64_data = BASE64_STANDARD.encode(&png_bytes);
        Some(format!("data:image/png;base64,{}", base64_data))
    } else {
        None
    }
}

#[component]
pub fn PdfViewer() -> impl IntoView {
    use leptos::logging::log;
    
    log!("PdfViewer component initializing...");
    let params = use_params_map();
    let store = expect_context::<DocumentStore>();

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

    let document = Memo::new(move |_| {
        let result = if let Some(id) = doc_id.get() {
            log!("Fetching document from store: {}", id);
            let doc = store.get_document(id);
            log!("Document fetched: {}", doc.is_some());
            doc
        } else {
            log!("No document ID available");
            None
        };
        result
    });

    // Server action for loading page images on demand
    let load_page_action = Action::new_local(move |(doc_id, page_index): &(String, usize)| {
        let doc_id = doc_id.clone();
        let page_index = *page_index;
        async move {
            log!("Loading page {} for document {}", page_index, doc_id);
            // TODO: Call the render_pdf_page server function once import issue is resolved
            // For now, return a simulated result to demonstrate the structure
            use crate::store::DocumentPage;
            Err(ServerFnError::new(anyhow::anyhow!("Page {} for document {} - server function import needs to be fixed", page_index, doc_id))) as Result<PageImageData, ServerFnError>
        }
    });
    
    // Current page data from either store cache or server action result
    let current_page = Memo::new(move |_| {
        let doc_id_str = doc_id.get().map(|id| id.to_string());
        let page_idx = page_id.get();
        
        // First check if we have the page in store
        if let Some(doc) = document.get() {
            if let Some(page) = doc.get_page(page_idx) {
                log!("Page {} found in store cache", page_idx);
                return Some(page.clone());
            }
        }
        
        // If not in store, check if we have it from the server action
        if let Some(Ok(page_data)) = load_page_action.value().get() {
            if page_data.page_index == page_idx {
                log!("Page {} loaded from server action", page_idx);
                // Convert PageImageData to DocumentPage format
                use crate::store::DocumentPage;
                return Some(DocumentPage {
                    page_index: page_data.page_index,
                    image_data: page_data.image_data.clone(),
                    width: page_data.width,
                    height: page_data.height,
                });
            }
        }
        
        // If we have doc_id but no page data, trigger loading
        if let Some(doc_id_str) = doc_id_str {
            log!("Page {} not available, triggering load", page_idx);
            load_page_action.dispatch_local((doc_id_str, page_idx));
        }
        
        log!("No page data available for page {}", page_idx);
        None
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
                                    move || {
                                        log!("PDF page content render function called");
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
                                            let is_loading = load_page_action.pending().get();
                                            div().class("text-center").child((
                                                h3().class("text-lg font-medium text-gray-900 mb-2")
                                                    .child(format!("Page {}", page_id.get() + 1)),
                                                if is_loading {
                                                    div().class("flex items-center justify-center space-x-2").child((
                                                        div().class("animate-spin rounded-full h-6 w-6 border-b-2 border-blue-600"),
                                                        p().class("text-blue-600").child("Loading page...")
                                                    )).into_any()
                                                } else {
                                                    p().class("text-gray-600")
                                                        .child("Page not found or still loading...").into_any()
                                                },
                                                p().class("text-sm text-gray-500 mt-2")
                                                    .child(format!("Document ID: {}", doc.id)),
                                            )).into_any()
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
        })
}
