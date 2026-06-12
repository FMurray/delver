//! No-code structural query builder (slice V4, DV-016) — the palette now
//! mirrors the DOM structure of the actual query: the parsed buffer renders
//! as a node tree with add-slots between/around nodes and inside structural
//! nodes; every node expands into an attribute FORM whose values are
//! discoverable (heading picker, rule-type selector, TYPE flows prefilled
//! from detected tables) so nobody has to know DocQL attribute names or
//! shapes by heart. The text editor below is "view source": every form edit
//! splices the buffer immediately, and manual edits round-trip back through
//! the parse (components::builder).
//!
//! The DV-012 doc-aware data fetch (headings + detected tables) survives as
//! the feed for the pickers; the old flat snippet lists and starter chips
//! are superseded by the tree itself.

use std::sync::Arc;

use leptos::prelude::*;
use leptos_router::hooks::use_location;

use crate::components::builder::{QueryBuilder, SlotKey, Snapshot};
use crate::components::query_panel::doc_id_from_path;
use crate::query_tree::{
    self, attr, fmt_num, match_def_by_name, quote_str, slot_menu, type_names, AttrValue,
    AttrWrite, HeuristicRow, MatchRuleNode, NodeKind, NodePath, QueryNode, QueryTree, RuleSpec,
};
use crate::snippets::{type_fields_from_columns, uniquify};
use crate::store::{PaletteData, TableEntry};

/// Heading candidates + detected tables for one document (server side:
/// `store::doc_palette`, DV-012).
#[server]
pub async fn get_doc_palette(doc_id: String) -> Result<PaletteData, ServerFnError> {
    crate::store::doc_palette(&doc_id)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

// ───────────────────────── transient UI state ─────────────────────────

/// "New TYPE…" flow state (lives at palette level so tree rebuilds do not
/// reset it).
#[derive(Debug, Clone, PartialEq)]
struct TypeDraft {
    table_path: NodePath,
    /// false → still picking a detected table; true → editing field rows.
    picked: bool,
    name: String,
    fields: Vec<(String, String)>,
}

/// Everything the recursive render pass needs (cloned per node — cheap:
/// signals are Copy, palette data is shared).
#[derive(Clone)]
struct Ctx {
    builder: QueryBuilder,
    palette: Option<Arc<PaletteData>>,
    /// Snapshot text (selection→editor sync needs UTF-16 offsets).
    text: Arc<String>,
    open_slot: RwSignal<Option<SlotKey>>,
    end_adding: RwSignal<Option<NodePath>>,
    type_draft: RwSignal<Option<TypeDraft>>,
    selected: Option<NodePath>,
    diag_path: Option<NodePath>,
    diag_msg: Option<String>,
}

// ───────────────────────── shared styling ─────────────────────────

const INPUT: &str = "w-full border border-gray-300 rounded px-1.5 py-0.5 text-xs focus:outline-none focus:ring-1 focus:ring-blue-400";
const SELECT: &str = "w-full border border-gray-300 rounded px-1 py-0.5 text-xs bg-white focus:outline-none focus:ring-1 focus:ring-blue-400";
const LABEL: &str = "block text-[10px] font-semibold text-gray-500 uppercase mt-2 mb-0.5";
const MINI_BTN: &str = "px-1.5 py-0.5 text-[10px] font-medium rounded border border-blue-300 text-blue-700 bg-blue-50 hover:bg-blue-100 disabled:opacity-40";
const SLOT_BTN: &str = "w-full text-left text-[10px] text-gray-400 hover:text-blue-700 hover:bg-blue-50 rounded px-1.5 py-0.5";

fn kind_chip(name: &str, kind: &NodeKind) -> (&'static str, String) {
    let label = match kind {
        NodeKind::MatchDef => "Match".to_string(),
        NodeKind::TypeDef => "TYPE".to_string(),
        NodeKind::Element => name.to_string(),
    };
    let class = match (kind, name) {
        (NodeKind::MatchDef, _) => "bg-violet-100 text-violet-800",
        (NodeKind::TypeDef, _) => "bg-slate-200 text-slate-800",
        (_, "Section") => "bg-blue-100 text-blue-800",
        (_, "TextChunk") => "bg-green-100 text-green-800",
        (_, "Table") => "bg-red-100 text-red-800",
        (_, "Paragraph") => "bg-teal-100 text-teal-800",
        (_, "Annotation") => "bg-amber-100 text-amber-800",
        (_, "Figure") => "bg-purple-100 text-purple-800",
        (_, "Image") => "bg-pink-100 text-pink-800",
        (_, "SubCorpus") => "bg-cyan-100 text-cyan-800",
        _ => "bg-gray-200 text-gray-700",
    };
    (class, label)
}

// ───────────────────────── form input helpers ─────────────────────────

/// Text input committing on `change` (blur/Enter) — the tree re-renders on
/// every buffer change, so per-keystroke commits would drop focus.
fn text_field(
    label: &'static str,
    value: String,
    placeholder: &'static str,
    on_commit: impl Fn(String) + 'static,
) -> AnyView {
    view! {
        <div>
            <span class=LABEL>{label}</span>
            <input
                type="text"
                class=INPUT
                placeholder=placeholder
                prop:value=value
                on:change=move |ev| on_commit(event_target_value(&ev))
            />
        </div>
    }
    .into_any()
}

fn num_field(
    label: &'static str,
    value: f64,
    step: f64,
    min: f64,
    max: f64,
    on_commit: impl Fn(f64) + 'static,
) -> AnyView {
    view! {
        <div>
            <span class=LABEL>{label}</span>
            <input
                type="number"
                class=INPUT
                step=fmt_num(step)
                min=fmt_num(min)
                max=fmt_num(max)
                prop:value=fmt_num(value)
                on:change=move |ev| {
                    if let Ok(v) = event_target_value(&ev).parse::<f64>() {
                        on_commit(v.clamp(min, max));
                    }
                }
            />
        </div>
    }
    .into_any()
}

// ───────────────────────── component ─────────────────────────

#[component]
pub fn QueryPalette() -> impl IntoView {
    let location = use_location();
    let current_doc = Memo::new(move |_| doc_id_from_path(&location.pathname.get()));
    let open = RwSignal::new(true);
    let builder = expect_context::<QueryBuilder>();

    // Transient builder-UI state, OUTSIDE the rebuilt tree closure so a
    // reparse never resets an open menu or a half-edited TYPE draft.
    let open_slot: RwSignal<Option<SlotKey>> = RwSignal::new(None);
    let end_adding: RwSignal<Option<NodePath>> = RwSignal::new(None);
    let type_draft: RwSignal<Option<TypeDraft>> = RwSignal::new(None);

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
        <Show when=move || current_doc.get().is_some()>
        <div class="mb-6 border-b border-gray-200 pb-4">
            <button
                class="w-full flex items-center justify-between text-sm font-semibold text-gray-900"
                on:click=move |_| open.update(|v| *v = !*v)
            >
                <span>"Query builder"</span>
                <span class="text-gray-400 text-xs">{move || if open.get() { "▾" } else { "▸" }}</span>
            </button>
            <Show when=move || open.get()>
                <Suspense fallback=move || view! {
                    <p class="text-xs text-gray-500 mt-2">"Loading document data..."</p>
                }>
                    {move || {
                        // Resource read directly in the Suspense-tracked
                        // closure (DV-009); the builder signals are plain
                        // signals and re-render this subtree on change.
                        let data = palette.get().flatten().map(Arc::new);
                        let snap = builder.snapshot.get();
                        let syntax = builder.syntax_error.get();
                        let selected = builder.selected.get();
                        let _ = (open_slot.get(), end_adding.get(), type_draft.get());
                        builder_body(
                            builder, data, snap, syntax, selected,
                            open_slot, end_adding, type_draft,
                        )
                    }}
                </Suspense>
            </Show>
        </div>
        </Show>
    }
}

