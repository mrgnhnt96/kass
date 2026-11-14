//! The dictionary: words dictation should get right, each in one or more places.

use super::ApiClient;
use crate::encode::component;
use crate::error::Result;
use crate::types::{
    Deleted, DictionaryEntry, DictionaryEntryCreate, DictionaryEntryUpdate, DictionaryListResponse,
    ResolvedDictionaryResponse,
};

impl ApiClient {
    pub fn list_dictionary(&self) -> Result<DictionaryListResponse> {
        self.fetch("GET", "/dictionary")
    }

    pub fn create_dictionary_entry(
        &self,
        entry: &DictionaryEntryCreate,
    ) -> Result<DictionaryEntry> {
        self.send("POST", "/dictionary", entry)
    }

    pub fn update_dictionary_entry(
        &self,
        id: &str,
        patch: &DictionaryEntryUpdate,
    ) -> Result<DictionaryEntry> {
        self.send("PATCH", &format!("/dictionary/{}", component(id)), patch)
    }

    pub fn delete_dictionary_entry(&self, id: &str) -> Result<Deleted> {
        self.fetch("DELETE", &format!("/dictionary/{}", component(id)))
    }

    /// The entries that apply in an app: its own, its style's and everywhere's.
    pub fn resolve_dictionary(&self, bundle_id: &str) -> Result<ResolvedDictionaryResponse> {
        self.fetch(
            "GET",
            &format!("/dictionary/resolved?bundle_id={}", component(bundle_id)),
        )
    }
}
