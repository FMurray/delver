//! Hand-rolled OTLP/HTTP JSON trace exporter (D-027).
//!
//! `--trace` exports the run's span tree to an OpenTelemetry collector
//! (Jaeger, otel-collector, otel-desktop-viewer, ...) over the standard
//! OTLP/HTTP JSON encoding — `POST {endpoint}/v1/traces` with a
//! `resourceSpans / scopeSpans / spans` body per the proto3 JSON mapping
//! (hex trace/span ids, `int64` timestamps as JSON strings).
//!
//! Deliberately NOT the `opentelemetry`/`tracing-opentelemetry` crate family:
//! the CLI is a short-lived process that needs exactly "collect spans, flush
//! once on exit", and this file plus the already-pinned `ureq`/`serde_json`/
//! `uuid` cover that without growing the locked dependency graph. The
//! encoding is unit-tested against the spec shape below.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Default OTLP/HTTP endpoint (the standard local collector port).
pub const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4318";

/// Resource attribute `service.name` for every exported span.
pub const SERVICE_NAME: &str = "delver";

/// Defensive caps so a pathological run cannot balloon memory: spans past the
/// cap are dropped (counted), events past the per-span cap are dropped.
const MAX_BUFFERED_SPANS: usize = 100_000;
const MAX_EVENTS_PER_SPAN: usize = 4_096;

// ─────────────────────────────────────────────────────────────────────────────
// Data model
// ─────────────────────────────────────────────────────────────────────────────

/// OTLP attribute value (the subset of `AnyValue` the CLI emits).
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    String(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

impl AttrValue {
    /// proto3 JSON mapping of `AnyValue`: `int64` values are JSON strings.
    fn to_json(&self) -> serde_json::Value {
        match self {
            AttrValue::String(s) => serde_json::json!({ "stringValue": s }),
            AttrValue::Int(i) => serde_json::json!({ "intValue": i.to_string() }),
            AttrValue::Double(d) => serde_json::json!({ "doubleValue": d }),
            AttrValue::Bool(b) => serde_json::json!({ "boolValue": b }),
        }
    }
}

/// A span event (tracing event attached to its enclosing span).
#[derive(Debug, Clone)]
pub struct OtlpEvent {
    pub name: String,
    pub time_unix_nano: u64,
    pub attributes: Vec<(String, AttrValue)>,
}

/// A finished span, ready to encode.
#[derive(Debug, Clone)]
pub struct OtlpSpan {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    pub start_unix_nano: u64,
    pub end_unix_nano: u64,
    pub attributes: Vec<(String, AttrValue)>,
    pub events: Vec<OtlpEvent>,
}

/// Shared buffer the layer fills and the flusher drains.
pub type SpanBuffer = Arc<Mutex<Vec<OtlpSpan>>>;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        },
    )
}

fn now_unix_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 16 random non-zero bytes (uuid v4 is the already-pinned entropy source).
fn new_trace_id() -> [u8; 16] {
    let id = *uuid::Uuid::new_v4().as_bytes();
    debug_assert_ne!(id, [0u8; 16]);
    id
}

/// 8 random bytes from a v4 uuid, nonzero by construction.
fn new_span_id() -> [u8; 8] {
    let uuid = uuid::Uuid::new_v4();
    let mut id = [0u8; 8];
    id.copy_from_slice(&uuid.as_bytes()[..8]);
    if id == [0u8; 8] {
        id[7] = 1;
    }
    id
}

// ─────────────────────────────────────────────────────────────────────────────
// Field visitor: tracing values → OTLP attributes
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct AttrVisitor {
    attrs: Vec<(String, AttrValue)>,
    /// The `message` field, extracted as the event name.
    message: Option<String>,
}

