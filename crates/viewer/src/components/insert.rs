//! Insert-into-query bus (DV-012): discover-mode surfaces (element side
//! panel, query palette) publish a [`SnippetSpec`]; the query panel consumes
//! it, renders the snippet against the current editor buffer (names stay
//! unique), and inserts at the CodeMirror cursor.
//!
//! The spec — not pre-rendered text — travels on the bus because identifier
//! uniqueness depends on the buffer contents at *insertion* time, which only
//! the editor side knows.

use leptos::prelude::*;

use crate::app::QueryContext;
use crate::snippets::SnippetSpec;

/// Global context: the pending insertion, with a nonce so the same spec can
/// be inserted twice in a row. `None` once consumed.
#[derive(Clone, Copy)]
pub struct InsertBus(pub RwSignal<Option<(SnippetSpec, u64)>>);

impl InsertBus {
    pub fn new() -> Self {
        Self(RwSignal::new(None))
    }
}

impl Default for InsertBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared styling for "insert into query" action chips.
pub const INSERT_CHIP: &str = "px-1.5 py-0.5 text-[10px] font-medium rounded border border-blue-300 text-blue-700 bg-blue-50 hover:bg-blue-100 disabled:opacity-40";

/// Click-handler helper: returns a closure that opens the query panel and
/// publishes `spec` on the bus.
pub fn use_request_insert() -> impl Fn(SnippetSpec) + Copy {
    let bus = expect_context::<InsertBus>();
    let QueryContext(_, set_show_query) = expect_context::<QueryContext>();
    move |spec: SnippetSpec| {
        set_show_query.set(true);
        let nonce = bus
            .0
            .get_untracked()
            .map(|(_, n)| n.wrapping_add(1))
            .unwrap_or(0);
        bus.0.set(Some((spec, nonce)));
    }
}
