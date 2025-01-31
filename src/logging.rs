use std::fmt;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Once;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Value, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::{
    filter::EnvFilter,
    fmt::{
        format::{self, FmtSpan, FormatEvent, FormatFields},
        FmtContext, FormattedFields,
    },
    layer::SubscriberExt,
    util::SubscriberInitExt,
    Layer,
};

use crate::parse::TextLine;
use std::collections::HashMap;
use uuid::Uuid;

// Define log targets as constants
pub const PDF_OPERATIONS: &str = "pdf_ops";
pub const PDF_PARSING: &str = "pdf_parse";
pub const PDF_FONTS: &str = "pdf_fonts";
pub const PDF_TEXT_OBJECT: &str = "pdf_text_object";
pub const PDF_TEXT_BLOCK: &str = "pdf_text_block";
pub const PDF_BT: &str = "pdf_bt";

// Global guard to keep the logger alive
static mut GUARD: Option<WorkerGuard> = None;
static INIT: Once = Once::new();

// Add these constants at the top
const DEBUG_TARGETS: &[&str] = &[
    PDF_OPERATIONS,
    PDF_TEXT_OBJECT,
    PDF_TEXT_BLOCK,
    PDF_BT,
    "delver_pdf::parse",
];

// Create a custom formatter for text object events
struct TextObjectFormatter;

impl<S, N> FormatEvent<S, N> for TextObjectFormatter
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();

        println!("metadata: {:?}", metadata);

        if event.metadata().is_span() {
            write!(&mut writer, "{} {}: ", metadata.level(), metadata.target())?;
        }

        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                let fields = &ext
                    .get::<FormattedFields<N>>()
                    .expect("will never be `None`");
                write!(writer, "{}", fields)?;
            }
        }

        // Format the actual event message
        ctx.field_format().format_fields(writer.by_ref(), event)?;

        writeln!(writer)
    }
}

#[derive(Clone, Default)]
pub struct DebugDataStore {
    message_arena: Arc<Mutex<Vec<String>>>, // Single source of truth for messages
    elements: Arc<Mutex<HashMap<Uuid, usize>>>, // element_id -> message_idx
    lines: Arc<Mutex<HashMap<Uuid, (Uuid, Vec<usize>)>>>, // line_id -> (block_id, message_indices)
    events: Arc<Mutex<HashMap<Uuid, Vec<usize>>>>, // line_id -> message_indices
}

impl DebugDataStore {
    fn record_element(&self, element_id: Uuid, line_id: Uuid, message: String) {
        let mut arena = self.message_arena.lock().unwrap();
        let idx = arena.len();
        arena.push(message.clone()); // Clone only for logging

        println!(
            "[STORE] Recording element {} for line {}",
            element_id, line_id
        );
        println!("  Message index: {}", idx);
        println!("  Message content: {}", message);

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

        println!("[STORE] Current state:");
        println!("  Total messages: {}", arena.len());
        println!("  Lines registered: {}", lines.len());
        println!(
            "  Events per line: {:#?}",
            events.iter().map(|(k, v)| (k, v.len())).collect::<Vec<_>>()
        );
    }

    pub fn get_events_for_line(&self, line_id: Uuid) -> Vec<String> {
        println!("[STORE] Retrieving events for line {}", line_id);

        let arena = self.message_arena.lock().unwrap();
        let events = self.events.lock().unwrap();

        println!("  Found event indices: {:?}", events.get(&line_id));

        events
            .get(&line_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| {
                        arena.get(idx).map(|msg| {
                            println!("  Retrieving index {}: {}", idx, msg);
                            msg.clone()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
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

impl<S> tracing_subscriber::Layer<S> for DebugLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("Span not found");
        let mut extensions = span.extensions_mut();

        let mut data = SpanData::default();
        let mut visitor = IdVisitor {
            element_id: &mut data.element_id,
            line_id: &mut data.line_id,
        };

        attrs.record(&mut visitor);
        extensions.insert(data);
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = IdVisitor {
            element_id: &mut None,
            line_id: &mut None,
        };
        event.record(&mut visitor);

        // Collect IDs from parent spans
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(data) = span.extensions().get::<SpanData>() {
                    if let Some(e_id) = data.element_id {
                        *visitor.element_id = visitor.element_id.or(Some(e_id));
                    }
                    if let Some(l_id) = data.line_id {
                        *visitor.line_id = visitor.line_id.or(Some(l_id));
                    }
                }
            }
        }

        // Record partial matches for debugging
        println!(
            "[DEBUG LAYER] Event captured - element: {:?}, line: {:?}",
            visitor.element_id, visitor.line_id
        );

        if let (Some(element_id), Some(line_id)) = (*visitor.element_id, *visitor.line_id) {
            let mut message = String::new();
            event.record(&mut MessageVisitor(&mut message));
            self.store.record_element(element_id, line_id, message);
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
}

impl DebugLayer {
    fn capture_element_context<S>(
        &self,
        event: &tracing::Event<'_>,
        ctx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> Option<String>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        event.parent().and_then(|span_id| {
            ctx.span(span_id).and_then(|span| {
                span.extensions()
                    .get::<FormattedFields<DefaultFields>>()
                    .map(|fields| fields.to_string())
            })
        })
    }
}

#[derive(Default)]
struct ContextVisitor {
    context: Option<String>,
}

impl tracing::field::Visit for ContextVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "bbox" {
            self.context = Some(format!("{:?}", value));
        }
    }
}

#[derive(Default)]
struct SpanContextExtractor {
    context: Option<String>,
}

impl tracing::field::Visit for SpanContextExtractor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "bbox" {
            self.context = Some(format!("{:?}", value));
        }
    }
}

pub fn init_logging(debug_ops: bool, store: DebugDataStore) -> WorkerGuard {
    init_debug_logging(store)
}

pub fn init_logging_with_dir(debug_ops: bool, log_dir: PathBuf) -> (WorkerGuard) {
    // Create directories if they don't exist
    std::fs::create_dir_all(&log_dir).expect("Failed to create log directory");

    let file_appender = RollingFileAppender::new(Rotation::NEVER, log_dir, "pdf-debug-ops.log");

    // Create the file writing layer for debug operations
    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);
    // let file_layer = tracing_subscriber::fmt::layer()
    //     .with_target(true)
    //     .with_thread_ids(true)
    //     .with_file(true)
    //     .with_line_number(true)
    //     .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE | FmtSpan::ENTER | FmtSpan::EXIT)
    //     .with_writer(non_blocking_appender)
    //     .with_filter(EnvFilter::new(format!(
    //         "{}={},{}={}",
    //         PDF_OPERATIONS,
    //         if debug_ops { "debug" } else { "info" },
    //         PDF_TEXT_OBJECT,
    //         if debug_ops { "trace" } else { "info" }
    //     )));

    let text_object_layer = tracing_subscriber::fmt::layer()
        .event_format(TextObjectFormatter)
        .with_writer(non_blocking_appender.clone())
        .with_filter(EnvFilter::new(format!("{}=debug", PDF_TEXT_OBJECT)));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE | FmtSpan::ENTER | FmtSpan::EXIT)
        .with_filter(
            EnvFilter::from_default_env()
                .add_directive(Level::INFO.into())
                .add_directive(format!("{}=info", PDF_PARSING).parse().unwrap())
                .add_directive(format!("{}=info", PDF_FONTS).parse().unwrap()),
        );

    INIT.call_once(|| {
        tracing_subscriber::registry()
            // .with(file_layer)
            .with(text_object_layer)
            .with(stdout_layer)
            .init();
    });

    guard
}
