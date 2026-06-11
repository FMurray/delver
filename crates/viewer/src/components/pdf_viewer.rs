//! Document page view: on-demand raster from the byte-cache, element bbox
//! overlays with per-kind toggles (Stage B kinds), and a click-through
//! "discover mode" side panel (DV-004).

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use uuid::Uuid;

use crate::components::file_upload::get_document_by_id;
use crate::store::{ElementOverlay, PageMeta};

/// Raster layout metadata for one page (placeholder info when the original
/// bytes are unavailable).
#[server]
pub async fn get_page_meta(doc_id: String, page_index: usize) -> Result<PageMeta, ServerFnError> {
    crate::store::page_raster(&doc_id, page_index)
        .await
        .map(|raster| raster.meta())
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

/// Elements on one page (payload bytes stripped) for overlays + side panel.
#[server]
pub async fn get_page_elements(
    doc_id: String,
    page_index: usize,
) -> Result<Vec<ElementOverlay>, ServerFnError> {
    crate::store::page_elements(&doc_id, page_index)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

/// Border/fill colors per element kind.
fn kind_colors(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "text" => ("rgba(59,130,246,0.9)", "rgba(59,130,246,0.08)"),
        "annotation" => ("rgba(245,158,11,0.9)", "rgba(245,158,11,0.12)"),
        "figure" => ("rgba(16,185,129,0.9)", "rgba(16,185,129,0.12)"),
        "path" => ("rgba(168,85,247,0.8)", "rgba(168,85,247,0.07)"),
        "image" => ("rgba(236,72,153,0.9)", "rgba(236,72,153,0.10)"),
        _ => ("rgba(107,114,128,0.9)", "rgba(107,114,128,0.10)"),
    }
}

const TOGGLE_KINDS: [&str; 5] = ["text", "annotation", "figure", "path", "image"];

/// Pager link styling; disabled state is cosmetic (`pointer-events-none`).
fn nav_button_class(disabled: bool) -> &'static str {
    if disabled {
        "px-3 py-1 bg-gray-100 text-gray-400 rounded opacity-50 pointer-events-none"
    } else {
        "px-3 py-1 bg-gray-100 text-gray-700 rounded hover:bg-gray-200"
    }
}

