//! "Documents in Store" as a file tree (DV-015): corpus → one collapsible
//! level per hive-style partition `key=value` (D-023,
//! `documents.metadata.partitions`) → compact document leaf. Documents
//! without partitions sit directly under their corpus.
//!
//! Ordering is deterministic: corpora and partition segments sort
//! alphabetically (the stored jsonb object is unordered, so the original
//! hive-path key order is lost — `DocumentSummary::partitions` is a
//! `BTreeMap`); documents inside a node keep the listing's newest-first
//! order. Built client-side from the single existing listing call — no
//! per-node requests.
//!
//! Default state: every corpus collapsed except the one containing the
//! currently open document, whose full partition path is expanded. The
//! initial expansion derives only from (listing data, route pathname), both
//! identical on server and client, so SSR and hydration render the same
//! subtree (DV-009).

use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos_router::hooks::use_location;

use crate::components::query_panel::doc_id_from_path;
use crate::store::DocumentSummary;

// ───────────────────────── pure tree building ─────────────────────────

/// One corpus subtree.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusNode {
    pub name: String,
    /// Total documents anywhere under this corpus.
    pub total: usize,
    /// Documents without partition tags (direct children of the corpus).
    pub direct_docs: Vec<DocumentSummary>,
    pub children: Vec<PartitionNode>,
}

/// One `key=value` level of a corpus' partition hierarchy.
#[derive(Debug, Clone, PartialEq)]
pub struct PartitionNode {
    /// `key=value` segment label.
    pub label: String,
    /// Total documents anywhere under this node.
    pub total: usize,
    /// Documents whose partition path terminates exactly here.
    pub direct_docs: Vec<DocumentSummary>,
    pub children: Vec<PartitionNode>,
}

