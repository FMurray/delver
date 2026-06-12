//! HTTP client for the ai_parse_document flow: Files API upload → SQL
//! Statement Execution → poll → result JSON → best-effort delete (DA-006).
//!
//! Mirrors delver-embed's shape: a thin `ureq` agent plus pure, unit-tested
//! helpers for every request body / response parse. API references
//! (documented in docs/DECISIONS-aiparse.md):
//! * Files API: `PUT|DELETE /api/2.0/fs/files{volume_path}`
//! * Statement Execution: `POST /api/2.0/sql/statements`,
//!   `GET /api/2.0/sql/statements/{id}`

use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::DbxConfig;
use crate::ParseDbxError;

/// Delay between statement-status polls.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Give up polling after this long (ai_parse on dense documents is slow; the
/// docs warn about latency, so the budget is generous).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(600);
/// Per-request HTTP timeout (uploads of ~100 MB documents need headroom).
const HTTP_TIMEOUT: Duration = Duration::from_secs(300);

pub struct DbxParseClient {
    config: DbxConfig,
    agent: ureq::Agent,
}

impl DbxParseClient {
    pub fn new(config: DbxConfig) -> Self {
        Self {
            config,
            agent: ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build(),
        }
    }

    /// Run `ai_parse_document` over `bytes`: upload to the configured UC
    /// volume under a unique temp name, execute + poll the statement, parse
    /// the result JSON, and delete the temp file (best effort — a leaked temp
    /// file is reported on stderr but never fails the parse).
    pub fn parse_document_bytes(
        &self,
        bytes: &[u8],
        file_name: &str,
    ) -> Result<Value, ParseDbxError> {
        let remote_path = temp_volume_path(&self.config.volume, file_name);
        self.upload(&remote_path, bytes)?;
        let result = self.execute_and_fetch(&remote_path);
        if let Err(e) = self.delete(&remote_path) {
            eprintln!("warning: failed to delete temp upload {remote_path}: {e}");
        }
        result
    }

    fn execute_and_fetch(&self, remote_path: &str) -> Result<Value, ParseDbxError> {
        let statement_id = self.execute_statement(remote_path)?;
        let response = self.poll_until_terminal(&statement_id)?;
        extract_parsed_json(&response)
    }

    /// PUT the bytes to the Files API.
    fn upload(&self, remote_path: &str, bytes: &[u8]) -> Result<(), ParseDbxError> {
        let url = files_api_url(&self.config.host, remote_path);
        self.agent
            .put(&url)
            .query("overwrite", "true")
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .set("Content-Type", "application/octet-stream")
            .send_bytes(bytes)
            .map_err(|e| http_error("uploading to", &url, e))?;
        Ok(())
    }

    /// DELETE the temp file from the Files API.
    fn delete(&self, remote_path: &str) -> Result<(), ParseDbxError> {
        let url = files_api_url(&self.config.host, remote_path);
        self.agent
            .delete(&url)
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .call()
            .map_err(|e| http_error("deleting", &url, e))?;
        Ok(())
    }

    /// POST the ai_parse_document statement; returns the statement id.
    fn execute_statement(&self, remote_path: &str) -> Result<String, ParseDbxError> {
        let url = format!("{}/api/2.0/sql/statements", self.config.host);
        let body = statement_body(&self.config.warehouse_id, remote_path)?;
        let response: Value = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .send_json(body)
            .map_err(|e| http_error("executing statement at", &url, e))?
            .into_json()
            .map_err(|e| ParseDbxError(format!("response from {url} is not JSON: {e}")))?;
        statement_id(&response)
    }

