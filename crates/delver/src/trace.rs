//! Execution-trace activation for the CLI (D-027).
//!
//! Default is EXACTLY the pre-T1 behavior: `index`/`query`/`search` install
//! no subscriber (stdout byte-identical, stderr 0 bytes); `process` installs
//! its historical debug-capture subscriber
//! ([`delver_core::logging::init_debug_logging`]).
//!
//! Activation:
//! * `--trace` (or `DELVER_TRACE=1`) — OTLP/HTTP JSON export to
//!   `$OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4318`,
//!   `scripts/dev-otel.sh` starts a local Jaeger). If the collector is
//!   unreachable the flag warns once on stderr and falls back to the
//!   hierarchical stderr tree, so `--trace` never silently does nothing.
//! * `--trace-stderr` — force the hierarchical tree on stderr (tracing-tree).
//! * `--trace-json <path>` — JSON-lines spans/events written to a file.
//!
//! All trace output rides on stderr / the collector / the file — stdout
//! stays reserved for data (D-013) in every mode.
//!
//! Filtering: `RUST_LOG` wins when set; otherwise [`DEFAULT_FILTER`] enables
//! the full delver span vocabulary while keeping dependencies at `warn` and
//! the per-operator content-stream firehose (`delver_core::parse` below
//! `info`) opt-in.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use clap::Args;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use delver_core::logging::{debug_capture_layer, init_debug_logging, DebugDataStore};

use crate::otel;

/// Default directive string when `RUST_LOG` is unset: the whole delver
/// vocabulary, dependencies at warn, and the byte-level content-stream
/// machinery (`delver_core::parse` trace/debug callsites) gated to info.
pub const DEFAULT_FILTER: &str =
    "warn,delver=trace,delver_core=trace,delver_store=trace,delver_core::parse=info";

/// Shared trace flags, flattened into every subcommand (D-027).
#[derive(Args, Debug, Default, Clone)]
pub struct TraceArgs {
    /// Trace the run's semantic execution to an OpenTelemetry collector
    /// (OTLP/HTTP JSON to $OTEL_EXPORTER_OTLP_ENDPOINT, default
    /// http://localhost:4318 — see scripts/dev-otel.sh); falls back to the
    /// stderr tree when the collector is unreachable. Env: DELVER_TRACE=1
    #[clap(long)]
    pub trace: bool,

    /// Print the trace as a hierarchical tree on stderr (no collector
    /// needed); stdout is untouched
    #[clap(long)]
    pub trace_stderr: bool,

    /// Write structured JSON-lines spans/events to this file
    #[clap(long, value_name = "PATH")]
    pub trace_json: Option<PathBuf>,
}

impl TraceArgs {
    fn otlp_requested(&self) -> bool {
        self.trace || env_flag("DELVER_TRACE")
    }

    fn any_enabled(&self) -> bool {
        self.otlp_requested() || self.trace_stderr || self.trace_json.is_some()
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// `RUST_LOG` when set (and parseable), else [`DEFAULT_FILTER`]. Built per
/// layer (EnvFilter is not Clone).
fn env_filter() -> EnvFilter {
    match std::env::var("RUST_LOG") {
        Ok(directives) if !directives.trim().is_empty() => EnvFilter::try_new(&directives)
            .unwrap_or_else(|e| {
                eprintln!("warning: invalid RUST_LOG ({e}); using the default trace filter");
                EnvFilter::new(DEFAULT_FILTER)
            }),
        _ => EnvFilter::new(DEFAULT_FILTER),
    }
}

/// Truncate one line of foreign text (collector error bodies) for a warning.
pub fn preview_line(s: &str, max_chars: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    let mut chars = line.chars();
    let cut: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// Keeps the trace pipeline alive for the duration of `main`; dropping it
/// flushes the OTLP buffer (one blocking POST — CLI processes are
/// short-lived) and the JSON-lines writer.
pub struct TraceGuard {
    otlp: Option<OtlpFlush>,
    _json_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// The default-path `process` debug subscriber guard (pre-T1 behavior).
    _debug_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

struct OtlpFlush {
    endpoint: String,
    subcommand: &'static str,
    buffer: otel::SpanBuffer,
}

impl Drop for TraceGuard {
    fn drop(&mut self) {
        if let Some(flush) = self.otlp.take() {
            match otel::flush(&flush.endpoint, &flush.buffer, flush.subcommand) {
                Ok(report) if report.spans > 0 => {
                    let ui_hint = report
                        .root_trace_id
                        .as_deref()
                        .map(|id| format!(" — http://localhost:16686/trace/{id}"))
                        .unwrap_or_default();
                    eprintln!(
                        "trace: exported {} spans to {}{}",
                        report.spans, flush.endpoint, ui_hint
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!("warning: trace export to {} failed: {e}", flush.endpoint),
            }
        }
    }
}

/// Install the trace subscriber for one CLI invocation.
///
/// `debug_store` is `Some` only for the `process` subcommand: with tracing
/// off it routes through the historical [`init_debug_logging`] verbatim;
/// with tracing on, the same capture layer (same target filter) is COMPOSED
/// with the trace layers rather than replaced (D-017/D-027).
pub fn init(
    args: &TraceArgs,
    subcommand: &'static str,
    debug_store: Option<DebugDataStore>,
) -> Result<TraceGuard> {
    if !args.any_enabled() {
        // Default: exactly today's behavior.
        let debug_guard = debug_store.map(init_debug_logging);
        return Ok(TraceGuard {
            otlp: None,
            _json_guard: None,
            _debug_guard: debug_guard,
        });
    }

    // OTLP is the primary target; probe first so --trace never silently
    // drops the trace on the floor.
    let mut fall_back_to_tree = false;
    let (otlp_layer, otlp_flush) = if args.otlp_requested() {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| otel::DEFAULT_OTLP_ENDPOINT.to_string());
        match otel::probe(&endpoint) {
            Ok(()) => {
                let buffer: otel::SpanBuffer = Arc::new(Mutex::new(Vec::new()));
                (
                    Some(otel::OtlpLayer::new(buffer.clone()).with_filter(env_filter())),
                    Some(OtlpFlush {
                        endpoint,
                        subcommand,
                        buffer,
                    }),
                )
            }
            Err(reason) => {
                eprintln!(
                    "warning: --trace: OTLP collector {endpoint} unreachable ({reason}); \
                     falling back to the stderr tree (scripts/dev-otel.sh starts a local Jaeger)"
                );
                fall_back_to_tree = true;
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    let tree_layer = (args.trace_stderr || fall_back_to_tree).then(|| {
        tracing_tree::HierarchicalLayer::new(2)
            .with_writer(std::io::stderr)
            .with_targets(true)
            .with_indent_lines(true)
            .with_filter(env_filter())
    });

    let (json_layer, json_guard) = match &args.trace_json {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating --trace-json file {}", path.display()))?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            let layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(writer)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_current_span(true)
                .with_span_list(true)
                .with_filter(env_filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    let debug_layer = debug_store.map(|store| debug_capture_layer(store));

    let subscriber = tracing_subscriber::registry()
        .with(otlp_layer)
        .with(tree_layer)
        .with(json_layer)
        .with(debug_layer);
    tracing::subscriber::set_global_default(subscriber)
        .context("installing the trace subscriber")?;

    Ok(TraceGuard {
        otlp: otlp_flush,
        _json_guard: json_guard,
        _debug_guard: None,
    })
}