#[component]
pub fn PdfViewer() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();

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

    // Kind toggles, optionally pre-set from the URL:
    // /viewer/<doc>/<page>?overlays=text,annotation,path
    let initial_overlays: Vec<String> = query
        .get_untracked()
        .get("overlays")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let kind_toggles: Vec<(&'static str, RwSignal<bool>)> = TOGGLE_KINDS
        .iter()
        .map(|kind| {
            (
                *kind,
                RwSignal::new(initial_overlays.iter().any(|k| k == kind)),
            )
        })
        .collect();

    // Selected element for the discover-mode side panel.
    let selected: RwSignal<Option<ElementOverlay>> = RwSignal::new(None);

    // Document summary.
    let document = Resource::new(
        move || doc_id.get().map(|id| id.to_string()),
        move |doc_id_opt| async move {
            match doc_id_opt {
                Some(doc_id) => get_document_by_id(doc_id).await.unwrap_or(None),
                None => None,
            }
        },
    );

    // Page raster layout (or placeholder reason).
    let page_meta = Resource::new(
        move || (doc_id.get().map(|id| id.to_string()), page_id.get()),
        move |(doc_id_opt, page_idx)| async move {
            match doc_id_opt {
                Some(doc_id) => get_page_meta(doc_id, page_idx).await.ok(),
                None => None,
            }
        },
    );

    // Elements for overlays.
    let elements = Resource::new(
        move || (doc_id.get().map(|id| id.to_string()), page_id.get()),
        move |(doc_id_opt, page_idx)| async move {
            match doc_id_opt {
                Some(doc_id) => get_page_elements(doc_id, page_idx).await.unwrap_or_default(),
                None => Vec::new(),
            }
        },
    );

    let toggles_for_bar = kind_toggles.clone();
    let toggles_for_page = kind_toggles.clone();

    view! {
        <div class="h-full flex-1 flex flex-col bg-gray-50 overflow-hidden">
            <Suspense fallback=move || view! {
                <div class="flex-1 flex items-center justify-center">
                    <div class="text-center">
                        <div class="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-600 mx-auto mb-4"></div>
                        <p class="text-gray-600">"Loading document..."</p>
                    </div>
                </div>
            }>
                {move || {
                    // Read every resource directly in this Suspense-tracked
                    // closure: resources read only inside nested reactive
                    // closures are not part of the boundary's await set, so
                    // SSR streaming serialized this subtree before page
                    // meta/elements resolved (racy empty overlays, DV-009).
                    let meta = page_meta.get().flatten();
                    let elems = elements.get().unwrap_or_default();
                    let Some(doc) = document.get().flatten() else {
                        return view! {
                            <div class="flex-1 flex items-center justify-center">
                                <div class="text-center">
                                    <h2 class="text-xl font-semibold text-gray-900 mb-2">"Document Not Found"</h2>
                                    <p class="text-gray-600">"The requested document could not be found in the store."</p>
                                </div>
                            </div>
                        }.into_any();
                    };
                    let total_pages = doc.page_count.max(1) as usize;
                    let doc_name = doc.name.clone();
                    let corpus = doc.corpus.clone();
                    let parse_version = doc.parse_version;
                    let toggles = toggles_for_bar.clone();
                    view! {
                        <header class="bg-white border-b border-gray-200 px-6 py-3">
                            <div class="flex items-center justify-between">
                                <div class="min-w-0 mr-4">
                                    <h1 class="text-lg font-semibold text-gray-900 truncate">{doc_name}</h1>
                                    <p class="text-xs text-gray-500">
                                        {format!("corpus {corpus} • parse v{parse_version} • ")}
                                        {move || format!("page {} of {}", page_id.get() + 1, total_pages)}
                                    </p>
                                </div>
                                // Prev/Next are plain links (the router intercepts
                                // same-origin clicks): programmatic `use_navigate`
                                // here ran during SSR resolve and panicked on
                                // js_sys::global() — see DV-009.
                                <div class="flex items-center space-x-2">
                                    <a
                                        class=move || nav_button_class(page_id.get() == 0)
                                        href=move || {
                                            let p = page_id.get();
                                            doc_id.get()
                                                .map(|id| format!("/viewer/{}/{}", id, p.saturating_sub(1)))
                                                .unwrap_or_default()
                                        }
                                    >"← Prev"</a>
                                    <a
                                        class=move || nav_button_class(page_id.get() + 1 >= total_pages)
                                        href=move || {
                                            let p = (page_id.get() + 1).min(total_pages - 1);
                                            doc_id.get()
                                                .map(|id| format!("/viewer/{}/{}", id, p))
                                                .unwrap_or_default()
                                        }
                                    >"Next →"</a>
                                </div>
                            </div>
                            <div class="flex items-center flex-wrap gap-4 mt-2">
                                <span class="text-xs font-medium text-gray-700">"Overlays:"</span>
                                {toggles.into_iter().map(|(kind, sig)| {
                                    let (border, _) = kind_colors(kind);
                                    view! {
                                        <label class="flex items-center space-x-1 text-xs text-gray-700 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                prop:checked=move || sig.get()
                                                on:change=move |_| sig.update(|v| *v = !*v)
                                            />
                                            <span
                                                class="inline-block w-3 h-3 rounded-sm"
                                                style=format!("background:{border}")
                                            ></span>
                                            <span>{kind}</span>
                                        </label>
                                    }
                                }).collect::<Vec<_>>()}
                                <label class="flex items-center space-x-1 text-xs text-gray-400" title="TABLE structure lands in Stage B3">
                                    <input type="checkbox" disabled />
                                    <span class="inline-block w-3 h-3 rounded-sm bg-gray-300"></span>
                                    <span class="italic">"table (coming in B3)"</span>
                                </label>
                            </div>
                        </header>
                        <div class="flex-1 flex overflow-hidden">
                            <main class="flex-1 p-6 overflow-auto">
                                <div class="flex justify-center">
                                    {page_view(
                                        doc_id.get().map(|id| id.to_string()).unwrap_or_default(),
                                        page_id.get(),
                                        meta,
                                        elems,
                                        toggles_for_page.clone(),
                                        selected,
                                    )}
                                </div>
                            </main>
                            {move || selected.get().map(|element| view! {
                                <ElementPanel element=element selected=selected />
                            })}
                        </div>
                    }.into_any()
                }}
            </Suspense>
        </div>
    }
}

