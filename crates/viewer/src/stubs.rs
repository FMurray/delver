use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct DebugDataStore;

impl DebugDataStore {
    pub fn get_events_by_target(&self, _target: &str) -> Vec<String> {
        Vec::new()
    }

    pub fn get_template_matches(&self, _template_id: Uuid) -> Vec<(Uuid, f32)> {
        Vec::new()
    }

    pub fn get_template_name(&self, _template_id: Uuid) -> Option<String> {
        None
    }

    pub fn get_templates(&self) -> Vec<(Uuid, String)> {
        Vec::new()
    }

    pub fn get_template_structure(&self, _template_id: Uuid) -> Option<Vec<String>> {
        None
    }

    pub fn get_children(&self, _parent_id: Uuid) -> Vec<Uuid> {
        Vec::new()
    }

    pub fn debug_dump_all_matches(&self) -> Vec<(Uuid, Uuid, f32)> {
        Vec::new()
    }

    pub fn get_content_by_id(&self, _content_id: &Uuid) -> Option<String> {
        None
    }
}

#[derive(Default)]
pub struct EntityEvents {
    pub messages: Vec<String>,
    pub children: Vec<EntityEvents>,
}
