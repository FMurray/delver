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

/// Discriminant of an element row, mirroring `delver_core::parse::ContentHandle`
/// (`Aux` rows split into their `AuxKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementKind {
    Text,
    Image,
    Annotation,
    Path,
    Figure,
    Blob,
}

impl ElementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ElementKind::Text => "text",
            ElementKind::Image => "image",
            ElementKind::Annotation => "annotation",
            ElementKind::Path => "path",
            ElementKind::Figure => "figure",
            ElementKind::Blob => "blob",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self, crate::StoreError> {
        match s {
            "text" => Ok(ElementKind::Text),
            "image" => Ok(ElementKind::Image),
            "annotation" => Ok(ElementKind::Annotation),
            "path" => Ok(ElementKind::Path),
            "figure" => Ok(ElementKind::Figure),
            "blob" => Ok(ElementKind::Blob),
            other => Err(crate::StoreError::Corrupt(format!(
                "unknown element kind {other:?}"
            ))),
        }
    }

    pub(crate) fn from_aux(kind: delver_core::parse::AuxKind) -> Self {
        use delver_core::parse::AuxKind;
        match kind {
            AuxKind::Annotation => ElementKind::Annotation,
            AuxKind::Path => ElementKind::Path,
            AuxKind::Figure => ElementKind::Figure,
            AuxKind::Blob => ElementKind::Blob,
        }
    }

    pub(crate) fn as_aux(self) -> Option<delver_core::parse::AuxKind> {
        use delver_core::parse::AuxKind;
        match self {
            ElementKind::Annotation => Some(AuxKind::Annotation),
            ElementKind::Path => Some(AuxKind::Path),
            ElementKind::Figure => Some(AuxKind::Figure),
            ElementKind::Blob => Some(AuxKind::Blob),
            ElementKind::Text | ElementKind::Image => None,
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

/// Embedded-file payload stored alongside a blob element row
/// (`blobs` table, D-016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRow {
    pub data: Vec<u8>,
    pub mime: Option<String>,
    pub filename: Option<String>,
}

/// One typed edge between two elements of a document (`element_refs`,
/// D-016). Mirrors `delver_core::parse::RefEdge` with store ids.
#[derive(Debug, Clone, PartialEq)]
pub struct RefEdgeRow {
    pub from_element: ElementId,
    pub to_element: ElementId,
    pub kind: String,
    pub metadata: serde_json::Value,
}

/// Everything `load_document` returns: the document's Info-dict metadata,
/// its element rows in global order, and its ref edges (D-016).
#[derive(Debug, Clone)]
pub struct LoadedDocument {
    pub document_id: DocumentId,
    /// PDF Info dict subset captured at ingest (`documents.metadata`).
    pub metadata: serde_json::Value,
    pub elements: Vec<ElementRow>,
    pub refs: Vec<RefEdgeRow>,
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
    /// Present only for `kind == Blob` rows loaded with their payload.
    pub blob: Option<BlobRow>,
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
