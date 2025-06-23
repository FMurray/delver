use opentelemetry::{global, trace::Tracer, KeyValue};
use std::fmt::Write;
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt, Layer};
use uuid::Uuid;

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

enum EntityType {
    Line,
    Template,
}

/// Re-export for backwards compatibility
pub use crate::persistent_store::{PersistentDebugStore as DebugDataStore, EntityEvents};

/// OpenTelemetry-based debug layer that sends traces to Jaeger
pub struct OtelDebugLayer;

#[derive(Default)]
struct SpanData {
    element_id: Option<Uuid>,
    line_id: Option<Uuid>,
    template_id: Option<Uuid>,
    match_id: Option<Uuid>,
}

impl OtelDebugLayer {
    pub fn new() -> Self {
        OtelDebugLayer
    }
}

impl<S: Subscriber + for<'span> LookupSpan<'span>> Layer<S> for OtelDebugLayer {
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

        // Capture message from the event
        let mut message = String::new();
        let mut message_visitor = MessageVisitor(&mut message);
        event.record(&mut message_visitor);

        // Get tracer for the appropriate domain
        let tracer_name = match event.metadata().target() {
            target if target.contains("template") => "delver-templates",
            target if target.contains("matcher") => "delver-template-matching",
            target if target.contains("pdf") => "delver-pdf-parsing",
            _ => "delver-entities",
        };

        let tracer = global::tracer(tracer_name);

        // Handle entity recording based on ID types
        match (
            *id_visitor.element_id,
            *id_visitor.line_id,
            *id_visitor.template_id,
            *id_visitor.match_id,
        ) {
            // Template registration (template without a match)
            (_, _, Some(template_id), None) => {
                let span = tracer
                    .span_builder("template_registration")
                    .with_attributes(vec![
                        KeyValue::new("template_id", template_id.to_string()),
                        KeyValue::new("entity_type", "template"),
                        KeyValue::new("message", message.clone()),
                        KeyValue::new("target", event.metadata().target()),
                    ])
                    .start(&tracer);

                // Extract template name if available
                if let Some(name_start) = message.find("template_name = ") {
                    if let Some(name_end) = message[name_start..].find(';') {
                        let raw_name = &message[name_start + 16..name_start + name_end];
                        let name = raw_name.trim_matches('"');
                        
                        let span_with_name = tracer
                            .span_builder("template_registration")
                            .with_attributes(vec![
                                KeyValue::new("template_id", template_id.to_string()),
                                KeyValue::new("template_name", name),
                                KeyValue::new("entity_type", "template"),
                                KeyValue::new("message", message.clone()),
                                KeyValue::new("target", event.metadata().target()),
                            ])
                            .start(&tracer);
                        span_with_name.end();
                    }
                }

                span.end();
            }
            // Template match
            (_, Some(line_id), Some(template_id), Some(_match_id)) => {
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

                let span = tracer
                    .span_builder("template_match")
                    .with_attributes(vec![
                        KeyValue::new("template_id", template_id.to_string()),
                        KeyValue::new("content_id", line_id.to_string()),
                        KeyValue::new("match_score", score.to_string()),
                        KeyValue::new("match_type", "template_content_match"),
                        KeyValue::new("message", message),
                        KeyValue::new("target", event.metadata().target()),
                    ])
                    .start(&tracer);

                span.end();

                // Also record relationship
                let rel_span = tracer
                    .span_builder("record_relationship")
                    .with_attributes(vec![
                        KeyValue::new("parent_id", template_id.to_string()),
                        KeyValue::new("children", format!("[{}]", line_id)),
                        KeyValue::new("relationship_type", "template_match"),
                    ])
                    .start(&tracer);
                rel_span.end();
            }
            // Standard element case
            (Some(e_id), _, _, _) => {
                let span = tracer
                    .span_builder("entity_line")
                    .with_attributes(vec![
                        KeyValue::new("entity_id", e_id.to_string()),
                        KeyValue::new("entity_type", "element"),
                        KeyValue::new("message", message),
                        KeyValue::new("target", event.metadata().target()),
                    ])
                    .start(&tracer);
                span.end();
            }
            // Standard line case
            (_, Some(l_id), _, _) => {
                let span = tracer
                    .span_builder("entity_line")
                    .with_attributes(vec![
                        KeyValue::new("entity_id", l_id.to_string()),
                        KeyValue::new("entity_type", "line"),
                        KeyValue::new("message", message),
                        KeyValue::new("target", event.metadata().target()),
                    ])
                    .start(&tracer);
                span.end();
            }
            // Generic event
            _ => {
                let span = tracer
                    .span_builder("generic_event")
                    .with_attributes(vec![
                        KeyValue::new("message", message),
                        KeyValue::new("target", event.metadata().target()),
                        KeyValue::new("level", event.metadata().level().to_string()),
                    ])
                    .start(&tracer);
                span.end();
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
            if let Some(parent_id) = rel_parent {
                let span = tracer
                    .span_builder("record_relationship")
                    .with_attributes(vec![
                        KeyValue::new("parent_id", parent_id.to_string()),
                        KeyValue::new("children", format!("{:?}", rel_children)),
                        KeyValue::new("relationship_type", "generic"),
                    ])
                    .start(&tracer);
                span.end();
            }
        }
    }
}

/// Initialize OpenTelemetry-based debug logging
/// This is a compatibility function for the old API
pub fn init_debug_logging(_store: DebugDataStore) -> tokio::task::JoinHandle<()> {
    // Return a dummy join handle since OpenTelemetry is initialized elsewhere
    tokio::spawn(async {
        // This is just a placeholder for compatibility
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    })
}

#[derive(Debug)]
struct IdVisitor<'a> {
    element_id: &'a mut Option<Uuid>,
    line_id: &'a mut Option<Uuid>,
    template_id: &'a mut Option<Uuid>,
    match_id: &'a mut Option<Uuid>,
}

impl tracing::field::Visit for IdVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "line_id" => *self.line_id = Uuid::parse_str(value).ok(),
            "element_id" => *self.element_id = Uuid::parse_str(value).ok(),
            "template_id" => *self.template_id = Uuid::parse_str(value).ok(),
            "match_id" => *self.match_id = Uuid::parse_str(value).ok(),
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
