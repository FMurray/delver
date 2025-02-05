use std::fmt::Write;
use std::sync::Once;
use std::sync::{Arc, Mutex};
use tracing::level_filters::LevelFilter;
use tracing::{Subscriber, Value};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{
    filter::EnvFilter, fmt::FormattedFields, layer::SubscriberExt, util::SubscriberInitExt, Layer,
};
use tracing_subscriber::{registry, Registry};

use serde_json;
use std::collections::HashMap;
use tracing_tree::HierarchicalLayer;
use uuid::Uuid;

use crate::dom::ElementType;

// Define log targets as constants
pub const PDF_OPERATIONS: &str = "pdf_ops";
pub const PDF_PARSING: &str = "pdf_parse";
pub const PDF_FONTS: &str = "pdf_fonts";
pub const PDF_TEXT_OBJECT: &str = "pdf_text_object";
pub const PDF_TEXT_BLOCK: &str = "pdf_text_block";
pub const PDF_BT: &str = "pdf_bt";

pub trait RelatesEntities {
    fn parent_entity(&self) -> Option<Uuid>;
    fn child_entities(&self) -> Vec<Uuid>;
}

pub const REL_PARENT: &str = "parent";
pub const REL_CHILDREN: &str = "children";
pub const REL_TYPE: &str = "rel_type";

// Add these constants at the top
const DEBUG_TARGETS: &[&str] = &[
    PDF_OPERATIONS,
    PDF_TEXT_OBJECT,
    PDF_TEXT_BLOCK,
    PDF_BT,
    "delver_pdf::parse",
];

enum EntityType {
    Element,
    Line,
    Block,
}

#[derive(Clone, Default)]
pub struct DebugDataStore {
    message_arena: Arc<Mutex<Vec<String>>>,
    elements: Arc<Mutex<HashMap<Uuid, usize>>>,
    lines: Arc<Mutex<HashMap<Uuid, (Uuid, Vec<usize>)>>>,
    events: Arc<Mutex<HashMap<Uuid, Vec<usize>>>>,
    lineage: Arc<Mutex<LineageStore>>,
}

#[derive(Default)]
struct LineageStore {
    children: HashMap<Uuid, Vec<Uuid>>,
    parents: HashMap<Uuid, Uuid>,
    entity_events: HashMap<Uuid, Vec<usize>>,
}

#[derive(Debug, Default)]
pub struct EntityEvents {
    pub messages: Vec<String>,
    pub children: Vec<EntityEvents>,
}

impl DebugDataStore {
    pub fn get_entity_lineage(&self, entity_id: Uuid) -> Vec<String> {
        let arena = self.message_arena.lock().unwrap();
        let lineage = self.lineage.lock().unwrap();

        let mut events = Vec::new();

        if let Some(indices) = lineage.entity_events.get(&entity_id) {
            for &idx in indices {
                if let Some(event) = arena.get(idx) {
                    events.push(event.clone());
                }
            }
        }

        if let Some(children) = lineage.children.get(&entity_id) {
            for child in children {
                if let Some(child_indices) = lineage.entity_events.get(child) {
                    for &idx in child_indices {
                        if let Some(msg) = arena.get(idx) {
                            events.push(msg.clone());
                        }
                    }
                }
            }
        }

        events
    }

    pub fn record_relationship(&self, parent: Option<Uuid>, children: Vec<Uuid>, rel_type: &str) {
        let mut lineage = self.lineage.lock().unwrap();

        if let Some(parent_id) = parent {
            // Check for circular reference
            if children.contains(&parent_id) {
                eprintln!(
                    "Circular relationship detected between {} and {:?}",
                    parent_id, children
                );
                return;
            }

            // Update parent -> children mapping
            let existing_children = lineage.children.entry(parent_id).or_default();
            for child_id in &children {
                if !existing_children.contains(child_id) {
                    existing_children.push(*child_id);
                }
            }

            // Update child -> parent mapping
            for child_id in &children {
                if let Some(existing_parent) = lineage.parents.get(child_id) {
                    if *existing_parent != parent_id {
                        eprintln!("Child {} already has parent {}", child_id, existing_parent);
                        continue;
                    }
                }
                lineage.parents.insert(*child_id, parent_id);
            }
        }
    }

