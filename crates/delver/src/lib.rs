//! delver: CLI and Python bindings over `delver-core` (parse + match) and
//! `delver-store` (persistent Postgres index).
//!
//! The functions in this module are the shared service layer for both the
//! `delver` binary and the PyO3 facade, so the two surfaces emit identical
//! JSON shapes (D-012). Keep both shells thin: routing/IO here, business
//! logic in `delver-core` / `delver-store`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use delver_core::layout::MatchContext;
use delver_core::process_parsed;
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

/// Read a PDF from disk and ingest it into `corpus` (created if absent).
///
/// Returns the shared ingest JSON shape:
/// `{"document_id", "created", "element_count", "corpus"}`.
pub fn ingest_file(
    store: &DelverStoreBlocking,
    path: &Path,
    corpus: &str,
    uri: Option<&str>,
    parse_version: i32,
) -> Result<serde_json::Value> {
    let corpus_id = store.ensure_corpus(corpus)?;
    let pdf_bytes =
        std::fs::read(path).with_context(|| format!("reading PDF {}", path.display()))?;
    let outcome: IngestOutcome =
        store.ingest_document(corpus_id, uri, &pdf_bytes, parse_version)?;
    let element_count = store.element_count(outcome.document_id)?;
    Ok(serde_json::json!({
        "document_id": outcome.document_id,
        "created": outcome.created,
        "element_count": element_count,
        "corpus": corpus,
    }))
}

/// Full-text search over a corpus (or one document when `doc` is given).
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
) -> Result<serde_json::Value> {
    let scope = match doc {
        Some(doc) => SearchScope::Document(doc),
        None => SearchScope::Corpus(store.ensure_corpus(corpus)?),
    };
    let hits = store.text_search(scope, query, limit)?;
    Ok(search_hits_json(&hits))
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
) -> Result<String> {
    let rows = store.load_document(doc)?;
    if rows.is_empty() {
        bail!("document {doc} has no stored elements (unknown id or empty document)");
    }
    let pages = delver_store::hydrate_pages(&rows);
    process_parsed(&pages, &MatchContext::default(), template_str, tokenizer)
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
            delver_core::process_pdf(&pdf_bytes, &template_str, Some(&tokenizer))
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
            )
            .map_err(to_py_err)?;
            Ok(value.to_string())
        }

        /// Execute DocQL template source over a stored document (hydrated
        /// from Postgres). Returns the outputs JSON (same payload as
        /// `process_pdf_file`). `tokenizer_model` of `None` or `"none"`
        /// uses character-based chunking.
        #[pyo3(signature = (doc_id, template, tokenizer_model=None))]
        fn run_template(
            &self,
            doc_id: String,
            template: String,
            tokenizer_model: Option<String>,
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
            crate::run_template_on_doc(&self.store, doc, &template, tokenizer.as_ref())
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
