//! Typed identifiers and row types for the persistent index.
//!
//! Ids are zero-cost newtypes over `Uuid` (`#[sqlx(transparent)]`) so they can
//! be bound/decoded directly while keeping corpus/document/element ids
//! unmixable at compile time.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            Serialize, Deserialize, sqlx::Type,
        )]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_newtype!(
    /// Identifier of a corpus row.
    CorpusId
);
uuid_newtype!(
    /// Identifier of a document row (one parse of one content hash).
    DocumentId
);
uuid_newtype!(
    /// Identifier of an element row; equals the parse-time element `Uuid`.
    ElementId
);

/// Discriminant of an element row, mirroring `delver_core::parse::ContentHandle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementKind {
    Text,
    Image,
}

impl ElementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ElementKind::Text => "text",
            ElementKind::Image => "image",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self, crate::StoreError> {
        match s {
            "text" => Ok(ElementKind::Text),
            "image" => Ok(ElementKind::Image),
            other => Err(crate::StoreError::Corrupt(format!(
                "unknown element kind {other:?}"
            ))),
        }
    }
}

impl fmt::Display for ElementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of an ingest call (D-008: idempotent per (corpus, sha256, parse_version)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub document_id: DocumentId,
    /// `true` if this call inserted the document; `false` if an identical
    /// (corpus, content hash, parse_version) document already existed.
    pub created: bool,
}

/// Image payload stored alongside an image element row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePayload {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub data: Vec<u8>,
}

/// One stored element row, sufficient to rebuild the in-memory index.
#[derive(Debug, Clone)]
pub struct ElementRow {
    pub id: ElementId,
    pub document_id: DocumentId,
    pub page: i32,
    pub kind: ElementKind,
    /// Global document-order index (PdfIndex sequence position).
    pub order_idx: i32,
    pub text: Option<String>,
    pub font_size: Option<f32>,
    pub font_name: Option<String>,
    /// Packed style signature captured at ingest. Font ids inside the key are
    /// process-local (interner), so treat this as informational only;
    /// hydration recomputes style state from the element fields.
    pub style_key: Option<i64>,
    /// (x0, y0, x1, y1) in top-left page coordinates.
    pub bbox: Option<(f32, f32, f32, f32)>,
    pub metadata: serde_json::Value,
    /// Present only for `kind == Image` rows loaded with their payload.
    pub image: Option<ImagePayload>,
}

/// Scope selector for [`crate::DelverStore::text_search`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Corpus(CorpusId),
    Document(DocumentId),
}

impl From<CorpusId> for SearchScope {
    fn from(id: CorpusId) -> Self {
        SearchScope::Corpus(id)
    }
}

impl From<DocumentId> for SearchScope {
    fn from(id: DocumentId) -> Self {
        SearchScope::Document(id)
    }
}

/// One full-text search hit, ranked by `ts_rank`.
#[derive(Debug, Clone)]
pub struct TextSearchHit {
    pub element_id: ElementId,
    pub document_id: DocumentId,
    pub page: i32,
    pub order_idx: i32,
    pub text: String,
    pub rank: f32,
}
