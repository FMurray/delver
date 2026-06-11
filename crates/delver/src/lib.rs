//! delver: CLI and Python bindings over `delver-core` (parse + match) and
//! `delver-store` (persistent Postgres index).
//!
//! The functions in this module are the shared service layer for both the
//! `delver` binary and the PyO3 facade, so the two surfaces emit identical
//! JSON shapes (D-012). Keep both shells thin: routing/IO here, business
//! logic in `delver-core` / `delver-store`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use delver_core::embed::Embedder;
use delver_core::layout::MatchContext;
use delver_core::process_parsed;
use delver_embed::DatabricksEmbedder;
use delver_store::blocking::DelverStoreBlocking;
use delver_store::{DocumentId, IngestOutcome, SearchScope, TextSearchHit};
use tokenizers::Tokenizer;

/// Local dev database (docs/DECISIONS.md D-002).
pub const DEFAULT_DB_URL: &str = "postgres://delver:delver@localhost:5433/delver";

/// Maximum characters of element text echoed in a search hit `snippet`.
const SNIPPET_MAX_CHARS: usize = 200;

/// Resolve the database URL: explicit flag > `DATABASE_URL` env > local dev
/// default (D-012).
pub fn resolve_db_url(flag: Option<&str>) -> String {
    flag.map(str::to_string)
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_DB_URL.to_string())
}

/// Connect to the store at the resolved URL (runs migrations).
pub fn connect_store(db_flag: Option<&str>) -> Result<DelverStoreBlocking> {
    let url = resolve_db_url(db_flag);
    DelverStoreBlocking::connect(&url)
        .with_context(|| format!("connecting to Postgres at {url}"))
}

/// Build the embedding backend for `EmbeddingSim(...)` matches (D-014).
/// Endpoint precedence mirrors `resolve_db_url`: explicit flag >
/// `DELVER_EMBED_ENDPOINT` env. `Ok(None)` when neither is set — templates
/// that need embeddings then fail loud at match time (D-006).
pub fn build_embedder(flag: Option<&str>) -> Result<Option<Arc<dyn Embedder>>> {
    let endpoint = flag
        .map(str::to_string)
        .or_else(|| std::env::var("DELVER_EMBED_ENDPOINT").ok());
    match endpoint {
        None => Ok(None),
        Some(endpoint) => {
            let embedder = DatabricksEmbedder::new(&endpoint)
                .with_context(|| format!("configuring embedding endpoint {endpoint:?}"))?;
            Ok(Some(Arc::new(embedder) as Arc<dyn Embedder>))
        }
    }
}

/// Load a tokenizer by Hugging Face model id. `"none"` disables token-based
/// chunking explicitly; a model that cannot be fetched degrades to `None`
/// (character-based chunking) with a warning on stderr.
pub fn load_tokenizer(model: &str) -> Option<Tokenizer> {
    if model.eq_ignore_ascii_case("none") {
        return None;
    }
    let tokenizer = Tokenizer::from_pretrained(model, None).ok();
    if tokenizer.is_none() {
        eprintln!(
            "warning: tokenizer {model:?} unavailable (network/auth?); \
             falling back to character-based chunking"
        );
    }
    tokenizer
}

/// Parse one `key=value` CLI argument (`--partition` / `--where`), splitting
/// at the first `=`. Empty key or value is an error (D-006).
pub fn parse_key_value(arg: &str) -> Result<(String, String)> {
    let Some((key, value)) = arg.split_once('=') else {
        bail!("expected key=value, got {arg:?}");
    };
    if key.is_empty() || value.is_empty() {
        bail!("expected non-empty key and value in key=value, got {arg:?}");
    }
    Ok((key.to_string(), value.to_string()))
}

