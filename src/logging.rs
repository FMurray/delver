use std::fmt::Write;
use std::sync::Once;
use std::sync::{Arc, Mutex};
use tracing::level_filters::LevelFilter;
use tracing::{Subscriber, Value};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
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
// Add new matcher-related targets
pub const MATCHER_OPERATIONS: &str = "matcher_operations";
pub const TEMPLATE_MATCH: &str = "template_match";

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
    MATCHER_OPERATIONS,
    TEMPLATE_MATCH,
];

enum EntityType {
    Element,
    Line,
    Block,
    Template, // Add a new entity type for templates
    Match,    // Add a new entity type for matches
}

#[derive(Clone, Default)]
pub struct DebugDataStore {
    message_arena: Arc<Mutex<Vec<String>>>,
    elements: Arc<Mutex<HashMap<Uuid, usize>>>,
    lines: Arc<Mutex<HashMap<Uuid, (Uuid, Vec<usize>)>>>,
    events: Arc<Mutex<HashMap<Uuid, Vec<usize>>>>,
    lineage: Arc<Mutex<LineageStore>>,
    // Add template-match tracking
    pub template_matches: Arc<Mutex<HashMap<Uuid, (Uuid, f32)>>>,
    // Add template names storage
    pub template_names: Arc<Mutex<HashMap<Uuid, String>>>,
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

    pub fn record_template_match(&self, template_id: Uuid, content_id: Uuid, score: f32) {
        let mut matches = self.template_matches.lock().unwrap();
        matches.insert(content_id, (template_id, score));

        self.record_relationship(Some(template_id), vec![content_id], "template_match");
    }

    // Get matches for a specific template
    pub fn get_template_matches(&self, template_id: Uuid) -> Vec<(Uuid, f32)> {
        let matches = self.template_matches.lock().unwrap();

        let children = self.get_children(template_id);

        children
            .iter()
            .filter_map(|content_id| {
                matches.get(content_id).map(|(t_id, score)| {
                    if *t_id == template_id {
                        (*content_id, *score)
                    } else {
                        (*content_id, *score)
                    }
                })
            })
            .collect()
    }

    // Get the template that matched a content element
    pub fn get_matching_template(&self, content_id: Uuid) -> Option<(Uuid, f32)> {
        let matches = self.template_matches.lock().unwrap();
        matches.get(&content_id).copied()
    }

    // Add this method to store template names
    pub fn set_template_name(&self, template_id: Uuid, name: String) {
        let mut names = self.template_names.lock().unwrap();
        names.insert(template_id, name);
    }

    // Implement get_template_name correctly
    pub fn get_template_name(&self, template_id: Uuid) -> Option<String> {
        let names = self.template_names.lock().unwrap();
        names.get(&template_id).cloned()
    }

    // Get template structure (from events)
    pub fn get_template_structure(&self, template_id: Uuid) -> Option<Vec<String>> {
        // Extract structure from template events
        let events = self.get_entity_events(template_id);
        if events.messages.is_empty() {
            return None;
        }

        // Parse structure from event messages - this is a simplified approach
        // You may need to customize based on your actual event format
        let structure = events
            .messages
            .iter()
            .filter_map(|msg| {
                if let Some(start) = msg.find("element_type = ") {
                    let element_info = &msg[start + "element_type = ".len()..];
                    if let Some(end) = element_info.find(';') {
                        return Some(element_info[..end].trim().to_string());
                    }
                }
                None
            })
            .collect::<Vec<_>>();

        if structure.is_empty() {
            None
        } else {
            Some(structure)
        }
    }

    // Get events by target type
    pub fn get_events_by_target(&self, target: &str) -> Vec<String> {
        let events = self.events.lock().unwrap();
        let messages = self.message_arena.lock().unwrap();

        let mut result = Vec::new();
        for (_, event_indices) in events.iter() {
            for idx in event_indices {
                if let Some(msg) = messages.get(*idx) {
                    if msg.contains(&format!("target={}", target)) {
                        result.push(msg.clone());
                    }
                }
            }
        }

        result
    }

    // Add this method to count all traces
    pub fn count_all_traces(&self) -> usize {
        let events = self.events.lock().unwrap();
        let mut total = 0;
        for (_, indices) in events.iter() {
            total += indices.len();
        }
        total
    }

    // Add a method to get all templates
    pub fn get_templates(&self) -> Vec<(Uuid, String)> {
        // Collect template IDs from template_names
        let templates = self.template_names.lock().unwrap();

        // Return a list of (template_id, name) pairs
        templates
            .iter()
            .map(|(id, name)| (*id, name.clone()))
            .collect()
    }

    // Count matches for a template using existing relationship tracking
    pub fn count_matches_for_template(&self, template_id: &Uuid) -> usize {
        let children = self.get_children(*template_id);
        let matches = self.template_matches.lock().unwrap();

        // Count children that are actually in template_matches
        children
            .iter()
            .filter(|child_id| matches.contains_key(child_id))
            .count()
    }