#[allow(clippy::too_many_arguments)]
fn builder_body(
    builder: QueryBuilder,
    palette: Option<Arc<PaletteData>>,
    snap: Option<Snapshot>,
    syntax: Option<String>,
    selected: Option<NodePath>,
    open_slot: RwSignal<Option<SlotKey>>,
    end_adding: RwSignal<Option<NodePath>>,
    type_draft: RwSignal<Option<TypeDraft>>,
) -> AnyView {
    let Some(snap) = snap else {
        // SSR + the instant before the first client-side parse (DV-009: the
        // hydrated initial render matches the server's).
        return view! {
            <p class="text-xs text-gray-400 mt-2">"Reading query structure..."</p>
        }
        .into_any();
    };
    let stale = syntax.is_some();
    let (diag_path, diag_msg) = match &snap.tree.compile {
        Some(d) => (d.node_path.clone(), Some(d.message.clone())),
        None => (None, None),
    };
    let ctx = Ctx {
        builder,
        palette,
        text: Arc::new(snap.text.clone()),
        open_slot,
        end_adding,
        type_draft,
        selected,
        diag_path,
        diag_msg: diag_msg.clone(),
    };

    let banner = syntax.map(|msg| view! {
        <div class="mt-2 border border-amber-300 bg-amber-50 rounded p-2 text-[11px] text-amber-800">
            <span class="font-semibold">"Showing last valid structure. "</span>
            "Fix the syntax in the editor to resume building ("{msg}")."
        </div>
    });
    let global_diag = (ctx.diag_path.is_none() && ctx.diag_msg.is_some()).then(|| {
        view! {
            <div class="mt-2 border border-red-300 bg-red-50 rounded p-2 text-[11px] text-red-700 break-words">
                {diag_msg.unwrap_or_default()}
            </div>
        }
    });

    let tree = if snap.tree.nodes.is_empty() {
        root_empty_slot(&ctx)
    } else {
        let mut rows: Vec<AnyView> = Vec::new();
        rows.push(slot_view(&ctx, SlotKey { parent: None, index: 0 }, None));
        for (i, node) in snap.tree.nodes.iter().enumerate() {
            rows.push(node_view(&ctx, node, &snap.tree, vec![i]));
            rows.push(slot_view(&ctx, SlotKey { parent: None, index: i + 1 }, None));
        }
        view! { <div class="mt-2">{rows}</div> }.into_any()
    };

    view! {
        <div class=move || if stale { "opacity-60 pointer-events-none" } else { "" }>
            {tree}
        </div>
        {banner}
        {global_diag}
    }
    .into_any()
}

/// Empty buffer → one big root slot ("a spot to insert a node").
fn root_empty_slot(ctx: &Ctx) -> AnyView {
    let key = SlotKey { parent: None, index: 0 };
    let is_open = ctx.open_slot.get_untracked().as_ref() == Some(&key);
    let open_slot = ctx.open_slot;
    let key_for_click = key.clone();
    let menu = is_open.then(|| slot_menu_view(ctx, key.clone(), None));
    view! {
        <div class="mt-2">
            <button
                class="w-full border-2 border-dashed border-gray-300 rounded-md py-3 text-xs text-gray-500 hover:border-blue-400 hover:text-blue-700"
                on:click=move |_| {
                    let key = key_for_click.clone();
                    open_slot.update(|s| {
                        *s = if s.as_ref() == Some(&key) { None } else { Some(key.clone()) };
                    });
                }
            >
                "+ Add the first node of your query"
            </button>
            {menu}
        </div>
    }
    .into_any()
}

/// A thin add-slot between/around nodes; click opens the context-legal menu.
fn slot_view(ctx: &Ctx, key: SlotKey, parent_element: Option<&str>) -> AnyView {
    let legal = slot_menu(parent_element);
    if legal.is_empty() {
        return ().into_any();
    }
    let is_open = ctx.open_slot.get_untracked().as_ref() == Some(&key);
    let open_slot = ctx.open_slot;
    let key_for_click = key.clone();
    let menu = is_open.then(|| slot_menu_view(ctx, key.clone(), parent_element));
    view! {
        <div>
            <button
                class=SLOT_BTN
                title="Insert a node here"
                on:click=move |_| {
                    let key = key_for_click.clone();
                    open_slot.update(|s| {
                        *s = if s.as_ref() == Some(&key) { None } else { Some(key.clone()) };
                    });
                }
            >
                "+"
            </button>
            {menu}
        </div>
    }
    .into_any()
}