impl Visit for AttrVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attrs
            .push((field.name().to_string(), AttrValue::Double(value)));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attrs
            .push((field.name().to_string(), AttrValue::Int(value)));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match i64::try_from(value) {
            Ok(v) => self
                .attrs
                .push((field.name().to_string(), AttrValue::Int(v))),
            Err(_) => self
                .attrs
                .push((field.name().to_string(), AttrValue::String(value.to_string()))),
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.attrs
            .push((field.name().to_string(), AttrValue::Bool(value)));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.attrs
                .push((field.name().to_string(), AttrValue::String(value.to_string())));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(rendered);
        } else {
            self.attrs
                .push((field.name().to_string(), AttrValue::String(rendered)));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The layer
// ─────────────────────────────────────────────────────────────────────────────

/// Per-span state carried in the span's extensions while it is alive.
struct ActiveSpan {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    name: String,
    start_unix_nano: u64,
    attributes: Vec<(String, AttrValue)>,
    events: Vec<OtlpEvent>,
    dropped_events: usize,
}

/// `tracing_subscriber::Layer` that assembles finished [`OtlpSpan`]s into a
/// shared buffer. Trace identity: a span inherits its parent's trace id
/// (explicit parent, else the contextual current span); parentless spans
/// start a new trace — for the CLI that is the one `cli.*` root span per run.
/// Events fired outside any enabled span are not exported (the stderr tree /
/// JSON layers still see them).
pub struct OtlpLayer {
    buffer: SpanBuffer,
}

impl OtlpLayer {
    pub fn new(buffer: SpanBuffer) -> Self {
        OtlpLayer { buffer }
    }
}

impl<S> Layer<S> for OtlpLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let parent = attrs
            .parent()
            .and_then(|pid| ctx.span(pid))
            .or_else(|| {
                if attrs.is_contextual() {
                    ctx.lookup_current()
                } else {
                    None
                }
            });
        let inherited = parent.as_ref().and_then(|p| {
            p.extensions()
                .get::<ActiveSpan>()
                .map(|a| (a.trace_id, a.span_id))
        });
        let (trace_id, parent_span_id) = match inherited {
            Some((trace_id, parent_span_id)) => (trace_id, Some(parent_span_id)),
            None => (new_trace_id(), None),
        };

        let mut visitor = AttrVisitor::default();
        attrs.record(&mut visitor);
        let meta = attrs.metadata();
        let mut attributes = visitor.attrs;
        attributes.push((
            "code.namespace".to_string(),
            AttrValue::String(meta.target().to_string()),
        ));
        attributes.push((
            "level".to_string(),
            AttrValue::String(meta.level().to_string()),
        ));

        span.extensions_mut().insert(ActiveSpan {
            trace_id,
            span_id: new_span_id(),
            parent_span_id,
            name: meta.name().to_string(),
            start_unix_nano: now_unix_nano(),
            attributes,
            events: Vec::new(),
            dropped_events: 0,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = AttrVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        if let Some(active) = extensions.get_mut::<ActiveSpan>() {
            active.attributes.extend(visitor.attrs);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // The event's explicit parent, else the contextual current span.
        let Some(span) = ctx.event_span(event) else {
            return;
        };

        let mut visitor = AttrVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        let name = visitor
            .message
            .take()
            .unwrap_or_else(|| meta.name().to_string());
        let mut attributes = visitor.attrs;
        attributes.push((
            "code.namespace".to_string(),
            AttrValue::String(meta.target().to_string()),
        ));
        attributes.push((
            "level".to_string(),
            AttrValue::String(meta.level().to_string()),
        ));

        let mut extensions = span.extensions_mut();
        if let Some(active) = extensions.get_mut::<ActiveSpan>() {
            if active.events.len() < MAX_EVENTS_PER_SPAN {
                active.events.push(OtlpEvent {
                    name,
                    time_unix_nano: now_unix_nano(),
                    attributes,
                });
            } else {
                active.dropped_events += 1;
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(mut active) = span.extensions_mut().remove::<ActiveSpan>() else {
            return;
        };
        if active.dropped_events > 0 {
            active.attributes.push((
                "delver.dropped_events".to_string(),
                AttrValue::Int(active.dropped_events as i64),
            ));
        }
        let finished = OtlpSpan {
            trace_id: active.trace_id,
            span_id: active.span_id,
            parent_span_id: active.parent_span_id,
            name: active.name,
            start_unix_nano: active.start_unix_nano,
            end_unix_nano: now_unix_nano(),
            attributes: active.attributes,
            events: active.events,
        };
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.len() < MAX_BUFFERED_SPANS {
            buffer.push(finished);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Encoding + transport
// ─────────────────────────────────────────────────────────────────────────────

/// Encode finished spans as one OTLP/HTTP JSON `ExportTraceServiceRequest`.
pub fn encode_resource_spans(
    resource_attrs: &[(String, AttrValue)],
    spans: &[OtlpSpan],
) -> serde_json::Value {
    let mut resource_attributes = vec![(
        "service.name".to_string(),
        AttrValue::String(SERVICE_NAME.to_string()),
    )];
    resource_attributes.extend(resource_attrs.iter().cloned());
    serde_json::json!({
        "resourceSpans": [{
            "resource": { "attributes": attrs_json(&resource_attributes) },
            "scopeSpans": [{
                "scope": {
                    "name": SERVICE_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "spans": spans.iter().map(span_json).collect::<Vec<_>>(),
            }],
        }],
    })
}

fn attrs_json(attrs: &[(String, AttrValue)]) -> serde_json::Value {
    serde_json::Value::Array(
        attrs
            .iter()
            .map(|(key, value)| serde_json::json!({ "key": key, "value": value.to_json() }))
            .collect(),
    )
}

fn span_json(span: &OtlpSpan) -> serde_json::Value {
    serde_json::json!({
        "traceId": hex(&span.trace_id),
        "spanId": hex(&span.span_id),
        // Root spans encode an empty parentSpanId per the JSON mapping.
        "parentSpanId": span.parent_span_id.map(|p| hex(&p)).unwrap_or_default(),
        "name": span.name,
        "kind": 1, // SPAN_KIND_INTERNAL
        "startTimeUnixNano": span.start_unix_nano.to_string(),
        "endTimeUnixNano": span.end_unix_nano.to_string(),
        "attributes": attrs_json(&span.attributes),
        "events": span.events.iter().map(|event| serde_json::json!({
            "timeUnixNano": event.time_unix_nano.to_string(),
            "name": event.name,
            "attributes": attrs_json(&event.attributes),
        })).collect::<Vec<_>>(),
        "status": {},
    })
}

fn traces_url(endpoint: &str) -> String {
    format!("{}/v1/traces", endpoint.trim_end_matches('/'))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()
}

/// Cheap reachability probe: POST an empty (valid) export request. `Err`
/// carries a human-readable reason for the fall-back warning.
pub fn probe(endpoint: &str) -> Result<(), String> {
    match agent()
        .post(&traces_url(endpoint))
        .set("Content-Type", "application/json")
        .send_string(r#"{"resourceSpans":[]}"#)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(format!("collector answered HTTP {code}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Result of a successful flush: how many spans were exported and the root
/// trace id (hex) for "open it in the UI" hints.
pub struct FlushReport {
    pub spans: usize,
    pub root_trace_id: Option<String>,
}

/// Drain the buffer and POST it to the collector. Spans are exported in one
/// request — CLI runs are one trace, well under practical body limits.
pub fn flush(endpoint: &str, buffer: &SpanBuffer, subcommand: &str) -> Result<FlushReport, String> {
    let spans: Vec<OtlpSpan> = {
        let mut buffer = buffer.lock().unwrap();
        std::mem::take(&mut *buffer)
    };
    if spans.is_empty() {
        return Ok(FlushReport {
            spans: 0,
            root_trace_id: None,
        });
    }
    let root_trace_id = spans
        .iter()
        .find(|s| s.parent_span_id.is_none())
        .or(spans.last())
        .map(|s| hex(&s.trace_id));

    let resource_attrs = vec![(
        "delver.subcommand".to_string(),
        AttrValue::String(subcommand.to_string()),
    )];
    let body = encode_resource_spans(&resource_attrs, &spans);
    match agent()
        .post(&traces_url(endpoint))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
    {
        Ok(_) => Ok(FlushReport {
            spans: spans.len(),
            root_trace_id,
        }),
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            Err(format!(
                "collector rejected export: HTTP {code} {}",
                crate::trace::preview_line(&detail, 200)
            ))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_span() -> OtlpSpan {
        OtlpSpan {
            trace_id: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f, 0x10,
            ],
            span_id: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11],
            parent_span_id: None,
            name: "cli.query".to_string(),
            start_unix_nano: 1_700_000_000_000_000_000,
            end_unix_nano: 1_700_000_001_500_000_000,
            attributes: vec![
                ("doc".to_string(), AttrValue::String("56e30967".to_string())),
                ("elements".to_string(), AttrValue::Int(26657)),
                ("threshold".to_string(), AttrValue::Double(0.6)),
                ("explicit".to_string(), AttrValue::Bool(true)),
            ],
            events: vec![OtlpEvent {
                name: "boundary_candidate".to_string(),
                time_unix_nano: 1_700_000_000_700_000_000,
                attributes: vec![("score".to_string(), AttrValue::Double(0.837))],
            }],
        }
    }

    #[test]
    fn encodes_the_otlp_http_json_spec_shape() {
        let child = OtlpSpan {
            parent_span_id: Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]),
            name: "pass1".to_string(),
            events: Vec::new(),
            ..sample_span()
        };
        let body = encode_resource_spans(&[], &[sample_span(), child]);

        // Top-level resourceSpans/scopeSpans/spans nesting with service.name.
        let resource_spans = &body["resourceSpans"][0];
        assert_eq!(
            resource_spans["resource"]["attributes"][0],
            serde_json::json!({ "key": "service.name", "value": { "stringValue": "delver" } })
        );
        let scope_spans = &resource_spans["scopeSpans"][0];
        assert_eq!(scope_spans["scope"]["name"], "delver");
        let spans = scope_spans["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 2);

        // Hex ids: 32 lowercase hex chars for traceId, 16 for spanId; root
        // parentSpanId is the empty string.
        let root = &spans[0];
        assert_eq!(root["traceId"], "0102030405060708090a0b0c0d0e0f10");
        assert_eq!(root["spanId"], "aabbccddeeff0011");
        assert_eq!(root["parentSpanId"], "");
        assert_eq!(spans[1]["parentSpanId"], "aabbccddeeff0011");

        // int64 timestamps are JSON strings (proto3 JSON mapping).
        assert_eq!(root["startTimeUnixNano"], "1700000000000000000");
        assert_eq!(root["endTimeUnixNano"], "1700000001500000000");
        assert_eq!(root["kind"], 1);

        // Attribute AnyValue mapping: string/int(string)/double/bool.
        let attrs = root["attributes"].as_array().unwrap();
        assert_eq!(
            attrs[0],
            serde_json::json!({ "key": "doc", "value": { "stringValue": "56e30967" } })
        );
        assert_eq!(
            attrs[1],
            serde_json::json!({ "key": "elements", "value": { "intValue": "26657" } })
        );
        assert_eq!(
            attrs[2],
            serde_json::json!({ "key": "threshold", "value": { "doubleValue": 0.6 } })
        );
        assert_eq!(
            attrs[3],
            serde_json::json!({ "key": "explicit", "value": { "boolValue": true } })
        );

        // Span events carry name + string timestamp + attributes.
        let event = &root["events"][0];
        assert_eq!(event["name"], "boundary_candidate");
        assert_eq!(event["timeUnixNano"], "1700000000700000000");
        assert_eq!(
            event["attributes"][0],
            serde_json::json!({ "key": "score", "value": { "doubleValue": 0.837 } })
        );
    }

    #[test]
    fn hex_and_ids_are_wellformed() {
        assert_eq!(hex(&[0x00, 0xff, 0x10]), "00ff10");
        let trace_id = new_trace_id();
        assert_ne!(trace_id, [0u8; 16]);
        assert_eq!(hex(&trace_id).len(), 32);
        let span_id = new_span_id();
        assert_ne!(span_id, [0u8; 8]);
        assert_eq!(hex(&span_id).len(), 16);
    }

    #[test]
    fn layer_assembles_parented_spans_and_events() {
        use tracing_subscriber::layer::SubscriberExt;

        let buffer: SpanBuffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(OtlpLayer::new(buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let root = tracing::info_span!("cli.query", doc = "abc");
            let _root = root.enter();
            {
                let child = tracing::info_span!("pass1", section = "MD&A");
                let _child = child.enter();
                tracing::debug!(score = 0.83, "boundary_candidate");
            }
        });

        let spans = buffer.lock().unwrap();
        // Child closes first, then root.
        assert_eq!(spans.len(), 2);
        let child = &spans[0];
        let root = &spans[1];
        assert_eq!(child.name, "pass1");
        assert_eq!(root.name, "cli.query");
        assert_eq!(root.parent_span_id, None);
        assert_eq!(child.parent_span_id, Some(root.span_id));
        assert_eq!(child.trace_id, root.trace_id, "one trace per run");
        assert_eq!(child.events.len(), 1);
        assert_eq!(child.events[0].name, "boundary_candidate");
        assert!(child
            .events[0]
            .attributes
            .iter()
            .any(|(k, v)| k == "score" && *v == AttrValue::Double(0.83)));
        assert!(root
            .attributes
            .iter()
            .any(|(k, v)| k == "doc" && *v == AttrValue::String("abc".to_string())));
    }
}
