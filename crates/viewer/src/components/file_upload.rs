use leptos::html::*;
use leptos::prelude::*;
use leptos::svg::*;
use leptos_router::hooks::use_navigate;
use server_fn::codec::{MultipartData, MultipartFormData};
use wasm_bindgen_futures::spawn_local;
use web_sys::FormData;
use wasm_bindgen::JsCast;

use crate::components::doc_tree::DocTree;
use crate::store::{DocumentSummary, UploadReceipt};

/// Upload a PDF: write original bytes to the local byte-cache, ingest into
/// the Postgres store (corpus from the form, default "viewer-dev"), return
/// the ingest receipt (DV-001/DV-002).
#[server(
    input = MultipartFormData,
)]
pub async fn upload_pdf(data: MultipartData) -> Result<UploadReceipt, ServerFnError> {
    // `.into_inner()` returns the inner `multer` stream; always `Some(_)` on
    // the server.
    let mut data = data.into_inner().unwrap();

    let mut buf: Vec<u8> = Vec::new();
    let mut filename = "upload.pdf".to_string();
    let mut corpus: Option<String> = None;

    while let Ok(Some(mut field)) = data.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        if name == "corpus" {
            if let Ok(value) = field.text().await {
                if !value.trim().is_empty() {
                    corpus = Some(value.trim().to_string());
                }
            }
            continue;
        }
        if let Some(field_filename) = field.file_name() {
            filename = field_filename.to_string();
        }
        while let Ok(Some(chunk)) = field.chunk().await {
            buf.extend_from_slice(chunk.as_ref());
        }
    }

    crate::store::ingest_upload(&filename, corpus.as_deref(), buf)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

/// All documents in the store (documents joined with corpora), newest first.
#[server]
pub async fn get_documents() -> Result<Vec<DocumentSummary>, ServerFnError> {
    crate::store::list_documents()
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

/// One document summary by id.
#[server]
pub async fn get_document_by_id(
    doc_id: String,
) -> Result<Option<DocumentSummary>, ServerFnError> {
    crate::store::document_summary(&doc_id)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn FileUpload() -> impl IntoView {
    let navigate = use_navigate();

    // Create signals for upload state
    let (upload_pending, set_upload_pending) = signal(false);
    let (upload_result, set_upload_result) = signal::<Option<Result<UploadReceipt, String>>>(None);

    // Handle successful upload and navigation with an effect
    Effect::new(move |_| {
        if let Some(Ok(receipt)) = upload_result.get() {
            navigate(&format!("/viewer/{}/0", receipt.document_id), Default::default());
        }
    });

    // Resource to load documents list (refetches after a successful upload)
    let documents = Resource::new(
        move || upload_result.get().is_some(),
        |_| async move { get_documents().await.unwrap_or_default() },
    );

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
                                        Ok(receipt) => {
                                            set_upload_result.set(Some(Ok(receipt)));
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
                                <div>
                                    <label class="block text-sm font-medium text-gray-700 mb-2" for="corpus_input">
                                        "Corpus"
                                    </label>
                                    <input
                                        class="block w-full text-sm text-gray-900 border border-gray-300 rounded-md bg-gray-50 px-3 py-2 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                        id="corpus_input"
                                        type="text"
                                        name="corpus"
                                        value="viewer-dev"
                                    />
                                </div>
                                <p class="text-xs text-gray-500">
                                    "PDF files only. Bytes are cached locally; elements are indexed in Postgres."
                                </p>
                            </div>
                        </div>
                        <button
                            type="submit"
                            class="w-full bg-blue-600 text-white py-2 px-4 rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 transition-colors duration-200 font-medium"
                            disabled=move || upload_pending.get()
                        >
                            {move || if upload_pending.get() { "Processing..." } else { "Upload & Index" }}
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
                            if upload_pending.get() {
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
                                        "Parsing and indexing PDF..."
                                    )).into_any()
                            } else if let Some(Ok(receipt)) = upload_result.get() {
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
                                                if receipt.created {
                                                    "Indexed new document"
                                                } else {
                                                    "Already indexed (deduplicated)"
                                                }
                                            )),
                                        div()
                                            .class("text-xs text-gray-600")
                                            .child(format!(
                                                "{} → corpus {} ({} pages, {} elements)",
                                                receipt.filename,
                                                receipt.corpus,
                                                receipt.page_count,
                                                receipt.element_count
                                            ))
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
                        .child("Documents in Store"),
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
                                            .child("No documents in the store yet")
                                            .into_any()
                                    } else {
                                        // File tree following the hive-style
                                        // partition tags captured at ingest
                                        // (D-023): corpus → key=value levels
                                        // → document leaf (DV-015).
                                        view! { <DocTree docs=docs /> }.into_any()
                                    }
                                })
                                    }}
                                </Suspense>
                            }
                        )
                ))
        ))
}
