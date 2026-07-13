use crate::domain::{
    Attributes, CollectionInfo, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile,
    NewCollection, NewItem, SecretValue,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;

/// An item identity always includes its owning collection.
///
/// Secret Service item paths are globally unique in practice, but accepting an item path on its
/// own makes it too easy to apply metadata obtained from one collection to an item in another.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemTarget {
    pub collection_path: String,
    pub item_path: String,
}

impl ItemTarget {
    pub fn new(collection_path: impl Into<String>, item_path: impl Into<String>) -> Self {
        Self {
            collection_path: collection_path.into(),
            item_path: item_path.into(),
        }
    }
}

impl From<&ItemInfo> for ItemTarget {
    fn from(item: &ItemInfo) -> Self {
        Self::new(&item.collection_path, &item.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreTarget {
    Collection(String),
    Item(ItemTarget),
}

impl fmt::Display for StoreTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collection(path) => write!(formatter, "collection {path}"),
            Self::Item(target) => write!(
                formatter,
                "item {} in collection {}",
                target.item_path, target.collection_path
            ),
        }
    }
}

/// A successful provider operation can still require attention.
///
/// In particular, the requested read or mutation may have succeeded while restoring a temporary
/// lock failed. Callers must show these warnings rather than presenting the operation as an
/// ordinary success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreWarning {
    TemporaryRelockFailed { target: StoreTarget, error: String },
}

impl fmt::Display for StoreWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TemporaryRelockFailed { target, error } => {
                write!(formatter, "could not restore the lock on {target}: {error}")
            }
        }
    }
}

/// A provider operation failed and one or more temporary-lock restorations failed too.
///
/// It is carried inside `anyhow::Error` so callers can both show the combined failure and retain
/// the structured cleanup warnings in durable reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreError {
    pub operation_error: String,
    pub warnings: Vec<StoreWarning>,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cleanup_errors = self
            .warnings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        write!(
            formatter,
            "operation failed: {}; lock restoration also failed: {cleanup_errors}",
            self.operation_error
        )
    }
}

impl std::error::Error for StoreError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreOutcome<T> {
    pub value: T,
    pub warnings: Vec<StoreWarning>,
}

impl<T> StoreOutcome<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            warnings: Vec::new(),
        }
    }

    pub fn with_warnings(value: T, warnings: Vec<StoreWarning>) -> Self {
        Self { value, warnings }
    }

    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> StoreOutcome<U> {
        StoreOutcome {
            value: map(self.value),
            warnings: self.warnings,
        }
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>>;
    async fn list_items(&self, collection_path: &str) -> Result<Vec<ItemInfo>>;

    async fn reveal_secret(&self, target: &ItemTarget) -> Result<StoreOutcome<SecretValue>>;
    async fn set_collection_label(
        &self,
        collection_path: &str,
        label: &str,
    ) -> Result<StoreOutcome<()>>;
    async fn set_item_label(&self, target: &ItemTarget, label: &str) -> Result<StoreOutcome<()>>;
    async fn set_item_attributes(
        &self,
        target: &ItemTarget,
        attributes: Attributes,
    ) -> Result<StoreOutcome<()>>;
    async fn replace_item_secret(
        &self,
        target: &ItemTarget,
        secret: &[u8],
        content_type: &str,
    ) -> Result<StoreOutcome<()>>;

    async fn create_collection(
        &self,
        collection: NewCollection,
    ) -> Result<StoreOutcome<CollectionInfo>>;
    async fn create_item(&self, item: NewItem) -> Result<StoreOutcome<ItemInfo>>;
    async fn delete_collection(&self, collection_path: &str) -> Result<StoreOutcome<()>>;
    async fn delete_item(&self, target: &ItemTarget) -> Result<StoreOutcome<()>>;

    /// An intentional lock/unlock action. Unlike temporary authorization, this state persists.
    async fn set_collection_locked(
        &self,
        collection_path: &str,
        locked: bool,
    ) -> Result<StoreOutcome<()>>;

    /// Retry lock restoration left pending by successful operations with cleanup warnings.
    async fn cleanup_temporary_unlocks(&self) -> Result<StoreOutcome<()>>;

    async fn export_metadata(&self) -> Result<MetadataFile> {
        let mut collections = Vec::new();
        for collection in self.list_collections().await? {
            let items = self
                .list_items(&collection.path)
                .await
                .with_context(|| format!("list items in collection {}", collection.path))?
                .into_iter()
                .map(|item| ItemMetadata {
                    path: item.path,
                    label: item.label,
                    locked: item.locked,
                    attributes: item.attributes,
                    created: item.created,
                    modified: item.modified,
                })
                .collect();
            collections.push(CollectionMetadata {
                path: collection.path,
                label: collection.label,
                locked: collection.locked,
                items,
            });
        }
        Ok(MetadataFile {
            version: 2,
            collections,
        }
        .sorted())
    }
}

mod memory;

pub use memory::{MemorySecretStore, StoreLogEntry, StoreOperation};