/// The context-legal node menu for an open slot.
fn slot_menu_view(ctx: &Ctx, key: SlotKey, parent_element: Option<&str>) -> AnyView {
    let builder = ctx.builder;
    let open_slot = ctx.open_slot;
    let items: Vec<AnyView> = slot_menu(parent_element)
        .iter()
        .map(|kind| {
            let kind = *kind;
            let key = key.clone();
            view! {
                <button
                    class="w-full text-left px-2 py-1 rounded hover:bg-blue-50"
                    on:click=move |_| {
                        builder.insert_at_slot(&key, kind);
                        open_slot.set(None);
                    }
                >
                    <span class="text-xs font-medium text-gray-800">{kind.label()}</span>
                    <span class="text-[10px] text-gray-500 block">{kind.hint()}</span>
                </button>
            }
            .into_any()
        })
        .collect();
    view! {
        <div class="border border-gray-200 rounded-md bg-white shadow-sm p-1 my-1">
            {items}
        </div>
    }
    .into_any()
}

// ───────────────────────── node rendering ─────────────────────────

fn node_view(ctx: &Ctx, node: &QueryNode, tree: &QueryTree, path: NodePath) -> AnyView {
    let is_selected = ctx.selected.as_ref() == Some(&path);
    let has_diag = ctx.diag_path.as_ref() == Some(&path);
    let (chip_class, chip_label) = kind_chip(&node.name, &node.kind);
    let summary = node_summary(node, tree);

    let builder = ctx.builder;
    let click_path = path.clone();
    let span = node.span;
    let text = Arc::clone(&ctx.text);
    let on_select = move |_| {
        let now_selected = builder.selected.get_untracked().as_ref() != Some(&click_path);
        builder
            .selected
            .set(now_selected.then(|| click_path.clone()));
        if now_selected {
            // Keep "view source" in sync (no-op while the panel is closed).
            #[cfg(feature = "hydrate")]
            crate::components::query_panel::cm::select_span(
                query_tree::byte_to_utf16(&text, span.0),
                query_tree::byte_to_utf16(&text, span.1),
            );
            #[cfg(not(feature = "hydrate"))]
            let _ = (&text, span);
        }
    };

    let delete_path = path.clone();
    let delete_btn = view! {
        <button
            class="text-gray-300 hover:text-red-600 text-xs px-1"
            title="Delete this node"
            on:click=move |ev| {
                ev.stop_propagation();
                builder.delete_node(&delete_path);
            }
        >"✕"</button>
    };

    let diag_dot = has_diag.then(|| view! {
        <span class="w-2 h-2 rounded-full bg-red-500 inline-block shrink-0" title="compile error"></span>
    });
    let diag_box = (has_diag && is_selected)
        .then(|| ctx.diag_msg.clone())
        .flatten()
        .map(|msg| view! {
            <div class="mt-1 border border-red-300 bg-red-50 rounded p-1.5 text-[10px] text-red-700 break-words">{msg}</div>
        });

    let form = is_selected.then(|| node_form(ctx, node, tree, &path));

    // Children + interleaved child slots for structural nodes.
    let children_view = if query_tree::has_child_slots(node) {
        let mut rows: Vec<AnyView> = Vec::new();
        rows.push(slot_view(
            ctx,
            SlotKey { parent: Some(path.clone()), index: 0 },
            Some(node.name.as_str()),
        ));
        for (i, child) in node.children.iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(i);
            rows.push(node_view(ctx, child, tree, child_path));
            rows.push(slot_view(
                ctx,
                SlotKey { parent: Some(path.clone()), index: i + 1 },
                Some(node.name.as_str()),
            ));
        }
        Some(view! {
            <div class="ml-3 border-l border-gray-200 pl-2 mt-1">{rows}</div>
        })
    } else {
        None
    };

    let row_class = if is_selected {
        "w-full flex items-center gap-1.5 rounded px-1 py-0.5 bg-blue-50 cursor-pointer"
    } else {
        "w-full flex items-center gap-1.5 rounded px-1 py-0.5 hover:bg-gray-50 cursor-pointer"
    };

    view! {
        <div class="my-0.5">
            <div class=row_class on:click=on_select>
                <span class=format!("text-[10px] font-semibold rounded px-1 py-px shrink-0 {chip_class}")>{chip_label}</span>
                <span class="flex-1 text-[11px] text-gray-700 truncate">{summary}</span>
                {diag_dot}
                {delete_btn}
            </div>
            {diag_box}
            {form}
            {children_view}
        </div>
    }
    .into_any()
}

/// One-line node summary for the tree row.
fn node_summary(node: &QueryNode, tree: &QueryTree) -> String {
    match node.kind {
        NodeKind::MatchDef => {
            let rule = node
                .rules
                .first()
                .map(rule_summary)
                .unwrap_or_else(|| "(empty)".to_string());
            format!("{} · {}", node.name, rule)
        }
        NodeKind::TypeDef => format!("{} · {} fields", node.name, node.fields.len()),
        NodeKind::Element => {
            let as_name = attr(node, "as")
                .and_then(|a| a.value.display())
                .map(|v| format!("as {v}"));
            let extra = match node.name.as_str() {
                "Section" => section_rule(node, tree, "match")
                    .map(|r| rule_summary(&r))
                    .or_else(|| {
                        attr(node, "match").map(|a| a.value.display().unwrap_or_else(|| "…".into()))
                    }),
                "Table" => attr(node, "type")
                    .and_then(|a| a.value.display())
                    .map(|t| format!("type {t}")),
                "TextChunk" => {
                    let size = attr(node, "chunkSize")
                        .and_then(|a| a.value.display())
                        .unwrap_or_else(|| "500".into());
                    Some(format!("size {size}"))
                }
                _ => None,
            };
            match (extra, as_name) {
                (Some(e), Some(a)) => format!("{e} · {a}"),
                (Some(e), None) => e,
                (None, Some(a)) => a,
                (None, None) => String::new(),
            }
        }
    }
}

fn rule_summary(rule: &MatchRuleNode) -> String {
    match rule.function.as_str() {
        "Text" | "Regex" | "EmbeddingSim" => {
            let mut p = rule.pattern.clone().unwrap_or_default();
            if p.chars().count() > 28 {
                p = p.chars().take(28).collect();
                p.push('…');
            }
            format!("{} \u{201c}{}\u{201d}", rule.function, p)
        }
        _ => rule.function.clone(),
    }
}

// ───────────────────────── per-kind forms ─────────────────────────