/// The page canvas: raster `<img>` (or placeholder) + absolutely positioned
/// bbox overlays scaled from PDF points to raster pixels.
fn page_view(
    doc_id: String,
    page_index: usize,
    meta: Option<PageMeta>,
    elems: Vec<ElementOverlay>,
    toggles: Vec<(&'static str, RwSignal<bool>)>,
    selected: RwSignal<Option<ElementOverlay>>,
) -> impl IntoView {
    // Container size and pts→px scale: from the raster when available,
    // otherwise a placeholder sized from the page's element extent.
    let (available, width_px, height_px, scale, reason) = match &meta {
        Some(meta) if meta.available => (
            true,
            meta.width_px as f32,
            meta.height_px as f32,
            meta.width_px as f32 / meta.width_pts.max(1.0),
            None,
        ),
        other => {
            let (mut max_x, mut max_y) = (612.0f32, 792.0f32);
            for e in &elems {
                if let Some((_, _, x1, y1)) = e.bbox {
                    max_x = max_x.max(x1);
                    max_y = max_y.max(y1);
                }
            }
            let scale = 1.5f32;
            let reason = other
                .as_ref()
                .and_then(|m| m.reason.clone())
                .unwrap_or_else(|| "page raster unavailable".to_string());
            (false, max_x * scale, max_y * scale, scale, Some(reason))
        }
    };

    let overlays: Vec<_> = elems
        .into_iter()
        .map(|element| {
            let kind_enabled = toggles
                .iter()
                .find(|(kind, _)| *kind == element.kind)
                .map(|(_, sig)| *sig);
            let bbox = element.bbox;
            (element, kind_enabled, bbox)
        })
        .collect();

    view! {
        <div class="bg-white rounded-lg shadow-lg p-4">
            <div
                class="relative border border-gray-200 overflow-hidden"
                style=format!("width:{width_px}px;height:{height_px}px")
            >
                {if available {
                    view! {
                        <img
                            src=format!("/api/v/docs/{doc_id}/pages/{page_index}/image.webp")
                            width=width_px as u32
                            height=height_px as u32
                            class="absolute top-0 left-0 w-full h-full select-none"
                            alt=format!("page {}", page_index + 1)
                        />
                    }.into_any()
                } else {
                    view! {
                        <div class="absolute top-0 left-0 w-full h-full bg-gray-50 flex items-center justify-center">
                            <div class="max-w-md text-center px-6">
                                <p class="text-sm font-medium text-gray-700 mb-1">"No page image"</p>
                                <p class="text-xs text-gray-500">{reason.unwrap_or_default()}</p>
                                <p class="text-xs text-gray-400 mt-2">"Element overlays below are drawn from the store at page scale."</p>
                            </div>
                        </div>
                    }.into_any()
                }}
                // Every overlay div is always in the tree; the kind toggle
                // only flips `display`. A structural `<Show>` here rendered
                // nothing under SSR streaming and then mismatched hydration —
                // reactive-attribute visibility keeps server and client DOM
                // identical regardless of toggle state (DV-009).
                {overlays.into_iter().filter_map(|(element, kind_enabled, bbox)| {
                    let enabled = kind_enabled?;
                    let (x0, y0, x1, y1) = bbox?;
                    let (border, fill) = kind_colors(&element.kind);
                    let base_style = format!(
                        "left:{:.1}px;top:{:.1}px;width:{:.1}px;height:{:.1}px;\
                         border:1.5px solid {border};background:{fill}",
                        x0 * scale,
                        y0 * scale,
                        (x1 - x0).max(1.0) * scale,
                        (y1 - y0).max(1.0) * scale,
                    );
                    let title = format!("{} #{}", element.kind, element.order_idx);
                    let style = move || {
                        if enabled.get() {
                            base_style.clone()
                        } else {
                            format!("{base_style};display:none")
                        }
                    };
                    Some(view! {
                        <div
                            class="absolute cursor-pointer"
                            style=style
                            title=title
                            on:click=move |_| selected.set(Some(element.clone()))
                        ></div>
                    })
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

/// Discover-mode side panel: text + metadata of the clicked element.
#[component]
fn ElementPanel(element: ElementOverlay, selected: RwSignal<Option<ElementOverlay>>) -> impl IntoView {
    let (border, fill) = kind_colors(&element.kind);
    let bbox = element
        .bbox
        .map(|(x0, y0, x1, y1)| format!("({x0:.1}, {y0:.1}) → ({x1:.1}, {y1:.1})"))
        .unwrap_or_else(|| "—".to_string());
    let metadata = serde_json::to_string_pretty(&element.metadata)
        .unwrap_or_else(|_| element.metadata.to_string());
    let font = match (&element.font_name, element.font_size) {
        (Some(name), Some(size)) => format!("{name} @ {size:.1}pt"),
        (Some(name), None) => name.clone(),
        (None, Some(size)) => format!("{size:.1}pt"),
        (None, None) => "—".to_string(),
    };

    view! {
        <aside class="bg-white border-l border-gray-200 shadow-lg" style="width:24rem;overflow-y:auto">
            <div class="p-4 border-b border-gray-200 flex items-center justify-between">
                <div class="flex items-center space-x-2">
                    <span
                        class="inline-block px-2 py-0.5 rounded text-xs font-semibold"
                        style=format!("border:1px solid {border};background:{fill}")
                    >{element.kind.clone()}</span>
                    <span class="text-xs text-gray-500">{format!("page {} • order {}", element.page, element.order_idx)}</span>
                </div>
                <button
                    class="text-gray-400 hover:text-gray-600 text-sm"
                    on:click=move |_| selected.set(None)
                >"✕ close"</button>
            </div>
            <div class="p-4 space-y-4 text-sm">
                <div>
                    <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Element id"</div>
                    <div class="font-mono text-xs text-gray-700 break-all">{element.id.clone()}</div>
                </div>
                <div>
                    <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Bounding box (pts)"</div>
                    <div class="font-mono text-xs text-gray-700">{bbox}</div>
                </div>
                <div>
                    <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Font"</div>
                    <div class="text-xs text-gray-700">{font}</div>
                </div>
                <div>
                    <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Text"</div>
                    <div class="text-xs text-gray-800 whitespace-pre-wrap bg-gray-50 rounded p-2" style="max-height:12rem;overflow-y:auto">
                        {element.text.clone().unwrap_or_else(|| "(no text)".to_string())}
                    </div>
                </div>
                <div>
                    <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Metadata"</div>
                    <pre class="text-xs text-gray-800 bg-gray-50 rounded p-2" style="max-height:16rem;overflow:auto">{metadata}</pre>
                </div>
            </div>
        </aside>
    }
}
