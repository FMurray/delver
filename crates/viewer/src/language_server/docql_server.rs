//! DocQL language server (websocket-framed JSON-RPC, DV-012).
//!
//! Validation runs the REAL delver-core pipeline — pest syntax parse for
//! positioned errors, then the full `parse_template` compile so every D-006
//! fail-loud check (TYPE … AS TABLE misuse, `type=` on non-Table, unknown
//! `method=`, undefined match references, bad regexes, SubCorpus/template
//! interpolation errors) surfaces as a diagnostic. Completions come from one
//! inventory table kept in lockstep with the grammar surface in
//! `delver-core/src/docql.{pest,rs}` (elements, per-element attribute keys,
//! match functions, TYPE field types).
//!
//! The previous revision embedded its own keyword list (no Annotation /
//! Figure / SubCorpus / TYPE, a `Cosine` function instead of the canonical
//! `EmbeddingSim`, `Table(match=…)` which the engine ignores) across THREE
//! drifting copies (typed tower-lsp methods, a JSON duplicate, and hover
//! text); the unused typed methods are deleted rather than refreshed.

use delver_core::docql::{parse_template, Rule, TemplateParser};
use pest::Parser;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug)]
pub struct DocQLLanguageServer {
    #[allow(dead_code)] // transport hook kept for a real LSP client
    pub client: MockClient,
    document_map: tokio::sync::RwLock<HashMap<String, String>>,
}

/// Transport stub retained for the websocket server-fn constructor.
#[derive(Debug)]
pub struct MockClient {
    pub sender: tokio::sync::mpsc::Sender<tower_lsp::jsonrpc::Response>,
}

// ───────────────────── completion inventory (current grammar) ─────────────────────
//
// Sources (read-only): docql.rs `element_type_from_name` (element names),
// attribute resolve/validate sites (`as`/`match`/`end_match`/`type`/
// `method`/`template`/`chunkSize`/`chunkOverlap`/`breakpointPercentile`/
// `description`), `function_call_to_match_config` (match functions; Cosine /
// Semantic parse as aliases of EmbeddingSim and are not advertised), and
// `udt` field types TEXT / INT / DECIMAL (D-021).

/// (label, detail, insert_text) — LSP `CompletionItemKind::CLASS`.
const ELEMENT_COMPLETIONS: &[(&str, &str, &str)] = &[
    (
        "Section",
        "Structural section: match=/end_match= boundaries, as= names the partition",
        "Section(match=$1, as=\"$2\") {\n  $0\n}",
    ),
    (
        "TextChunk",
        "Chunk text: chunkSize/chunkOverlap, method=\"tokens\"|\"semantic\", template=\"…{text}…\"",
        "TextChunk(chunkSize=500, chunkOverlap=150)",
    ),
    (
        "Paragraph",
        "Paragraph element (chunking attributes like TextChunk)",
        "Paragraph(match=\"$1\")",
    ),
    (
        "Table",
        "Collect detected tables in scope; type=\"Name\" extracts typed records (D-021)",
        "Table(as=\"$1\")",
    ),
    (
        "Annotation",
        "Collect annotation aux elements in scope (D-016)",
        "Annotation(as=\"$1\")",
    ),
    (
        "Figure",
        "Collect figure aux elements in scope (D-016)",
        "Figure(as=\"$1\")",
    ),
    (
        "Image",
        "Image element; children process matched images",
        "Image {\n  $0\n}",
    ),
    (
        "SubCorpus",
        "Top-level declaration: description interpolates into TextChunk template= (D-022)",
        "SubCorpus(description=\"$1\", as=\"$2\")",
    ),
];

/// (label, detail, insert_text) — keyword-ish top-level constructs.
const KEYWORD_COMPLETIONS: &[(&str, &str, &str)] = &[
    (
        "Match",
        "Reusable match definition: Match<Section> Name { Text(…) … }",
        "Match<Section> $1 {\n  $0\n}",
    ),
    (
        "TYPE",
        "User-defined table type: TYPE Name AS TABLE ( field TEXT, … ); (D-021)",
        "TYPE $1 AS TABLE (\n  $2 TEXT,\n);",
    ),
];