/// Infer hive-style partitions from the *directory* components of `path`
/// (Stage C, D-023): `/loans/state=CA/type=Auto/loan1.pdf` yields
/// `[("state","CA"), ("type","Auto")]`. The file name itself never counts;
/// segments without `=` (or with an empty key/value) are skipped.
pub fn infer_partitions_from_path(path: &Path) -> Vec<(String, String)> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    parent
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(seg) => seg.to_str(),
            _ => None,
        })
        .filter_map(|seg| {
            let (key, value) = seg.split_once('=')?;
            (!key.is_empty() && !value.is_empty())
                .then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// Key/value pairs as the JSON object stored under
/// `documents.metadata.partitions` (later duplicate keys win).
pub fn partitions_json(pairs: &[(String, String)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    serde_json::Value::Object(map)
}

/// Read a PDF from disk and ingest it into `corpus` (created if absent).
/// `partitions` (inferred-from-path first, explicit `--partition` flags
/// after, so explicit wins on key conflicts) are stored under
/// `documents.metadata.partitions` — also on idempotent re-ingest, so
/// existing documents can be (re)tagged (D-023).
///
/// Returns the shared ingest JSON shape:
/// `{"document_id", "created", "element_count", "corpus", "partitions"}`.
pub fn ingest_file(
    store: &DelverStoreBlocking,
    path: &Path,
    corpus: &str,
    uri: Option<&str>,
    parse_version: i32,
    partitions: &[(String, String)],
) -> Result<serde_json::Value> {
    let corpus_id = store.ensure_corpus(corpus)?;
    let pdf_bytes =
        std::fs::read(path).with_context(|| format!("reading PDF {}", path.display()))?;
    let outcome: IngestOutcome =
        store.ingest_document(corpus_id, uri, &pdf_bytes, parse_version)?;
    let partitions = partitions_json(partitions);
    if !partitions.as_object().map_or(true, |m| m.is_empty()) {
        store.set_document_partitions(outcome.document_id, &partitions)?;
    }
    let element_count = store.element_count(outcome.document_id)?;
    Ok(serde_json::json!({
        "document_id": outcome.document_id,
        "created": outcome.created,
        "element_count": element_count,
        "corpus": corpus,
        "partitions": partitions,
    }))
}

/// Full-text search over a corpus (or one document when `doc` is given).
/// `partitions` (`--where key=value`, D-023) restricts corpus scope to
/// documents whose stored partitions contain every pair; it cannot be
/// combined with `doc` (a single document either matches or the search is
/// pointless — fail loud).
///
/// Returns the shared search JSON shape: an array of
/// `{"element_id", "document_id", "page", "rank", "snippet"}` ranked by
/// `ts_rank` descending.
pub fn search_store(
    store: &DelverStoreBlocking,
    query: &str,
    corpus: &str,
    doc: Option<DocumentId>,
    limit: i64,
    partitions: &[(String, String)],
) -> Result<serde_json::Value> {
    let hits = if partitions.is_empty() {
        let scope = match doc {
            Some(doc) => SearchScope::Document(doc),
            None => SearchScope::Corpus(store.ensure_corpus(corpus)?),
        };
        store.text_search(scope, query, limit)?
    } else {
        if doc.is_some() {
            bail!("--where filters corpus documents and cannot be combined with --doc");
        }
        let corpus_id = store.ensure_corpus(corpus)?;
        store.text_search_filtered(corpus_id, query, limit, Some(&partitions_json(partitions)))?
    };
    Ok(search_hits_json(&hits))
}

/// Execute a DocQL template across every document of `corpus` that matches
/// the `--where` partition filter (all documents when empty), Stage C D-023.
///
/// Returns a JSON object keyed by document id (ascending), each value the
/// document's outputs array — exactly what `run_template_on_doc` would
/// return for that document. An empty object means no document matched the
/// filter (a data condition, not an error).
pub fn run_template_on_corpus(
    store: &DelverStoreBlocking,
    corpus: &str,
    partitions: &[(String, String)],
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
    embedder: Option<Arc<dyn Embedder>>,
) -> Result<String> {
    let corpus_id = store.ensure_corpus(corpus)?;
    let filter = (!partitions.is_empty()).then(|| partitions_json(partitions));
    let docs = store.documents_matching(corpus_id, filter.as_ref())?;
    let mut by_doc = serde_json::Map::new();
    for doc in docs {
        let outputs = run_template_on_doc(store, doc, template_str, tokenizer, embedder.clone())
            .with_context(|| format!("running template on document {doc}"))?;
        by_doc.insert(doc.to_string(), serde_json::from_str(&outputs)?);
    }
    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(
        by_doc,
    ))?)
}

/// Hydrate a stored document and execute a DocQL template over it.
///
/// Runs the exact fresh-parse pipeline via `delver_core::process_parsed`
/// (D-012); named destinations are not persisted yet, so the hydrated path
/// uses `MatchContext::default()`. Returns the outputs JSON (same payload as
/// `process_pdf`).
pub fn run_template_on_doc(
    store: &DelverStoreBlocking,
    doc: DocumentId,
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
    embedder: Option<Arc<dyn Embedder>>,
) -> Result<String> {
    let loaded = store.load_document(doc)?;
    if loaded.elements.is_empty() {
        bail!("document {doc} has no stored elements (unknown id or empty document)");
    }
    let pages = delver_store::hydrate_pages(&loaded.elements);
    let mut match_context = MatchContext::default();
    match_context.embedder = embedder.into();
    process_parsed(&pages, &match_context, template_str, tokenizer)
}

fn search_hits_json(hits: &[TextSearchHit]) -> serde_json::Value {
    let hits: Vec<serde_json::Value> = hits
        .iter()
        .map(|hit| {
            serde_json::json!({
                "element_id": hit.element_id,
                "document_id": hit.document_id,
                "page": hit.page,
                "rank": hit.rank,
                "snippet": snippet(&hit.text),
            })
        })
        .collect();
    serde_json::Value::Array(hits)
}

fn snippet(text: &str) -> String {
    let mut snippet: String = text.chars().take(SNIPPET_MAX_CHARS).collect();
    if snippet.len() < text.len() {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn infers_partitions_from_directory_components_only() {
        assert_eq!(
            infer_partitions_from_path(Path::new("/loans/state=CA/type=Auto/loan1.pdf")),
            pairs(&[("state", "CA"), ("type", "Auto")])
        );
        // The file name never contributes, even when it contains '='.
        assert_eq!(
            infer_partitions_from_path(Path::new("/data/state=NY/k=v.pdf")),
            pairs(&[("state", "NY")])
        );
        // Non key=value segments and empty keys/values are skipped.
        assert_eq!(
            infer_partitions_from_path(Path::new("plain/dir/=x/y=/file.pdf")),
            Vec::<(String, String)>::new()
        );
        // Relative paths work; a bare file name has no parent components.
        assert_eq!(
            infer_partitions_from_path(Path::new("year=2015/doc.pdf")),
            pairs(&[("year", "2015")])
        );
        assert_eq!(infer_partitions_from_path(Path::new("doc.pdf")), Vec::new());
    }

    #[test]
    fn parses_key_value_arguments() {
        assert_eq!(
            parse_key_value("state=CA").unwrap(),
            ("state".to_string(), "CA".to_string())
        );
        // Split happens at the FIRST '='; the value may contain '='.
        assert_eq!(
            parse_key_value("expr=a=b").unwrap(),
            ("expr".to_string(), "a=b".to_string())
        );
        assert!(parse_key_value("noequals").is_err());
        assert!(parse_key_value("=v").is_err());
        assert!(parse_key_value("k=").is_err());
    }

    #[test]
    fn partitions_json_last_duplicate_wins() {
        let value = partitions_json(&pairs(&[("state", "CA"), ("state", "NY")]));
        assert_eq!(value, serde_json::json!({"state": "NY"}));
    }
}

#[cfg(feature = "extension-module")]
mod python {
    use pyo3::prelude::*;

    use delver_store::DocumentId;
    use tokenizers::Tokenizer;

    /// Process a PDF file using a template and return extracted data as JSON
    #[pyfunction]
    fn process_pdf_file(pdf_path: String, template_path: String) -> PyResult<String> {
        let pdf_bytes = std::fs::read(pdf_path)?;
        let template_str = std::fs::read_to_string(template_path)?;
        let tokenizer = Tokenizer::from_pretrained("Qwen/Qwen2-7B-Instruct", None).unwrap();

        let (json, _blocks, _doc) =
            delver_core::process_pdf(&pdf_bytes, &template_str, Some(&tokenizer), None)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

        Ok(json)
    }

    /// Persistent document index backed by Postgres (thin wrapper over
    /// `delver_store::blocking::DelverStoreBlocking`; JSON shapes identical
    /// to the `delver index/search/query` CLI, see D-012).
    #[pyclass(name = "DelverStore")]
    struct PyDelverStore {
        store: delver_store::blocking::DelverStoreBlocking,
    }

    #[pymethods]
    impl PyDelverStore {
        /// Connect (and run migrations). `db_url` falls back to the
        /// `DATABASE_URL` env var, then the local dev database.
        #[new]
        #[pyo3(signature = (db_url=None))]
        fn new(db_url: Option<String>) -> PyResult<Self> {
            let store = crate::connect_store(db_url.as_deref()).map_err(to_py_err)?;
            Ok(Self { store })
        }

        /// Ingest a PDF file into `corpus`. Returns JSON:
        /// `{"document_id", "created", "element_count", "corpus"}`.
        #[pyo3(signature = (path, corpus, uri=None, parse_version=None))]
        fn ingest(
            &self,
            path: String,
            corpus: String,
            uri: Option<String>,
            parse_version: Option<i32>,
        ) -> PyResult<String> {
            let value = crate::ingest_file(
                &self.store,
                std::path::Path::new(&path),
                &corpus,
                uri.as_deref(),
                parse_version.unwrap_or(1),
                &[],
            )
            .map_err(to_py_err)?;
            Ok(value.to_string())
        }

        /// Full-text search over a corpus. Returns a JSON array of
        /// `{"element_id", "document_id", "page", "rank", "snippet"}`.
        #[pyo3(signature = (query, corpus, limit=None))]
        fn search(&self, query: String, corpus: String, limit: Option<usize>) -> PyResult<String> {
            let value = crate::search_store(
                &self.store,
                &query,
                &corpus,
                None,
                limit.unwrap_or(10) as i64,
                &[],
            )
            .map_err(to_py_err)?;
            Ok(value.to_string())
        }

        /// Execute DocQL template source over a stored document (hydrated
        /// from Postgres). Returns the outputs JSON (same payload as
        /// `process_pdf_file`). `tokenizer_model` of `None` or `"none"`
        /// uses character-based chunking. `embed_endpoint` (name or full
        /// URL; falls back to $DELVER_EMBED_ENDPOINT) enables
        /// EmbeddingSim(...) matches via Databricks serving (D-014).
        #[pyo3(signature = (doc_id, template, tokenizer_model=None, embed_endpoint=None))]
        fn run_template(
            &self,
            doc_id: String,
            template: String,
            tokenizer_model: Option<String>,
            embed_endpoint: Option<String>,
        ) -> PyResult<String> {
            let doc = uuid::Uuid::parse_str(&doc_id)
                .map(DocumentId)
                .map_err(|e| {
                    PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                        "invalid document id {doc_id:?}: {e}"
                    ))
                })?;
            let tokenizer = tokenizer_model
                .as_deref()
                .and_then(crate::load_tokenizer);
            let embedder = crate::build_embedder(embed_endpoint.as_deref()).map_err(to_py_err)?;
            crate::run_template_on_doc(&self.store, doc, &template, tokenizer.as_ref(), embedder)
                .map_err(to_py_err)
        }
    }

    fn to_py_err(e: anyhow::Error) -> PyErr {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("{e:#}"))
    }

    /// A Python module implemented in Rust
    #[pymodule(name = "delver_pdf")]
    fn delver_pdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(process_pdf_file, m)?)?;
        m.add_class::<PyDelverStore>()?;
        Ok(())
    }
}
