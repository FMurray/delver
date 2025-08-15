use leptos::html::*;
use leptos::logging::log;
use leptos::prelude::*;
use leptos::svg::*;

use leptos_router::hooks::use_navigate;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use server_fn::codec::{MultipartData, MultipartFormData};

use crate::store::{DocumentStore, PdfDocument};

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

#[component]
pub fn FileUpload() -> impl IntoView {
    /// Upload and process PDF file, returning document metadata only
    #[server(
        input = MultipartFormData,
    )]
    pub async fn upload_pdf(data: MultipartData) -> Result<DocumentMetadata, ServerFnError> {
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

        log!("Loading PDF document from {} bytes...", buf.len());
        let pdf_document = pdfium.load_pdf_from_byte_slice(&buf, None).unwrap();
        let pages = pdf_document.pages();
        let page_count = pages.len() as usize;
        log!("PDF loaded successfully with {} pages", page_count);

        // Generate a document ID for this upload
        let doc_id = uuid::Uuid::new_v4().to_string();
        log!("Generated document ID: {}", doc_id);

        // Only extract page dimensions for metadata - don't render images yet
        let mut page_dimensions = Vec::new();
        log!("Extracting page dimensions for {} pages...", page_count);

        for page_index in 0..page_count {
            let page: PdfPage = pdf_document.pages().get(page_index as u16).map_err(|e| {
                ServerFnError::new(anyhow::anyhow!("Failed to get page {}: {}", page_index, e))
            })?;

            // Use a reasonable DPI for web viewing (150 DPI is a good balance)
            let scale_factor = 150.0 / 72.0; // 72 DPI is PDF default
            let width = (page.width().value * scale_factor) as f32;
            let height = (page.height().value * scale_factor) as f32;

            page_dimensions.push((width, height));
        }

        // Store the PDF data for later page rendering
        // TODO: Consider using a proper storage system (Redis, file system, etc.)
        // For now, we'll implement lazy loading on the client side

        log!("PDF metadata extraction complete");
        Ok(DocumentMetadata {
            id: doc_id,
            filename,
            page_count,
            page_dimensions,
        })
    }

    /// Render a specific page of a PDF document on demand
    #[server]
    pub async fn render_pdf_page(
        doc_id: String,
        page_index: usize,
    ) -> Result<PageImageData, ServerFnError> {
        log!("Rendering page {} for document {}", page_index, doc_id);

        // TODO: In a real application, you'd retrieve the PDF from storage
        // For now, this is a placeholder that shows the structure
        // You'll need to implement proper PDF storage and retrieval

        Err(ServerFnError::new(anyhow::anyhow!(
            "PDF storage not implemented yet - page {} for doc {}",
            page_index,
            doc_id
        )))
    }

    let store = expect_context::<DocumentStore>();
    let navigate = use_navigate();

    // Create a server action for uploading PDFs  
    let upload_action = ServerAction::<UploadPdf>::new();

    // Handle successful upload and navigation with an effect
    Effect::new(move |_| {
        if let Some(Ok(metadata)) = upload_action.value().get() {
            log!(
                "Processing upload result for: {} ({} pages)",
                metadata.filename,
                metadata.page_count
            );

            if let Ok(doc_id) = uuid::Uuid::parse_str(&metadata.id) {
                log!("Creating document with ID: {}", doc_id);

                // Create a new document in the store with metadata only
                let mut document = PdfDocument::new(metadata.filename.clone());
                document.id = doc_id;
                document.total_pages = metadata.page_count;
                // Don't add pages yet - they'll be loaded on demand

                log!("Adding document metadata to store...");
                store.add_document(document);

                log!("Navigating to viewer...");
                navigate(&format!("/viewer/{}/0", metadata.id), Default::default());

                log!(
                    "Document upload complete: {} with {} pages",
                    metadata.filename,
                    metadata.page_count
                );
            }
        }
    });

    div()
        .class("space-y-6")
        .child((
            div().child((
                h3()
                    .class("text-lg font-medium text-gray-900 mb-4")
                    .child("Upload PDF Document"),
                view! {
                    <ActionForm action=upload_action class="space-y-4" enctype="multipart/form-data">
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
                            disabled=move || upload_action.pending().get()
                        >
                            {move || if upload_action.pending().get() { "Processing..." } else { "Upload & Analyze" }}
                        </button>
                    </ActionForm>
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
                            if upload_action.input().read().is_none() && upload_action.value().read().is_none() {
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
                            } else if upload_action.pending().get() {
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
                            } else if let Some(Ok(metadata)) = upload_action.value().get() {
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
                            } else {
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
                                        "Upload failed. Please try again."
                                    )).into_any()
                            }
                        })
                ))
        ))
}
