use leptos::html::*;
use leptos::logging::log;
use leptos::prelude::*;
use leptos::svg::*;
use leptos_router::hooks::use_navigate;
use serde::{Deserialize, Serialize};
use server_fn::codec::{MultipartData, MultipartFormData};
use leptos::ev;
use wasm_bindgen_futures::spawn_local;
use web_sys::FormData;
use wasm_bindgen::JsCast;

use crate::store::PdfDocument;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub id: String,
    pub filename: String,
    pub page_count: usize,
    pub page_dimensions: Vec<(f32, f32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageImageData {
    pub page_index: usize,
    pub image_data: Vec<u8>, // RGBA bytes
    pub width: f32,
    pub height: f32,
}

/// Upload and process PDF file, storing it in SQLite
#[server(
    input = MultipartFormData,
)]
pub async fn upload_pdf(data: MultipartData) -> Result<DocumentMetadata, ServerFnError> {
    // Get database pool
    let pool = crate::store::get_database_pool().await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database connection failed: {}", e)))?;

    // `.into_inner()` returns the inner `multer` stream
    // it is `None` if we call this on the client, but always `Some(_)` on the server, so is safe to
    // unwrap
    let mut data = data.into_inner().unwrap();

    let mut buf = bytes::BytesMut::new();
    let mut filename = "Unknown".to_string();

    log!("Starting file upload processing...");
    while let Ok(Some(mut field)) = data.next_field().await {
        log!("\n[NEXT FIELD]\n");
        let name = field.name().unwrap_or_default().to_string();
        log!("  [NAME] {name}");

        if let Some(field_filename) = field.file_name() {
            filename = field_filename.to_string();
            log!("  [FILENAME] {}", filename);
        }

        while let Ok(Some(chunk)) = field.chunk().await {
            buf.extend_from_slice(chunk.as_ref());
        }
    }

    log!(
        "File upload data received: {} bytes for {}",
        buf.len(),
        filename
    );

    if buf.is_empty() {
        return Err(ServerFnError::new(anyhow::anyhow!("No file uploaded")));
    }

    // Process PDF and render all pages in a blocking thread
    let buf_clone = buf.clone();
    let (page_count, page_dimensions, rendered_pages) = tokio::task::spawn_blocking(move || {
        use pdfium_render::prelude::*;
        
        log!("Initializing PDF library...");
        // Try multiple library locations for cargo-leptos compatibility
        // Prefer runtime env var; fall back to compile-time value from build.rs via option_env!
        let runtime_or_compile_time_path = std::env::var("PDFIUM_LIBRARY_PATH")
            .ok()
            .or_else(|| option_env!("PDFIUM_LIBRARY_PATH").map(|s| s.to_string()));

        log!("PDF library path: {:?}", runtime_or_compile_time_path);
        let pdfium = if let Some(custom_path) = runtime_or_compile_time_path {
            Pdfium::new(
                Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&custom_path))
                    .or_else(|_| {
                        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
                    })
                    .or_else(|_| Pdfium::bind_to_system_library())
                    .unwrap(),
            )
        } else {
            // Fallback to default pattern when no custom path is set
            Pdfium::default()
        };

        log!("Loading PDF document from {} bytes...", buf_clone.len());
        let pdf_document = pdfium.load_pdf_from_byte_slice(&buf_clone, None)
            .map_err(|e| anyhow::anyhow!("Failed to load PDF: {}", e))?;
        let pages = pdf_document.pages();
        let page_count = pages.len() as usize;
        log!("PDF loaded successfully with {} pages", page_count);

        // Extract page dimensions and render all pages
        let mut page_dimensions = Vec::new();
        let mut rendered_pages = Vec::new();
        log!("Rendering all {} pages...", page_count);

        for page_index in 0..page_count {
            let page: PdfPage = pdf_document.pages().get(page_index as u16)
                .map_err(|e| anyhow::anyhow!("Failed to get page {}: {}", page_index, e))?;

            // Use a reasonable DPI for web viewing (150 DPI is a good balance)
            let scale_factor = 150.0 / 72.0; // 72 DPI is PDF default
            let width = (page.width().value * scale_factor) as f32;
            let height = (page.height().value * scale_factor) as f32;

            page_dimensions.push((width, height));

            // Render the page to RGBA image data
            let render_config = PdfRenderConfig::new()
                .set_target_width(width as i32)
                .set_target_height(height as i32)
                .use_lcd_text_rendering(true)
                .render_annotations(true)
                .render_form_data(false);

            let bitmap: PdfBitmap = page
                .render_with_config(&render_config)
                .map_err(|e| anyhow::anyhow!("Failed to render page {}: {}", page_index, e))?;

            // Convert to RGBA bytes
            let image_data = bitmap.as_rgba_bytes();
            
            rendered_pages.push((page_index, image_data, width, height));
            log!("Rendered page {} ({}x{})", page_index + 1, width as i32, height as i32);
        }
        
        Ok::<(usize, Vec<(f32, f32)>, Vec<(usize, Vec<u8>, f32, f32)>), anyhow::Error>((page_count, page_dimensions, rendered_pages))
    }).await
    .map_err(|e| ServerFnError::new(anyhow::anyhow!("PDF processing task failed: {}", e)))?
    .map_err(|e| ServerFnError::new(e))?;

    // Store the PDF document in SQLite
    log!("Storing document in database...");
    let document = crate::store::PdfDocument::create(&pool, filename.clone(), buf.to_vec(), page_count).await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Failed to store document: {}", e)))?;

    // Store all rendered pages in the database
    log!("Storing {} rendered pages in database...", rendered_pages.len());
    for (page_index, image_data, width, height) in rendered_pages {
        crate::store::DocumentPage::create(&pool, document.id, page_index, image_data, width, height).await
            .map_err(|e| ServerFnError::new(anyhow::anyhow!("Failed to store page {}: {}", page_index, e)))?;
    }

    log!("PDF upload and storage complete with all pages rendered");
    Ok(DocumentMetadata {
        id: document.id.to_string(),
        filename,
        page_count,
        page_dimensions,
    })
}

