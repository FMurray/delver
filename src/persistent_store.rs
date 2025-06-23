use anyhow::{Context, Result};
use opentelemetry::trace::{TraceId, SpanId, Status};
use opentelemetry::{global, trace::Tracer, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{trace as sdktrace, Resource};
use opentelemetry_semantic_conventions as semcov;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tracing::debug;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};
use uuid::Uuid;

const JAEGER_QUERY_URL: &str = "http://localhost:16686";
const JAEGER_COLLECTOR_URL: &str = "http://localhost:14268/api/traces";

#[derive(Clone)]
pub struct PersistentDebugStore {
    jaeger_query_url: String,
    http_client: reqwest::Client,
    service_name: String,
}

#[derive(Debug, Default)]
pub struct EntityEvents {
    pub messages: Vec<String>,
    pub children: Vec<EntityEvents>,
}

#[derive(Debug, Deserialize)]
struct JaegerResponse {
    data: Vec<JaegerTrace>,
}

#[derive(Debug, Deserialize)]
struct JaegerTrace {
    #[serde(rename = "traceID")]
    trace_id: String,
    spans: Vec<JaegerSpan>,
}

#[derive(Debug, Deserialize)]
struct JaegerSpan {
    #[serde(rename = "spanID")]
    span_id: String,
    #[serde(rename = "operationName")]
    operation_name: String,
    #[serde(rename = "startTime")]
    start_time: u64,
    tags: Vec<JaegerTag>,
}

#[derive(Debug, Deserialize)]
struct JaegerTag {
    key: String,
    value: String,
}

impl Default for PersistentDebugStore {
    fn default() -> Self {
        Self::new("http://localhost:16686", "delver-pdf").unwrap()
    }
}