/// Per-element attribute keys (label, detail); insert is `key=`.
const ELEMENT_ATTRS: &[(&str, &[(&str, &str)])] = &[
    (
        "Section",
        &[
            ("match", "Match definition name, inline function, or string"),
            ("end_match", "Boundary match ending the section"),
            ("as", "Output name for the section partition"),
        ],
    ),
    (
        "TextChunk",
        &[
            ("chunkSize", "Token/char budget per chunk (default 500)"),
            ("chunkOverlap", "Carried-over budget between chunks"),
            ("method", "\"tokens\" (default) or \"semantic\" (D-020)"),
            (
                "breakpointPercentile",
                "Valley percentile 0..=100 for method=\"semantic\" (default 25)",
            ),
            ("template", "Interpolation template; {text} = chunk text (D-022)"),
            ("as", "Output name"),
        ],
    ),
    (
        "Paragraph",
        &[
            ("match", "Match definition name, inline function, or string"),
            ("chunkSize", "Token/char budget per chunk"),
            ("chunkOverlap", "Carried-over budget between chunks"),
            ("method", "\"tokens\" (default) or \"semantic\" (D-020)"),
            ("as", "Output name"),
        ],
    ),
    (
        "Table",
        &[
            ("as", "Output name"),
            ("type", "TYPE name for typed record extraction (D-021)"),
        ],
    ),
    ("Annotation", &[("as", "Output name")]),
    ("Figure", &[("as", "Output name")]),
    ("Image", &[("as", "Output name")]),
    (
        "SubCorpus",
        &[
            ("description", "Interpolates as {name} into template= strings"),
            ("as", "Declaration name"),
        ],
    ),
];

/// (label, detail, insert_text) — match functions inside Match bodies /
/// match= attributes. `Cosine` / `Semantic` still parse as aliases of
/// `EmbeddingSim` but only the canonical name is advertised (D-014).
const MATCH_FUNCTION_COMPLETIONS: &[(&str, &str, &str)] = &[
    (
        "Text",
        "Fuzzy text match: Text(\"pattern\", threshold=0.6)",
        "Text(\"$1\", threshold=0.6)",
    ),
    (
        "Regex",
        "Regular-expression match (compiled at template compile)",
        "Regex(\"$1\")",
    ),
    (
        "Heuristic",
        "Property comparisons (ANDed): font_size/font_name/text/text_length/page/x0/y0/x1/y1",
        "Heuristic($1)",
    ),
    (
        "EmbeddingSim",
        "Embedding similarity: EmbeddingSim(\"query\", threshold=0.7, endpoint=\"…\", model=\"…\")",
        "EmbeddingSim(\"$1\", threshold=0.7)",
    ),
    (
        "FirstMatch",
        "First matching alternative: FirstMatch(Text(…), Regex(…))",
        "FirstMatch($1)",
    ),
];

/// TYPE field types (D-021, `udt`).
const TYPE_FIELD_COMPLETIONS: &[(&str, &str)] = &[
    ("TEXT", "Verbatim cell text"),
    ("INT", "Integer; strips %, $, commas; (n) = negative"),
    ("DECIMAL", "Decimal; strips %, $, commas; (n) = negative"),
];

// ───────────────────── completion context detection ─────────────────────

/// Where the cursor sits, derived from the text before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionCtx {
    /// Top level or inside an element body: elements + Match/TYPE keywords.
    Statement,
    /// Inside `Name( …` for a known element: its attribute keys.
    ElementAttrs(String),
    /// Inside a `Match<…> Name { …` body or `FirstMatch(…`: match functions.
    MatchBody,
    /// Inside `TYPE Name AS TABLE ( …`: field type names.
    TypeFields,
    /// Inside a string literal: no completions.
    InString,
}