/// Get a specific page of a PDF document from cache
#[server]
pub async fn get_pdf_page(
    doc_id: String,
    page_index: usize,
) -> Result<PageImageData, ServerFnError> {
    log!("Getting page {} for document {}", page_index, doc_id);

    // Get database pool
    let pool = crate::store::get_database_pool().await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database connection failed: {}", e)))?;

    // Get the page from cache (should always be there since we render all pages during upload)
    let doc_uuid = uuid::Uuid::parse_str(&doc_id)
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Invalid document ID: {}", e)))?;
    
    let cached_page = crate::store::DocumentPage::get_by_document_and_page(&pool, doc_uuid, page_index).await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database error: {}", e)))?
        .ok_or_else(|| ServerFnError::new(anyhow::anyhow!("Page {} not found for document {}", page_index, doc_id)))?;

    log!("Returning page {} for document {}", page_index, doc_id);
    Ok(PageImageData {
        page_index: cached_page.page_index,
        image_data: cached_page.image_data,
        width: cached_page.width,
        height: cached_page.height,
    })
}

/// Get all documents for the documents list
#[server]
pub async fn get_documents() -> Result<Vec<PdfDocument>, ServerFnError> {
    let pool = crate::store::get_database_pool().await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database connection failed: {}", e)))?;

    crate::store::PdfDocument::get_all(&pool).await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Failed to get documents: {}", e)))
}

/// Get a single document by ID (fallback for newly uploaded documents)
#[server]
pub async fn get_document_by_id(doc_id: String) -> Result<Option<PdfDocument>, ServerFnError> {
    let pool = crate::store::get_database_pool().await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database connection failed: {}", e)))?;

    let doc_uuid = uuid::Uuid::parse_str(&doc_id)
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Invalid document ID: {}", e)))?;

    crate::store::PdfDocument::get_by_id(&pool, doc_uuid).await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Failed to get document: {}", e)))
}

/// Delete a document and all its pages
#[server]
pub async fn delete_document(doc_id: String) -> Result<bool, ServerFnError> {
    let pool = crate::store::get_database_pool().await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Database connection failed: {}", e)))?;

    let doc_uuid = uuid::Uuid::parse_str(&doc_id)
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Invalid document ID: {}", e)))?;

    crate::store::PdfDocument::delete(&pool, doc_uuid).await
        .map_err(|e| ServerFnError::new(anyhow::anyhow!("Failed to delete document: {}", e)))
}

