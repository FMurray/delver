//! Document page view: on-demand raster from the byte-cache, element bbox
//! overlays with per-kind toggles (Stage B kinds), the discover-mode
//! element inspector in a persistent right sidebar (DV-004, DV-014), and
//! Ctrl+F-style results mode over the latest run's provenance sidecar
//! (slice V6, DV-018): results bar, per-match highlights, match-stepping
//! Prev/Next with section page-filters and near-miss warnings.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use uuid::Uuid;

use crate::app::{InspectorContext, ResultsBus};
use crate::components::file_upload::get_document_by_id;
use crate::components::insert::{use_request_insert, INSERT_CHIP};
use crate::results::{
    clamp_current, initial_current, next_pos, page_indicator, prev_pos, section_chip_label,
    visible_indices, SharedResults,
};
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

/// Ctrl+F match highlight (V6) — deliberately distinct from the kind-colored
/// discover overlays: a translucent yellow fill with an amber border.
const HIGHLIGHT_BASE: &str =
    "background:rgba(250,204,21,0.40);border:1.5px solid rgba(202,138,4,0.9)";
/// The current match gets orange emphasis (the Ctrl+F "active" convention).
const HIGHLIGHT_CURRENT: &str = "background:rgba(249,115,22,0.45);\
     border:2px solid rgba(194,65,12,0.95);box-shadow:0 0 0 2px rgba(249,115,22,0.35)";

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

    // ── Ctrl+F results mode (V6, DV-018) ────────────────────────────────
    // All of this is client-side post-run state: results are `None` during
    // SSR and at hydration (runs never execute server-side, DV-013), so
    // every reactive read below renders its empty state identically on
    // server and client (DV-009).
    let results_bus = expect_context::<ResultsBus>();

    // The latest run's results, but only while ITS document is open.
    let active_results = Memo::new(move |_| -> Option<SharedResults> {
        let doc = doc_id.get()?.to_string();
        let results = results_bus.results.get()?;
        (results.doc_id == doc).then_some(results)
    });

    // Match indices visible under the section page-filter, in match order.
    let visible = Memo::new(move |_| match active_results.get() {
        Some(results) => visible_indices(&results, results_bus.section_filter.get()),
        None => Vec::new(),
    });

    // Position of the match one step away from the current cursor
    // (wrapping, Ctrl+F-style). Pure reads — shared by the Prev/Next click
    // handlers and the n/p keys.
    let nav_target = move |dir: i32| -> Option<usize> {
        let vis_len = visible.get().len();
        if vis_len == 0 {
            return None;
        }
        let cur = clamp_current(results_bus.current.get(), vis_len);
        Some(if dir > 0 {
            next_pos(cur, vis_len)
        } else {
            prev_pos(cur, vis_len)
        })
    };
    // 1-based page of the match the cursor sits ON. The Prev/Next hrefs
    // point HERE: their click handlers advance the cursor synchronously
    // first, the reactive href re-renders, and the router's delegated
    // listener then reads the anchor — so navigation always lands on the
    // (new) current match's page, with zero dependence on microtask/flush
    // ordering between the two listeners (DV-018).
    let current_match_page = move || -> Option<u32> {
        let results = active_results.get();
        let results = results.as_ref()?;
        let vis = visible.get();
        if vis.is_empty() {
            return None;
        }
        let cur = clamp_current(results_bus.current.get(), vis.len());
        Some(results.matches.get(*vis.get(cur)?)?.page)
    };
    let in_results = move || active_results.get().is_some();
    let page_url = move |page_index: usize| {
        doc_id
            .get()
            .map(|id| format!("/viewer/{id}/{page_index}"))
            .unwrap_or_default()
    };

    // A NEW run landed (run_id changed): seed the cursor to the first
    // visible match on the page being viewed — running never yanks
    // navigation away (DV-018) — else the first match overall. Client-only
    // (effects never run during SSR).
    Effect::new(move |prev: Option<Option<u64>>| {
        let run_id = active_results.get().map(|r| r.run_id);
        if let Some(results) = active_results.get() {
            if prev.flatten() != run_id {
                let page = page_id.get_untracked() as u32 + 1;
                let vis =
                    visible_indices(&results, results_bus.section_filter.get_untracked());
                results_bus.current.set(initial_current(&results, &vis, page));
            }
        }
        run_id
    });

    // n/p step through matches by clicking the real Prev/Next anchors, so
    // navigation stays on the DV-009 plain-link path. Wasm-only code; the
    // listener detaches with the component.
    #[cfg(feature = "hydrate")]
    {
        use leptos::ev;
        use wasm_bindgen::JsCast;

        let handle = window_event_listener(ev::keydown, move |ev| {
            if ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
                return;
            }
            let key = ev.key();
            if key != "n" && key != "p" {
                return;
            }
            // Never hijack typing (inputs, selects, the CodeMirror editor).
            let typing = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .map(|el| {
                    let tag = el.tag_name().to_ascii_uppercase();
                    tag == "INPUT"
                        || tag == "TEXTAREA"
                        || tag == "SELECT"
                        || el.closest(".CodeMirror").ok().flatten().is_some()
                })
                .unwrap_or(false);
            if typing
                || active_results.get_untracked().is_none()
                || visible.get_untracked().is_empty()
            {
                return;
            }
            let id = if key == "n" {
                "v6-next-match"
            } else {
                "v6-prev-match"
            };
            if let Some(el) = document().get_element_by_id(id) {
                if let Ok(el) = el.dyn_into::<web_sys::HtmlElement>() {
                    el.click();
                }
            }
        });
        on_cleanup(move || handle.remove());
    }

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
                                        // In a section page-filter the indicator
                                        // shows the span: "page 17 of 16–29" (V6).
                                        {move || {
                                            let page = page_id.get() as u32 + 1;
                                            let span = active_results.get().and_then(|r| {
                                                results_bus
                                                    .section_filter
                                                    .get()
                                                    .and_then(|i| r.sections.get(i).cloned())
                                            });
                                            page_indicator(page, total_pages, span.as_ref())
                                        }}
                                    </p>
                                </div>
                                // Prev/Next are plain links (the router intercepts
                                // same-origin clicks): programmatic `use_navigate`
                                // here ran during SSR resolve and panicked on
                                // js_sys::global() — see DV-009. In results mode
                                // (V6) the SAME pair steps through MATCHES: the
                                // click advances the cursor synchronously and the
                                // href tracks the CURRENT match's page, so the
                                // router (whose delegated listener runs after
                                // ours) reads the already-updated target (DV-018).
                                // Compact "pg" steppers keep plain page nav
                                // reachable.
                                <div class="flex items-center space-x-2">
                                    <a
                                        class="px-2 py-1 bg-gray-50 text-gray-500 rounded text-xs hover:bg-gray-200"
                                        style=move || if in_results() { "" } else { "display:none" }
                                        title="Previous page (plain page navigation)"
                                        href=move || page_url(page_id.get().saturating_sub(1))
                                    >"‹ pg"</a>
                                    <a
                                        class="px-2 py-1 bg-gray-50 text-gray-500 rounded text-xs hover:bg-gray-200"
                                        style=move || if in_results() { "" } else { "display:none" }
                                        title="Next page (plain page navigation)"
                                        href=move || page_url((page_id.get() + 1).min(total_pages - 1))
                                    >"pg ›"</a>
                                    <a
                                        id="v6-prev-match"
                                        class=move || {
                                            if in_results() {
                                                nav_button_class(visible.get().is_empty())
                                            } else {
                                                nav_button_class(page_id.get() == 0)
                                            }
                                        }
                                        title=move || if in_results() {
                                            "Previous match (p)"
                                        } else {
                                            "Previous page"
                                        }
                                        href=move || {
                                            if in_results() {
                                                current_match_page()
                                                    .map(|page| {
                                                        page_url(page.saturating_sub(1) as usize)
                                                    })
                                                    .unwrap_or_default()
                                            } else {
                                                page_url(page_id.get().saturating_sub(1))
                                            }
                                        }
                                        on:click=move |_| {
                                            if in_results() {
                                                if let Some(pos) = nav_target(-1) {
                                                    results_bus.current.set(pos);
                                                }
                                            }
                                        }
                                    >{move || if in_results() { "← Prev match" } else { "← Prev" }}</a>
                                    <a
                                        id="v6-next-match"
                                        class=move || {
                                            if in_results() {
                                                nav_button_class(visible.get().is_empty())
                                            } else {
                                                nav_button_class(page_id.get() + 1 >= total_pages)
                                            }
                                        }
                                        title=move || if in_results() {
                                            "Next match (n)"
                                        } else {
                                            "Next page"
                                        }
                                        href=move || {
                                            if in_results() {
                                                current_match_page()
                                                    .map(|page| {
                                                        page_url(page.saturating_sub(1) as usize)
                                                    })
                                                    .unwrap_or_default()
                                            } else {
                                                page_url((page_id.get() + 1).min(total_pages - 1))
                                            }
                                        }
                                        on:click=move |_| {
                                            if in_results() {
                                                if let Some(pos) = nav_target(1) {
                                                    results_bus.current.set(pos);
                                                }
                                            }
                                        }
                                    >{move || if in_results() { "Next match →" } else { "Next →" }}</a>
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
                        // Results bar (V6): match count + "x of N", section
                        // chips, near-miss warnings, exit. Rendered only in
                        // results mode — `active_results` is None during SSR
                        // and at hydration, so the initial tree is identical
                        // on both sides (DV-013/DV-009).
                        {results_bar(results_bus, active_results, visible, page_id, doc_id)}
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
                                        results_bus,
                                        active_results,
                                        visible,
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