    // Get content matches for a template
    pub fn get_content_matches_for_template(&self, template_id: &Uuid) -> Vec<Uuid> {
        self.get_children(*template_id)
    }

    // Helper to get entity name
    fn get_entity_name(&self, id: &Uuid) -> Option<String> {
        let events = self.events.lock().unwrap();
        if let Some(indices) = events.get(id) {
            if !indices.is_empty() {
                let messages = self.message_arena.lock().unwrap();
                let idx = indices[0];
                let message = &messages[idx];
                // Extract name from message string
                if let Some(start) = message.find("template_name = ") {
                    let name_part = &message[start + "template_name = ".len()..];
                    if let Some(end) = name_part.find(';') {
                        return Some(name_part[..end].trim().to_string());
                    } else {
                        return Some(name_part.trim().to_string());
                    }
                }
            }
        }
        None
    }

    // Add method to get content text by ID
    pub fn get_content_by_id(&self, content_id: &Uuid) -> Option<String> {
        // Use events collection to find content
        let events = self.events.lock().unwrap();
        if let Some(indices) = events.get(content_id) {
            if !indices.is_empty() {
                let messages = self.message_arena.lock().unwrap();
                let message = &messages[indices[0]];
                return Some(message.clone());
            }
        }

        // If no direct message, return the UUID as string
        Some(content_id.to_string())
    }

    // In the DebugDataStore implementation, add this method to directly inspect the matches
    pub fn debug_dump_all_matches(&self) -> Vec<(Uuid, Uuid, f32)> {
        let matches = self.template_matches.lock().unwrap();
        matches
            .iter()
            .map(|(content_id, (template_id, score))| (*content_id, *template_id, *score))
            .collect()
    }
}

pub struct DebugLayer {
    store: DebugDataStore,
}

#[derive(Default)]
struct SpanData {
    element_id: Option<Uuid>,
    line_id: Option<Uuid>,
    template_id: Option<Uuid>, // Add tracking for template IDs
    match_id: Option<Uuid>,    // Add tracking for match IDs
}

// Add these debug helpers
use std::sync::atomic::{AtomicUsize, Ordering};
static EVENT_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn init_debug_logging(store: DebugDataStore) -> WorkerGuard {
    // Reset counter for this session
    EVENT_COUNTER.store(0, Ordering::SeqCst);

    // Print debug information
    println!("LOGGING: Initializing debug logging system");

    // Create a debug layer with the store
    let debug_layer = DebugLayer::new(store);

    // Create a non-blocking file appender to get the WorkerGuard
    let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());

    // Create a filter that explicitly allows our targets
    let filter = tracing_subscriber::filter::filter_fn(|metadata| {
        let target = metadata.target();

        if target.contains("matcher_operations")
            || target.contains("template_match")
            || target.contains("logging")
        {
            println!("LOGGING: Allowing event with target: {}", target);
            return true;
        }

        // Filter other targets as needed
        target.starts_with("delver_pdf") || target.contains("pdf") || target.contains("template")
    });

    // Install the subscriber
    let subscriber = tracing_subscriber::registry()
        .with(debug_layer)
        .with(filter);

    println!("LOGGING: Setting global default subscriber");

    // Set the global default
    match tracing::subscriber::set_global_default(subscriber) {
        Ok(_) => println!("LOGGING: Global subscriber set successfully"),
        Err(e) => println!("LOGGING: Failed to set global subscriber: {}", e),
    }

    // Return the guard to keep logging active
    guard
}

impl<S: Subscriber + for<'span> LookupSpan<'span>> Layer<S> for DebugLayer {
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        // Extract IDs from event
        let mut id_visitor = IdVisitor {
            element_id: &mut None,
            line_id: &mut None,
            template_id: &mut None,
            match_id: &mut None,
        };
        event.record(&mut id_visitor);