fn node_form(ctx: &Ctx, node: &QueryNode, tree: &QueryTree, path: &NodePath) -> AnyView {
    let inner = match (&node.kind, node.name.as_str()) {
        (NodeKind::MatchDef, _) => match_def_form(ctx, node, path),
        (NodeKind::TypeDef, _) => type_def_form(ctx, node, path),
        (NodeKind::Element, "Section") => section_form(ctx, node, tree, path),
        (NodeKind::Element, "TextChunk") => textchunk_form(ctx, node, path),
        (NodeKind::Element, "Table") => table_form(ctx, node, tree, path),
        (NodeKind::Element, "SubCorpus") => subcorpus_form(ctx, node, path),
        (NodeKind::Element, "Annotation" | "Figure" | "Image" | "Paragraph") => {
            as_only_form(ctx, node, path)
        }
        (NodeKind::Element, _) => view! {
            <p class="text-[10px] text-gray-500">
                "No form for this element — edit it in the query editor."
            </p>
        }
        .into_any(),
    };
    view! {
        <div class="mt-1 mb-2 border border-gray-200 rounded-md bg-gray-50 p-2">
            {inner}
        </div>
    }
    .into_any()
}

/// `as=` editor shared by every element form.
fn as_field(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let path = path.clone();
    let current = attr(node, "as")
        .and_then(|a| a.value.display())
        .unwrap_or_default();
    text_field("output name (as)", current, "e.g. my_section", move |v| {
        let v = v.trim().to_string();
        let write = if v.is_empty() {
            AttrWrite::Remove
        } else {
            AttrWrite::Set(quote_str(&v))
        };
        builder.set_attrs(&path, vec![("as".to_string(), write)]);
    })
}

fn as_only_form(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    as_field(ctx, node, path)
}

fn subcorpus_form(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let desc = attr(node, "description")
        .and_then(|a| a.value.display())
        .unwrap_or_default();
    let desc_path = path.clone();
    view! {
        <div>
            {text_field("description", desc, "what this corpus is about", move |v| {
                builder.set_attrs(
                    &desc_path,
                    vec![("description".to_string(), AttrWrite::Set(quote_str(&v)))],
                );
            })}
            {as_field(ctx, node, path)}
        </div>
    }
    .into_any()
}

fn textchunk_form(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let num_attr = |name: &str, default: f64| {
        attr(node, name)
            .and_then(|a| match a.value {
                AttrValue::Num(n) => Some(n),
                _ => None,
            })
            .unwrap_or(default)
    };
    let chunk_size = num_attr("chunkSize", 500.0);
    let chunk_overlap = num_attr("chunkOverlap", 150.0);
    let breakpoint = num_attr("breakpointPercentile", 25.0);
    let method = attr(node, "method")
        .and_then(|a| a.value.display())
        .unwrap_or_else(|| "tokens".to_string());
    let semantic = method == "semantic";

    let size_path = path.clone();
    let overlap_path = path.clone();
    let method_path = path.clone();
    let bp_path = path.clone();

    let breakpoint_field = semantic.then(|| {
        num_field("breakpoint percentile", breakpoint, 5.0, 0.0, 100.0, move |v| {
            builder.set_attrs(
                &bp_path,
                vec![("breakpointPercentile".to_string(), AttrWrite::Set(fmt_num(v)))],
            );
        })
    });

    view! {
        <div>
            <div class="grid grid-cols-2 gap-2">
                {num_field("chunk size", chunk_size, 50.0, 1.0, 100000.0, move |v| {
                    builder.set_attrs(
                        &size_path,
                        vec![("chunkSize".to_string(), AttrWrite::Set(fmt_num(v)))],
                    );
                })}
                {num_field("chunk overlap", chunk_overlap, 25.0, 0.0, 100000.0, move |v| {
                    builder.set_attrs(
                        &overlap_path,
                        vec![("chunkOverlap".to_string(), AttrWrite::Set(fmt_num(v)))],
                    );
                })}
            </div>
            <span class=LABEL>"method"</span>
            <select
                class=SELECT
                on:change=move |ev| {
                    let v = event_target_value(&ev);
                    let updates = if v == "semantic" {
                        vec![("method".to_string(), AttrWrite::Set("\"semantic\"".to_string()))]
                    } else {
                        // tokens is the engine default: drop the attribute
                        // (and the semantic-only percentile with it).
                        vec![
                            ("method".to_string(), AttrWrite::Remove),
                            ("breakpointPercentile".to_string(), AttrWrite::Remove),
                        ]
                    };
                    builder.set_attrs(&method_path, updates);
                }
            >
                <option value="tokens" selected=!semantic>"tokens (default)"</option>
                <option value="semantic" selected=semantic>"semantic"</option>
            </select>
            {breakpoint_field}
        </div>
    }
    .into_any()
}

// ── Section: the heading-picker + rule-type match editor ──

/// What the Section form needs to know about its current `match`/`end_match`.
struct CurrentRule {
    kind: &'static str,
    pattern: String,
    threshold: f64,
    endpoint: String,
    rows: Vec<HeuristicRow>,
    /// false → unsupported shape (FirstMatch, bare value): render read-only.
    editable: bool,
    source: String,
    /// Clauses beyond the first in the referenced definition.
    extra_clauses: usize,
}

/// Resolve a Section's match attribute to its first rule (through the named
/// definition when the value is an identifier).
fn section_rule(node: &QueryNode, tree: &QueryTree, attr_key: &str) -> Option<MatchRuleNode> {
    let a = attr(node, attr_key)?;
    match &a.value {
        AttrValue::Ident(name) => match_def_by_name(tree, name)?.1.rules.first().cloned(),
        AttrValue::Str(s) => Some(MatchRuleNode {
            function: "Text".to_string(),
            span: a.value_span,
            source: a.value_raw.clone(),
            pattern: Some(s.clone()),
            threshold: Some(0.6),
            endpoint: None,
            comparisons: Vec::new(),
            nested: Vec::new(),
        }),
        _ => None,
    }
}

fn current_rule(node: &QueryNode, tree: &QueryTree, attr_key: &str) -> Option<CurrentRule> {
    let extra = attr(node, attr_key)
        .and_then(|a| match &a.value {
            AttrValue::Ident(name) => match_def_by_name(tree, name).map(|(_, d)| d.rules.len()),
            _ => None,
        })
        .map(|n| n.saturating_sub(1))
        .unwrap_or(0);
    let rule = section_rule(node, tree, attr_key)?;
    Some(rule_to_current(&rule, extra))
}

