use leptos::html::*;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use leptos::ev;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlImageElement, Blob, BlobPropertyBag};

use crate::components::file_upload::{get_pdf_page, get_documents, get_document_by_id};

// Helper function to render WebP data to a canvas using an HTML Image element
fn render_webp_to_canvas(canvas: &HtmlCanvasElement, webp_data: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    let context = canvas
        .get_context("2d")?
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()?;

    // Create a blob from WebP data
    let uint8_array = js_sys::Uint8Array::new_with_length(webp_data.len() as u32);
    uint8_array.copy_from(webp_data);
    
    let blob_parts = js_sys::Array::new();
    blob_parts.push(&uint8_array);
    
    let blob_property_bag = BlobPropertyBag::new();
    blob_property_bag.set_type("image/webp");
    
    let blob = Blob::new_with_u8_array_sequence_and_options(&blob_parts, &blob_property_bag)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    
    // Create an Image element and load the WebP
    let img = HtmlImageElement::new()?;
    let img_clone = img.clone();
    let canvas_clone = canvas.clone();
    let context_clone = context.clone();
    let url_clone = url.clone();
    
    let onload = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        // Set canvas size to match image
        canvas_clone.set_width(img_clone.natural_width());
        canvas_clone.set_height(img_clone.natural_height());
        
        // Draw the image to the canvas
        let _ = context_clone.draw_image_with_html_image_element(&img_clone, 0.0, 0.0);
        
        // Clean up the object URL
        let _ = web_sys::Url::revoke_object_url(&url_clone);
    }) as Box<dyn FnMut()>);
    
    img.set_onload(Some(onload.as_ref().unchecked_ref()));
    onload.forget(); // Keep the closure alive
    
    img.set_src(&url);
    
    Ok(())
}



#[component]
pub fn PdfViewer() -> impl IntoView {
    let params = use_params_map();

    let doc_id = Memo::new(move |_| {
        let result = params.with(|params| {
            params
                .get("doc_id")
                .and_then(|id| Uuid::parse_str(&id).ok())
        });
        result
    });

    let page_id = Memo::new(move |_| {
        let result = params.with(|params| {
            params
                .get("page_id")
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or(0)
        });
        result
    });

    // Resource to load the document metadata - refresh when doc_id changes
    let documents = Resource::new(
        move || doc_id.get(),
        move |_| async move {
            get_documents().await.unwrap_or_default()
        }
    );

    // Fallback resource to get individual document if not found in main list
    let fallback_document = Resource::new(
        move || doc_id.get().map(|id| id.to_string()),
        move |doc_id_opt| async move {
            if let Some(doc_id) = doc_id_opt {
                get_document_by_id(doc_id).await.unwrap_or(None)
            } else {
                None
            }
        }
    );

    // Resource for loading page images on demand (now returns PNG bytes)
    let page_resource = Resource::new(
        move || (doc_id.get().map(|id| id.to_string()), page_id.get()),
        move |(doc_id_opt, page_idx)| {
            async move {
                if let Some(doc_id) = doc_id_opt {
                    let result = get_pdf_page(doc_id.clone(), page_idx).await;
                    result
                } else {
                    Err(server_fn::ServerFnError::new(anyhow::anyhow!("No document ID")))
                }
            }
        }
    );
    
    // Current page data from server action result

    // Canvas ref for rendering
    let canvas_ref = NodeRef::<Canvas>::new();

    div()
        .class("h-full flex flex-col bg-gray-50")
        .child(move || {
            
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
                // Find the document within the Suspense boundary
                if let Some(id) = doc_id.get() {
                    // First try to find in the main documents list
                    let doc = if let Some(docs) = documents.get() {
                        docs.into_iter().find(|doc| doc.id == id)
                    } else {
                        None
                    };
                    
                    // If not found in main list, try the fallback individual document resource
                    let doc = doc.or_else(|| fallback_document.get().flatten());
                    
                    if let Some(doc) = doc {
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
                                            p().class("text-sm text-gray-600").child(move || format!(
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
                                            span().class("text-sm text-gray-500").child({
                                                let total_pages = doc.total_pages;
                                                move || format!("{} / {}", page_id.get() + 1, total_pages)
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
                                            view! {
                                                <Suspense fallback=move || view! {
                                                    <div class="text-center">
                                                        <h3 class="text-lg font-medium text-gray-900 mb-4">
                                                            {move || format!("Page {}", page_id.get() + 1)}
                                                        </h3>
                                                        <div class="flex items-center justify-center space-x-2 mb-4">
                                                            <div class="animate-spin rounded-full h-6 w-6 border-b-2 border-blue-600"></div>
                                                            <p class="text-blue-600">"Loading page..."</p>
                                                        </div>
                                                    </div>
                                                }>
                                                    {move || {
                                                // Access page_resource within Suspense boundary  
                                                if let Some(Ok(webp_bytes)) = page_resource.get() {
                                                    
                                                    // Effect to render to canvas when WebP data changes
                                                    Effect::new({
                                                        move |_| {
                                                            if let Some(canvas_element) = canvas_ref.get() {
                                                                if let Ok(canvas) = canvas_element.dyn_into::<HtmlCanvasElement>() {
                                                                   let _ = render_webp_to_canvas(&canvas, &webp_bytes);
                                                                }
                                                            }
                                                        }
                                                    });
                                                    
                                                    div().class("text-center").child((
                                                        h3().class("text-lg font-medium text-gray-900 mb-4")
                                                            .child(move || format!("Page {}", page_id.get() + 1)),
                                                        div().class("flex justify-center mb-2").child(
                                                            canvas()
                                                                .class("border border-gray-200 max-w-full h-auto")
                                                                .node_ref(canvas_ref)
                                                        )
                                                    )).into_any()
                                                } else {
                                                    div().class("text-center").child((
                                                        h3().class("text-lg font-medium text-gray-900 mb-2")
                                                            .child(move || format!("Page {}", page_id.get() + 1)),
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
                } else {
                    div()
                        .class("flex-1 flex items-center justify-center")
                        .child(
                            div().class("text-center").child((
                                h2().class("text-xl font-semibold text-gray-900 mb-2")
                                    .child("Loading..."),
                                p().class("text-gray-600")
                                    .child("Please wait while the document loads."),
                            )),
                        )
                        .into_any()
                }
                    }}
                </Suspense>
            }
        })
}