    pub fn get_children(&self, entity_id: Uuid) -> Vec<Uuid> {
        self.lineage
            .lock()
            .unwrap()
            .children
            .get(&entity_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_parent(&self, entity_id: Uuid) -> Option<Uuid> {
        self.lineage
            .lock()
            .unwrap()
            .parents
            .get(&entity_id)
            .copied()
    }

    pub fn get_entity_events(&self, entity_id: Uuid) -> EntityEvents {
        // First collect all data we need while holding the lock
        let (messages, children_ids) = {
            let lineage = self.lineage.lock().unwrap();
            let arena = self.message_arena.lock().unwrap();

            let messages = lineage
                .entity_events
                .get(&entity_id)
                .map(|indices| {
                    indices
                        .iter()
                        .filter_map(|&idx| arena.get(idx).cloned())
                        .collect()
                })
                .unwrap_or_default();

            let children_ids = lineage
                .children
                .get(&entity_id)
                .cloned()
                .unwrap_or_default();

            (messages, children_ids)
        }; // Locks released here

        // Now process children without holding the lock
        let mut children = Vec::new();
        for child_id in children_ids {
            children.push(self.get_entity_events(child_id));
        }

        EntityEvents { messages, children }
    }

    fn record_entity(&self, entity_id: Uuid, entity_type: EntityType, message: String) {
        let mut arena = self.message_arena.lock().unwrap();
        let idx = arena.len();
        arena.push(message.clone());

        let mut lineage = self.lineage.lock().unwrap();
        lineage
            .entity_events
            .entry(entity_id)
            .or_default()
            .push(idx);
    }
}

pub struct DebugLayer {
    store: DebugDataStore,
}

#[derive(Default)]
struct SpanData {
    element_id: Option<Uuid>,
    line_id: Option<Uuid>,
}

impl<S> Layer<S> for DebugLayer
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut id_visitor = IdVisitor {
            element_id: &mut None,
            line_id: &mut None,
        };
        event.record(&mut id_visitor);

        // Collect IDs from parent spans
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(data) = span.extensions().get::<SpanData>() {
                    if let Some(e_id) = data.element_id {
                        *id_visitor.element_id = id_visitor.element_id.or(Some(e_id));
                    }
                    if let Some(l_id) = data.line_id {
                        *id_visitor.line_id = id_visitor.line_id.or(Some(l_id));
                    }
                }
            }
        }

        // Capture operator data from the event message
        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));

        match (*id_visitor.element_id, *id_visitor.line_id) {
            (Some(e_id), None) => {
                self.store.record_entity(e_id, EntityType::Element, message);
            }
            (None, Some(l_id)) => {
                self.store.record_entity(l_id, EntityType::Line, message);
            }
            _ => {}
        }

        let mut rel_parent = None;
        let mut rel_children = Vec::new();
        let mut rel_visitor = RelationshipVisitor {
            parent: &mut rel_parent,
            children: &mut rel_children,
        };

        event.record(&mut rel_visitor);

        if !rel_children.is_empty() {
            self.store.record_relationship(rel_parent, rel_children, "");
        }
    }
}

#[derive(Debug)]
struct IdVisitor<'a> {
    element_id: &'a mut Option<Uuid>,
    line_id: &'a mut Option<Uuid>,
    // children: &'a mut Vec<Uuid>,
}

impl tracing::field::Visit for IdVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "line_id" => *self.element_id = Uuid::parse_str(value).ok(),
            "element_id" => *self.line_id = Uuid::parse_str(value).ok(),
            // "children" => {
            //     if let Ok(ids) = serde_json::from_str::<Vec<Uuid>>(value) {
            //         self.children.extend(ids);
            //     }
            // }
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let value = format!("{:?}", value);
        self.record_str(field, &value)
    }
}

struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        write!(self.0, "{} = {:?}; ", field.name(), value).unwrap();
    }
}

struct RelationshipVisitor<'a> {
    parent: &'a mut Option<Uuid>,
    children: &'a mut Vec<Uuid>,
}

impl tracing::field::Visit for RelationshipVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            REL_PARENT => *self.parent = Uuid::parse_str(value).ok(),
            REL_CHILDREN => {
                if let Ok(ids) = serde_json::from_str::<Vec<Uuid>>(value) {
                    self.children.extend(ids);
                } else {
                    let cleaned = value.trim_matches(|c| c == '[' || c == ']');
                    for id_str in cleaned.split(',') {
                        if let Ok(id) = Uuid::parse_str(id_str.trim()) {
                            self.children.push(id);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let value = format!("{:?}", value);
        self.record_str(field, &value)
    }
}

// pub struct SubscriberConfig {
//     pub subscriber: ,
//     pub _guard: tracing_appender::non_blocking::WorkerGuard,
// }

pub fn init_debug_logging(store: DebugDataStore) -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());

    let debug_layer = DebugLayer { store }.with_filter(
        EnvFilter::try_new(
            DEBUG_TARGETS
                .iter()
                .map(|t| format!("{}={}", t, "debug"))
                .collect::<Vec<_>>()
                .join(","),
        )
        .unwrap(),
    );

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::new()
                .with_writer(writer)
                .with_filter(EnvFilter::from_default_env()),
        )
        .with(debug_layer)
        .init();

    guard

    // let tree_layer = HierarchicalLayer::default()
    //     .with_writer(std::io::stdout)
    //     .with_indent_lines(true)
    //     .with_indent_amount(2)
    //     .with_thread_names(true)
    //     .with_thread_ids(true)
    //     .with_verbose_exit(false)
    //     .with_verbose_entry(false)
    //     .with_targets(true)
    //     .with_filter(
    //         EnvFilter::builder()
    //             .with_default_directive(LevelFilter::DEBUG.into())
    //             .parse(DEBUG_TARGETS.join(",debug,"))?,
    //     );

    // let subscriber = Registry::default().with(debug_layer).with(tree_layer);

    // Ok(Box::new(subscriber))
}

// pub fn init_logging(
//     debug_ops: bool,
//     store: DebugDataStore,
// ) -> Result<(), Box<dyn std::error::Error>> {
//     init_debug_logging(store)?;
//     Ok(())
// }
