use std::fmt;
use std::path::PathBuf;
use std::sync::Once;
use tracing::{Event, Level, Subscriber};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
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

pub fn init_logging(debug_ops: bool) -> WorkerGuard {
    let file_appender = RollingFileAppender::new(Rotation::HOURLY, "logs", "pdf-debug-ops.log");
    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

    // Create a prettier format for the file output
    let file_format = tracing_subscriber::fmt::format()
        .with_level(true)
        .with_target(true)
        .with_ansi(true)
        .pretty();

    // Single layer for file logging that captures both targets
    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(file_format)
        .with_writer(non_blocking_appender)
        .with_filter(EnvFilter::new("pdf_text_object=debug,pdf_text_block=info"));

    // Create a different format for stdout
    let stdout_format = tracing_subscriber::fmt::format()
        .with_level(true)
        .with_target(false) // Hide target in console output
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(true) // Enable ANSI colors in console
        .compact(); // Use compact formatting for console

    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(stdout_format)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE) // Fewer span events for console
        .with_filter(
            EnvFilter::from_default_env()
                .add_directive(Level::INFO.into())
                .add_directive(format!("{}=info", PDF_PARSING).parse().unwrap())
                .add_directive(format!("{}=info", PDF_FONTS).parse().unwrap()),
        );

    INIT.call_once(|| {
        tracing_subscriber::registry()
            .with(file_layer)
            .with(stdout_layer)
            .init();
    });

    guard
}

pub fn init_logging_with_dir(debug_ops: bool, log_dir: PathBuf) -> WorkerGuard {
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
