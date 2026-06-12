//! Document page view: on-demand raster from the byte-cache, element bbox
//! overlays with per-kind toggles (Stage B kinds), and the discover-mode
//! element inspector in a persistent right sidebar (DV-004, DV-014).

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use uuid::Uuid;

use crate::app::InspectorContext;
use crate::components::file_upload::get_document_by_id;
use crate::components::insert::{use_request_insert, INSERT_CHIP};
use crate::snippets::{column_specs, AuxRefKind, CellLite, SnippetSpec};
use crate::store::{CellOverlay, ElementOverlay, PageMeta};

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
        "table" => ("rgba(220,38,38,0.95)", "rgba(220,38,38,0.04)"),
        _ => ("rgba(107,114,128,0.9)", "rgba(107,114,128,0.10)"),
    }
}

/// Inner cell-grid styling for table overlays (D-018 cells).
const CELL_BORDER: &str = "rgba(220,38,38,0.45)";
const HEADER_FILL: &str = "rgba(220,38,38,0.16)";

const TOGGLE_KINDS: [&str; 6] = ["text", "annotation", "figure", "path", "image", "table"];

/// Dense (row, col)-addressable text grid built from a table's cells:
/// `grid[row][col] = (text, is_header)`. Sized from the element metadata's
/// n_rows/n_cols when present (D-018 writes them at detection time), else
/// from the cells' max indices; out-of-range cells are dropped.
fn cell_text_grid(
    metadata: &serde_json::Value,
    cells: &[CellOverlay],
) -> Vec<Vec<(String, bool)>> {
    let from_meta = |key: &str| metadata.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    let n_rows = (from_meta("n_rows").max(cells.iter().map(|c| c.row as i64 + 1).max().unwrap_or(0)))
        .max(0) as usize;
    let n_cols = (from_meta("n_cols").max(cells.iter().map(|c| c.col as i64 + 1).max().unwrap_or(0)))
        .max(0) as usize;
    let mut grid = vec![vec![(String::new(), false); n_cols]; n_rows];
    for cell in cells {
        if let Some(slot) = grid
            .get_mut(cell.row.max(0) as usize)
            .and_then(|row| row.get_mut(cell.col.max(0) as usize))
        {
            *slot = (cell.text.clone().unwrap_or_default(), cell.is_header);
        }
    }
    grid
}

/// "n_rows × n_cols • strategy • confidence" from a table element's
/// metadata (the exact keys D-018 persists).
fn table_summary(metadata: &serde_json::Value) -> String {
    let int = |key: &str| {
        metadata
            .get(key)
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    let strategy = metadata
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let confidence = metadata
        .get("confidence")
        .and_then(|v| v.as_f64())
        .map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "?".to_string());
    format!(
        "{} rows × {} cols • {strategy} • confidence {confidence}",
        int("n_rows"),
        int("n_cols"),
    )
}

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

    // Selected element for the discover-mode inspector (right sidebar).
    let selected: RwSignal<Option<ElementOverlay>> = RwSignal::new(None);
    let InspectorContext(show_inspector, set_show_inspector) =
        use_context::<InspectorContext>().expect("inspector context in doc view");

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
                                        set_show_inspector,
                                    )}
                                </div>
                            </main>
                            // Persistent right sidebar (DV-014): empty-state
                            // hint until an element is clicked. `selected`
                            // starts None on server and client alike, so SSR
                            // and hydration render the same subtree.
                            <Show when=move || show_inspector.get()>
                                <InspectorPanel selected=selected />
                            </Show>
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
    set_show_inspector: WriteSignal<bool>,
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
                    // Table cell grid (D-018): thin inner borders from each
                    // cell's bbox, positioned relative to the table overlay
                    // (so the kind toggle's display:none hides them too);
                    // header cells get a fill tint. pointer-events:none keeps
                    // clicks landing on the table element itself.
                    let cell_grid = element.cells.as_deref().unwrap_or_default().iter()
                        .filter_map(|cell| {
                            let (cx0, cy0, cx1, cy1) = cell.bbox?;
                            let header_fill = if cell.is_header {
                                format!("background:{HEADER_FILL};")
                            } else {
                                String::new()
                            };
                            let style = format!(
                                "left:{:.1}px;top:{:.1}px;width:{:.1}px;height:{:.1}px;\
                                 border:1px solid {CELL_BORDER};{header_fill}pointer-events:none",
                                (cx0 - x0) * scale,
                                (cy0 - y0) * scale,
                                (cx1 - cx0).max(1.0) * scale,
                                (cy1 - cy0).max(1.0) * scale,
                            );
                            Some(view! { <div class="absolute" style=style></div> })
                        })
                        .collect::<Vec<_>>();
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
                            // Clicking an element also surfaces the inspector
                            // when it was collapsed — click-select must never
                            // look dead (DV-014).
                            on:click=move |_| {
                                selected.set(Some(element.clone()));
                                set_show_inspector.set(true);
                            }
                        >{cell_grid}</div>
                    })
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

