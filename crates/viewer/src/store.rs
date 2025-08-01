use delver_core::layout::TextBlock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DocumentState {
    pub id: Uuid,
    pub pdf_bytes: Option<Vec<u8>>,
    pub pdf_name: Option<String>,
    pub blocks: Vec<TextBlock>,
    pub current_page: usize,
    pub pdf_dimensions: Vec<(f32, f32)>,
    pub show_text: bool,
    pub show_lines: bool,
    pub show_blocks: bool,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Store {
    pub documents: Vec<DocumentState>,
    #[serde(skip)]
    name: String,
}

#[cfg(target_arch = "wasm32")]
impl Store {
    pub fn new(name: &str) -> Store {
        use web_sys::window;

        let mut store: Store = window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(name).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();

        store.name = name.to_string();
        store
    }

    pub fn save(&self) {
        use web_sys::window;

        window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| {
                let json = serde_json::to_string(&self).unwrap();
                s.set_item(&self.name, &json).ok()
            });
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Store {
    pub fn new(name: &str) -> Store {
        let mut store = Store::default();
        store.name = name.to_string();
        store
    }

    pub fn save(&self) {
        // No-op on desktop
    }
}
