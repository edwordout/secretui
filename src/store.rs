use crate::domain::{
    Attributes, CollectionInfo, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile,
    NewCollection, NewItem, SecretValue,
};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

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

// Deterministic in-memory provider used by unit and integration-style tests.

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreOperation {
    ListCollections,
    ListItems,
    RevealSecret,
    SetCollectionLabel,
    SetItemLabel,
    SetItemAttributes,
    ReplaceItemSecret,
    CreateCollection,
    CreateItem,
    DeleteCollection,
    DeleteItem,
    SetCollectionLocked,
    UnlockCollection,
    UnlockItem,
    RelockCollection,
    RelockItem,
    CleanupTemporaryUnlocks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreLogEntry {
    pub operation: StoreOperation,
    pub target: Option<StoreTarget>,
}

#[derive(Clone)]
struct MemoryItem {
    info: ItemInfo,
    secret: Zeroizing<Vec<u8>>,
    content_type: String,
}

#[derive(Clone)]
struct InjectedFailure {
    operation: StoreOperation,
    occurrence: usize,
    message: String,
}

#[derive(Default)]
struct MemoryState {
    collections: BTreeMap<String, CollectionInfo>,
    items: BTreeMap<String, MemoryItem>,
    pending_relocks: BTreeSet<StoreTarget>,
    failures: Vec<InjectedFailure>,
    delays: BTreeMap<StoreOperation, Duration>,
    operation_counts: BTreeMap<StoreOperation, usize>,
    log: Vec<StoreLogEntry>,
    next_collection: u64,
    next_item: u64,
    next_timestamp: u64,
}

#[derive(Clone, Default)]
pub struct MemorySecretStore {
    state: Arc<Mutex<MemoryState>>,
}

impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("MemorySecretStore")
            .field("collection_count", &state.collections.len())
            .field("item_count", &state.items.len())
            .field("pending_relocks", &state.pending_relocks)
            .field("operation_count", &state.log.len())
            .finish()
    }
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn insert_collection(&self, collection: CollectionInfo) {
        self.state()
            .collections
            .insert(collection.path.clone(), collection);
    }

    /// Insert or replace a fixture item, including its binary or text secret.
    pub fn insert_item(
        &self,
        item: ItemInfo,
        secret: Vec<u8>,
        content_type: impl Into<String>,
    ) -> Result<()> {
        let mut state = self.state();
        if !state.collections.contains_key(&item.collection_path) {
            bail!("collection {} does not exist", item.collection_path);
        }
        state.next_timestamp = state
            .next_timestamp
            .max(item.created.unwrap_or_default())
            .max(item.modified.unwrap_or_default());
        state.items.insert(
            item.path.clone(),
            MemoryItem {
                info: item,
                secret: Zeroizing::new(secret),
                content_type: content_type.into(),
            },
        );
        Ok(())
    }

    /// Replace only provider-visible item metadata, preserving the stored secret.
    ///
    /// Tests can combine this with [`Self::set_delay`] from another clone to model an external
    /// concurrent mutation without ever reading or copying secret bytes.
    pub fn update_item_info(&self, item: ItemInfo) -> Result<()> {
        let mut state = self.state();
        if !state.collections.contains_key(&item.collection_path) {
            bail!("collection {} does not exist", item.collection_path);
        }
        state.next_timestamp = state
            .next_timestamp
            .max(item.created.unwrap_or_default())
            .max(item.modified.unwrap_or_default());
        let stored = state
            .items
            .get_mut(&item.path)
            .with_context(|| format!("item {} does not exist", item.path))?;
        stored.info = item;
        Ok(())
    }

    pub fn remove_collection(&self, collection_path: &str) {
        let mut state = self.state();
        state.collections.remove(collection_path);
        state
            .items
            .retain(|_, item| item.info.collection_path != collection_path);
    }

    pub fn remove_item(&self, item_path: &str) {
        self.state().items.remove(item_path);
    }

    /// Fail a one-based occurrence of an operation. Injecting `UnlockItem` or
    /// `UnlockCollection` models prompt cancellation; injecting a relock operation models
    /// provider cleanup failure.
    pub fn inject_failure(
        &self,
        operation: StoreOperation,
        occurrence: usize,
        message: impl Into<String>,
    ) {
        assert!(occurrence > 0, "failure occurrence is one-based");
        self.state().failures.push(InjectedFailure {
            operation,
            occurrence,
            message: message.into(),
        });
    }

    pub fn clear_failures(&self) {
        self.state().failures.clear();
    }

    pub fn set_delay(&self, operation: StoreOperation, delay: Duration) {
        self.state().delays.insert(operation, delay);
    }

    pub fn mutation_log(&self) -> Vec<StoreLogEntry> {
        self.state().log.clone()
    }

    pub fn operation_log(&self) -> Vec<StoreLogEntry> {
        self.mutation_log()
    }

    pub fn clear_log(&self) {
        self.state().log.clear();
    }

    pub fn pending_temporary_unlocks(&self) -> Vec<StoreTarget> {
        self.state().pending_relocks.iter().cloned().collect()
    }

    pub fn collection(&self, collection_path: &str) -> Option<CollectionInfo> {
        self.state().collections.get(collection_path).cloned()
    }

    pub fn item(&self, item_path: &str) -> Option<ItemInfo> {
        self.state()
            .items
            .get(item_path)
            .map(|item| item.info.clone())
    }

    async fn begin_operation(
        &self,
        operation: StoreOperation,
        target: Option<StoreTarget>,
    ) -> Result<()> {
        let (delay, failure) = {
            let mut state = self.state();
            state.log.push(StoreLogEntry { operation, target });
            let occurrence = state.operation_counts.entry(operation).or_default();
            *occurrence += 1;
            let occurrence = *occurrence;
            let delay = state.delays.get(&operation).copied();
            let failure = state
                .failures
                .iter()
                .find(|failure| failure.operation == operation && failure.occurrence == occurrence)
                .map(|failure| failure.message.clone());
            (delay, failure)
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(message) = failure {
            bail!("{operation:?} failed: {message}");
        }
        Ok(())
    }

    fn verify_target(state: &MemoryState, target: &ItemTarget) -> Result<()> {
        if !state.collections.contains_key(&target.collection_path) {
            bail!("collection {} does not exist", target.collection_path);
        }
        let item = state
            .items
            .get(&target.item_path)
            .with_context(|| format!("item {} does not exist", target.item_path))?;
        if item.info.collection_path != target.collection_path {
            bail!(
                "item {} belongs to collection {}, not {}",
                target.item_path,
                item.info.collection_path,
                target.collection_path
            );
        }
        Ok(())
    }

    async fn prepare_collection(&self, collection_path: &str) -> Result<Vec<StoreTarget>> {
        let locked = self
            .state()
            .collections
            .get(collection_path)
            .with_context(|| format!("collection {collection_path} does not exist"))?
            .locked;
        let target = StoreTarget::Collection(collection_path.to_owned());
        if !locked {
            return Ok(if self.state().pending_relocks.contains(&target) {
                vec![target]
            } else {
                Vec::new()
            });
        }

        {
            let mut state = self.state();
            state.pending_relocks.insert(target.clone());
        }
        if let Err(operation_error) = self
            .begin_operation(StoreOperation::UnlockCollection, Some(target.clone()))
            .await
        {
            return Err(self
                .finish_error(vec![target], operation_error, &BTreeSet::new())
                .await);
        }
        let update_result = {
            let mut state = self.state();
            if let Some(collection) = state.collections.get_mut(collection_path) {
                collection.locked = false;
                for item in state
                    .items
                    .values_mut()
                    .filter(|item| item.info.collection_path == collection_path)
                {
                    item.info.locked = false;
                }
                Ok(())
            } else {
                Err(anyhow!(
                    "collection {collection_path} disappeared after unlock"
                ))
            }
        };
        if let Err(operation_error) = update_result {
            return Err(self
                .finish_error(vec![target], operation_error, &BTreeSet::new())
                .await);
        }
        Ok(vec![target])
    }

    async fn prepare_item(&self, target: &ItemTarget) -> Result<Vec<StoreTarget>> {
        let (collection_locked, item_locked) = {
            let state = self.state();
            Self::verify_target(&state, target)?;
            (
                state.collections[&target.collection_path].locked,
                state.items[&target.item_path].info.locked,
            )
        };
        if collection_locked {
            return self.prepare_collection(&target.collection_path).await;
        }
        let collection_target = StoreTarget::Collection(target.collection_path.clone());
        if self.state().pending_relocks.contains(&collection_target) {
            return Ok(vec![collection_target]);
        }
        if !item_locked {
            let item_target = StoreTarget::Item(target.clone());
            return Ok(if self.state().pending_relocks.contains(&item_target) {
                vec![item_target]
            } else {
                Vec::new()
            });
        }

        let store_target = StoreTarget::Item(target.clone());
        self.state().pending_relocks.insert(store_target.clone());
        if let Err(operation_error) = self
            .begin_operation(StoreOperation::UnlockItem, Some(store_target.clone()))
            .await
        {
            return Err(self
                .finish_error(vec![store_target], operation_error, &BTreeSet::new())
                .await);
        }
        let update_result = {
            let mut state = self.state();
            if let Some(item) = state.items.get_mut(&target.item_path) {
                item.info.locked = false;
                Ok(())
            } else {
                Err(anyhow!(
                    "item {} disappeared after unlock",
                    target.item_path
                ))
            }
        };
        if let Err(operation_error) = update_result {
            return Err(self
                .finish_error(vec![store_target], operation_error, &BTreeSet::new())
                .await);
        }
        Ok(vec![store_target])
    }

    async fn relock(&self, target: &StoreTarget) -> Result<()> {
        let operation = match target {
            StoreTarget::Collection(_) => StoreOperation::RelockCollection,
            StoreTarget::Item(_) => StoreOperation::RelockItem,
        };
        self.begin_operation(operation, Some(target.clone()))
            .await?;
        let mut state = self.state();
        match target {
            StoreTarget::Collection(collection_path) => {
                state
                    .collections
                    .get_mut(collection_path)
                    .with_context(|| format!("collection {collection_path} disappeared"))?
                    .locked = true;
                for item in state
                    .items
                    .values_mut()
                    .filter(|item| item.info.collection_path == *collection_path)
                {
                    item.info.locked = true;
                }
                state.pending_relocks.retain(|pending| match pending {
                    StoreTarget::Collection(path) => path != collection_path,
                    StoreTarget::Item(item) => item.collection_path != collection_path.as_str(),
                });
            }
            StoreTarget::Item(item_target) => {
                Self::verify_target(&state, item_target)?;
                state
                    .items
                    .get_mut(&item_target.item_path)
                    .expect("target was verified")
                    .info
                    .locked = true;
            }
        }
        state.pending_relocks.remove(target);
        Ok(())
    }

    async fn finish_success<T>(
        &self,
        restores: Vec<StoreTarget>,
        value: T,
        skip: &BTreeSet<StoreTarget>,
    ) -> StoreOutcome<T> {
        let mut warnings = Vec::new();
        for target in restores.into_iter().rev() {
            if skip.contains(&target) {
                self.state().pending_relocks.remove(&target);
                continue;
            }
            if let Err(error) = self.relock(&target).await {
                warnings.push(StoreWarning::TemporaryRelockFailed {
                    target,
                    error: format!("{error:#}"),
                });
            }
        }
        StoreOutcome::with_warnings(value, warnings)
    }

    async fn finish_error(
        &self,
        restores: Vec<StoreTarget>,
        operation_error: anyhow::Error,
        skip: &BTreeSet<StoreTarget>,
    ) -> anyhow::Error {
        let outcome = self.finish_success(restores, (), skip).await;
        if outcome.warnings.is_empty() {
            return operation_error;
        }
        StoreError {
            operation_error: format!("{operation_error:#}"),
            warnings: outcome.warnings,
        }
        .into()
    }
}