/// "Insert into query" actions for one element (DV-012): context-appropriate
/// snippet specs published on the insert bus. The spec is rendered against
/// the live editor buffer at insertion time (names stay unique).
fn insert_actions(element: &ElementOverlay) -> Option<AnyView> {
    let insert = use_request_insert();
    let page = element.page;
    match element.kind.as_str() {
        "text" => {
            let text = element.text.clone()?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let match_text = text.clone();
            Some(view! {
                <div class="flex flex-wrap gap-1.5">
                    <button
                        class=INSERT_CHIP
                        title="Insert Match<Section> block matching this text"
                        on:click=move |_| insert(SnippetSpec::TextMatch { text: match_text.clone() })
                    >"match only"</button>
                    <button
                        class=INSERT_CHIP
                        title="Insert match block + Section { TextChunk } scaffold"
                        on:click=move |_| insert(SnippetSpec::SectionScaffold { text: text.clone() })
                    >"section scaffold"</button>
                </div>
            }.into_any())
        }
        "table" => {
            let cells: Vec<CellLite> = element
                .cells
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|c| CellLite {
                    row: c.row,
                    col: c.col,
                    text: c.text.clone(),
                    is_header: c.is_header,
                })
                .collect();
            let n_cols = element
                .metadata
                .get("n_cols")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(cells.iter().map(|c| c.col as i64 + 1).max().unwrap_or(0))
                .max(0) as usize;
            let columns = column_specs(n_cols, &cells);
            let typed_ok = !columns.is_empty();
            Some(view! {
                <div class="flex flex-wrap gap-1.5">
                    <button
                        class=INSERT_CHIP
                        title=format!("Insert Table(as=\"table_p{page}\")")
                        on:click=move |_| insert(SnippetSpec::TableRef { page })
                    >{format!("Table(as=\"table_p{page}\")")}</button>
                    <button
                        class=INSERT_CHIP
                        disabled=!typed_ok
                        title="Insert TYPE … AS TABLE scaffold (fields from headers, DECIMAL for numeric columns) + typed Table"
                        on:click=move |_| insert(SnippetSpec::TypedTable { page, columns: columns.clone() })
                    >"typed…"</button>
                </div>
            }.into_any())
        }
        "annotation" => Some(view! {
            <div class="flex flex-wrap gap-1.5">
                <button
                    class=INSERT_CHIP
                    title="Insert Annotation selector"
                    on:click=move |_| insert(SnippetSpec::AuxRef { kind: AuxRefKind::Annotation, page })
                >{format!("Annotation(as=\"annotation_p{page}\")")}</button>
            </div>
        }.into_any()),
        "figure" => Some(view! {
            <div class="flex flex-wrap gap-1.5">
                <button
                    class=INSERT_CHIP
                    title="Insert Figure selector"
                    on:click=move |_| insert(SnippetSpec::AuxRef { kind: AuxRefKind::Figure, page })
                >{format!("Figure(as=\"figure_p{page}\")")}</button>
            </div>
        }.into_any()),
        _ => None,
    }
}

/// Persistent right sidebar housing the discover-mode element inspector
/// (DV-014). Mirrors the left aside's chrome (`app::SidePanel`) with
/// `border-l` instead of `border-r`; collapsible through
/// [`InspectorContext`]'s nav toggle. Shows an empty-state hint until an
/// element is clicked. `selected` starts `None` on server and client, so the
/// initial subtree is identical across hydration (DV-009).
#[component]
pub fn InspectorPanel(selected: RwSignal<Option<ElementOverlay>>) -> impl IntoView {
    view! {
        <aside class="w-96 bg-white border-l border-gray-200 shadow-lg transition-all duration-300 ease-in-out">
            <div class="h-full flex flex-col">
                {move || match selected.get() {
                    Some(element) => view! {
                        <ElementPanel element=element selected=selected />
                    }
                    .into_any(),
                    None => view! {
                        <div class="p-4 border-b border-gray-200">
                            <h2 class="text-sm font-semibold text-gray-900">"Element inspector"</h2>
                            <p class="text-xs text-gray-600 mt-1">"Discover mode"</p>
                        </div>
                        <div class="flex-1 p-6 overflow-y-auto">
                            <p class="text-sm text-gray-500 italic">
                                "Click any element on the page to inspect it."
                            </p>
                        </div>
                    }
                    .into_any(),
                }}
            </div>
        </aside>
    }
}