/// The results bar (V6, DV-018): match position, section page-filter chips,
/// near-miss warnings with click-to-jump page links, and the exit button.
/// Renders nothing until a run lands (results are client-side post-run
/// state, DV-013); the JSON results panel in the query panel is untouched —
/// this bar is the navigable view over the same run.
fn results_bar(
    results_bus: ResultsBus,
    active_results: Memo<Option<SharedResults>>,
    visible: Memo<Vec<usize>>,
    page_id: Memo<usize>,
    doc_id: Memo<Option<Uuid>>,
) -> impl IntoView {
    move || {
        active_results.get().map(|results| {
            // "x of N matches" — tracks the cursor and the section filter.
            let pos_label = move || {
                let vis = visible.get();
                if vis.is_empty() {
                    return "0 matches".to_string();
                }
                let cur = clamp_current(results_bus.current.get(), vis.len());
                format!("{} of {} matches", cur + 1, vis.len())
            };

            // Section chips: "all" + one per distinct section attribution.
            let chips = (!results.sections.is_empty()).then(|| {
                let all_chip = {
                    let results = results.clone();
                    let class = move || {
                        if results_bus.section_filter.get().is_none() {
                            "px-2 py-0.5 rounded-full text-[11px] font-medium bg-blue-600 text-white"
                        } else {
                            "px-2 py-0.5 rounded-full text-[11px] font-medium bg-gray-100 text-gray-700 hover:bg-gray-200"
                        }
                    };
                    view! {
                        <button
                            class=class
                            title="Show matches on every page"
                            on:click=move |_| {
                                results_bus.section_filter.set(None);
                                let vis = visible_indices(&results, None);
                                let page = page_id.get_untracked() as u32 + 1;
                                results_bus.current.set(initial_current(&results, &vis, page));
                            }
                        >"all"</button>
                    }
                };
                let section_chips = results
                    .sections
                    .iter()
                    .enumerate()
                    .map(|(i, span)| {
                        let label = section_chip_label(span);
                        let results = results.clone();
                        let class = move || {
                            if results_bus.section_filter.get() == Some(i) {
                                "px-2 py-0.5 rounded-full text-[11px] font-medium bg-blue-600 text-white"
                            } else {
                                "px-2 py-0.5 rounded-full text-[11px] font-medium bg-gray-100 text-gray-700 hover:bg-gray-200"
                            }
                        };
                        view! {
                            <button
                                class=class
                                title="Filter navigation to this section's pages"
                                on:click=move |_| {
                                    results_bus.section_filter.set(Some(i));
                                    let vis = visible_indices(&results, Some(i));
                                    let page = page_id.get_untracked() as u32 + 1;
                                    results_bus
                                        .current
                                        .set(initial_current(&results, &vis, page));
                                }
                            >{label}</button>
                        }
                    })
                    .collect::<Vec<_>>();
                view! {
                    <div class="flex items-center flex-wrap gap-1.5">
                        <span class="text-[10px] font-medium text-gray-500 uppercase">"Sections:"</span>
                        {all_chip}
                        {section_chips}
                    </div>
                }
            });

            // Near-miss warnings (D-024 → V6): one amber row per miss, each
            // closest-candidate page reference a plain link (DV-009).
            let misses = (!results.misses.is_empty()).then(|| {
                let rows = results
                    .misses
                    .iter()
                    .map(|miss| {
                        let head = format!(
                            "match '{}' matched nothing at threshold {}",
                            miss.match_name, miss.threshold
                        );
                        let closest = miss
                            .near_misses
                            .iter()
                            .enumerate()
                            .map(|(j, near)| {
                                let sep = if j == 0 { " — closest: " } else { "; " };
                                let target = near.page.saturating_sub(1) as usize;
                                let href = move || {
                                    doc_id
                                        .get()
                                        .map(|id| format!("/viewer/{id}/{target}"))
                                        .unwrap_or_default()
                                };
                                view! {
                                    <span>
                                        {sep}
                                        <span class="italic">{format!("'{}'", near.text)}</span>
                                        {format!(" ({:.2}, ", near.score)}
                                        <a
                                            class="underline font-semibold hover:text-amber-950"
                                            title="Jump to this page"
                                            href=href
                                        >{format!("p{}", near.page)}</a>
                                        ")"
                                    </span>
                                }
                            })
                            .collect::<Vec<_>>();
                        let none_note = miss
                            .near_misses
                            .is_empty()
                            .then_some(" — no fuzzy-text candidates in scope");
                        view! {
                            <div class="text-[11px] text-amber-800 bg-amber-50 border border-amber-200 rounded px-2 py-1">
                                <span class="font-semibold">"⚠ "</span>
                                {head}{closest}{none_note}
                            </div>
                        }
                    })
                    .collect::<Vec<_>>();
                view! { <div class="flex flex-col gap-1 mt-1">{rows}</div> }
            });

            view! {
                <div id="v6-results-bar" class="bg-white border-b border-gray-200 px-6 py-2">
                    <div class="flex items-center flex-wrap gap-3">
                        <span class="text-xs font-semibold text-gray-900">"Results"</span>
                        <span id="v6-match-pos" class="text-xs text-gray-700">{pos_label}</span>
                        {chips}
                        <span class="flex-1"></span>
                        <button
                            class="text-gray-400 hover:text-gray-600 text-xs"
                            title="Exit results mode (clears highlights; Prev/Next return to page navigation)"
                            on:click=move |_| results_bus.clear()
                        >"✕ exit"</button>
                    </div>
                    {misses}
                </div>
            }
        })
    }
}

