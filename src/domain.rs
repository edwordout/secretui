use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zeroize::Zeroize;

pub type Attributes = BTreeMap<String, String>;

pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionInfo {
    pub path: String,
    pub label: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemInfo {
    pub collection_path: String,
    pub path: String,
    pub label: String,
    pub locked: bool,
    pub attributes: Attributes,
    pub created: Option<u64>,
    pub modified: Option<u64>,
}

pub struct NewItem {
    pub collection_path: String,
    pub label: String,
    pub attributes: Attributes,
    pub secret: SecretBytes,
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewCollection {
    pub label: String,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetadataFile {
    pub version: u32,
    pub collections: Vec<CollectionMetadata>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetadataImportSummary {
    pub collections_changed: usize,
    pub items_changed: usize,
    pub paths_missing: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionMetadata {
    pub path: String,
    pub label: String,
    pub locked: bool,
    pub items: Vec<ItemMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemMetadata {
    pub path: String,
    pub label: String,
    pub locked: bool,
    pub attributes: Attributes,
    pub created: Option<u64>,
    pub modified: Option<u64>,
}

impl MetadataFile {
    pub fn sorted(mut self) -> Self {
        self.collections.sort_by(|a, b| a.path.cmp(&b.path));
        for collection in &mut self.collections {
            collection.items.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self
    }
}
