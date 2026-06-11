//! Embedding abstraction for `EmbeddingSim(...)` match execution (D-005, D-014).
//!
//! `delver-core` stays free of HTTP/model dependencies: this module defines
//! only the `Embedder` trait plus the plumbing needed to thread an
//! implementation through the match pipeline (`MatchContext.embedder` →
//! `PdfIndex` → matcher). Concrete backends (Databricks serving endpoints,
//! deterministic mocks) live in the `delver-embed` crate.

use std::fmt;
use std::sync::Arc;

/// Error from an embedding backend.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedError(pub String);

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "embedding error: {}", self.0)
    }
}

impl std::error::Error for EmbedError {}

/// Text embedding backend used to execute `EmbeddingSim(...)` matches.
///
/// Implementations must return exactly one vector per input text, in input
/// order. All vectors produced by one backend must share a dimension.
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Newtype over `Option<Arc<dyn Embedder>>` so the structs that carry an
/// embedder (`MatchContext`, `PdfIndex`) can keep deriving
/// `Debug`/`Default` — trait objects have no `Debug`.
#[derive(Clone, Default)]
pub struct SharedEmbedder(Option<Arc<dyn Embedder>>);

impl SharedEmbedder {
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        Self(Some(embedder))
    }

    pub fn get(&self) -> Option<&dyn Embedder> {
        self.0.as_deref()
    }

    pub fn is_configured(&self) -> bool {
        self.0.is_some()
    }
}

impl From<Option<Arc<dyn Embedder>>> for SharedEmbedder {
    fn from(value: Option<Arc<dyn Embedder>>) -> Self {
        Self(value)
    }
}

impl From<Arc<dyn Embedder>> for SharedEmbedder {
    fn from(value: Arc<dyn Embedder>) -> Self {
        Self(Some(value))
    }
}

impl fmt::Debug for SharedEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(_) => f.write_str("SharedEmbedder(Some(..))"),
            None => f.write_str("SharedEmbedder(None)"),
        }
    }
}

// `dyn Embedder` is `Send + Sync`, so a panic during `embed` cannot expose
// more broken state through this shared handle than through any other
// `Arc<dyn ..>`. Asserting unwind safety keeps structs that carry an embedder
// (`MatchContext`, `PdfIndex`) usable inside `catch_unwind` (tests rely on
// this).
impl std::panic::UnwindSafe for SharedEmbedder {}
impl std::panic::RefUnwindSafe for SharedEmbedder {}