/// Scan the prefix up to (0-based) `line`/`character`, tracking string
/// literals, paren frames (with the identifier that opened them), and brace
/// frames (match-definition bodies detected from clause text containing
/// `Match<` at that nesting level).
pub fn completion_context(text: &str, line: usize, character: usize) -> CompletionCtx {
    let mut prefix = String::new();
    for (i, l) in text.split('\n').enumerate() {
        match i.cmp(&line) {
            std::cmp::Ordering::Less => {
                prefix.push_str(l);
                prefix.push('\n');
            }
            std::cmp::Ordering::Equal => {
                let upto: String = l.chars().take(character).collect();
                prefix.push_str(&upto);
                break;
            }
            std::cmp::Ordering::Greater => break,
        }
    }

    #[derive(Debug)]
    enum Frame {
        Paren { word: String },
        Brace { match_body: bool },
    }

    let mut frames: Vec<Frame> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    // The most recent identifier (kept across whitespace so `TABLE (` and
    // `Section (` still attribute the paren to the word before it).
    let mut word = String::new();
    let mut prev_was_word = false;
    let mut clause = String::new(); // text at the current brace level, outside parens

    for ch in prefix.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        let mut is_word_char = false;
        match ch {
            '"' => {
                in_string = true;
                word.clear();
            }
            '(' => {
                frames.push(Frame::Paren { word: word.clone() });
                word.clear();
            }
            ')' => {
                if matches!(frames.last(), Some(Frame::Paren { .. })) {
                    frames.pop();
                }
                word.clear();
            }
            '{' => {
                let match_body = clause.contains("Match<");
                frames.push(Frame::Brace { match_body });
                clause.clear();
                word.clear();
            }
            '}' => {
                if matches!(frames.last(), Some(Frame::Brace { .. })) {
                    frames.pop();
                }
                clause.clear();
                word.clear();
            }
            ';' => {
                clause.clear();
                word.clear();
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                if !prev_was_word {
                    word.clear();
                }
                word.push(c);
                is_word_char = true;
            }
            c if c.is_whitespace() => {} // keep the completed word
            _ => word.clear(),
        }
        prev_was_word = is_word_char;
        if !matches!(frames.last(), Some(Frame::Paren { .. })) && ch != '{' && ch != '}' {
            clause.push(ch);
        }
    }

    if in_string {
        return CompletionCtx::InString;
    }
    match frames.last() {
        Some(Frame::Paren { word }) => {
            if word == "TABLE" {
                CompletionCtx::TypeFields
            } else if word == "FirstMatch" {
                CompletionCtx::MatchBody
            } else if ELEMENT_ATTRS.iter().any(|(name, _)| name == word) {
                CompletionCtx::ElementAttrs(word.clone())
            } else {
                CompletionCtx::Statement
            }
        }
        Some(Frame::Brace { match_body: true }) => CompletionCtx::MatchBody,
        Some(Frame::Brace { match_body: false }) | None => CompletionCtx::Statement,
    }
}

/// LSP CompletionItem JSON for a context. Insert texts use `$N` snippet
/// placeholders (`insertTextFormat: 2`); the CodeMirror client strips them.
pub fn completions_for(ctx: &CompletionCtx) -> Vec<Value> {
    let item = |label: &str, kind: u32, detail: &str, insert: &str| {
        json!({
            "label": label,
            "kind": kind,
            "detail": detail,
            "insertText": insert,
            "insertTextFormat": 2,
        })
    };
    match ctx {
        CompletionCtx::InString => Vec::new(),
        CompletionCtx::Statement => ELEMENT_COMPLETIONS
            .iter()
            .map(|(l, d, i)| item(l, 7, d, i)) // CLASS
            .chain(
                KEYWORD_COMPLETIONS
                    .iter()
                    .map(|(l, d, i)| item(l, 14, d, i)), // KEYWORD
            )
            .collect(),
        CompletionCtx::ElementAttrs(element) => ELEMENT_ATTRS
            .iter()
            .find(|(name, _)| name == element)
            .map(|(_, attrs)| {
                attrs
                    .iter()
                    .map(|(l, d)| item(l, 10, d, &format!("{l}="))) // PROPERTY
                    .collect()
            })
            .unwrap_or_default(),
        CompletionCtx::MatchBody => MATCH_FUNCTION_COMPLETIONS
            .iter()
            .map(|(l, d, i)| item(l, 3, d, i)) // FUNCTION
            .collect(),
        CompletionCtx::TypeFields => TYPE_FIELD_COMPLETIONS
            .iter()
            .map(|(l, d)| item(l, 14, d, l)) // KEYWORD
            .collect(),
    }
}

