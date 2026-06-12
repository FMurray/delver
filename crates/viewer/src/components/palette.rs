//! Doc-aware query palette (DV-012): a collapsible side-panel section for
//! the open document listing section-heading candidates and detected tables,
//! plus starter templates. Every entry publishes a [`SnippetSpec`] on the
//! insert bus — the same generators the element side panel uses.

use leptos::prelude::*;
use leptos_router::hooks::use_location;

use crate::components::insert::{use_request_insert, INSERT_CHIP as CHIP};
use crate::components::query_panel::doc_id_from_path;
use crate::snippets::SnippetSpec;
use crate::store::{PaletteData, TableEntry};

/// Heading candidates + detected tables for one document (server side:
/// `store::doc_palette`, DV-012).
#[server]
pub async fn get_doc_palette(doc_id: String) -> Result<PaletteData, ServerFnError> {
    crate::store::doc_palette(&doc_id)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

/// The table whose typed scaffold pre-fills the "typed table" starter:
/// prefer tables with at least one named header column, then highest
/// confidence.
fn best_typed_table(tables: &[TableEntry]) -> Option<&TableEntry> {
    tables
        .iter()
        .filter(|t| !t.columns.is_empty())
        .max_by(|a, b| {
            let a_named = a.columns.iter().any(|c| c.header.is_some());
            let b_named = b.columns.iter().any(|c| c.header.is_some());
            a_named.cmp(&b_named).then(
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        })
}

const ROW: &str = "w-full text-left text-xs text-gray-800 hover:bg-blue-50 rounded px-1.5 py-1 flex items-start justify-between gap-2";

#[component]
pub fn QueryPalette() -> impl IntoView {
    let location = use_location();
    let current_doc = Memo::new(move |_| doc_id_from_path(&location.pathname.get()));
    let open = RwSignal::new(true);
    let insert = use_request_insert();

    let palette = Resource::new(
        move || current_doc.get(),
        |doc| async move {
            match doc {
                Some(doc) => get_doc_palette(doc).await.ok(),
                None => None,
            }
        },
    );

    view! {
        <div class="mt-6 border-t border-gray-200 pt-4">
            <button
                class="w-full flex items-center justify-between text-sm font-semibold text-gray-900"
                on:click=move |_| open.update(|v| *v = !*v)
            >
                <span>"Query palette"</span>
                <span class="text-gray-400 text-xs">{move || if open.get() { "▾" } else { "▸" }}</span>
            </button>
            <Show when=move || open.get()>
                <Suspense fallback=move || view! {
                    <p class="text-xs text-gray-500 mt-2">"Loading palette..."</p>
                }>
                    {move || {
                        // Read the resource directly in the Suspense-tracked
                        // closure (DV-009: nested reads are not awaited under
                        // streaming SSR).
                        let data = palette.get().flatten();
                        match data {
                            None => view! {
                                <p class="text-xs text-gray-500 mt-2">
                                    "Open a document to see its headings and tables."
                                </p>
                            }.into_any(),
                            Some(data) => palette_body(data, insert).into_any(),
                        }
                    }}
                </Suspense>
            </Show>
        </div>
    }
}

fn palette_body(data: PaletteData, insert: impl Fn(SnippetSpec) + Copy + 'static) -> impl IntoView {
    let first_heading = data.headings.first().map(|h| h.text.clone());
    let typed_starter = best_typed_table(&data.tables)
        .map(|t| (t.page, t.columns.clone()));

    // Starter templates: pre-filled from the open document where possible.
    let sections_text = first_heading.clone().unwrap_or_else(|| "Introduction".to_string());
    let tables_text = first_heading.unwrap_or_else(|| "Introduction".to_string());
    let typed_disabled = typed_starter.is_none();
    let starters = view! {
        <div class="mt-3">
            <div class="text-[10px] font-semibold text-gray-500 uppercase mb-1">"Starter templates"</div>
            <div class="flex flex-wrap gap-1.5">
                <button class=CHIP on:click=move |_| insert(SnippetSpec::PlainChunks)>
                    "plain chunks"
                </button>
                <button class=CHIP on:click={
                    let text = sections_text.clone();
                    move |_| insert(SnippetSpec::SectionScaffold { text: text.clone() })
                }>"sections + chunks"</button>
                <button class=CHIP on:click={
                    let text = tables_text.clone();
                    move |_| insert(SnippetSpec::SectionWithTable { text: text.clone() })
                }>"tables in section"</button>
                <button
                    class=CHIP
                    disabled=typed_disabled
                    on:click={
                        let starter = typed_starter.clone();
                        move |_| {
                            if let Some((page, columns)) = starter.clone() {
                                insert(SnippetSpec::TypedTable { page, columns });
                            }
                        }
                    }
                >"typed table"</button>
            </div>
        </div>
    };

    let headings = view! {
        <div class="mt-3">
            <div class="text-[10px] font-semibold text-gray-500 uppercase mb-1">
                {format!("Headings ({})", data.headings.len())}
            </div>
            {if data.headings.is_empty() {
                view! { <p class="text-xs text-gray-400">"(no heading candidates)"</p> }.into_any()
            } else {
                view! {
                    <div class="space-y-0.5" style="max-height:14rem;overflow-y:auto">
                        {data.headings.into_iter().map(|h| {
                            let text = h.text.clone();
                            let match_text = h.text.clone();
                            let label = h.text.clone();
                            view! {
                                <div class=ROW>
                                    <button
                                        class="flex-1 text-left truncate"
                                        title=format!("Insert Section scaffold for \u{201c}{}\u{201d}", h.text)
                                        on:click=move |_| insert(SnippetSpec::SectionScaffold { text: text.clone() })
                                    >
                                        <span class="text-gray-400 font-mono text-[10px] mr-1">{format!("p{}", h.page)}</span>
                                        {label}
                                    </button>
                                    <button
                                        class=CHIP
                                        title="Insert match block only"
                                        on:click=move |_| insert(SnippetSpec::TextMatch { text: match_text.clone() })
                                    >"match"</button>
                                </div>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                }.into_any()
            }}
        </div>
    };

    let tables = view! {
        <div class="mt-3">
            <div class="text-[10px] font-semibold text-gray-500 uppercase mb-1">
                {format!("Tables ({})", data.tables.len())}
            </div>
            {if data.tables.is_empty() {
                view! { <p class="text-xs text-gray-400">"(no tables detected)"</p> }.into_any()
            } else {
                view! {
                    <div class="space-y-0.5" style="max-height:14rem;overflow-y:auto">
                        {data.tables.into_iter().map(|t| {
                            let page = t.page;
                            let columns = t.columns.clone();
                            let typed_ok = !t.columns.is_empty();
                            view! {
                                <div class=ROW>
                                    <button
                                        class="flex-1 text-left truncate"
                                        title=format!("Insert Table(as=\"table_p{page}\")")
                                        on:click=move |_| insert(SnippetSpec::TableRef { page })
                                    >
                                        <span class="text-gray-400 font-mono text-[10px] mr-1">{format!("p{}", t.page)}</span>
                                        {format!("{}×{} • {} • {:.2}", t.n_rows, t.n_cols, t.strategy, t.confidence)}
                                    </button>
                                    <button
                                        class=CHIP
                                        disabled=!typed_ok
                                        title="Insert TYPE … AS TABLE scaffold + typed Table"
                                        on:click=move |_| insert(SnippetSpec::TypedTable { page, columns: columns.clone() })
                                    >"typed"</button>
                                </div>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                }.into_any()
            }}
        </div>
    };

    view! {
        <div>
            {starters}
            {headings}
            {tables}
        </div>
    }
}