fn rule_to_current(rule: &MatchRuleNode, extra_clauses: usize) -> CurrentRule {
    let kind = match rule.function.as_str() {
        "Text" => "Text",
        "Regex" => "Regex",
        "Heuristic" => "Heuristic",
        "EmbeddingSim" | "Cosine" | "Semantic" => "EmbeddingSim",
        _ => "",
    };
    CurrentRule {
        kind: if kind.is_empty() { "Text" } else { kind },
        pattern: rule.pattern.clone().unwrap_or_default(),
        threshold: rule.threshold.unwrap_or(0.6),
        endpoint: rule.endpoint.clone().unwrap_or_default(),
        rows: rule.comparisons.clone(),
        editable: !kind.is_empty(),
        source: rule.source.clone(),
        extra_clauses,
    }
}

fn default_heuristic_rows() -> Vec<HeuristicRow> {
    vec![HeuristicRow {
        property: "font_size".to_string(),
        op: ">".to_string(),
        value_raw: "14".to_string(),
    }]
}

fn section_form(ctx: &Ctx, node: &QueryNode, tree: &QueryTree, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let match_editor = rule_editor(ctx, node, tree, path, "match");

    // end_match: present → editor + remove; absent → add-disclosure.
    let end_present = attr(node, "end_match").is_some();
    let end_block = if end_present {
        let remove_path = path.clone();
        view! {
            <div class="mt-2 pt-2 border-t border-gray-200">
                <div class="flex items-center justify-between">
                    <span class="text-[10px] font-semibold text-gray-500 uppercase">"end boundary"</span>
                    <button
                        class="text-[10px] text-red-500 hover:underline"
                        on:click=move |_| builder.remove_attr(&remove_path, "end_match")
                    >"remove"</button>
                </div>
                {rule_editor(ctx, node, tree, path, "end_match")}
            </div>
        }
        .into_any()
    } else {
        let adding = ctx.end_adding.get_untracked().as_ref() == Some(path);
        if adding {
            view! {
                <div class="mt-2 pt-2 border-t border-gray-200">
                    <span class="text-[10px] font-semibold text-gray-500 uppercase">"end boundary"</span>
                    {rule_editor(ctx, node, tree, path, "end_match")}
                </div>
            }
            .into_any()
        } else {
            let end_adding = ctx.end_adding;
            let add_path = path.clone();
            view! {
                <button
                    class="mt-2 text-[10px] text-blue-600 hover:underline"
                    on:click=move |_| end_adding.set(Some(add_path.clone()))
                >"+ end boundary (where the section stops)"</button>
            }
            .into_any()
        }
    };

    view! {
        <div>
            <span class=LABEL>"section start (match)"</span>
            {match_editor}
            {as_field(ctx, node, path)}
            {end_block}
        </div>
    }
    .into_any()
}

/// Shared commit sink for the rule editor: `(rule, set_as)` — `set_as` is
/// only produced by heading picks routed at a Section's `match=`.
type CommitRule = Arc<dyn Fn(RuleSpec, Option<String>) + Send + Sync>;

/// The rule editor for a Section's `match`/`end_match`: heading picker +
/// rule-type selector + per-type fields, committing through
/// `set_section_match` (heading picks auto-slug `as=`).
fn rule_editor(
    ctx: &Ctx,
    node: &QueryNode,
    tree: &QueryTree,
    path: &NodePath,
    attr_key: &'static str,
) -> AnyView {
    let builder = ctx.builder;
    let current = current_rule(node, tree, attr_key);
    let commit: CommitRule = {
        let path = path.clone();
        Arc::new(move |rule: RuleSpec, set_as: Option<String>| {
            builder.set_section_match(&path, attr_key, rule, set_as);
        })
    };
    // Heading picks slug `as=` only for the start boundary.
    let as_ctx = (attr_key == "match").then(|| {
        (
            Arc::clone(&ctx.text),
            attr(node, "as").and_then(|a| a.value.display()),
        )
    });
    rule_editor_ui(ctx, current, commit, as_ctx)
}

/// The same editor for a Match definition node (commits via `set_def_rule`;
/// no `as=` slugging).
fn def_rule_editor(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let current = node
        .rules
        .first()
        .map(|r| rule_to_current(r, node.rules.len().saturating_sub(1)));
    let commit: CommitRule = {
        let path = path.clone();
        Arc::new(move |rule: RuleSpec, _set_as: Option<String>| {
            builder.set_def_rule(&path, rule);
        })
    };
    rule_editor_ui(ctx, current, commit, None)
}