/// The page canvas: raster `<img>` (or placeholder) + absolutely positioned
/// bbox overlays scaled from PDF points to raster pixels, plus the V6
/// match-highlight layer (always in the tree, display-toggled — DV-009).
#[allow(clippy::too_many_arguments)]
fn page_view(
    doc_id: String,
    page_index: usize,
    meta: Option<PageMeta>,
    elems: Vec<ElementOverlay>,
    toggles: Vec<(&'static str, RwSignal<bool>)>,
    selected: RwSignal<Option<ElementOverlay>>,
    set_show_inspector: WriteSignal<bool>,
    results_bus: ResultsBus,
    active_results: Memo<Option<SharedResults>>,
    visible: Memo<Vec<usize>>,
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

    // V6 highlight layer data (id, bbox, element for click-to-inspect) —
    // captured before the discover overlays consume `elems`.
    let highlight_elems: Vec<(String, (f32, f32, f32, f32), ElementOverlay)> = elems
        .iter()
        .filter_map(|e| Some((e.id.clone(), e.bbox?, e.clone())))
        .collect();

    // Element ids to highlight on THIS page: (every visible match's ids,
    // the current match's ids). One memo per page render, O(1) lookups per
    // element style — recomputed on cursor/filter/run changes only.
    let store_page = page_index as u32 + 1;
    let hl_sets: Memo<(HashSet<String>, HashSet<String>)> = Memo::new(move |_| {
        let Some(results) = active_results.get() else {
            return (HashSet::new(), HashSet::new());
        };
        let vis = visible.get();
        let cur = clamp_current(results_bus.current.get(), vis.len());
        crate::results::highlight_sets(&results, &vis, cur, store_page)
    });

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
                // V6 match highlights: a second per-element layer, ALWAYS in
                // the tree with visibility driven by the style attribute
                // (the DV-009 overlay discipline). Results are client-side
                // post-run state (DV-013), so SSR and hydration both render
                // every highlight as display:none — structurally identical.
                {highlight_elems.into_iter().map(|(id, (x0, y0, x1, y1), element)| {
                    let base = format!(
                        "left:{:.1}px;top:{:.1}px;width:{:.1}px;height:{:.1}px",
                        x0 * scale,
                        y0 * scale,
                        (x1 - x0).max(1.0) * scale,
                        (y1 - y0).max(1.0) * scale,
                    );
                    let style = move || {
                        hl_sets.with(|(all, current)| {
                            if current.contains(&id) {
                                format!("{base};{HIGHLIGHT_CURRENT}")
                            } else if all.contains(&id) {
                                format!("{base};{HIGHLIGHT_BASE}")
                            } else {
                                format!("{base};display:none")
                            }
                        })
                    };
                    view! {
                        <div
                            class="absolute cursor-pointer"
                            style=style
                            title="matched element"
                            // Highlights are clickable like overlays: jump
                            // straight into the inspector (DV-014).
                            on:click=move |_| {
                                selected.set(Some(element.clone()));
                                set_show_inspector.set(true);
                            }
                        ></div>
                    }
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