/// A document's partition path as `key=value` segments, ALPHABETICAL by key
/// (deterministic stand-in for the unpersisted hive order, DV-015).
pub fn partition_path(doc: &DocumentSummary) -> Vec<String> {
    doc.partitions
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Split `docs` at `depth`: documents whose path ends here stay direct;
/// the rest group by their `depth`-th segment (alphabetical) and recurse.
fn build_partition_level(
    docs: Vec<(Vec<String>, DocumentSummary)>,
    depth: usize,
) -> (Vec<DocumentSummary>, Vec<PartitionNode>) {
    let mut direct = Vec::new();
    let mut groups: BTreeMap<String, Vec<(Vec<String>, DocumentSummary)>> = BTreeMap::new();
    for (path, doc) in docs {
        match path.get(depth) {
            None => direct.push(doc),
            Some(segment) => groups.entry(segment.clone()).or_default().push((path, doc)),
        }
    }
    let children = groups
        .into_iter()
        .map(|(label, docs)| {
            let total = docs.len();
            let (direct_docs, children) = build_partition_level(docs, depth + 1);
            PartitionNode {
                label,
                total,
                direct_docs,
                children,
            }
        })
        .collect();
    (direct, children)
}

/// Group the flat listing into corpus subtrees (corpora alphabetical;
/// document order within nodes preserved from the listing).
pub fn build_corpus_nodes(docs: Vec<DocumentSummary>) -> Vec<CorpusNode> {
    let mut by_corpus: BTreeMap<String, Vec<DocumentSummary>> = BTreeMap::new();
    for doc in docs {
        by_corpus.entry(doc.corpus.clone()).or_default().push(doc);
    }
    by_corpus
        .into_iter()
        .map(|(name, docs)| {
            let total = docs.len();
            let with_paths = docs
                .into_iter()
                .map(|doc| (partition_path(&doc), doc))
                .collect();
            let (direct_docs, children) = build_partition_level(with_paths, 0);
            CorpusNode {
                name,
                total,
                direct_docs,
                children,
            }
        })
        .collect()
}

// ───────────────────────────── components ─────────────────────────────

const NODE_ROW: &str = "w-full flex items-center gap-1 text-left text-xs text-gray-800 hover:bg-gray-100 rounded px-1 py-0.5";
const INDENT_REM: f32 = 0.75;

fn indent_style(depth: usize) -> String {
    format!("padding-left:{:.2}rem", depth as f32 * INDENT_REM)
}

#[component]
pub fn DocTree(docs: Vec<DocumentSummary>) -> impl IntoView {
    // The open document decides the default-expanded path. Untracked read:
    // expansion defaults are computed when the listing (re)loads, not reset
    // on every client-side navigation. SSR and hydration see the same
    // pathname, so the initial tree is identical (DV-009).
    let location = use_location();
    let current_id = doc_id_from_path(&location.pathname.get_untracked());
    let current_path: Option<(String, Vec<String>)> = current_id
        .as_deref()
        .and_then(|id| docs.iter().find(|d| d.id == id))
        .map(|doc| (doc.corpus.clone(), partition_path(doc)));

    let nodes = build_corpus_nodes(docs);
    view! {
        <div class="space-y-0.5">
            {nodes
                .into_iter()
                .map(|node| {
                    let on_path: Option<&[String]> = match &current_path {
                        Some((corpus, path)) if *corpus == node.name => Some(path.as_slice()),
                        _ => None,
                    };
                    corpus_view(node, on_path, current_id.as_deref())
                })
                .collect::<Vec<_>>()}
        </div>
    }
}

fn corpus_view(node: CorpusNode, on_path: Option<&[String]>, current_id: Option<&str>) -> impl IntoView {
    let expanded = RwSignal::new(on_path.is_some());
    let CorpusNode {
        name,
        total,
        direct_docs,
        children,
    } = node;
    // `Show` children may run repeatedly, so the body closure owns its data
    // and rebuilds from clones on each expansion.
    let on_path: Vec<String> = on_path.unwrap_or(&[]).to_vec();
    let current_id: Option<String> = current_id.map(str::to_string);
    let title = name.clone();
    view! {
        <div>
            <button class=NODE_ROW on:click=move |_| expanded.update(|v| *v = !*v)>
                <span class="text-gray-400 w-3 shrink-0">
                    {move || if expanded.get() { "▾" } else { "▸" }}
                </span>
                <span class="font-medium truncate" title=title>{name}</span>
                <span class="ml-auto text-[10px] text-gray-400 shrink-0">{total}</span>
            </button>
            <Show when=move || expanded.get()>
                {node_body(
                    direct_docs.clone(),
                    children.clone(),
                    &on_path,
                    current_id.as_deref(),
                    1,
                )}
            </Show>
        </div>
    }
}

/// Recursive partition level. Returns `AnyView` (recursive `impl IntoView`
/// would be an infinite type).
fn partition_view(
    node: PartitionNode,
    on_path: &[String],
    current_id: Option<&str>,
    depth: usize,
) -> AnyView {
    let on_this_path = on_path.first() == Some(&node.label);
    let expanded = RwSignal::new(on_this_path);
    let PartitionNode {
        label,
        total,
        direct_docs,
        children,
    } = node;
    let child_path: Vec<String> = if on_this_path {
        on_path[1..].to_vec()
    } else {
        Vec::new()
    };
    let current_id: Option<String> = current_id.map(str::to_string);
    let title = label.clone();
    view! {
        <div>
            <button
                class=NODE_ROW
                style=indent_style(depth)
                on:click=move |_| expanded.update(|v| *v = !*v)
            >
                <span class="text-gray-400 w-3 shrink-0">
                    {move || if expanded.get() { "▾" } else { "▸" }}
                </span>
                <span class="font-mono text-[11px] truncate" title=title>{label}</span>
                <span class="ml-auto text-[10px] text-gray-400 shrink-0">{total}</span>
            </button>
            <Show when=move || expanded.get()>
                {node_body(
                    direct_docs.clone(),
                    children.clone(),
                    &child_path,
                    current_id.as_deref(),
                    depth + 1,
                )}
            </Show>
        </div>
    }
    .into_any()
}

/// Shared "expanded contents" of a corpus or partition node: child partition
/// levels first, then the documents that terminate here.
fn node_body(
    direct_docs: Vec<DocumentSummary>,
    children: Vec<PartitionNode>,
    on_path: &[String],
    current_id: Option<&str>,
    depth: usize,
) -> AnyView {
    let children = children
        .into_iter()
        .map(|child| partition_view(child, on_path, current_id, depth))
        .collect::<Vec<_>>();
    let docs = direct_docs
        .into_iter()
        .map(|doc| doc_leaf(doc, current_id, depth))
        .collect::<Vec<_>>();
    view! {
        <div>
            {children}
            {docs}
        </div>
    }
    .into_any()
}

/// Compact document leaf: name, pages, source marker, View link. Plain
/// `<a href>` — the router intercepts same-origin clicks (DV-009).
fn doc_leaf(doc: DocumentSummary, current_id: Option<&str>, depth: usize) -> impl IntoView {
    let is_current = current_id == Some(doc.id.as_str());
    let card_class = if is_current {
        "flex items-center justify-between gap-2 p-1.5 bg-blue-50 border border-blue-200 rounded-md"
    } else {
        "flex items-center justify-between gap-2 p-1.5 bg-white border border-gray-200 rounded-md hover:bg-gray-50"
    };
    let (dot_class, dot_title) = if doc.has_source {
        ("inline-block w-2 h-2 rounded-full bg-green-500 shrink-0", "source bytes cached")
    } else {
        ("inline-block w-2 h-2 rounded-full bg-amber-500 shrink-0", "no source bytes (overlays only)")
    };
    view! {
        <div style=indent_style(depth) class="py-0.5">
            <div class=card_class>
                <div class="flex-1 min-w-0">
                    <div class="text-xs font-medium text-gray-900 truncate" title=doc.name.clone()>
                        {doc.name.clone()}
                    </div>
                    <div class="text-[10px] text-gray-500">
                        {format!(
                            "{} pages • v{} • {}",
                            doc.page_count,
                            doc.parse_version,
                            doc.parsed_at.format("%Y-%m-%d %H:%M"),
                        )}
                    </div>
                </div>
                <span class=dot_class title=dot_title></span>
                <a
                    class="text-xs px-2 py-1 bg-blue-100 text-blue-700 rounded hover:bg-blue-200 shrink-0"
                    href=format!("/viewer/{}/0", doc.id)
                >"View"</a>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(corpus: &str, name: &str, partitions: &[(&str, &str)]) -> DocumentSummary {
        DocumentSummary {
            id: uuid::Uuid::new_v4().to_string(),
            corpus: corpus.to_string(),
            name: name.to_string(),
            uri: None,
            page_count: 1,
            parse_version: 1,
            parsed_at: chrono::Utc::now(),
            has_source: false,
            partitions: partitions
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn partition_path_is_alphabetical_by_key() {
        // Hive order was year/company; stored jsonb is unordered, so the
        // documented deterministic order is alphabetical (DV-015).
        let d = doc("demo", "a", &[("year", "2015"), ("company", "3M")]);
        assert_eq!(partition_path(&d), vec!["company=3M", "year=2015"]);
    }

    #[test]
    fn corpora_sort_alphabetically_and_count_descendants() {
        let nodes = build_corpus_nodes(vec![
            doc("zeta", "z1", &[]),
            doc("demo", "d1", &[("company", "3M"), ("year", "2015")]),
            doc("demo", "d2", &[("company", "3M"), ("year", "2016")]),
        ]);
        assert_eq!(
            nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            vec!["demo", "zeta"]
        );
        assert_eq!(nodes[0].total, 2);
        assert_eq!(nodes[1].total, 1);
    }

    #[test]
    fn unpartitioned_docs_sit_directly_under_their_corpus() {
        let nodes = build_corpus_nodes(vec![
            doc("mixed", "plain", &[]),
            doc("mixed", "tagged", &[("state", "CA")]),
        ]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].direct_docs.len(), 1);
        assert_eq!(nodes[0].direct_docs[0].name, "plain");
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].label, "state=CA");
        assert_eq!(nodes[0].children[0].direct_docs.len(), 1);
    }

    #[test]
    fn nested_levels_group_by_segment_and_keep_doc_order() {
        let nodes = build_corpus_nodes(vec![
            doc("demo", "first", &[("company", "3M"), ("year", "2016")]),
            doc("demo", "second", &[("company", "3M"), ("year", "2015")]),
            doc("demo", "other", &[("company", "ACME")]),
        ]);
        let company_levels = &nodes[0].children;
        assert_eq!(
            company_levels.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            vec!["company=3M", "company=ACME"]
        );
        let three_m = &company_levels[0];
        assert_eq!(three_m.total, 2);
        assert!(three_m.direct_docs.is_empty());
        assert_eq!(
            three_m.children.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            vec!["year=2015", "year=2016"]
        );
        // "other" terminates at company=ACME (path shorter than siblings).
        assert_eq!(company_levels[1].direct_docs.len(), 1);
    }

    #[test]
    fn docs_within_a_node_keep_listing_order() {
        let nodes = build_corpus_nodes(vec![
            doc("c", "newest", &[("k", "v")]),
            doc("c", "older", &[("k", "v")]),
        ]);
        let leaf = &nodes[0].children[0];
        assert_eq!(
            leaf.direct_docs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["newest", "older"]
        );
    }
}
