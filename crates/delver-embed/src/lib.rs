//! Embedding backends for delver's `EmbeddingSim(...)` matcher
//! (docs/DECISIONS.md D-005/D-014).
//!
//! * [`DatabricksEmbedder`] — calls a Databricks model-serving endpoint
//!   (`https://$DATABRICKS_HOST/serving-endpoints/{name}/invocations`, Bearer
//!   `$DATABRICKS_TOKEN`) or any full URL.
//! * [`MockEmbedder`] — deterministic, seedable backend for tests; unknown
//!   texts embed to a vector orthogonal to every seeded vector.
//!
//! No test in this crate touches the network: request-body construction and
//! response parsing are pure functions unit-tested against canned JSON.

use std::collections::HashMap;
use std::time::Duration;

use delver_core::embed::{EmbedError, Embedder};
use serde_json::{json, Value};

// ─────────────────────────────────────────────────────────────────────────────
// Databricks serving-endpoint backend
// ─────────────────────────────────────────────────────────────────────────────

pub struct DatabricksEmbedder {
    url: String,
    token: String,
    model: Option<String>,
    agent: ureq::Agent,
}

impl DatabricksEmbedder {
    /// `endpoint` is either a serving-endpoint name (resolved against
    /// `$DATABRICKS_HOST`) or a full `http(s)://` URL used as-is.
    /// `$DATABRICKS_TOKEN` is always required.
    pub fn new(endpoint: &str) -> Result<Self, EmbedError> {
        let url = resolve_endpoint_url(endpoint, std::env::var("DATABRICKS_HOST").ok().as_deref())?;
        let token = std::env::var("DATABRICKS_TOKEN").map_err(|_| {
            EmbedError(
                "DATABRICKS_TOKEN is not set (required to call Databricks serving endpoints)"
                    .to_string(),
            )
        })?;
        Ok(Self {
            url,
            token,
            model: None,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(120))
                .build(),
        })
    }

    /// Optional `model` field to include in the request body (some gateways
    /// route on it; Databricks endpoints usually ignore it).
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// Resolved invocations URL (for diagnostics/tests).
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Resolve an endpoint name or URL to the invocations URL.
/// Pure function so it is unit-testable without env mutation.
pub fn resolve_endpoint_url(endpoint: &str, host: Option<&str>) -> Result<String, EmbedError> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_string());
    }
    let host = host.filter(|h| !h.is_empty()).ok_or_else(|| {
        EmbedError(format!(
            "embedding endpoint '{endpoint}' is a name, but DATABRICKS_HOST is not set; \
             set DATABRICKS_HOST or pass a full https:// URL"
        ))
    })?;
    let host = host.trim_end_matches('/');
    let base = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    };
    Ok(format!("{base}/serving-endpoints/{endpoint}/invocations"))
}

/// Request body for a batch of texts: `{"input": [...]}` (the
/// OpenAI-compatible shape Databricks embedding endpoints accept), plus an
/// optional `"model"` field.
pub fn request_body(texts: &[&str], model: Option<&str>) -> Value {
    let mut body = json!({ "input": texts });
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    body
}