/// Inspector contents for one clicked element: kind badge, id/bbox/font,
/// insert-into-query actions, table structure, text, metadata.
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

    // Discover → query: insert actions for this element kind (DV-012).
    let insert_block = insert_actions(&element).map(|actions| view! {
        <div>
            <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Insert into query"</div>
            {actions}
        </div>
    });

    // Table structure section (kind=table, D-018): n_rows × n_cols, strategy,
    // confidence from element metadata + the cell text grid.
    let table_section = element.cells.as_ref().map(|cells| {
        let summary = table_summary(&element.metadata);
        let grid = cell_text_grid(&element.metadata, cells);
        view! {
            <div>
                <div class="text-xs font-medium text-gray-500 uppercase mb-1">"Table structure"</div>
                <div class="text-xs text-gray-700 mb-2">{summary}</div>
                <div class="bg-gray-50 rounded p-2" style="max-height:18rem;overflow:auto">
                    <table class="text-xs text-gray-800 border-collapse">
                        <tbody>
                            {grid.into_iter().map(|row| view! {
                                <tr>
                                    {row.into_iter().map(|(text, is_header)| {
                                        let class = if is_header {
                                            "border border-gray-300 px-1.5 py-0.5 align-top font-semibold"
                                        } else {
                                            "border border-gray-300 px-1.5 py-0.5 align-top"
                                        };
                                        let style = if is_header {
                                            format!("background:{HEADER_FILL}")
                                        } else {
                                            String::new()
                                        };
                                        view! { <td class=class style=style>{text}</td> }
                                    }).collect::<Vec<_>>()}
                                </tr>
                            }).collect::<Vec<_>>()}
                        </tbody>
                    </table>
                </div>
            </div>
        }
    });

    view! {
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
            <div class="flex-1 p-4 space-y-4 text-sm overflow-y-auto">
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
                {insert_block}
                {table_section}
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
    }
}

#[cfg(test)]
mod tests {
    use super::{cell_text_grid, table_summary};
    use crate::store::CellOverlay;

    fn cell(row: i32, col: i32, text: &str, is_header: bool) -> CellOverlay {
        CellOverlay {
            row,
            col,
            row_span: 1,
            col_span: 1,
            bbox: Some((0.0, 0.0, 1.0, 1.0)),
            text: if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            },
            is_header,
        }
    }

    #[test]
    fn grid_is_sized_from_metadata_and_placed_by_row_col() {
        let metadata = serde_json::json!({"n_rows": 2, "n_cols": 3});
        // Sparse cells: (0,0) header, (1,2) body; everything else empty.
        let grid = cell_text_grid(
            &metadata,
            &[cell(0, 0, "Sales", true), cell(1, 2, "10,328", false)],
        );
        assert_eq!(grid.len(), 2);
        assert_eq!(grid[0].len(), 3);
        assert_eq!(grid[0][0], ("Sales".to_string(), true));
        assert_eq!(grid[0][1], (String::new(), false));
        assert_eq!(grid[1][2], ("10,328".to_string(), false));
    }

    #[test]
    fn grid_grows_to_cell_extent_when_metadata_is_absent() {
        let grid = cell_text_grid(&serde_json::json!({}), &[cell(2, 1, "x", false)]);
        assert_eq!(grid.len(), 3);
        assert_eq!(grid[0].len(), 2);
        assert_eq!(grid[2][1], ("x".to_string(), false));
    }

    #[test]
    fn summary_renders_d018_metadata_keys() {
        let metadata = serde_json::json!({
            "n_rows": 10, "n_cols": 7, "strategy": "ruled", "confidence": 1.0
        });
        assert_eq!(
            table_summary(&metadata),
            "10 rows × 7 cols • ruled • confidence 1.00"
        );
        assert_eq!(
            table_summary(&serde_json::json!({})),
            "? rows × ? cols • ? • confidence ?"
        );
    }
}
