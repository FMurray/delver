use delver_core::docql::{Rule, TemplateParser};
use pest::Parser;
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_lsp::lsp_types::*;

#[derive(Debug)]
pub struct DocQLLanguageServer {
    pub client: MockClient,
    document_map: tokio::sync::RwLock<HashMap<Url, String>>,
}

// Mock client for sending responses
#[derive(Debug)]
pub struct MockClient {
    pub sender: tokio::sync::mpsc::Sender<tower_lsp::jsonrpc::Response>,
}

impl DocQLLanguageServer {
    pub fn new(client: MockClient) -> Self {
        Self {
            client,
            document_map: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    async fn validate_docql(&self, uri: &Url, text: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        match TemplateParser::parse(Rule::template, text) {
            Ok(_) => {
                // Parsing successful, no errors
                log::info!("DocQL parsing successful for {}", uri);
            }
            Err(e) => {
                // Convert pest error to LSP diagnostic
                let diagnostic = self.pest_error_to_diagnostic(&e);
                diagnostics.push(diagnostic);
                log::warn!("DocQL parsing failed for {}: {}", uri, e);
            }
        }

        diagnostics
    }

    fn pest_error_to_diagnostic(&self, error: &pest::error::Error<Rule>) -> Diagnostic {
        // For pest errors, we'll use a simple approach - default to line 0
        let message = error.to_string();

        Diagnostic::new(
            Range::new(
                Position::new(0, 0),
                Position::new(0, 1), // Single character range for now
            ),
            Some(DiagnosticSeverity::ERROR),
            None,
            Some("docql-parser".to_string()),
            message,
            None,
            None,
        )
    }

    fn get_element_completions(&self) -> Vec<CompletionItem> {
        vec![
            CompletionItem {
                label: "Section".to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Document section element".to_string()),
                documentation: Some(Documentation::String(
                    "Defines a section that can contain other elements".to_string(),
                )),
                insert_text: Some("Section(match=\"\") {\n\t$0\n}".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Paragraph".to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Paragraph element".to_string()),
                documentation: Some(Documentation::String(
                    "Defines a paragraph text element".to_string(),
                )),
                insert_text: Some("Paragraph(match=\"\")".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "TextChunk".to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Text chunk element".to_string()),
                documentation: Some(Documentation::String(
                    "Defines a text chunk for processing".to_string(),
                )),
                insert_text: Some("TextChunk(chunkSize=500, chunkOverlap=150)".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Table".to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Table element".to_string()),
                documentation: Some(Documentation::String(
                    "Defines a table structure".to_string(),
                )),
                insert_text: Some("Table(match=\"\")".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Image".to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Image element".to_string()),
                documentation: Some(Documentation::String(
                    "Defines an image element that can have children for processing".to_string(),
                )),
                insert_text: Some("Image {\n\t$0\n}".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Match".to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("Match definition".to_string()),
                documentation: Some(Documentation::String(
                    "Defines a reusable match configuration".to_string(),
                )),
                insert_text: Some("Match<$1> $2 {\n\t$0\n}".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
        ]
    }

    fn get_function_completions(&self) -> Vec<CompletionItem> {
        vec![
            CompletionItem {
                label: "Text".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("Text matching function".to_string()),
                documentation: Some(Documentation::String(
                    "Text(pattern, threshold=0.8) - Matches text content".to_string(),
                )),
                insert_text: Some("Text(\"$1\", threshold=0.8)".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Cosine".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("Semantic matching function".to_string()),
                documentation: Some(Documentation::String(
                    "Cosine(pattern, threshold=0.7) - Semantic similarity matching".to_string(),
                )),
                insert_text: Some("Cosine(\"$1\", threshold=0.7)".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
            CompletionItem {
                label: "Regex".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("Regular expression matching".to_string()),
                documentation: Some(Documentation::String(
                    "Regex(pattern) - Regular expression pattern matching".to_string(),
                )),
                insert_text: Some("Regex(\"$1\")".to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            },
        ]
    }
}

impl DocQLLanguageServer {
    pub async fn initialize(
        &self,
        _: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "DocQL Language Server".to_string(),
                version: Some("0.1.0".to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![
                        "<".to_string(),
                        "(".to_string(),
                        "\"".to_string(),
                        " ".to_string(),
                    ]),
                    work_done_progress_options: Default::default(),
                    all_commit_characters: None,
                    completion_item: None,
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        identifier: Some("docql".to_string()),
                        inter_file_dependencies: false,
                        workspace_diagnostics: false,
                        work_done_progress_options: Default::default(),
                    },
                )),
                ..ServerCapabilities::default()
            },
        })
    }

    pub async fn initialized(&self, _: InitializedParams) {
        log::info!("DocQL Language Server initialized!");
    }

    pub async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    pub async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text.clone();

        // Store the document
        self.document_map
            .write()
            .await
            .insert(uri.clone(), text.clone());

        // Validate and send diagnostics
        let diagnostics = self.validate_docql(&uri, &text).await;
        self.publish_diagnostics(uri, diagnostics, None).await;
    }

    pub async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;

        // Get the latest text (assuming full document sync)
        if let Some(change) = params.content_changes.into_iter().next() {
            let text = change.text;

            // Update stored document
            self.document_map
                .write()
                .await
                .insert(uri.clone(), text.clone());

            // Validate and send diagnostics
            let diagnostics = self.validate_docql(&uri, &text).await;
            self.publish_diagnostics(uri, diagnostics, None).await;
        }
    }

    pub async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let documents = self.document_map.read().await;
        if let Some(text) = documents.get(uri) {
            // Get the line content up to the cursor position
            let lines: Vec<&str> = text.lines().collect();
            if let Some(line) = lines.get(position.line as usize) {
                let line_prefix = &line[..std::cmp::min(position.character as usize, line.len())];

                let mut completions = Vec::new();

                // Provide element completions for main context
                if line_prefix.trim().is_empty()
                    || line_prefix.ends_with('{')
                    || line_prefix.ends_with('\n')
                {
                    completions.extend(self.get_element_completions());
                }

                // Provide function completions inside match expressions
                if line_prefix.contains("match=") || line_prefix.contains("Match<") {
                    completions.extend(self.get_function_completions());
                }

                return Ok(Some(CompletionResponse::Array(completions)));
            }
        }

        Ok(None)
    }

    pub async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let documents = self.document_map.read().await;
        if let Some(text) = documents.get(uri) {
            let lines: Vec<&str> = text.lines().collect();
            if let Some(line) = lines.get(position.line as usize) {
                let words: Vec<&str> = line.split_whitespace().collect();

                // Simple hover support for known elements
                for word in words {
                    match word {
                        "Section" => {
                            return Ok(Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: "**Section**\n\nDefines a document section that can contain other elements. Use `match` attribute to specify matching criteria.".to_string(),
                                }),
                                range: None,
                            }));
                        }
                        "TextChunk" => {
                            return Ok(Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: "**TextChunk**\n\nDefines a text chunk for processing. Supports `chunkSize` and `chunkOverlap` attributes.".to_string(),
                                }),
                                range: None,
                            }));
                        }
                        "Image" => {
                            return Ok(Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: "**Image**\n\nDefines an image element. Can contain child elements for image processing like ImageSummary, ImageBytes, etc.".to_string(),
                                }),
                                range: None,
                            }));
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(None)
    }

    async fn publish_diagnostics(
        &self,
        uri: Url,
        diagnostics: Vec<Diagnostic>,
        _version: Option<i32>,
    ) {
        // For now, we'll just log the diagnostics instead of sending via WebSocket
        log::info!("Diagnostics for {}: {:?}", uri, diagnostics);
    }

    /// Process a raw LSP message and return appropriate responses
    pub async fn process_lsp_message(&self, message: &str) -> Vec<String> {
        let mut responses = Vec::new();

        if let Ok(request) = serde_json::from_str::<Value>(message) {
            if let Some(method) = request.get("method").and_then(|m| m.as_str()) {
                match method {
                    "initialize" => {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": request.get("id"),
                            "result": {
                                "capabilities": {
                                    "textDocumentSync": 1,
                                    "completionProvider": {
                                        "resolveProvider": false,
                                        "triggerCharacters": ["<", "(", "\"", " "]
                                    },
                                    "hoverProvider": true
                                }
                            }
                        });
                        responses.push(response.to_string());
                    }
                    "textDocument/didChange" => {
                        if let Some(params) = request.get("params") {
                            if let Some(changes) =
                                params.get("contentChanges").and_then(|c| c.as_array())
                            {
                                if let Some(first_change) = changes.first() {
                                    if let Some(text) =
                                        first_change.get("text").and_then(|t| t.as_str())
                                    {
                                        // Use actual DocQL validation
                                        let diagnostic_response =
                                            self.validate_and_create_diagnostics(text).await;
                                        responses.push(diagnostic_response);
                                    }
                                }
                            }
                        }
                    }
                    "textDocument/completion" => {
                        if let Some(params) = request.get("params") {
                            let completion_response = self.handle_completion_request(params).await;
                            if let Some(response) = completion_response {
                                responses.push(
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": request.get("id"),
                                        "result": response
                                    })
                                    .to_string(),
                                );
                            }
                        }
                    }
                    "textDocument/hover" => {
                        if let Some(params) = request.get("params") {
                            let hover_response = self.handle_hover_request(params).await;
                            if let Some(response) = hover_response {
                                responses.push(
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": request.get("id"),
                                        "result": response
                                    })
                                    .to_string(),
                                );
                            }
                        }
                    }
                    _ => {
                        // Generic acknowledgment for other methods
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
        }

        responses
    }

    /// Validate DocQL text and create diagnostics response
    async fn validate_and_create_diagnostics(&self, text: &str) -> String {
        match TemplateParser::parse(Rule::template, text) {
            Ok(_) => {
                // Clear diagnostics for valid syntax
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": "file:///query.docql",
                        "diagnostics": []
                    }
                })
                .to_string()
            }
            Err(e) => {
                // Send syntax error diagnostic
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": "file:///query.docql",
                        "diagnostics": [{
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": {"line": 0, "character": 1}
                            },
                            "severity": 1,
                            "message": format!("DocQL syntax error: {}", e)
                        }]
                    }
                })
                .to_string()
            }
        }
    }

    /// Handle completion requests
    async fn handle_completion_request(&self, _params: &Value) -> Option<Value> {
        // Return element completions as JSON
        Some(json!([
            {
                "label": "Section",
                "kind": 7, // CompletionItemKind::CLASS
                "detail": "Document section element",
                "documentation": "Defines a section that can contain other elements",
                "insertText": "Section(match=\"\") {\n\t$0\n}",
                "insertTextFormat": 2 // InsertTextFormat::SNIPPET
            },
            {
                "label": "TextChunk",
                "kind": 7,
                "detail": "Text chunk element",
                "documentation": "Defines a text chunk for processing",
                "insertText": "TextChunk(chunkSize=500, chunkOverlap=150)",
                "insertTextFormat": 2
            },
            {
                "label": "Image",
                "kind": 7,
                "detail": "Image element",
                "documentation": "Defines an image element that can have children for processing",
                "insertText": "Image {\n\t$0\n}",
                "insertTextFormat": 2
            },
            {
                "label": "Match",
                "kind": 14, // CompletionItemKind::KEYWORD
                "detail": "Match definition",
                "documentation": "Defines a reusable match configuration",
                "insertText": "Match<$1> $2 {\n\t$0\n}",
                "insertTextFormat": 2
            }
        ]))
    }

    /// Handle hover requests
    async fn handle_hover_request(&self, params: &Value) -> Option<Value> {
        // Simple hover support - in a real implementation, we'd parse the position
        // and provide context-specific hover information
        Some(json!({
            "contents": {
                "kind": "markdown",
                "value": "**DocQL Element**\n\nHover over DocQL elements for documentation."
            }
        }))
    }
}