// ───────────────────── diagnostics (real parser + compiler) ─────────────────────

/// Diagnostics from the CURRENT delver-core pipeline: a pest syntax error
/// with its real (line, col), else any `parse_template` compile error
/// (fail-loud semantics, D-006) anchored at 0:0 with the full message.
pub fn diagnostics_for(text: &str) -> Vec<Value> {
    match TemplateParser::parse(Rule::template, text) {
        Err(e) => {
            let ((l0, c0), (l1, c1)) = match e.line_col {
                pest::error::LineColLocation::Pos((l, c)) => ((l, c), (l, c + 1)),
                pest::error::LineColLocation::Span((l0, c0), (l1, c1)) => ((l0, c0), (l1, c1)),
            };
            // pest is 1-based, LSP 0-based.
            vec![json!({
                "range": {
                    "start": {"line": l0.saturating_sub(1), "character": c0.saturating_sub(1)},
                    "end": {"line": l1.saturating_sub(1), "character": c1.saturating_sub(1).max(c0)},
                },
                "severity": 1,
                "source": "docql-parser",
                "message": format!("DocQL syntax error: {e}"),
            })]
        }
        Ok(_) => match parse_template(text) {
            Ok(_) => Vec::new(),
            Err(e) => vec![json!({
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 1},
                },
                "severity": 1,
                "source": "docql-compile",
                "message": format!("DocQL compile error: {e}"),
            })],
        },
    }
}

// ───────────────────── JSON-RPC processing ─────────────────────

const DEFAULT_URI: &str = "file:///query.docql";

impl DocQLLanguageServer {
    pub fn new(client: MockClient) -> Self {
        Self {
            client,
            document_map: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Process a raw LSP message and return the responses to send.
    pub async fn process_lsp_message(&self, message: &str) -> Vec<String> {
        let mut responses = Vec::new();

        let Ok(request) = serde_json::from_str::<Value>(message) else {
            return responses;
        };
        let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
            return responses;
        };
        match method {
            "initialize" => {
                responses.push(
                    json!({
                        "jsonrpc": "2.0",
                        "id": request.get("id"),
                        "result": {
                            "capabilities": {
                                "textDocumentSync": 1,
                                "completionProvider": {
                                    "resolveProvider": false,
                                    "triggerCharacters": ["<", "(", " "]
                                },
                                "hoverProvider": false
                            }
                        }
                    })
                    .to_string(),
                );
            }
            "textDocument/didOpen" => {
                let params = request.get("params");
                let uri = uri_from(params, &["textDocument", "uri"]);
                if let Some(text) = params
                    .and_then(|p| p.pointer("/textDocument/text"))
                    .and_then(|t| t.as_str())
                {
                    self.document_map
                        .write()
                        .await
                        .insert(uri.clone(), text.to_string());
                    responses.push(publish_diagnostics(&uri, diagnostics_for(text)));
                }
            }
            "textDocument/didChange" => {
                let params = request.get("params");
                let uri = uri_from(params, &["textDocument", "uri"]);
                if let Some(text) = params
                    .and_then(|p| p.get("contentChanges"))
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                {
                    self.document_map
                        .write()
                        .await
                        .insert(uri.clone(), text.to_string());
                    responses.push(publish_diagnostics(&uri, diagnostics_for(text)));
                }
            }
            "textDocument/completion" => {
                let params = request.get("params");
                let uri = uri_from(params, &["textDocument", "uri"]);
                let line = position_part(params, "line");
                let character = position_part(params, "character");
                let documents = self.document_map.read().await;
                let text = documents.get(&uri).map(String::as_str).unwrap_or("");
                let ctx = completion_context(text, line, character);
                responses.push(
                    json!({
                        "jsonrpc": "2.0",
                        "id": request.get("id"),
                        "result": completions_for(&ctx)
                    })
                    .to_string(),
                );
            }
            _ => {
                // Generic acknowledgment for requests (id present) only.
                if request.get("id").is_some() {
                    responses.push(
                        json!({
                            "jsonrpc": "2.0",
                            "id": request.get("id"),
                            "result": null
                        })
                        .to_string(),
                    );
                }
            }
        }

        responses
    }
}

fn uri_from(params: Option<&Value>, path: &[&str]) -> String {
    let mut cur = params;
    for key in path {
        cur = cur.and_then(|v| v.get(key));
    }
    cur.and_then(|v| v.as_str()).unwrap_or(DEFAULT_URI).to_string()
}

fn position_part(params: Option<&Value>, part: &str) -> usize {
    params
        .and_then(|p| p.pointer(&format!("/position/{part}")))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize
}

fn publish_diagnostics(uri: &str, diagnostics: Vec<Value>) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": diagnostics }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── context detection ──