        // Collect IDs from parent spans if available
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(data) = span.extensions().get::<SpanData>() {
                    if let Some(e_id) = data.element_id {
                        *id_visitor.element_id = id_visitor.element_id.or(Some(e_id));
                    }
                    if let Some(l_id) = data.line_id {
                        *id_visitor.line_id = id_visitor.line_id.or(Some(l_id));
                    }
                    if let Some(t_id) = data.template_id {
                        *id_visitor.template_id = id_visitor.template_id.or(Some(t_id));
                    }
                    if let Some(m_id) = data.match_id {
                        *id_visitor.match_id = id_visitor.match_id.or(Some(m_id));
                    }
                }
            }
        }

        // println!(
        //     "EXTRACTED: element_id={:?}, line_id={:?}, template_id={:?}, match_id={:?}",
        //     id_visitor.element_id, id_visitor.line_id, id_visitor.template_id, id_visitor.match_id
        // );

        // Capture message from the event
        let mut message = String::new();
        let mut message_visitor = MessageVisitor(&mut message);
        event.record(&mut message_visitor);

        // Handle entity recording based on ID types
        match (
            *id_visitor.element_id,
            *id_visitor.line_id,
            *id_visitor.template_id,
            *id_visitor.match_id,
        ) {
            // Template registration (template without a match)
            (_, _, Some(template_id), None) => {
                println!(
                    "CAPTURE: Found template_id={} in registration event",
                    template_id
                );
                self.store
                    .record_entity(template_id, EntityType::Template, message.clone());

                // Extract template name if this is a registration
                // Find template name in message
                let mut template_name = None;
                if let Some(name_start) = message.find("template_name = ") {
                    if let Some(name_end) = message[name_start..].find(';') {
                        let raw_name = &message[name_start + 16..name_start + name_end];
                        let name = raw_name.trim_matches('"');
                        template_name = Some(name.to_string());
                    }
                }

                // Store template name if available
                if let Some(name) = template_name {
                    println!(
                        "REGISTRATION: Recording template: {} = {}",
                        template_id, name
                    );
                    let mut templates = self.store.template_names.lock().unwrap();
                    templates.insert(template_id, name);
                }
            }
            (_, Some(line_id), Some(template_id), Some(match_id)) => {
                println!(
                    "CAPTURE: Found template match between template={} and line={}",
                    template_id, line_id
                );

                // Extract score from the event
                let mut score = 0.0;
                if let Some(score_start) = message.find("score = ") {
                    if let Some(score_end) = message[score_start..].find(';') {
                        if let Ok(s) = message[score_start + 8..score_start + score_end]
                            .trim()
                            .parse::<f32>()
                        {
                            score = s;
                        }
                    }
                }

                println!("MATCH: Recording template match with score {}", score);

                // Record template match with score
                self.store
                    .record_template_match(template_id, line_id, score);

                // Record relationship between template and line
                self.store
                    .record_relationship(Some(template_id), vec![line_id], "template_match");
            }
            // Standard element case
            (Some(e_id), _, _, _) => {
                self.store.record_entity(e_id, EntityType::Element, message);
            }
            // Standard line case
            (_, Some(l_id), _, _) => {
                self.store.record_entity(l_id, EntityType::Line, message);
            }
            // Other combinations - record all available entities
            _ => {
                if let Some(e_id) = *id_visitor.element_id {
                    self.store
                        .record_entity(e_id, EntityType::Element, message.clone());
                }
                if let Some(l_id) = *id_visitor.line_id {
                    self.store
                        .record_entity(l_id, EntityType::Line, message.clone());
                }
                // Let in this position unstable?
                // if let Some(t_id) = *id_visitor.template_id
                //     && event.metadata().target() != TEMPLATE_MATCH
                // {
                //     // Only record template here if not already handled in specific patterns
                //     self.store
                //         .record_entity(t_id, EntityType::Template, message.clone());
                // }
            }
        }

        // Handle relationships using RelationshipVisitor
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

impl DebugLayer {
    pub fn new(store: DebugDataStore) -> Self {
        DebugLayer { store }
    }
}

#[derive(Debug)]
struct IdVisitor<'a> {
    element_id: &'a mut Option<Uuid>,
    line_id: &'a mut Option<Uuid>,
    template_id: &'a mut Option<Uuid>, // Add template ID field
    match_id: &'a mut Option<Uuid>,    // Add match ID field
}

impl tracing::field::Visit for IdVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "line_id" => *self.line_id = Uuid::parse_str(value).ok(),
            "element_id" => *self.element_id = Uuid::parse_str(value).ok(),
            "template_id" => *self.template_id = Uuid::parse_str(value).ok(), // Handle template ID
            "match_id" => *self.match_id = Uuid::parse_str(value).ok(),       // Handle match ID
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

struct RecordContentIDVisitor<'a>(&'a mut Option<Uuid>, &'a mut f32, &'a mut Option<String>);

impl<'a> tracing::field::Visit for RecordContentIDVisitor<'a> {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if field.name() == "score" {
            *self.1 = value as f32;
            println!("VISITOR: Found score: {}", value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "template_name" {
            *self.2 = Some(value.to_string());
            println!("VISITOR: Found template_name: {}", value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let value = format!("{:?}", value);
        if field.name() == "content_id" {
            if let Ok(id) = Uuid::parse_str(&value.trim_matches('"')) {
                *self.0 = Some(id);
                println!("VISITOR: Found content_id: {}", id);
            }
        }
    }
}

// pub struct SubscriberConfig {
//     pub subscriber: ,
//     pub _guard: tracing_appender::non_blocking::WorkerGuard,
// }

// pub fn init_logging(
//     debug_ops: bool,
//     store: DebugDataStore,
// ) -> Result<(), Box<dyn std::error::Error>> {
//     init_debug_logging(store)?;
//     Ok(())
// }