#[async_trait]
impl SecretStore for MemorySecretStore {
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        self.begin_operation(StoreOperation::ListCollections, None)
            .await?;
        Ok(self.state().collections.values().cloned().collect())
    }

    async fn list_items(&self, collection_path: &str) -> Result<Vec<ItemInfo>> {
        self.begin_operation(
            StoreOperation::ListItems,
            Some(StoreTarget::Collection(collection_path.to_owned())),
        )
        .await?;
        let state = self.state();
        if !state.collections.contains_key(collection_path) {
            bail!("collection {collection_path} does not exist");
        }
        Ok(state
            .items
            .values()
            .filter(|item| item.info.collection_path == collection_path)
            .map(|item| item.info.clone())
            .collect())
    }

    async fn reveal_secret(&self, target: &ItemTarget) -> Result<StoreOutcome<SecretValue>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::RevealSecret,
                Some(StoreTarget::Item(target.clone())),
            )
            .await?;
            let state = self.state();
            Self::verify_target(&state, target)?;
            let item = &state.items[&target.item_path];
            Ok(SecretValue::new(
                item.secret.to_vec(),
                item.content_type.clone(),
            ))
        }
        .await;
        match operation {
            Ok(value) => Ok(self.finish_success(restores, value, &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn set_collection_label(
        &self,
        collection_path: &str,
        label: &str,
    ) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_collection(collection_path).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::SetCollectionLabel,
                Some(StoreTarget::Collection(collection_path.to_owned())),
            )
            .await?;
            self.state()
                .collections
                .get_mut(collection_path)
                .with_context(|| format!("collection {collection_path} disappeared"))?
                .label = label.to_owned();
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(self.finish_success(restores, (), &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn set_item_label(&self, target: &ItemTarget, label: &str) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::SetItemLabel,
                Some(StoreTarget::Item(target.clone())),
            )
            .await?;
            let mut state = self.state();
            Self::verify_target(&state, target)?;
            state.next_timestamp += 1;
            let modified = state.next_timestamp;
            let item = state.items.get_mut(&target.item_path).unwrap();
            item.info.label = label.to_owned();
            item.info.modified = Some(modified);
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(self.finish_success(restores, (), &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn set_item_attributes(
        &self,
        target: &ItemTarget,
        attributes: Attributes,
    ) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::SetItemAttributes,
                Some(StoreTarget::Item(target.clone())),
            )
            .await?;
            let mut state = self.state();
            Self::verify_target(&state, target)?;
            state.next_timestamp += 1;
            let modified = state.next_timestamp;
            let item = state.items.get_mut(&target.item_path).unwrap();
            item.info.attributes = attributes;
            item.info.modified = Some(modified);
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(self.finish_success(restores, (), &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn replace_item_secret(
        &self,
        target: &ItemTarget,
        secret: &[u8],
        content_type: &str,
    ) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::ReplaceItemSecret,
                Some(StoreTarget::Item(target.clone())),
            )
            .await?;
            let mut state = self.state();
            Self::verify_target(&state, target)?;
            state.next_timestamp += 1;
            let modified = state.next_timestamp;
            let item = state.items.get_mut(&target.item_path).unwrap();
            item.secret.zeroize();
            *item.secret = secret.to_vec();
            item.content_type = content_type.to_owned();
            item.info.modified = Some(modified);
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(self.finish_success(restores, (), &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn create_collection(
        &self,
        collection: NewCollection,
    ) -> Result<StoreOutcome<CollectionInfo>> {
        self.begin_operation(StoreOperation::CreateCollection, None)
            .await?;
        let mut state = self.state();
        state.next_collection += 1;
        let path = format!(
            "/org/freedesktop/secrets/collection/secretui_memory_{}",
            state.next_collection
        );
        let info = CollectionInfo {
            path: path.clone(),
            label: collection.label,
            locked: false,
        };
        state.collections.insert(path, info.clone());
        Ok(StoreOutcome::new(info))
    }

    async fn create_item(&self, item: NewItem) -> Result<StoreOutcome<ItemInfo>> {
        let restores = self.prepare_collection(&item.collection_path).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::CreateItem,
                Some(StoreTarget::Collection(item.collection_path.clone())),
            )
            .await?;
            let mut state = self.state();
            if !state.collections.contains_key(&item.collection_path) {
                bail!("collection {} disappeared", item.collection_path);
            }
            state.next_item += 1;
            state.next_timestamp += 1;
            let timestamp = state.next_timestamp;
            let path = format!(
                "{}/secretui_memory_{}",
                item.collection_path, state.next_item
            );
            let info = ItemInfo {
                collection_path: item.collection_path,
                path: path.clone(),
                label: item.label,
                locked: false,
                attributes: item.attributes,
                created: Some(timestamp),
                modified: Some(timestamp),
            };
            state.items.insert(
                path,
                MemoryItem {
                    info: info.clone(),
                    secret: Zeroizing::new(item.secret.as_slice().to_vec()),
                    content_type: item.content_type,
                },
            );
            Ok(info)
        }
        .await;
        match operation {
            Ok(info) => Ok(self.finish_success(restores, info, &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn delete_collection(&self, collection_path: &str) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_collection(collection_path).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::DeleteCollection,
                Some(StoreTarget::Collection(collection_path.to_owned())),
            )
            .await?;
            let mut state = self.state();
            if state.collections.remove(collection_path).is_none() {
                bail!("collection {collection_path} disappeared");
            }
            state
                .items
                .retain(|_, item| item.info.collection_path != collection_path);
            Ok(())
        }
        .await;
        let skip = BTreeSet::from([StoreTarget::Collection(collection_path.to_owned())]);
        match operation {
            Ok(()) => {
                self.state().pending_relocks.retain(|target| match target {
                    StoreTarget::Collection(path) => path != collection_path,
                    StoreTarget::Item(item) => item.collection_path != collection_path,
                });
                Ok(self.finish_success(restores, (), &skip).await)
            }
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn delete_item(&self, target: &ItemTarget) -> Result<StoreOutcome<()>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            self.begin_operation(
                StoreOperation::DeleteItem,
                Some(StoreTarget::Item(target.clone())),
            )
            .await?;
            let mut state = self.state();
            Self::verify_target(&state, target)?;
            state.items.remove(&target.item_path);
            Ok(())
        }
        .await;
        let deleted_item = StoreTarget::Item(target.clone());
        let skip = BTreeSet::from([deleted_item]);
        match operation {
            Ok(()) => {
                self.state()
                    .pending_relocks
                    .remove(&StoreTarget::Item(target.clone()));
                Ok(self.finish_success(restores, (), &skip).await)
            }
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn set_collection_locked(
        &self,
        collection_path: &str,
        locked: bool,
    ) -> Result<StoreOutcome<()>> {
        self.begin_operation(
            StoreOperation::SetCollectionLocked,
            Some(StoreTarget::Collection(collection_path.to_owned())),
        )
        .await?;
        let mut state = self.state();
        state
            .collections
            .get_mut(collection_path)
            .with_context(|| format!("collection {collection_path} does not exist"))?
            .locked = locked;
        for item in state
            .items
            .values_mut()
            .filter(|item| item.info.collection_path == collection_path)
        {
            item.info.locked = locked;
        }
        state.pending_relocks.retain(|target| match target {
            StoreTarget::Collection(path) => path != collection_path,
            StoreTarget::Item(item) => item.collection_path != collection_path,
        });
        Ok(StoreOutcome::new(()))
    }

    async fn cleanup_temporary_unlocks(&self) -> Result<StoreOutcome<()>> {
        self.begin_operation(StoreOperation::CleanupTemporaryUnlocks, None)
            .await?;
        let targets = self
            .state()
            .pending_relocks
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        Ok(self.finish_success(targets, (), &BTreeSet::new()).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked_store() -> (MemorySecretStore, ItemTarget) {
        let store = MemorySecretStore::new();
        let collection_path = "/org/freedesktop/secrets/collection/test";
        let item_path = "/org/freedesktop/secrets/collection/test/item";
        store.insert_collection(CollectionInfo {
            path: collection_path.into(),
            label: "test".into(),
            locked: true,
        });
        store
            .insert_item(
                ItemInfo {
                    collection_path: collection_path.into(),
                    path: item_path.into(),
                    label: "item".into(),
                    locked: true,
                    attributes: Attributes::new(),
                    created: Some(1),
                    modified: Some(1),
                },
                vec![0, 0xff],
                "application/octet-stream",
            )
            .unwrap();
        (store, ItemTarget::new(collection_path, item_path))
    }

    #[tokio::test]
    async fn reveal_restores_a_locked_collection_and_never_logs_secret_bytes() {
        let (store, target) = locked_store();
        let outcome = store.reveal_secret(&target).await.unwrap();
        assert_eq!(outcome.value.secret.as_slice(), &[0, 0xff]);
        assert!(outcome.warnings.is_empty());
        assert!(store.collection(&target.collection_path).unwrap().locked);
        assert!(store.item(&target.item_path).unwrap().locked);
        assert!(store
            .mutation_log()
            .iter()
            .all(|entry| !format!("{entry:?}").contains("255")));
    }

    #[tokio::test]
    async fn successful_operation_reports_and_retains_failed_relock() {
        let (store, target) = locked_store();
        store.inject_failure(StoreOperation::RelockCollection, 1, "provider unavailable");
        let outcome = store.set_item_label(&target, "changed").await.unwrap();
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(store.item(&target.item_path).unwrap().label, "changed");
        assert_eq!(
            store.pending_temporary_unlocks(),
            vec![StoreTarget::Collection(target.collection_path.clone())]
        );

        let cleanup = store.cleanup_temporary_unlocks().await.unwrap();
        assert!(cleanup.warnings.is_empty());
        assert!(store.pending_temporary_unlocks().is_empty());
        assert!(store.collection(&target.collection_path).unwrap().locked);
    }

    #[tokio::test]
    async fn next_operation_retries_a_pending_collection_relock() {
        let (store, target) = locked_store();
        store.inject_failure(StoreOperation::RelockCollection, 1, "provider unavailable");
        let edit = store.set_item_label(&target, "changed").await.unwrap();
        assert_eq!(edit.warnings.len(), 1);
        assert!(!store.collection(&target.collection_path).unwrap().locked);

        let reveal = store.reveal_secret(&target).await.unwrap();
        assert!(reveal.warnings.is_empty());
        assert!(store.collection(&target.collection_path).unwrap().locked);
        assert!(store.pending_temporary_unlocks().is_empty());
    }

    #[tokio::test]
    async fn operation_and_relock_failures_are_combined() {
        let (store, target) = locked_store();
        store.inject_failure(StoreOperation::SetItemLabel, 1, "write rejected");
        store.inject_failure(StoreOperation::RelockCollection, 1, "relock rejected");
        let error = store.set_item_label(&target, "changed").await.unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("write rejected"));
        assert!(message.contains("relock rejected"));
        assert_eq!(store.item(&target.item_path).unwrap().label, "item");
        assert_eq!(store.pending_temporary_unlocks().len(), 1);
    }

    #[tokio::test]
    async fn failed_secret_read_still_restores_the_collection_lock() {
        let (store, target) = locked_store();
        store.inject_failure(StoreOperation::RevealSecret, 1, "read rejected");

        let error = store.reveal_secret(&target).await.err().unwrap();
        assert!(format!("{error:#}").contains("read rejected"));
        assert!(store.collection(&target.collection_path).unwrap().locked);
        assert!(store.item(&target.item_path).unwrap().locked);
        assert!(store.pending_temporary_unlocks().is_empty());
    }

    #[tokio::test]
    async fn failed_delete_relocks_item_but_successful_delete_does_not() {
        let (store, target) = locked_store();
        store
            .set_collection_locked(&target.collection_path, false)
            .await
            .unwrap();
        store
            .state()
            .items
            .get_mut(&target.item_path)
            .unwrap()
            .info
            .locked = true;
        store.inject_failure(StoreOperation::DeleteItem, 1, "delete rejected");
        assert!(store.delete_item(&target).await.is_err());
        assert!(store.item(&target.item_path).unwrap().locked);

        store.clear_failures();
        store.delete_item(&target).await.unwrap();
        assert!(store.item(&target.item_path).is_none());
        assert!(store.pending_temporary_unlocks().is_empty());
    }

    #[tokio::test]
    async fn successful_delete_restores_an_originally_locked_collection() {
        let (store, target) = locked_store();
        let outcome = store.delete_item(&target).await.unwrap();
        assert!(outcome.warnings.is_empty());
        assert!(store.item(&target.item_path).is_none());
        assert!(store.collection(&target.collection_path).unwrap().locked);
        assert!(store.pending_temporary_unlocks().is_empty());
    }

    #[tokio::test]
    async fn intentional_unlock_is_persistent_and_not_a_cleanup_target() {
        let (store, target) = locked_store();
        store.inject_failure(StoreOperation::RelockCollection, 1, "relock rejected");
        let edit = store.set_item_label(&target, "changed").await.unwrap();
        assert_eq!(edit.warnings.len(), 1);
        assert_eq!(store.pending_temporary_unlocks().len(), 1);

        store
            .set_collection_locked(&target.collection_path, false)
            .await
            .unwrap();
        assert!(!store.collection(&target.collection_path).unwrap().locked);
        assert!(store.pending_temporary_unlocks().is_empty());

        store.cleanup_temporary_unlocks().await.unwrap();
        assert!(!store.collection(&target.collection_path).unwrap().locked);
    }

    #[tokio::test]
    async fn deleting_collection_clears_pending_item_relocks() {
        let (store, target) = locked_store();
        store
            .set_collection_locked(&target.collection_path, false)
            .await
            .unwrap();
        store
            .state()
            .items
            .get_mut(&target.item_path)
            .unwrap()
            .info
            .locked = true;
        store.inject_failure(StoreOperation::RelockItem, 1, "relock rejected");
        let edit = store.set_item_label(&target, "changed").await.unwrap();
        assert_eq!(edit.warnings.len(), 1);
        assert_eq!(store.pending_temporary_unlocks().len(), 1);

        store
            .delete_collection(&target.collection_path)
            .await
            .unwrap();
        assert!(store.pending_temporary_unlocks().is_empty());
        assert!(store.collection(&target.collection_path).is_none());
    }

    #[tokio::test]
    async fn creating_an_item_restores_the_collection_lock() {
        let (store, target) = locked_store();
        let outcome = store
            .create_item(NewItem {
                collection_path: target.collection_path.clone(),
                label: "new item".into(),
                attributes: Attributes::new(),
                secret: crate::domain::SecretBytes::new(vec![1, 2, 3]),
                content_type: "application/octet-stream".into(),
            })
            .await
            .unwrap();
        assert!(outcome.warnings.is_empty());
        assert!(store.collection(&target.collection_path).unwrap().locked);
        assert!(store.item(&outcome.value.path).unwrap().locked);
    }

    #[tokio::test]
    async fn parent_mismatch_fails_before_unlock_or_write() {
        let (store, mut target) = locked_store();
        store.insert_collection(CollectionInfo {
            path: "/org/freedesktop/secrets/collection/other".into(),
            label: "other".into(),
            locked: false,
        });
        target.collection_path = "/org/freedesktop/secrets/collection/other".into();
        let error = store.set_item_label(&target, "changed").await.unwrap_err();
        assert!(format!("{error:#}").contains("belongs to collection"));
        assert!(store.mutation_log().is_empty());
    }
}