    fn ctx(text: &str) -> CompletionCtx {
        // Cursor at end of text.
        let line = text.matches('\n').count();
        let character = text.split('\n').next_back().unwrap_or("").chars().count();
        completion_context(text, line, character)
    }

    #[test]
    fn context_statement_at_top_level_and_in_element_bodies() {
        assert_eq!(ctx(""), CompletionCtx::Statement);
        assert_eq!(ctx("Section(match=X, as=\"y\") {\n  "), CompletionCtx::Statement);
        // After a closed element the context resets.
        assert_eq!(ctx("TextChunk(chunkSize=500)\n"), CompletionCtx::Statement);
    }

    #[test]
    fn context_attrs_inside_known_element_parens() {
        assert_eq!(
            ctx("Table("),
            CompletionCtx::ElementAttrs("Table".to_string())
        );
        assert_eq!(
            ctx("Section(match=MDA, "),
            CompletionCtx::ElementAttrs("Section".to_string())
        );
        // Closed parens pop the frame.
        assert_eq!(ctx("Table(as=\"t\")"), CompletionCtx::Statement);
    }

    #[test]
    fn context_match_bodies_and_strings_and_type_fields() {
        assert_eq!(ctx("Match<Section> MDA {\n  "), CompletionCtx::MatchBody);
        assert_eq!(ctx("Match<Section> M { FirstMatch("), CompletionCtx::MatchBody);
        assert_eq!(ctx("Match<Section> M { Text(\""), CompletionCtx::InString);
        assert_eq!(ctx("TYPE Seg AS TABLE ( "), CompletionCtx::TypeFields);
        // A `{` from a previous match definition does not leak.
        assert_eq!(ctx("Match<Section> M { Text(\"x\") }\n"), CompletionCtx::Statement);
        // Escaped quote stays inside the string.
        assert_eq!(ctx("Text(\"a\\\""), CompletionCtx::InString);
    }

    #[test]
    fn completion_inventory_covers_current_grammar() {
        let labels = |ctx: &CompletionCtx| -> Vec<String> {
            completions_for(ctx)
                .iter()
                .map(|c| c["label"].as_str().unwrap().to_string())
                .collect()
        };
        let statement = labels(&CompletionCtx::Statement);
        for expected in [
            "Section", "TextChunk", "Table", "Annotation", "Figure", "Paragraph",
            "Image", "SubCorpus", "Match", "TYPE",
        ] {
            assert!(statement.contains(&expected.to_string()), "missing {expected}");
        }
        let functions = labels(&CompletionCtx::MatchBody);
        for expected in ["Text", "Regex", "Heuristic", "EmbeddingSim", "FirstMatch"] {
            assert!(functions.contains(&expected.to_string()), "missing {expected}");
        }
        assert_eq!(labels(&CompletionCtx::TypeFields), ["TEXT", "INT", "DECIMAL"]);
        let table_attrs = labels(&CompletionCtx::ElementAttrs("Table".into()));
        assert_eq!(table_attrs, ["as", "type"]);
        let chunk_attrs = labels(&CompletionCtx::ElementAttrs("TextChunk".into()));
        for expected in ["chunkSize", "chunkOverlap", "method", "template", "as"] {
            assert!(chunk_attrs.contains(&expected.to_string()), "missing {expected}");
        }
        assert!(labels(&CompletionCtx::InString).is_empty());
    }