    /// GET the statement status until SUCCEEDED (returning the full
    /// response) or a terminal failure / the poll budget (fail-loud).
    fn poll_until_terminal(&self, statement_id: &str) -> Result<Value, ParseDbxError> {
        let url = format!(
            "{}/api/2.0/sql/statements/{statement_id}",
            self.config.host
        );
        let started = std::time::Instant::now();
        loop {
            let response: Value = self
                .agent
                .get(&url)
                .set("Authorization", &format!("Bearer {}", self.config.token))
                .call()
                .map_err(|e| http_error("polling", &url, e))?
                .into_json()
                .map_err(|e| ParseDbxError(format!("response from {url} is not JSON: {e}")))?;

            match statement_state(&response)? {
                StatementState::Succeeded => return Ok(response),
                StatementState::Running => {
                    if started.elapsed() > POLL_TIMEOUT {
                        return Err(ParseDbxError(format!(
                            "statement {statement_id} still running after \
                             {}s; giving up (the document may be very dense — \
                             see ai_parse_document latency notes)",
                            POLL_TIMEOUT.as_secs()
                        )));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                StatementState::Failed(message) => {
                    return Err(ParseDbxError(format!(
                        "statement {statement_id} failed: {message}"
                    )))
                }
            }
        }
    }
}

// ─────────────────────────── pure helpers (unit-tested) ───────────────────────────

/// Unique temp path under the volume; the file name is sanitized to a safe
/// charset so the path can be embedded as a SQL string literal.
pub fn temp_volume_path(volume: &str, file_name: &str) -> String {
    let safe: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "document.pdf".to_string()
    } else {
        safe
    };
    format!(
        "{}/delver-tmp-{}-{safe}",
        volume.trim_end_matches('/'),
        Uuid::new_v4()
    )
}

/// Files API URL for a volume path (`PUT`/`DELETE /api/2.0/fs/files{path}`).
pub fn files_api_url(host: &str, remote_path: &str) -> String {
    format!(
        "{}/api/2.0/fs/files{}",
        host.trim_end_matches('/'),
        remote_path
    )
}

/// Statement Execution request body running ai_parse_document (output schema
/// pinned to 2.0) over one uploaded file. The volume path is embedded as a
/// SQL string literal: `READ_FILES` takes a constant path (parameter markers
/// are not supported in table-valued function arguments), and the path is
/// fully server-generated from a UUID plus a sanitized file name, so the
/// literal is injection-free by construction (defense in depth: quotes are
/// rejected here).
pub fn statement_body(warehouse_id: &str, remote_path: &str) -> Result<Value, ParseDbxError> {
    if remote_path.contains('\'') || remote_path.contains('\\') {
        return Err(ParseDbxError(format!(
            "volume path {remote_path:?} contains characters that cannot be \
             embedded in a SQL literal"
        )));
    }
    let statement = format!(
        "SELECT to_json(ai_parse_document(content, map('version', '2.0'))) AS parsed \
         FROM READ_FILES('{remote_path}', format => 'binaryFile')"
    );
    Ok(json!({
        "warehouse_id": warehouse_id,
        "statement": statement,
        "wait_timeout": "30s",
        "on_wait_timeout": "CONTINUE",
        "format": "JSON_ARRAY",
        "disposition": "INLINE",
    }))
}

/// Statement id from an execute response.
pub fn statement_id(response: &Value) -> Result<String, ParseDbxError> {
    response
        .get("statement_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ParseDbxError(format!(
                "statement execution response has no statement_id (keys: [{}])",
                object_keys(response)
            ))
        })
}

/// Simplified statement lifecycle for polling decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementState {
    Running,
    Succeeded,
    Failed(String),
}

/// Map `status.state` (+ `status.error.message`) onto [`StatementState`].
/// Unknown states are an error — never an infinite poll loop (D-006).
pub fn statement_state(response: &Value) -> Result<StatementState, ParseDbxError> {
    let state = response
        .pointer("/status/state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ParseDbxError(format!(
                "statement response has no status.state (keys: [{}])",
                object_keys(response)
            ))
        })?;
    match state {
        "PENDING" | "RUNNING" => Ok(StatementState::Running),
        "SUCCEEDED" => Ok(StatementState::Succeeded),
        "FAILED" | "CANCELED" | "CLOSED" => {
            let message = response
                .pointer("/status/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no error message provided");
            Ok(StatementState::Failed(format!("{state}: {message}")))
        }
        other => Err(ParseDbxError(format!(
            "statement entered unknown state {other:?}"
        ))),
    }
}

/// Pull the single ai_parse_document JSON document out of a SUCCEEDED
/// statement response (`result.data_array[0][0]` is the `to_json` string).
pub fn extract_parsed_json(response: &Value) -> Result<Value, ParseDbxError> {
    if response
        .pointer("/manifest/truncated")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err(ParseDbxError(
            "statement result is truncated (INLINE disposition limit); the \
             parsed document is too large to fetch inline"
                .to_string(),
        ));
    }
    let cell = response
        .pointer("/result/data_array/0/0")
        .ok_or_else(|| {
            ParseDbxError(
                "statement succeeded but returned no rows \
                 (expected result.data_array[0][0] to hold the parsed JSON)"
                    .to_string(),
            )
        })?;
    let text = cell.as_str().ok_or_else(|| {
        ParseDbxError(format!(
            "expected the parsed JSON as a string cell, got {cell}"
        ))
    })?;
    serde_json::from_str(text)
        .map_err(|e| ParseDbxError(format!("ai_parse_document output is not valid JSON: {e}")))
}

fn object_keys(value: &Value) -> String {
    match value.as_object() {
        Some(map) => map.keys().cloned().collect::<Vec<_>>().join(", "),
        None => "non-object".to_string(),
    }
}

