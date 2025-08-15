use leptos::html::*;
use leptos::prelude::*;

#[component]
pub fn PdfViewer() -> impl IntoView {
    view! {
        <div class="h-full flex flex-col bg-gray-50">
            <div class="flex-1 flex items-center justify-center">
                <div class="text-center">
                    <h2 class="text-xl font-semibold text-gray-900 mb-2">"PDF Viewer"</h2>
                    <p class="text-gray-600">"PDF viewer functionality coming soon."</p>
                </div>
            </div>
        </div>
    }
}
