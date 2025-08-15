use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentPage {
    pub page_index: usize,
    pub image_data: Vec<u8>, // RGBA image data
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfDocument {
    pub id: Uuid,
    pub name: String,
    pub total_pages: usize,
    pub pages: HashMap<usize, DocumentPage>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PdfDocument {
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            total_pages: 0,
            pages: HashMap::new(),
            created_at: chrono::Utc::now(),
        }
    }

    pub fn add_page(&mut self, page: DocumentPage) {
        self.total_pages = self.total_pages.max(page.page_index + 1);
        self.pages.insert(page.page_index, page);
    }

    pub fn get_page(&self, page_index: usize) -> Option<&DocumentPage> {
        self.pages.get(&page_index)
    }
}

#[derive(Debug, Clone)]
pub struct DocumentStore {
    pub documents: RwSignal<HashMap<Uuid, PdfDocument>>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            documents: RwSignal::new(HashMap::new()),
        }
    }

    pub fn add_document(&self, document: PdfDocument) -> Uuid {
        let id = document.id;
        self.documents.update(|docs| {
            docs.insert(id, document);
        });
        id
    }

    pub fn get_document(&self, id: Uuid) -> Option<PdfDocument> {
        self.documents.with(|docs| docs.get(&id).cloned())
    }

    pub fn get_all_documents(&self) -> Vec<PdfDocument> {
        self.documents.with(|docs| docs.values().cloned().collect())
    }

    pub fn remove_document(&self, id: Uuid) -> bool {
        let mut removed = false;
        self.documents.update(|docs| {
            removed = docs.remove(&id).is_some();
        });
        removed
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
