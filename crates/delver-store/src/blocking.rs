//! Synchronous facade over [`DelverStore`] for CLI / Python callers (D-003).
//!
//! Owns a private current-thread tokio runtime and forwards every call with
//! `block_on`. Do not use from inside an async context.

use std::collections::BTreeMap;

use delver_core::parse::PageContents;
use tokio::runtime::{Builder, Runtime};

use crate::error::StoreError;
use crate::store::DelverStore;
use crate::types::{
    CorpusId, DocumentId, ElementRow, IngestOutcome, SearchScope, TextSearchHit,
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
        self.runtime
            .block_on(self.store.ingest_document(corpus, uri, pdf_bytes, parse_version))
    }

    pub fn ingest_parsed(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        pages: &BTreeMap<u32, PageContents>,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        self.runtime.block_on(
            self.store
                .ingest_parsed(corpus, uri, pdf_bytes, pages, parse_version),
        )
    }

    pub fn element_count(&self, doc: DocumentId) -> Result<i64, StoreError> {
        self.runtime.block_on(self.store.element_count(doc))
    }

    pub fn load_document(&self, doc: DocumentId) -> Result<Vec<ElementRow>, StoreError> {
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
