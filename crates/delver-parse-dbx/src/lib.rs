//! Databricks `ai_parse_document` parsing backend for delver
//! (slice P1 part 2, docs/DECISIONS-aiparse.md DA-005…DA-008).
//!
//! Flow (document-level, one engine per parse_version):
//! 1. upload the document bytes to a Unity Catalog volume (Files API),
//! 2. execute `ai_parse_document` over the file via the SQL Statement
//!    Execution API on a SQL warehouse,
//! 3. poll until the statement reaches a terminal state,
//! 4. fetch the result JSON (`to_json` of the VARIANT output, schema 2.0),
//! 5. delete the uploaded temp file (best effort),
//! 6. map the response onto delver's `ParsedDocument` so the existing
//!    `delver-store` ingest path persists it (text elements with page+bbox,
//!    tables as kind=table + `table_cells`, figures as kind=figure).
//!
//! Configuration is strictly environment-driven (see [`DbxConfig`]); no
//! Databricks workspace is ever contacted unless the caller explicitly
//! selects the `ai-parse` engine (or `auto` resolves to it) with complete
//! configuration. No test in this crate touches the network: request
//! construction and response parsing are pure functions tested against
//! canned JSON, and the live end-to-end test is gated behind
//! `DELVER_DBX_LIVE=1`.

mod client;
mod config;
mod html_table;
mod map;

pub use client::{DbxParseClient, POLL_INTERVAL, POLL_TIMEOUT};
pub use config::DbxConfig;
pub use map::map_ai_parse_response;

/// Error type for every fallible operation in this crate (config, HTTP,
/// statement execution, response mapping). Message-only by design, mirroring
/// `delver_core::embed::EmbedError` — callers surface it verbatim (D-006).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDbxError(pub String);

impl std::fmt::Display for ParseDbxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseDbxError {}
