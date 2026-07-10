use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Attributes = BTreeMap<String, String>;

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
    pub content_type: Option<String>,
    pub created: Option<u64>,
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewItem {
    pub collection_path: String,
    pub label: String,
    pub attributes: Attributes,
    pub secret: Vec<u8>,
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
    pub content_type: Option<String>,
    pub created: Option<u64>,
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretBackupFile {
    pub version: u32,
    pub collections: Vec<CollectionBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionBackup {
    pub path: String,
    pub label: String,
    pub items: Vec<ItemBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemBackup {
    pub path: String,
    pub label: String,
    pub attributes: Attributes,
    pub content_type: String,
    pub secret_base64: String,
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

impl SecretBackupFile {
    pub fn sorted(mut self) -> Self {
        self.collections.sort_by(|a, b| a.path.cmp(&b.path));
        for collection in &mut self.collections {
            collection.items.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self
    }
}
