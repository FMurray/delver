//! Synchronous facade over [`DelverStore`] for CLI / Python callers (D-003).
//!
//! Owns a private current-thread tokio runtime and forwards every call with
//! `block_on`. Do not use from inside an async context.

use delver_core::parse::ParsedDocument;
use tokio::runtime::{Builder, Runtime};

use crate::error::StoreError;
use crate::store::DelverStore;
use crate::types::{
    CorpusId, DocumentId, ElementRow, IngestOutcome, LoadedDocument, SearchScope, TextSearchHit,
};

#[derive(Debug)]
pub struct DelverStoreBlocking {
    runtime: Runtime,
    store: DelverStore,
}

impl DelverStoreBlocking {
    /// Connect, run migrations, and record index metadata (blocking).
    pub fn connect(url: &str) -> Result<Self, StoreError> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let store = runtime.block_on(DelverStore::connect(url))?;
        Ok(Self { runtime, store })
    }

    /// Access the async store (e.g. to share the pool with async code).
    pub fn inner(&self) -> &DelverStore {
        &self.store
    }

    pub fn ensure_corpus(&self, name: &str) -> Result<CorpusId, StoreError> {
        self.runtime.block_on(self.store.ensure_corpus(name))
    }

    pub fn ingest_document(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        self.runtime.block_on(
            self.store
                .ingest_document(corpus, uri, pdf_bytes, parse_version),
        )
    }

    pub fn ingest_parsed(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        parsed: &ParsedDocument,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        self.runtime.block_on(self.store.ingest_parsed(
            corpus,
            uri,
            pdf_bytes,
            parsed,
            parse_version,
        ))
    }

    pub fn element_count(&self, doc: DocumentId) -> Result<i64, StoreError> {
        self.runtime.block_on(self.store.element_count(doc))
    }

    pub fn load_document(&self, doc: DocumentId) -> Result<LoadedDocument, StoreError> {
        self.runtime.block_on(self.store.load_document(doc))
    }

    pub fn text_search(
        &self,
        scope: impl Into<SearchScope>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<TextSearchHit>, StoreError> {
        self.runtime
            .block_on(self.store.text_search(scope, query, limit))
    }

    pub fn text_search_filtered(
        &self,
        corpus: CorpusId,
        query: &str,
        limit: i64,
        metadata_filter: Option<&serde_json::Value>,
    ) -> Result<Vec<TextSearchHit>, StoreError> {
        self.runtime.block_on(
            self.store
                .text_search_filtered(corpus, query, limit, metadata_filter),
        )
    }

    pub fn set_document_partitions(
        &self,
        doc: DocumentId,
        partitions: &serde_json::Value,
    ) -> Result<(), StoreError> {
        self.runtime
            .block_on(self.store.set_document_partitions(doc, partitions))
    }

    pub fn documents_matching(
        &self,
        corpus: CorpusId,
        metadata_filter: Option<&serde_json::Value>,
    ) -> Result<Vec<DocumentId>, StoreError> {
        self.runtime
            .block_on(self.store.documents_matching(corpus, metadata_filter))
    }

    pub fn document_metadata(
        &self,
        doc: DocumentId,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        self.runtime.block_on(self.store.document_metadata(doc))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn elements_in_bbox(
        &self,
        doc: DocumentId,
        page: i32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) -> Result<Vec<ElementRow>, StoreError> {
        self.runtime
            .block_on(self.store.elements_in_bbox(doc, page, x0, y0, x1, y1))
    }
}