/// One rule editor implementation for both sinks.
fn rule_editor_ui(
    ctx: &Ctx,
    current: Option<CurrentRule>,
    commit: CommitRule,
    as_ctx: Option<(Arc<String>, Option<String>)>,
) -> AnyView {
    let (kind, pattern, threshold, endpoint, rows, editable, source, extra) = match &current {
        Some(c) => (
            c.kind,
            c.pattern.clone(),
            c.threshold,
            c.endpoint.clone(),
            c.rows.clone(),
            c.editable,
            c.source.clone(),
            c.extra_clauses,
        ),
        None => ("Text", String::new(), 0.6, String::new(), Vec::new(), true, String::new(), 0),
    };

    if !editable {
        return view! {
            <div class="text-[10px] text-gray-600">
                <span class="font-semibold">"Custom rule"</span>
                " (edit in the query editor):"
                <code class="block mt-0.5 bg-white border border-gray-200 rounded p-1 font-mono break-all">{source}</code>
            </div>
        }
        .into_any();
    }

    // Heading picker (palette data; picking switches the rule to Text with
    // that pattern — the primary no-code flow).
    let headings = ctx
        .palette
        .as_ref()
        .map(|p| p.headings.clone())
        .unwrap_or_default();
    let picker = {
        let commit = Arc::clone(&commit);
        let threshold_now = threshold;
        let options: Vec<AnyView> = headings
            .iter()
            .map(|h| {
                let mut label = format!("p{} · {}", h.page, h.text);
                if label.chars().count() > 46 {
                    label = label.chars().take(46).collect::<String>() + "…";
                }
                view! { <option value=h.text.clone()>{label}</option> }.into_any()
            })
            .collect();
        let empty = headings.is_empty();
        view! {
            <select
                class=SELECT
                disabled=empty
                on:change=move |ev| {
                    let text = event_target_value(&ev);
                    if text.is_empty() {
                        return;
                    }
                    let set_as = as_ctx.as_ref().map(|(buffer_text, current_as)| {
                        query_tree::section_names_for(
                            &text,
                            buffer_text,
                            current_as.as_deref(),
                        ).1
                    });
                    commit(
                        RuleSpec::Text { pattern: text, threshold: threshold_now },
                        set_as,
                    );
                }
            >
                <option value="" selected=true>
                    {if empty { "(no headings detected)" } else { "Pick a detected heading…" }}
                </option>
                {options}
            </select>
        }
    };

    // Rule-type selector: switching carries the pattern over.
    let type_selector = {
        let commit = Arc::clone(&commit);
        let pattern_now = pattern.clone();
        let rows_now = rows.clone();
        view! {
            <select
                class=SELECT
                on:change=move |ev| {
                    let rule = match event_target_value(&ev).as_str() {
                        "Regex" => RuleSpec::Regex { pattern: pattern_now.clone() },
                        "Heuristic" => RuleSpec::Heuristic {
                            rows: if rows_now.is_empty() { default_heuristic_rows() } else { rows_now.clone() },
                        },
                        "EmbeddingSim" => RuleSpec::EmbeddingSim {
                            pattern: pattern_now.clone(),
                            threshold: 0.7,
                            endpoint: None,
                        },
                        _ => RuleSpec::Text { pattern: pattern_now.clone(), threshold: 0.6 },
                    };
                    commit(rule, None);
                }
            >
                <option value="Text" selected=kind == "Text">"Text (fuzzy)"</option>
                <option value="Regex" selected=kind == "Regex">"Regex"</option>
                <option value="Heuristic" selected=kind == "Heuristic">"Heuristic (font/position)"</option>
                <option value="EmbeddingSim" selected=kind == "EmbeddingSim">"Embedding similarity"</option>
            </select>
        }
    };

    // Per-kind fields.
    let fields: AnyView = match kind {
        "Regex" => {
            let commit = Arc::clone(&commit);
            text_field("pattern (regex)", pattern.clone(), "e.g. ^Item\\s+7", move |v| {
                commit(RuleSpec::Regex { pattern: v }, None);
            })
        }
        "Heuristic" => {
            let commit = Arc::clone(&commit);
            heuristic_editor(rows.clone(), move |rule, set_as| commit(rule, set_as))
        }
        "EmbeddingSim" => {
            let c1 = Arc::clone(&commit);
            let c2 = Arc::clone(&commit);
            let c3 = Arc::clone(&commit);
            let (t1, e1) = (threshold, endpoint.clone());
            let (p2, e2) = (pattern.clone(), endpoint.clone());
            let (p3, t3) = (pattern.clone(), threshold);
            let ep = |e: &str| (!e.trim().is_empty()).then(|| e.trim().to_string());
            view! {
                <div>
                    {text_field("query text", pattern.clone(), "what the content is about", move |v| {
                        c1(RuleSpec::EmbeddingSim { pattern: v, threshold: t1, endpoint: ep(&e1) }, None);
                    })}
                    {num_field("threshold", threshold, 0.05, 0.0, 1.0, move |v| {
                        c2(RuleSpec::EmbeddingSim { pattern: p2.clone(), threshold: v, endpoint: ep(&e2) }, None);
                    })}
                    {text_field("endpoint", endpoint.clone(), "serving endpoint name or URL", move |v| {
                        c3(RuleSpec::EmbeddingSim { pattern: p3.clone(), threshold: t3, endpoint: ep(&v) }, None);
                    })}
                </div>
            }
            .into_any()
        }
        _ => {
            // Text
            let c1 = Arc::clone(&commit);
            let c2 = Arc::clone(&commit);
            let t1 = threshold;
            let p2 = pattern.clone();
            view! {
                <div>
                    {text_field("heading text", pattern.clone(), "or type any heading text", move |v| {
                        c1(RuleSpec::Text { pattern: v, threshold: t1 }, None);
                    })}
                    {num_field("threshold (fuzzy 0–1)", threshold, 0.05, 0.0, 1.0, move |v| {
                        c2(RuleSpec::Text { pattern: p2.clone(), threshold: v }, None);
                    })}
                </div>
            }
            .into_any()
        }
    };

    let no_rule_hint = current.is_none().then(|| view! {
        <p class="text-[10px] text-amber-700 mt-1">
            "No match yet — pick a heading (or fill the fields) to define where this starts."
        </p>
    });
    let extra_note = (extra > 0).then(|| view! {
        <p class="text-[10px] text-gray-400 mt-1">
            {format!("+{extra} more clause(s) in the match block — preserved; edit them in the editor.")}
        </p>
    });

    view! {
        <div class="mt-1">
            {picker}
            <div class="mt-1">{type_selector}</div>
            {no_rule_hint}
            {fields}
            {extra_note}
        </div>
    }
    .into_any()
}

const HEUR_PROPS: &[(&str, bool)] = &[
    ("font_size", false),
    ("font_name", true),
    ("page", false),
    ("x0", false),
    ("y0", false),
    ("x1", false),
    ("y1", false),
    ("text_length", false),
    ("text", true),
];
const HEUR_OPS: &[&str] = &[">", ">=", "<", "<=", "==", "!="];

fn heuristic_prop_is_string(prop: &str) -> bool {
    HEUR_PROPS.iter().any(|(p, s)| *p == prop && *s)
}

