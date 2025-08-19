use leptos::prelude::*;

#[component]
pub fn Toggle(
    show: ReadSignal<bool>,
    set_show: WriteSignal<bool>,
    #[prop(into, optional)] class: Option<String>,
    #[prop(into, optional)] aria_label: Option<String>,
) -> impl IntoView {
    let base_class = "inline-flex items-center justify-center p-2 rounded-md text-gray-400 hover:text-gray-500 hover:bg-gray-100 focus:outline-none focus:ring-2 focus:ring-inset focus:ring-blue-500 transition-colors duration-200";
    let full_class = match class {
        Some(c) => format!("{} {}", base_class, c),
        None => base_class.to_string(),
    };

    let label = aria_label.unwrap_or_else(|| "Toggle panel".to_string());

    view! {
        <button
            class=full_class
            on:click=move |_| {
                set_show.set(!show.get());
            }
            aria-label=label
        >
            <Show
                when=move || show.get()
                fallback=|| view! {
                    // Menu icon (hamburger)
                    <svg class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" />
                    </svg>
                }
            >
                // X icon (close)
                <svg class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" />
                </svg>
            </Show>
        </button>
    }
}