/// Parse an embeddings response. Accepts the two common shapes:
///
/// * `{"data": [{"embedding": [...], "index": 0}, ...]}` (OpenAI-compatible;
///   reordered by `index` when present)
/// * `{"predictions": [[...], ...]}` (MLflow pyfunc serving)
///
/// Anything else is rejected with an error naming the keys found, and the
/// vector count must equal `expected` (D-006: no silent partial results).
pub fn parse_response(value: &Value, expected: usize) -> Result<Vec<Vec<f32>>, EmbedError> {
    let vectors = if let Some(data) = value.get("data").and_then(Value::as_array) {
        let mut rows: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for (position, item) in data.iter().enumerate() {
            let embedding = item.get("embedding").ok_or_else(|| {
                EmbedError(format!(
                    "response item {position} in \"data\" has no \"embedding\" field"
                ))
            })?;
            let index = item
                .get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .unwrap_or(position);
            rows.push((index, parse_vector(embedding, position)?));
        }
        rows.sort_by_key(|(index, _)| *index);
        rows.into_iter().map(|(_, vector)| vector).collect()
    } else if let Some(predictions) = value.get("predictions").and_then(Value::as_array) {
        predictions
            .iter()
            .enumerate()
            .map(|(position, row)| parse_vector(row, position))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let keys = match value.as_object() {
            Some(map) => map.keys().cloned().collect::<Vec<_>>().join(", "),
            None => format!("non-object JSON ({})", json_type_name(value)),
        };
        return Err(EmbedError(format!(
            "unrecognized embeddings response shape: expected {{\"data\": \
             [{{\"embedding\": [...]}}]}} or {{\"predictions\": [[...]]}}, got keys: [{keys}]"
        )));
    };

    if vectors.len() != expected {
        return Err(EmbedError(format!(
            "endpoint returned {} embeddings for {} inputs",
            vectors.len(),
            expected
        )));
    }
    Ok(vectors)
}

fn parse_vector(value: &Value, position: usize) -> Result<Vec<f32>, EmbedError> {
    let arr = value.as_array().ok_or_else(|| {
        EmbedError(format!(
            "embedding {position} is not an array (got {})",
            json_type_name(value)
        ))
    })?;
    arr.iter()
        .map(|component| {
            component
                .as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| EmbedError(format!("embedding {position} has non-numeric component")))
        })
        .collect()
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

impl Embedder for DatabricksEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = request_body(texts, self.model.as_deref());
        let response = self
            .agent
            .post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, response) => {
                    let detail: String = response
                        .into_string()
                        .unwrap_or_default()
                        .chars()
                        .take(500)
                        .collect();
                    EmbedError(format!(
                        "Databricks endpoint {} returned HTTP {code}: {detail}",
                        self.url
                    ))
                }
                other => EmbedError(format!("request to {} failed: {other}", self.url)),
            })?;
        let value: Value = response
            .into_json()
            .map_err(|e| EmbedError(format!("response from {} is not JSON: {e}", self.url)))?;
        parse_response(&value, texts.len())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic mock backend for tests
// ─────────────────────────────────────────────────────────────────────────────

/// Deterministic embedder seeded with a `text → vector` map.
///
/// All seeded vectors are padded with one trailing `0.0` component; texts not
/// in the map embed to the unit vector on that extra axis, which is therefore
/// orthogonal (cosine similarity 0) to every seeded vector by construction.
pub struct MockEmbedder {
    vectors: HashMap<String, Vec<f32>>,
    default: Vec<f32>,
}

impl MockEmbedder {
    pub fn new(seed: HashMap<String, Vec<f32>>) -> Self {
        let dim = seed.values().map(Vec::len).max().unwrap_or(2) + 1;
        let vectors = seed
            .into_iter()
            .map(|(text, mut vector)| {
                vector.resize(dim, 0.0); // pad; last component stays 0.0
                (text, vector)
            })
            .collect();
        let mut default = vec![0.0; dim];
        default[dim - 1] = 1.0;
        Self { vectors, default }
    }
}

impl Embedder for MockEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|text| {
                self.vectors
                    .get(*text)
                    .cloned()
                    .unwrap_or_else(|| self.default.clone())
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_without_model() {
        let body = request_body(&["alpha", "beta"], None);
        assert_eq!(body, json!({ "input": ["alpha", "beta"] }));
    }

    #[test]
    fn request_body_with_model() {
        let body = request_body(&["alpha"], Some("bge-large-en"));
        assert_eq!(
            body,
            json!({ "input": ["alpha"], "model": "bge-large-en" })
        );
    }