fn http_error(action: &str, url: &str, e: ureq::Error) -> ParseDbxError {
    match e {
        ureq::Error::Status(code, response) => {
            let detail: String = response
                .into_string()
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            ParseDbxError(format!("{action} {url} returned HTTP {code}: {detail}"))
        }
        other => ParseDbxError(format!("{action} {url} failed: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_path_is_unique_and_sanitized() {
        let a = temp_volume_path("/Volumes/m/d/v/", "10-K (final).pdf");
        let b = temp_volume_path("/Volumes/m/d/v/", "10-K (final).pdf");
        assert_ne!(a, b, "temp names must be unique");
        assert!(a.starts_with("/Volumes/m/d/v/delver-tmp-"), "got {a}");
        assert!(a.ends_with("10-K__final_.pdf"), "got {a}");
        assert!(!a.contains(' ') && !a.contains('(') && !a.contains('\''));
    }

    #[test]
    fn files_api_url_shape() {
        assert_eq!(
            files_api_url(
                "https://dbc-x.cloud.databricks.com",
                "/Volumes/m/d/v/f.pdf"
            ),
            "https://dbc-x.cloud.databricks.com/api/2.0/fs/files/Volumes/m/d/v/f.pdf"
        );
    }

    #[test]
    fn statement_body_shape() {
        let body = statement_body("wh123", "/Volumes/m/d/v/delver-tmp-1-f.pdf").unwrap();
        assert_eq!(body["warehouse_id"], "wh123");
        assert_eq!(body["format"], "JSON_ARRAY");
        assert_eq!(body["disposition"], "INLINE");
        let statement = body["statement"].as_str().unwrap();
        assert!(
            statement.contains("ai_parse_document(content, map('version', '2.0'))"),
            "statement must pin schema 2.0: {statement}"
        );
        assert!(
            statement
                .contains("READ_FILES('/Volumes/m/d/v/delver-tmp-1-f.pdf', format => 'binaryFile')"),
            "statement must read the uploaded file: {statement}"
        );
        assert!(statement.contains("to_json"), "VARIANT must be serialized");
    }

    #[test]
    fn statement_body_rejects_unembeddable_paths() {
        let err = statement_body("w", "/Volumes/m/d/v/it's.pdf").expect_err("quote rejected");
        assert!(err.0.contains("SQL literal"), "got: {err}");
    }

    #[test]
    fn statement_id_parses_and_fails_loud() {
        let ok = serde_json::json!({"statement_id": "abc", "status": {"state": "PENDING"}});
        assert_eq!(statement_id(&ok).unwrap(), "abc");
        let err = statement_id(&serde_json::json!({"oops": 1})).expect_err("missing id");
        assert!(err.0.contains("statement_id") && err.0.contains("oops"));
    }

    #[test]
    fn statement_states_map_to_poll_decisions() {
        let state = |s: &str| serde_json::json!({"status": {"state": s}});
        assert_eq!(statement_state(&state("PENDING")).unwrap(), StatementState::Running);
        assert_eq!(statement_state(&state("RUNNING")).unwrap(), StatementState::Running);
        assert_eq!(
            statement_state(&state("SUCCEEDED")).unwrap(),
            StatementState::Succeeded
        );
        let failed = serde_json::json!({
            "status": {"state": "FAILED", "error": {"message": "TABLE_OR_VIEW_NOT_FOUND"}}
        });
        match statement_state(&failed).unwrap() {
            StatementState::Failed(message) => {
                assert!(message.contains("FAILED") && message.contains("TABLE_OR_VIEW_NOT_FOUND"))
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let err = statement_state(&state("SOMETHING_NEW")).expect_err("unknown state");
        assert!(err.0.contains("SOMETHING_NEW"));
    }

    #[test]
    fn extract_parsed_json_happy_path() {
        let inner = r#"{"document": {"elements": []}, "metadata": {"version": "2.0"}}"#;
        let response = serde_json::json!({
            "status": {"state": "SUCCEEDED"},
            "manifest": {"truncated": false, "total_row_count": 1},
            "result": {"data_array": [[inner]]}
        });
        let parsed = extract_parsed_json(&response).unwrap();
        assert_eq!(parsed["metadata"]["version"], "2.0");
    }

    #[test]
    fn extract_parsed_json_fails_on_truncation_and_empty() {
        let truncated = serde_json::json!({
            "manifest": {"truncated": true},
            "result": {"data_array": [["{}"]]}
        });
        assert!(extract_parsed_json(&truncated)
            .expect_err("truncated must fail")
            .0
            .contains("truncated"));

        let empty = serde_json::json!({"result": {"data_array": []}});
        assert!(extract_parsed_json(&empty)
            .expect_err("no rows must fail")
            .0
            .contains("no rows"));

        let not_json = serde_json::json!({"result": {"data_array": [["{nope"]]}});
        assert!(extract_parsed_json(&not_json)
            .expect_err("bad JSON must fail")
            .0
            .contains("not valid JSON"));
    }
}