impl PersistentDebugStore {
    pub fn new(jaeger_query_url: &str, service_name: &str) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            jaeger_query_url: jaeger_query_url.to_string(),
            http_client,
            service_name: service_name.to_string(),
        })
    }

    /// Initialize OpenTelemetry tracing with Jaeger exporter
    pub async fn init_tracing(&self) -> Result<()> {
        // Create a gRPC exporter
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint("http://localhost:4317"),
            )
            .with_trace_config(
                sdktrace::config().with_resource(Resource::new(vec![
                    KeyValue::new(semcov::resource::SERVICE_NAME, self.service_name.clone()),
                    KeyValue::new(semcov::resource::SERVICE_VERSION, "0.1.0"),
                ])),
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .context("Failed to install OpenTelemetry tracer")?;

        // Set the tracer as global
        global::set_tracer_provider(tracer.provider().unwrap());

        // Create OpenTelemetry layer for tracing-subscriber
        let telemetry_layer = OpenTelemetryLayer::new(tracer);

        // Build the subscriber
        let subscriber = Registry::default().with(telemetry_layer);

        // Set the global subscriber
        tracing::subscriber::set_global_default(subscriber)
            .context("Failed to set global subscriber")?;

        Ok(())
    }

    /// Query Jaeger for traces by service name
    async fn query_traces(&self, service: &str, limit: Option<usize>) -> Result<Vec<JaegerTrace>> {
        let limit = limit.unwrap_or(100);
        let url = format!(
            "{}/api/traces?service={}&limit={}",
            self.jaeger_query_url, service, limit
        );

        debug!("Querying Jaeger: {}", url);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("Failed to query Jaeger")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Jaeger query failed with status: {}",
                response.status()
            ));
        }

        let jaeger_response: JaegerResponse = response
            .json()
            .await
            .context("Failed to parse Jaeger response")?;

        Ok(jaeger_response.data)
    }

    /// Find spans by operation name pattern
    async fn find_spans_by_operation(&self, operation_pattern: &str) -> Result<Vec<JaegerSpan>> {
        let traces = self.query_traces(&self.service_name, Some(1000)).await?;
        
        let mut matching_spans = Vec::new();
        for trace in traces {
            for span in trace.spans {
                if span.operation_name.contains(operation_pattern) {
                    matching_spans.push(span);
                }
            }
        }

        Ok(matching_spans)
    }

    /// Find spans by tag value
    async fn find_spans_by_tag(&self, tag_key: &str, tag_value: &str) -> Result<Vec<JaegerSpan>> {
        let traces = self.query_traces(&self.service_name, Some(1000)).await?;
        
        let mut matching_spans = Vec::new();
        for trace in traces {
            for span in trace.spans {
                if span.tags.iter().any(|tag| tag.key == tag_key && tag.value == tag_value) {
                    matching_spans.push(span);
                }
            }
        }

        Ok(matching_spans)
    }

    /// Get entity lineage - equivalent to the old in-memory method
    pub async fn get_entity_lineage(&self, entity_id: Uuid) -> Vec<String> {
        match self.find_spans_by_tag("entity_id", &entity_id.to_string()).await {
            Ok(spans) => spans.into_iter().map(|span| span.operation_name).collect(),
            Err(e) => {
                debug!("Failed to get entity lineage for {}: {}", entity_id, e);
                Vec::new()
            }
        }
    }

    /// Record a relationship between entities
    pub fn record_relationship(&self, parent: Option<Uuid>, children: Vec<Uuid>, rel_type: &str) {
        if let Some(parent_id) = parent {
            let tracer = global::tracer("delver-relationships");
            let span = tracer
                .span_builder("record_relationship")
                .with_attributes(vec![
                    KeyValue::new("parent_id", parent_id.to_string()),
                    KeyValue::new("children", format!("{:?}", children)),
                    KeyValue::new("relationship_type", rel_type.to_string()),
                ])
                .start(&tracer);

            // End span immediately since this is just recording metadata
            span.end();
        }
    }

    /// Get children of an entity
    pub async fn get_children(&self, entity_id: Uuid) -> Vec<Uuid> {
        match self.find_spans_by_tag("parent_id", &entity_id.to_string()).await {
            Ok(spans) => {
                let mut children = Vec::new();
                for span in spans {
                    for tag in span.tags {
                        if tag.key == "children" {
                            // Parse the children list from the tag value
                            if let Ok(child_ids) = serde_json::from_str::<Vec<Uuid>>(&tag.value) {
                                children.extend(child_ids);
                            }
                        }
                    }
                }
                children
            }
            Err(e) => {
                debug!("Failed to get children for {}: {}", entity_id, e);
                Vec::new()
            }
        }
    }

    /// Get parent of an entity
    pub async fn get_parent(&self, entity_id: Uuid) -> Option<Uuid> {
        match self.find_spans_by_tag("entity_id", &entity_id.to_string()).await {
            Ok(spans) => {
                for span in spans {
                    for tag in span.tags {
                        if tag.key == "parent_id" {
                            if let Ok(parent_id) = Uuid::parse_str(&tag.value) {
                                return Some(parent_id);
                            }
                        }
                    }
                }
                None
            }
            Err(e) => {
                debug!("Failed to get parent for {}: {}", entity_id, e);
                None
            }
        }
    }

    /// Get entity events - recursively build the event tree
    pub async fn get_entity_events(&self, entity_id: Uuid) -> EntityEvents {
        let mut entity_events = EntityEvents::default();

        // Get spans for this entity
        if let Ok(spans) = self.find_spans_by_tag("entity_id", &entity_id.to_string()).await {
            entity_events.messages = spans.into_iter().map(|span| span.operation_name).collect();
        }

        // Get children and their events recursively
        let children = self.get_children(entity_id).await;
        for child_id in children {
            let child_events = self.get_entity_events(child_id).await;
            entity_events.children.push(child_events);
        }

        entity_events
    }

    /// Record a template match
    pub fn record_template_match(&self, template_id: Uuid, content_id: Uuid, score: f32) {
        let tracer = global::tracer("delver-template-matching");
        let span = tracer
            .span_builder("template_match")
            .with_attributes(vec![
                KeyValue::new("template_id", template_id.to_string()),
                KeyValue::new("content_id", content_id.to_string()),
                KeyValue::new("match_score", score.to_string()),
                KeyValue::new("match_type", "template_content_match"),
            ])
            .start(&tracer);

        span.end();

        // Also record the relationship
        self.record_relationship(Some(template_id), vec![content_id], "template_match");
    }

    /// Get template matches
    pub async fn get_template_matches(&self, template_id: Uuid) -> Vec<(Uuid, f32)> {
        match self.find_spans_by_operation("template_match").await {
            Ok(spans) => {
                let mut matches = Vec::new();
                for span in spans {
                    let mut template_matches = false;
                    let mut content_id = None;
                    let mut score = 0.0;

                    for tag in &span.tags {
                        match tag.key.as_str() {
                            "template_id" => {
                                if let Ok(tid) = Uuid::parse_str(&tag.value) {
                                    template_matches = tid == template_id;
                                }
                            }
                            "content_id" => {
                                if let Ok(cid) = Uuid::parse_str(&tag.value) {
                                    content_id = Some(cid);
                                }
                            }
                            "match_score" => {
                                if let Ok(s) = tag.value.parse::<f32>() {
                                    score = s;
                                }
                            }
                            _ => {}
                        }
                    }

                    if template_matches {
                        if let Some(cid) = content_id {
                            matches.push((cid, score));
                        }
                    }
                }
                matches
            }
            Err(e) => {
                debug!("Failed to get template matches for {}: {}", template_id, e);
                Vec::new()
            }
        }
    }

    /// Get the matching template for content
    pub async fn get_matching_template(&self, content_id: Uuid) -> Option<(Uuid, f32)> {
        match self.find_spans_by_tag("content_id", &content_id.to_string()).await {
            Ok(spans) => {
                for span in spans {
                    if span.operation_name == "template_match" {
                        let mut template_id = None;
                        let mut score = 0.0;

                        for tag in &span.tags {
                            match tag.key.as_str() {
                                "template_id" => {
                                    if let Ok(tid) = Uuid::parse_str(&tag.value) {
                                        template_id = Some(tid);
                                    }
                                }
                                "match_score" => {
                                    if let Ok(s) = tag.value.parse::<f32>() {
                                        score = s;
                                    }
                                }
                                _ => {}
                            }
                        }

                        if let Some(tid) = template_id {
                            return Some((tid, score));
                        }
                    }
                }
                None
            }
            Err(e) => {
                debug!("Failed to get matching template for {}: {}", content_id, e);
                None
            }
        }
    }

    /// Set template name
    pub fn set_template_name(&self, template_id: Uuid, name: String) {
        let tracer = global::tracer("delver-templates");
        let span = tracer
            .span_builder("template_registration")
            .with_attributes(vec![
                KeyValue::new("template_id", template_id.to_string()),
                KeyValue::new("template_name", name),
            ])
            .start(&tracer);

        span.end();
    }

    /// Get template name
    pub async fn get_template_name(&self, template_id: Uuid) -> Option<String> {
        match self.find_spans_by_tag("template_id", &template_id.to_string()).await {
            Ok(spans) => {
                for span in spans {
                    if span.operation_name == "template_registration" {
                        for tag in &span.tags {
                            if tag.key == "template_name" {
                                return Some(tag.value.clone());
                            }
                        }
                    }
                }
                None
            }
            Err(e) => {
                debug!("Failed to get template name for {}: {}", template_id, e);
                None
            }
        }
    }

    /// Get all templates
    pub async fn get_templates(&self) -> Vec<(Uuid, String)> {
        match self.find_spans_by_operation("template_registration").await {
            Ok(spans) => {
                let mut templates = Vec::new();
                for span in spans {
                    let mut template_id = None;
                    let mut template_name = None;

                    for tag in &span.tags {
                        match tag.key.as_str() {
                            "template_id" => {
                                if let Ok(tid) = Uuid::parse_str(&tag.value) {
                                    template_id = Some(tid);
                                }
                            }
                            "template_name" => {
                                template_name = Some(tag.value.clone());
                            }
                            _ => {}
                        }
                    }

                    if let (Some(tid), Some(name)) = (template_id, template_name) {
                        templates.push((tid, name));
                    }
                }
                templates
            }
            Err(e) => {
                debug!("Failed to get templates: {}", e);
                Vec::new()
            }
        }
    }

    /// Count matches for a template
    pub async fn count_matches_for_template(&self, template_id: &Uuid) -> usize {
        self.get_template_matches(*template_id).await.len()
    }

    /// Get content matches for a template
    pub async fn get_content_matches_for_template(&self, template_id: &Uuid) -> Vec<Uuid> {
        self.get_template_matches(*template_id)
            .await
            .into_iter()
            .map(|(content_id, _score)| content_id)
            .collect()
    }

    /// Get content by ID - for now, return the UUID as string
    pub async fn get_content_by_id(&self, content_id: &Uuid) -> Option<String> {
        // Try to find spans with this content_id and extract meaningful content
        if let Ok(spans) = self.find_spans_by_tag("entity_id", &content_id.to_string()).await {
            for span in spans {
                // Look for text content in span tags
                for tag in &span.tags {
                    if tag.key == "text" || tag.key == "content" {
                        return Some(tag.value.clone());
                    }
                }
            }
        }

        // Fallback to UUID string
        Some(content_id.to_string())
    }

    /// Get events by target type
    pub async fn get_events_by_target(&self, target: &str) -> Vec<String> {
        match self.find_spans_by_tag("target", target).await {
            Ok(spans) => spans.into_iter().map(|span| span.operation_name).collect(),
            Err(e) => {
                debug!("Failed to get events by target {}: {}", target, e);
                Vec::new()
            }
        }
    }

    /// Count all traces
    pub async fn count_all_traces(&self) -> usize {
        match self.query_traces(&self.service_name, Some(10000)).await {
            Ok(traces) => traces.iter().map(|t| t.spans.len()).sum(),
            Err(e) => {
                debug!("Failed to count traces: {}", e);
                0
            }
        }
    }

    /// Wait for Jaeger to be ready
    pub async fn wait_for_jaeger(&self, timeout_secs: u64) -> Result<()> {
        let start = SystemTime::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed().unwrap_or(timeout) >= timeout {
                return Err(anyhow::anyhow!("Timeout waiting for Jaeger"));
            }

            // Try to query Jaeger services endpoint
            let url = format!("{}/api/services", self.jaeger_query_url);
            if let Ok(response) = self.http_client.get(&url).send().await {
                if response.status().is_success() {
                    debug!("Jaeger is ready");
                    return Ok(());
                }
            }

            debug!("Waiting for Jaeger to be ready...");
            sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Record an entity with OpenTelemetry traces
pub fn record_entity(entity_id: Uuid, entity_type: &str, message: String) {
    let tracer = global::tracer("delver-entities");
    let span = tracer
        .span_builder(format!("entity_{}", entity_type))
        .with_attributes(vec![
            KeyValue::new("entity_id", entity_id.to_string()),
            KeyValue::new("entity_type", entity_type),
            KeyValue::new("message", message),
        ])
        .start(&tracer);

    span.end();
}