    #[test]
    fn parse_response_data_shape() {
        let value = json!({
            "data": [
                { "embedding": [1.0, 0.0], "index": 0 },
                { "embedding": [0.0, 1.0], "index": 1 }
            ],
            "model": "bge",
            "usage": { "total_tokens": 4 }
        });
        let vectors = parse_response(&value, 2).expect("data shape parses");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parse_response_data_shape_reorders_by_index() {
        let value = json!({
            "data": [
                { "embedding": [0.0, 1.0], "index": 1 },
                { "embedding": [1.0, 0.0], "index": 0 }
            ]
        });
        let vectors = parse_response(&value, 2).expect("out-of-order data parses");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parse_response_predictions_shape() {
        let value = json!({ "predictions": [[0.5, 0.5], [0.25, 0.75]] });
        let vectors = parse_response(&value, 2).expect("predictions shape parses");
        assert_eq!(vectors, vec![vec![0.5, 0.5], vec![0.25, 0.75]]);
    }

    #[test]
    fn parse_response_rejects_unknown_shape() {
        let value = json!({ "outputs": [[1.0]] });
        let err = parse_response(&value, 1).expect_err("unknown shape must be rejected");
        assert!(
            err.0.contains("unrecognized embeddings response shape") && err.0.contains("outputs"),
            "error should name the unexpected keys: {err}"
        );
    }

    #[test]
    fn parse_response_rejects_count_mismatch() {
        let value = json!({ "predictions": [[1.0, 0.0]] });
        let err = parse_response(&value, 2).expect_err("count mismatch must be rejected");
        assert!(
            err.0.contains("1 embeddings for 2 inputs"),
            "error should state the mismatch: {err}"
        );
    }

    #[test]
    fn resolve_endpoint_url_from_name_and_host() {
        let url = resolve_endpoint_url("databricks-bge", Some("dbc-x.cloud.databricks.com"))
            .expect("name resolves");
        assert_eq!(
            url,
            "https://dbc-x.cloud.databricks.com/serving-endpoints/databricks-bge/invocations"
        );
        // host may already carry a scheme and/or trailing slash
        let url = resolve_endpoint_url("bge", Some("https://dbc-y.cloud.databricks.com/"))
            .expect("prefixed host resolves");
        assert_eq!(
            url,
            "https://dbc-y.cloud.databricks.com/serving-endpoints/bge/invocations"
        );
    }

    #[test]
    fn resolve_endpoint_url_passes_through_full_urls() {
        let url = resolve_endpoint_url("https://example.com/api/embed", None)
            .expect("full URL passes through");
        assert_eq!(url, "https://example.com/api/embed");
    }

    #[test]
    fn resolve_endpoint_url_requires_host_for_names() {
        let err = resolve_endpoint_url("databricks-bge", None).expect_err("name without host");
        assert!(
            err.0.contains("DATABRICKS_HOST"),
            "error should mention the missing variable: {err}"
        );
    }

    #[test]
    fn mock_embedder_is_deterministic_and_orthogonal_by_default() {
        let mock = MockEmbedder::new(HashMap::from([
            ("known".to_string(), vec![1.0, 0.0]),
            ("other".to_string(), vec![0.6, 0.8]),
        ]));
        let first = mock.embed(&["known", "unknown text"]).unwrap();
        let second = mock.embed(&["known", "unknown text"]).unwrap();
        assert_eq!(first, second, "mock must be deterministic");

        // seeded vectors are padded with a trailing zero
        assert_eq!(first[0], vec![1.0, 0.0, 0.0]);
        // unknown text gets the extra-axis unit vector: orthogonal to all seeds
        assert_eq!(first[1], vec![0.0, 0.0, 1.0]);
        let dot: f32 = first[0]
            .iter()
            .zip(first[1].iter())
            .map(|(a, b)| a * b)
            .sum();
        assert_eq!(dot, 0.0, "default must be orthogonal to seeded vectors");
    }
}
