use leptos::html::*;
use leptos::logging::log;
use leptos::prelude::*;
use leptos::svg::*;

use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::{FormData, HtmlFormElement};
use pdfium_render::prelude::*;
use server_fn::codec::{MultipartData, MultipartFormData};

#[component]
pub fn FileUpload() -> impl IntoView {
    /// A simple file upload function, which does just returns the length of the file.
    ///
    /// On the server, this uses the `multer` crate, which provides a streaming API.
    #[server(
        input = MultipartFormData,
    )]
    pub async fn file_length(data: MultipartData) -> Result<usize, ServerFnError> {
        // `.into_inner()` returns the inner `multer` stream
        // it is `None` if we call this on the client, but always `Some(_)` on the server, so is safe to
        // unwrap
        let mut data = data.into_inner().unwrap();

        let mut buf = bytes::BytesMut::new();
        while let Ok(Some(mut field)) = data.next_field().await {
            log!("\n[NEXT FIELD]\n");
            let name = field.name().unwrap_or_default().to_string();
            log!("  [NAME] {name}");
            while let Ok(Some(chunk)) = field.chunk().await {
                buf.extend_from_slice(chunk.as_ref());
            }
        }

        // Try multiple library locations for cargo-leptos compatibility
        // Prefer runtime env var; fall back to compile-time value from build.rs via option_env!
        let runtime_or_compile_time_path = std::env::var("PDFIUM_LIBRARY_PATH")
            .ok()
            .or_else(|| option_env!("PDFIUM_LIBRARY_PATH").map(|s| s.to_string()));

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

        let pdf_document = pdfium.load_pdf_from_byte_slice(&buf, None).unwrap();
        let _pages = pdf_document.pages();

        Ok(buf.len())
    }

    let upload_action = Action::new_local(|data: &FormData| {
        // `MultipartData` implements `From<FormData>`
        file_length(data.clone().into())
    });

    div()
        .class("space-y-6")
        .child((
            div().child((
                h3()
                    .class("text-lg font-medium text-gray-900 mb-4")
                    .child("Upload PDF Document"),
                form()
                    .class("space-y-4")
                    .on(leptos::ev::submit, move |ev: leptos::ev::SubmitEvent| {
                        ev.prevent_default();
                        let target = ev.target().unwrap().unchecked_into::<HtmlFormElement>();
                        let form_data = FormData::new_with_form(&target).unwrap();
                        upload_action.dispatch_local(form_data);
                    })
                    .child((
                        div()
                            .class("border-2 border-dashed border-gray-300 rounded-lg p-6 text-center hover:border-gray-400 transition-colors duration-200")
                            .child(
                                div()
                                    .class("space-y-4")
                                    .child((
                                        div()
                                            .class("flex justify-center")
                                            .child(
                                                svg()
                                                    .class("h-12 w-12 text-gray-400")
                                                    .attr("fill", "none")
                                                    .attr("viewBox", "0 0 24 24")
                                                    .attr("stroke", "currentColor")
                                                    .child(
                                                        path()
                                                            .attr("stroke-linecap", "round")
                                                            .attr("stroke-linejoin", "round")
                                                            .attr("stroke-width", "2")
                                                            .attr("d", "M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12")
                                                    )
                                            ),
                                        div().child((
                                            label()
                                                .class("block text-sm font-medium text-gray-700 mb-2")
                                                .attr("for", "file_input")
                                                .child("Choose PDF file to upload"),
                                            input()
                                                .class("block w-full text-sm text-gray-900 border border-gray-300 rounded-md cursor-pointer bg-gray-50 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-blue-500 file:mr-4 file:py-2 file:px-4 file:rounded-l-md file:border-0 file:text-sm file:font-medium file:bg-blue-50 file:text-blue-700 hover:file:bg-blue-100")
                                                .id("file_input")
                                                .attr("type", "file")
                                                .attr("name", "file_to_upload")
                                                .attr("accept", ".pdf")
                                        )),
                                        p()
                                            .class("text-xs text-gray-500")
                                            .child("PDF files only, up to 50MB")
                                    ))
                            ),
                        button()
                            .attr("type", "submit")
                            .class("w-full bg-blue-600 text-white py-2 px-4 rounded-md hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 transition-colors duration-200 font-medium")
                            .prop("disabled", move || upload_action.pending().get())
                            .child(move || if upload_action.pending().get() { "Processing..." } else { "Upload & Analyze" })
                    ))
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
                            } else if let Some(Ok(value)) = upload_action.value().get() {
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
                                            .child(format!("File size: {:.1} KB", value as f64 / 1024.0))
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