#[component]
pub fn FileUpload() -> impl IntoView {
    let navigate = use_navigate();
    let navigate_clone = navigate.clone();

    // Create signals for upload state
    let (upload_pending, set_upload_pending) = signal(false);
    let (upload_result, set_upload_result) = signal::<Option<Result<DocumentMetadata, String>>>(None);

    // Handle successful upload and navigation with an effect
    Effect::new(move |_| {
        if let Some(Ok(metadata)) = upload_result.get() {
            log!(
                "Processing upload result for: {} ({} pages)",
                metadata.filename,
                metadata.page_count
            );

            log!("Navigating to viewer...");
            navigate_clone(&format!("/viewer/{}/0", metadata.id), Default::default());

            log!(
                "Document upload complete: {} with {} pages",
                metadata.filename,
                metadata.page_count
            );
        }
    });

    // Resource to load documents list
    let documents = Resource::new(|| (), |_| async move { 
        get_documents().await.unwrap_or_default()
    });

    div()
        .class("space-y-6")
        .child((
            div().child((
                h3()
                    .class("text-lg font-medium text-gray-900 mb-4")
                    .child("Upload PDF Document"),
                view! {
                    <form 
                        on:submit=move |ev| {
                            ev.prevent_default();
                            if let Some(form) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlFormElement>().ok()) {
                                let form_data = FormData::new_with_form(&form).unwrap();
                                let multipart_data: MultipartData = form_data.into();
                                
                                set_upload_pending.set(true);
                                set_upload_result.set(None);
                                
                                spawn_local(async move {
                                    match upload_pdf(multipart_data).await {
                                        Ok(metadata) => {
                                            set_upload_result.set(Some(Ok(metadata)));
                                        }
                                        Err(e) => {
                                            set_upload_result.set(Some(Err(e.to_string())));
                                        }
                                    }
                                    set_upload_pending.set(false);
                                });
                            }
                        }
                        enctype="multipart/form-data"
                    >
                        <div class="border-2 border-dashed border-gray-300 rounded-lg p-6 text-center hover:border-gray-400 transition-colors duration-200">
                            <div class="space-y-4">
                                <div class="flex justify-center">
                                    <svg class="h-12 w-12 text-gray-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12"/>
                                    </svg>
                                </div>
                                <div>
                                    <label class="block text-sm font-medium text-gray-700 mb-2" for="file_input">
                                        "Choose PDF file to upload"
                                    </label>
                                    <input 
                                        class="block w-full text-sm text-gray-900 border border-gray-300 rounded-md cursor-pointer bg-gray-50 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-blue-500 file:mr-4 file:py-2 file:px-4 file:rounded-l-md file:border-0 file:text-sm file:font-medium file:bg-blue-50 file:text-blue-700 hover:file:bg-blue-100"
                                        id="file_input"
                                        type="file"
                                        name="data"
                                        accept=".pdf"
                                    />
                                </div>
                                <p class="text-xs text-gray-500">
                                    "PDF files only, up to 50MB"
                                </p>
                            </div>
                        </div>
                        <button
                            type="submit"
                            class="w-full bg-blue-600 text-white py-2 px-4 rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 transition-colors duration-200 font-medium"
                            disabled=move || upload_pending.get()
                        >
                            {move || if upload_pending.get() { "Processing..." } else { "Upload & Analyze" }}
                        </button>
                    </form>
                }
            )),
            // Status and Results Section
            div()
                .class("border-t border-gray-200 pt-6")
                .child((
                    h4()
                        .class("text-sm font-medium text-gray-900 mb-3")
                        .child("Upload Status"),
                    div()
                        .class("bg-gray-50 rounded-md p-4")
                        .child(move || {
                            if upload_result.get().is_none() && !upload_pending.get() {
                                div()
                                    .class("flex items-center text-sm text-gray-600")
                                    .child((
                                        svg()
                                            .class("h-4 w-4 mr-2 text-gray-400")
                                            .attr("fill", "none")
                                            .attr("viewBox", "0 0 24 24")
                                            .attr("stroke", "currentColor")
                                            .child(
                                                path()
                                                    .attr("stroke-linecap", "round")
                                                    .attr("stroke-linejoin", "round")
                                                    .attr("stroke-width", "2")
                                                    .attr("d", "M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z")
                                            ),
                                        "Ready to upload a PDF document"
                                    )).into_any()
                            } else if upload_pending.get() {
                                div()
                                    .class("flex items-center text-sm text-blue-600")
                                    .child((
                                        svg()
                                            .class("animate-spin h-4 w-4 mr-2")
                                            .attr("fill", "none")
                                            .attr("viewBox", "0 0 24 24")
                                            .child((
                                                circle()
                                                    .class("opacity-25")
                                                    .attr("cx", "12")
                                                    .attr("cy", "12")
                                                    .attr("r", "10")
                                                    .attr("stroke", "currentColor")
                                                    .attr("stroke-width", "4"),
                                                path()
                                                    .class("opacity-75")
                                                    .attr("fill", "currentColor")
                                                    .attr("d", "M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z")
                                            )),
                                        "Processing PDF document..."
                                    )).into_any()
                            } else if let Some(Ok(metadata)) = upload_result.get() {
                                div()
                                    .class("space-y-2")
                                    .child((
                                        div()
                                            .class("flex items-center text-sm text-green-600")
                                            .child((
                                                svg()
                                                    .class("h-4 w-4 mr-2")
                                                    .attr("fill", "none")
                                                    .attr("viewBox", "0 0 24 24")
                                                    .attr("stroke", "currentColor")
                                                    .child(
                                                        path()
                                                            .attr("stroke-linecap", "round")
                                                            .attr("stroke-linejoin", "round")
                                                            .attr("stroke-width", "2")
                                                            .attr("d", "M5 13l4 4L19 7")
                                                    ),
                                                "Upload successful!"
                                            )),
                                        div()
                                            .class("text-xs text-gray-600")
                                            .child(format!("Document: {} ({} pages)", metadata.filename, metadata.page_count))
                                    )).into_any()
                            } else if let Some(Err(error)) = upload_result.get() {
                                div()
                                    .class("flex items-center text-sm text-red-600")
                                    .child((
                                        svg()
                                            .class("h-4 w-4 mr-2")
                                            .attr("fill", "none")
                                            .attr("viewBox", "0 0 24 24")
                                            .attr("stroke", "currentColor")
                                            .child(
                                                path()
                                                    .attr("stroke-linecap", "round")
                                                    .attr("stroke-linejoin", "round")
                                                    .attr("stroke-width", "2")
                                                    .attr("d", "M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z")
                                            ),
                                        format!("Upload failed: {}", error)
                                    )).into_any()
                            } else {
                                div()
                                    .class("flex items-center text-sm text-gray-600")
                                    .child((
                                        svg()
                                            .class("h-4 w-4 mr-2 text-gray-400")
                                            .attr("fill", "none")
                                            .attr("viewBox", "0 0 24 24")
                                            .attr("stroke", "currentColor")
                                            .child(
                                                path()
                                                    .attr("stroke-linecap", "round")
                                                    .attr("stroke-linejoin", "round")
                                                    .attr("stroke-width", "2")
                                                    .attr("d", "M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z")
                                            ),
                                        "Ready to upload a PDF document"
                                    )).into_any()
                            }
                        })
                )),
            // Documents List Section
            div()
                .class("border-t border-gray-200 pt-6")
                .child((
                    h4()
                        .class("text-sm font-medium text-gray-900 mb-3")
                        .child("Your Documents"),
                    div()
                        .class("space-y-2")
                        .child(
                            view! {
                                <Suspense fallback=move || view! {
                                    <div class="flex items-center text-sm text-gray-600">
                                        <svg class="animate-spin h-4 w-4 mr-2" fill="none" viewBox="0 0 24 24">
                                            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                                        </svg>
                                        "Loading documents..."
                                    </div>
                                }>
                                    {move || {
                                documents.get().map(|docs| {
                                    if docs.is_empty() {
                                        div()
                                            .class("text-sm text-gray-500 italic")
                                            .child("No documents uploaded yet")
                                            .into_any()
                                    } else {
                                        div()
                                            .class("space-y-2")
                                            .child(docs.into_iter().map(|doc| {
                                                div()
                                                    .class("flex items-center justify-between p-3 bg-white border border-gray-200 rounded-lg hover:bg-gray-50")
                                                    .child((
                                                        div()
                                                            .class("flex-1")
                                                            .child((
                                                                div()
                                                                    .class("text-sm font-medium text-gray-900")
                                                                    .child(doc.name.clone()),
                                                                div()
                                                                    .class("text-xs text-gray-500")
                                                                    .child(format!("{} pages • {}", doc.total_pages, doc.created_at.format("%Y-%m-%d %H:%M")))
                                                            )),
                                                        div()
                                                            .class("flex items-center space-x-2")
                                                            .child((
                                                                button()
                                                                    .class("text-xs px-2 py-1 bg-blue-100 text-blue-700 rounded hover:bg-blue-200")
                                                                    .on(ev::click, {
                                                                        let doc_id = doc.id.to_string();
                                                                        let navigate_copy = navigate.clone();
                                                                        move |_| {
                                                                            navigate_copy(&format!("/viewer/{}/0", doc_id), Default::default());
                                                                        }
                                                                    })
                                                                    .child("View"),
                                                                button()
                                                                    .class("text-xs px-2 py-1 bg-red-100 text-red-700 rounded hover:bg-red-200")
                                                                    .on(ev::click, {
                                                                        let doc_id = doc.id.to_string();
                                                                        let documents_copy = documents.clone();
                                                                        move |_| {
                                                                            let doc_id_clone = doc_id.clone();
                                                                            spawn_local(async move {
                                                                                if let Ok(_) = delete_document(doc_id_clone).await {
                                                                                    documents_copy.refetch();
                                                                                }
                                                                            });
                                                                        }
                                                                    })
                                                                    .child("Delete")
                                                            ))
                                                    ))
                                            }).collect::<Vec<_>>())
                                            .into_any()
                                    }
                                })
                                    }}
                                </Suspense>
                            }
                        )
                ))
        ))
}