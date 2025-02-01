use std::fmt::Write;
use std::sync::Once;
use std::sync::{Arc, Mutex};
use tracing::level_filters::LevelFilter;
use tracing::Subscriber;
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
            // Update parent -> children mapping
            lineage
                .children
                .entry(parent_id)
                .or_default()
                .extend(children.iter().copied());

            // Update child -> parent mapping
            for child_id in &children {
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

    pub fn get_entity_events(&self, entity_id: Uuid) -> Vec<String> {
        let lineage = self.lineage.lock().unwrap();
        let arena = self.message_arena.lock().unwrap();

        lineage
            .entity_events
            .get(&entity_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| arena.get(idx).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn record_element(&self, element_id: Uuid, line_id: Uuid, message: String) {
        println!(
            "[STORE] Recording element {} (line {}): {}",
            element_id, line_id, message
        );

        let mut arena = self.message_arena.lock().unwrap();
        let idx = arena.len();
        arena.push(message.clone());
        println!("[STORE] Message stored at index {}", idx);

        let mut elements = self.elements.lock().unwrap();
        let mut lines = self.lines.lock().unwrap();
        let mut events = self.events.lock().unwrap();

        elements.insert(element_id, idx);
        lines
            .entry(line_id)
            .or_insert((Uuid::nil(), Vec::new()))
            .1
            .push(idx);
        events.entry(line_id).or_default().push(idx);

        // Track entity-event association
        self.lineage
            .lock()
            .unwrap()
            .entity_events
            .entry(element_id)
            .or_default()
            .push(idx);

        // Also track line-event association
        self.lineage
            .lock()
            .unwrap()
            .entity_events
            .entry(line_id)
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
        println!("[DEBUG] Event received: {:?}", event.metadata().name());

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

        // After message capture
        println!("[DEBUG] Captured message: {}", message);

        if let (Some(e_id), Some(l_id)) = (*id_visitor.element_id, *id_visitor.line_id) {
            println!("[DEBUG] Recording element {} in line {}", e_id, l_id);
            self.store.record_element(e_id, l_id, message);
        } else {
            println!("[DEBUG] No element/line IDs found");
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
}

impl tracing::field::Visit for IdVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "element_id" => *self.element_id = Uuid::parse_str(value).ok(),
            "line_id" => *self.line_id = Uuid::parse_str(value).ok(),
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

pub fn init_debug_logging(
    store: DebugDataStore,
) -> Result<Box<dyn tracing::Subscriber + Send + Sync>, Box<dyn std::error::Error>> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::DEBUG.into())
        .parse(DEBUG_TARGETS.join(",debug,"))?;

    let debug_layer = DebugLayer { store }.with_filter(filter);

    let tree_layer = HierarchicalLayer::default()
        .with_writer(std::io::stdout)
        .with_indent_lines(true)
        .with_indent_amount(2)
        .with_thread_names(true)
        .with_thread_ids(true)
        .with_verbose_exit(false)
        .with_verbose_entry(false)
        .with_targets(true)
        .with_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .parse(DEBUG_TARGETS.join(",debug,"))?,
        );

    let subscriber = Registry::default().with(debug_layer).with(tree_layer);

    Ok(Box::new(subscriber))
}

pub fn init_logging(
    debug_ops: bool,
    store: DebugDataStore,
) -> Result<(), Box<dyn std::error::Error>> {
    init_debug_logging(store)?;
    Ok(())
}
