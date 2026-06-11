//! delver-store: persistent document index over Postgres.
//!
//! Stage A of the DocQL full-spec work (docs/DECISIONS.md D-002/D-003/D-008):
//! `delver-core` stays pure and synchronous; this crate owns all database
//! concerns. Postgres is the source of truth and the in-memory `PdfIndex` is
//! derived data — [`hydrate_index`] rebuilds it so matching behaves exactly
//! like a fresh parse (enforced by `tests/roundtrip.rs`).

pub mod blocking;
mod error;
mod hydrate;
mod store;
mod types;

pub use error::StoreError;
pub use hydrate::{hydrate_index, hydrate_pages};
pub use store::{DelverStore, SCHEMA_VERSION};
pub use types::{
    BlobRow, CorpusId, DocumentId, ElementId, ElementKind, ElementRow, ImagePayload, IngestOutcome,
    LoadedDocument, RefEdgeRow, SearchScope, TextSearchHit,
};