/// Property/comparator/value rows for `Heuristic(...)` — every part picked
/// from the supported set, never typed from memory.
fn heuristic_editor(
    rows: Vec<HeuristicRow>,
    commit: impl Fn(RuleSpec, Option<String>) + Clone + 'static,
) -> AnyView {
    let rows = if rows.is_empty() { default_heuristic_rows() } else { rows };
    let n_rows = rows.len();

    let commit_rows = {
        let commit = commit.clone();
        move |rows: Vec<HeuristicRow>| commit(RuleSpec::Heuristic { rows }, None)
    };

    let row_views: Vec<AnyView> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let prop_options: Vec<AnyView> = HEUR_PROPS
                .iter()
                .map(|(p, _)| {
                    let p = *p;
                    view! { <option value=p selected=p == row.property>{p}</option> }.into_any()
                })
                .collect();
            let op_options: Vec<AnyView> = HEUR_OPS
                .iter()
                .map(|o| {
                    let o = *o;
                    view! { <option value=o selected=o == row.op>{o}</option> }.into_any()
                })
                .collect();
            let display_value = row
                .value_raw
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(&row.value_raw)
                .to_string();

            let (rows_a, rows_b, rows_c) = (rows.clone(), rows.clone(), rows.clone());
            let (ca, cb, cc, cd) = (
                commit_rows.clone(),
                commit_rows.clone(),
                commit_rows.clone(),
                commit_rows.clone(),
            );
            let rows_d = rows.clone();
            view! {
                <div class="flex items-center gap-1 mt-1">
                    <select class=SELECT style="width:38%" on:change=move |ev| {
                        let mut rows = rows_a.clone();
                        let prop = event_target_value(&ev);
                        // String properties only take == / !=; fix the op up.
                        if heuristic_prop_is_string(&prop)
                            && !matches!(rows[i].op.as_str(), "==" | "!=")
                        {
                            rows[i].op = "==".to_string();
                        }
                        rows[i].property = prop;
                        ca(rows);
                    }>{prop_options}</select>
                    <select class=SELECT style="width:18%" on:change=move |ev| {
                        let mut rows = rows_b.clone();
                        rows[i].op = event_target_value(&ev);
                        cb(rows);
                    }>{op_options}</select>
                    <input type="text" class=INPUT style="width:30%" prop:value=display_value
                        on:change=move |ev| {
                            let mut rows = rows_c.clone();
                            let raw = event_target_value(&ev);
                            rows[i].value_raw = if heuristic_prop_is_string(&rows[i].property) {
                                quote_str(raw.trim())
                            } else {
                                raw.trim()
                                    .parse::<f64>()
                                    .map(fmt_num)
                                    .unwrap_or_else(|_| "0".to_string())
                            };
                            cc(rows);
                        }
                    />
                    <button class="text-gray-300 hover:text-red-600 text-xs" title="remove condition"
                        on:click=move |_| {
                            let mut rows = rows_d.clone();
                            if rows.len() > 1 {
                                rows.remove(i);
                                cd(rows);
                            }
                        }
                    >"✕"</button>
                </div>
            }
            .into_any()
        })
        .collect();

    let add_rows = rows.clone();
    let add_commit = commit_rows.clone();
    view! {
        <div>
            <span class=LABEL>{format!("conditions (all must hold, {n_rows})")}</span>
            {row_views}
            <button class="mt-1 text-[10px] text-blue-600 hover:underline" on:click=move |_| {
                let mut rows = add_rows.clone();
                rows.extend(default_heuristic_rows());
                add_commit(rows);
            }>"+ condition"</button>
        </div>
    }
    .into_any()
}

// ── Match definition node ──

fn match_def_form(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let rename_path = path.clone();
    let name = node.name.clone();
    let target = node.match_target.clone().unwrap_or_else(|| "Section".into());
    let editor = def_rule_editor(ctx, node, path);

    view! {
        <div>
            {text_field("name (referenced as match=…)", name, "MatchName", move |v| {
                builder.rename_declaration(&rename_path, v.trim());
            })}
            <p class="text-[10px] text-gray-400 mt-0.5">{format!("matches against: {target}")}</p>
            {editor}
        </div>
    }
    .into_any()
}

// ── TYPE definition node ──

fn type_def_form(ctx: &Ctx, node: &QueryNode, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let rename_path = path.clone();
    let fields: Vec<(String, String)> = node
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.field_type.clone()))
        .collect();

    let commit_fields = {
        let path = path.clone();
        move |fields: Vec<(String, String)>| builder.set_type_fields(&path, fields)
    };
    let rows = type_field_rows(fields, commit_fields);

    view! {
        <div>
            {text_field("type name (used by table type=…)", node.name.clone(), "TypeName", move |v| {
                builder.rename_declaration(&rename_path, v.trim());
            })}
            {rows}
        </div>
    }
    .into_any()
}

const FIELD_TYPES: &[&str] = &["TEXT", "INT", "DECIMAL"];

/// Editable field rows (name input + type dropdown + remove, plus add) used
/// by both the TYPE node form and the "New TYPE…" draft.
fn type_field_rows(
    fields: Vec<(String, String)>,
    commit: impl Fn(Vec<(String, String)>) + Clone + 'static,
) -> AnyView {
    let n = fields.len();
    let row_views: Vec<AnyView> = fields
        .iter()
        .enumerate()
        .map(|(i, (fname, ftype))| {
            let type_options: Vec<AnyView> = FIELD_TYPES
                .iter()
                .map(|t| {
                    let t = *t;
                    view! { <option value=t selected=t == ftype>{t}</option> }.into_any()
                })
                .collect();
            let (fa, fb, fc) = (fields.clone(), fields.clone(), fields.clone());
            let (ca, cb, cc) = (commit.clone(), commit.clone(), commit.clone());
            view! {
                <div class="flex items-center gap-1 mt-1">
                    <input type="text" class=INPUT style="width:55%" prop:value=fname.clone()
                        on:change=move |ev| {
                            let mut fields = fa.clone();
                            let slug = crate::snippets::slug_identifier(&event_target_value(&ev));
                            if !slug.is_empty() {
                                fields[i].0 = slug;
                                ca(fields);
                            }
                        }
                    />
                    <select class=SELECT style="width:32%" on:change=move |ev| {
                        let mut fields = fb.clone();
                        fields[i].1 = event_target_value(&ev);
                        cb(fields);
                    }>{type_options}</select>
                    <button class="text-gray-300 hover:text-red-600 text-xs" title="remove field"
                        on:click=move |_| {
                            let mut fields = fc.clone();
                            if fields.len() > 1 {
                                fields.remove(i);
                                cc(fields);
                            }
                        }
                    >"✕"</button>
                </div>
            }
            .into_any()
        })
        .collect();

    let add_fields = fields.clone();
    let add_commit = commit.clone();
    view! {
        <div>
            <span class=LABEL>{format!("fields ({n})")}</span>
            {row_views}
            <button class="mt-1 text-[10px] text-blue-600 hover:underline" on:click=move |_| {
                let mut fields = add_fields.clone();
                let base = format!("field{}", fields.len() + 1);
                fields.push((base, "TEXT".to_string()));
                add_commit(fields);
            }>"+ field"</button>
        </div>
    }
    .into_any()
}