    // ── diagnostics ──

    #[test]
    fn diagnostics_clean_on_valid_current_grammar_template() {
        let template = r#"
TYPE Seg AS TABLE ( metric TEXT, y2015 DECIMAL );

SubCorpus(description="California auto loans", as="CA_auto_loans")

Match<Section> MDA {
  FirstMatch(Text("Management Discussion", threshold=0.6), Regex("M.*D.*A"))
}

Section(match=MDA, as="mda") {
  TextChunk(chunkSize=500, chunkOverlap=150, method="semantic", template="{CA_auto_loans}: {text}")
  Table(as="t", type="Seg")
}
"#;
        let diags = diagnostics_for(template);
        assert!(diags.is_empty(), "expected clean, got {diags:?}");
    }

    #[test]
    fn diagnostics_surface_syntax_errors_with_position() {
        let diags = diagnostics_for("Section(match=) {}");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0]["source"], "docql-parser");
        assert_eq!(diags[0]["range"]["start"]["line"], 0);
        assert!(diags[0]["range"]["start"]["character"].as_u64().unwrap() > 0);
    }

    #[test]
    fn diagnostics_surface_compile_errors_from_the_real_pipeline() {
        // Parses, but `type=` references an undefined TYPE (D-021 fail-loud).
        let diags = diagnostics_for("Table(as=\"t\", type=\"Nope\")");
        assert_eq!(diags.len(), 1, "expected one compile diagnostic: {diags:?}");
        assert_eq!(diags[0]["source"], "docql-compile");
        let msg = diags[0]["message"].as_str().unwrap();
        assert!(msg.contains("Nope"), "message should name the type: {msg}");
        // Unknown chunking method is also caught at compile (D-020).
        let diags = diagnostics_for("TextChunk(method=\"banana\")");
        assert_eq!(diags.len(), 1);
        assert!(diags[0]["message"].as_str().unwrap().contains("banana"));
    }

    // ── end-to-end JSON-RPC ──

    fn server() -> DocQLLanguageServer {
        let (sender, _rx) = tokio::sync::mpsc::channel(1);
        DocQLLanguageServer::new(MockClient { sender })
    }

    #[tokio::test]
    async fn did_change_stores_text_and_completion_uses_position() {
        let s = server();
        let did_change = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": "file:///query.docql", "version": 1},
                "contentChanges": [{"text": "Table("}]
            }
        });
        let responses = s.process_lsp_message(&did_change.to_string()).await;
        assert_eq!(responses.len(), 1);
        let diag: Value = serde_json::from_str(&responses[0]).unwrap();
        assert_eq!(diag["method"], "textDocument/publishDiagnostics");
        // "Table(" alone is a syntax error — fail-loud diagnostics present.
        assert!(!diag["params"]["diagnostics"].as_array().unwrap().is_empty());

        let completion = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": "file:///query.docql"},
                "position": {"line": 0, "character": 6}
            }
        });
        let responses = s.process_lsp_message(&completion.to_string()).await;
        assert_eq!(responses.len(), 1);
        let resp: Value = serde_json::from_str(&responses[0]).unwrap();
        assert_eq!(resp["id"], 7);
        let labels: Vec<&str> = resp["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["as", "type"], "cursor inside Table( offers its attrs");
    }
}