// ── Table: as + type dropdown + the "New TYPE…" flow ──

fn table_form(ctx: &Ctx, node: &QueryNode, tree: &QueryTree, path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let declared = type_names(tree);
    let current_type = attr(node, "type").and_then(|a| a.value.display());
    let type_draft = ctx.type_draft;

    let draft_open = ctx
        .type_draft
        .get_untracked()
        .is_some_and(|d| &d.table_path == path);

    let type_select = {
        let select_path = path.clone();
        let draft_path = path.clone();
        let options: Vec<AnyView> = declared
            .iter()
            .map(|n| {
                let n = n.clone();
                let sel = current_type.as_deref() == Some(n.as_str());
                view! { <option value=n.clone() selected=sel>{n.clone()}</option> }.into_any()
            })
            .collect();
        view! {
            <span class=LABEL>"typed extraction (type)"</span>
            <select class=SELECT on:change=move |ev| {
                match event_target_value(&ev).as_str() {
                    "__none__" => {
                        builder.set_attrs(&select_path, vec![("type".to_string(), AttrWrite::Remove)]);
                    }
                    "__new__" => {
                        type_draft.set(Some(TypeDraft {
                            table_path: draft_path.clone(),
                            picked: false,
                            name: String::new(),
                            fields: Vec::new(),
                        }));
                    }
                    name => {
                        builder.set_attrs(
                            &select_path,
                            vec![("type".to_string(), AttrWrite::Set(quote_str(name)))],
                        );
                    }
                }
            }>
                <option value="__none__" selected=current_type.is_none()>"(raw rows — no type)"</option>
                {options}
                <option value="__new__" selected=draft_open>"New TYPE from a detected table…"</option>
            </select>
        }
    };

    let draft_view = draft_open
        .then(|| new_type_flow(ctx, path))
        .unwrap_or_else(|| ().into_any());

    view! {
        <div>
            {as_field(ctx, node, path)}
            {type_select}
            {draft_view}
        </div>
    }
    .into_any()
}

/// The "New TYPE…" flow: pick one of the doc's detected tables → field rows
/// prefilled from its columns (snippets.rs inference) → editable → emits the
/// TYPE at the top of the buffer and points this Table at it.
fn new_type_flow(ctx: &Ctx, table_path: &NodePath) -> AnyView {
    let builder = ctx.builder;
    let type_draft = ctx.type_draft;
    let draft = ctx.type_draft.get_untracked().unwrap_or(TypeDraft {
        table_path: table_path.clone(),
        picked: false,
        name: String::new(),
        fields: Vec::new(),
    });

    if !draft.picked {
        // Step 1: pick a detected table.
        let tables: Vec<TableEntry> = ctx
            .palette
            .as_ref()
            .map(|p| p.tables.clone())
            .unwrap_or_default();
        let buffer_text = Arc::clone(&ctx.text);
        let rows: Vec<AnyView> = tables
            .iter()
            .filter(|t| !t.columns.is_empty())
            .map(|t| {
                let label = format!(
                    "p{} · {}×{} · {} · {:.2}",
                    t.page, t.n_rows, t.n_cols, t.strategy, t.confidence
                );
                let columns = t.columns.clone();
                let page = t.page;
                let tp = draft.table_path.clone();
                let buffer_text = Arc::clone(&buffer_text);
                view! {
                    <button class="w-full text-left text-[11px] text-gray-700 hover:bg-blue-50 rounded px-1.5 py-0.5"
                        on:click=move |_| {
                            type_draft.set(Some(TypeDraft {
                                table_path: tp.clone(),
                                picked: true,
                                name: uniquify(&buffer_text, &format!("TableP{page}")),
                                fields: type_fields_from_columns(&columns),
                            }));
                        }
                    >{label}</button>
                }
                .into_any()
            })
            .collect();
        let empty = rows.is_empty();
        return view! {
            <div class="mt-1 border border-blue-200 bg-blue-50/50 rounded p-1.5">
                <div class="text-[10px] font-semibold text-gray-600 mb-1">
                    "Pick a detected table to prefill the fields:"
                </div>
                {if empty {
                    view! { <p class="text-[10px] text-gray-400">"(no tables with usable columns)"</p> }.into_any()
                } else {
                    view! { <div style="max-height:10rem;overflow-y:auto">{rows}</div> }.into_any()
                }}
                <button class="mt-1 text-[10px] text-gray-500 hover:underline"
                    on:click=move |_| type_draft.set(None)
                >"cancel"</button>
            </div>
        }
        .into_any();
    }

    // Step 2: editable name + field rows, then create.
    let name_ok = !draft.name.is_empty()
        && draft.name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && draft.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let fields_ok = !draft.fields.is_empty()
        && draft.fields.iter().all(|(n, _)| !n.is_empty())
        && {
            let mut names: Vec<&String> = draft.fields.iter().map(|(n, _)| n).collect();
            names.sort();
            names.windows(2).all(|w| w[0] != w[1])
        };
    let can_create = name_ok && fields_ok;

    let name_draft = draft.clone();
    let name_input = text_field("TYPE name", draft.name.clone(), "TableP26", move |v| {
        let mut d = name_draft.clone();
        d.name = v.trim().to_string();
        type_draft.set(Some(d));
    });

    let rows_draft = draft.clone();
    let rows = type_field_rows(draft.fields.clone(), move |fields| {
        let mut d = rows_draft.clone();
        d.fields = fields;
        type_draft.set(Some(d));
    });

    let create_draft = draft.clone();
    view! {
        <div class="mt-1 border border-blue-200 bg-blue-50/50 rounded p-1.5">
            {name_input}
            {rows}
            <div class="flex gap-2 mt-2">
                <button class=MINI_BTN disabled=!can_create on:click=move |_| {
                    builder.add_type_for_table(
                        &create_draft.table_path,
                        &create_draft.name,
                        create_draft.fields.clone(),
                    );
                    type_draft.set(None);
                }>"Create TYPE & apply"</button>
                <button class="text-[10px] text-gray-500 hover:underline"
                    on:click=move |_| type_draft.set(None)
                >"cancel"</button>
            </div>
        </div>
    }
    .into_any()
}
