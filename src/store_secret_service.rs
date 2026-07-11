use crate::domain::*;
use crate::store::{ItemTarget, SecretStore, StoreError, StoreOutcome, StoreTarget, StoreWarning};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use secret_service::{EncryptionType, SecretService};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard};
use zbus::zvariant::OwnedObjectPath;
use zeroize::Zeroizing;

pub struct SecretServiceStore {
    service: SecretService<'static>,
    pending_relocks: Mutex<BTreeSet<StoreTarget>>,
}

impl SecretServiceStore {
    pub async fn connect() -> Result<Self> {
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("connect to Secret Service")?;
        Ok(Self {
            service,
            pending_relocks: Mutex::new(BTreeSet::new()),
        })
    }

    fn pending_relocks(&self) -> MutexGuard<'_, BTreeSet<StoreTarget>> {
        self.pending_relocks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn path(path: &str) -> Result<OwnedObjectPath> {
        OwnedObjectPath::try_from(path.to_owned())
            .map_err(|error| anyhow!(error))
            .with_context(|| format!("invalid D-Bus object path {path:?}"))
    }

    fn attrs_ref(attributes: &Attributes) -> HashMap<&str, &str> {
        attributes
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect()
    }

    async fn collection_info(&self, collection_path: &str) -> Result<CollectionInfo> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await
            .with_context(|| format!("open collection {collection_path}"))?;
        let actual_path = collection.collection_path.to_string();
        if actual_path != collection_path {
            bail!(
                "provider returned collection {actual_path} when {collection_path} was requested"
            );
        }
        Ok(CollectionInfo {
            path: actual_path,
            label: collection
                .get_label()
                .await
                .with_context(|| format!("read label for collection {collection_path}"))?,
            locked: collection
                .is_locked()
                .await
                .with_context(|| format!("read lock state for collection {collection_path}"))?,
        })
    }

    async fn verify_item_parent(&self, target: &ItemTarget) -> Result<()> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(&target.collection_path)?)
            .await
            .with_context(|| format!("open collection {}", target.collection_path))?;
        let items = collection
            .get_all_items()
            .await
            .with_context(|| format!("list items in collection {}", target.collection_path))?;
        if !items
            .iter()
            .any(|item| item.item_path.as_str() == target.item_path)
        {
            bail!(
                "item {} is not a member of collection {}",
                target.item_path,
                target.collection_path
            );
        }
        Ok(())
    }

    async fn item_info(&self, target: &ItemTarget) -> Result<ItemInfo> {
        self.verify_item_parent(target).await?;
        self.item_info_after_parent_verified(target).await
    }

    async fn item_info_after_parent_verified(&self, target: &ItemTarget) -> Result<ItemInfo> {
        let item = self
            .service
            .get_item_by_path(Self::path(&target.item_path)?)
            .await
            .with_context(|| format!("open item {}", target.item_path))?;
        let actual_path = item.item_path.to_string();
        if actual_path != target.item_path {
            bail!(
                "provider returned item {actual_path} when {} was requested",
                target.item_path
            );
        }
        let label = item
            .get_label()
            .await
            .with_context(|| format!("read label for item {}", target.item_path))?;
        let locked = item
            .is_locked()
            .await
            .with_context(|| format!("read lock state for item {}", target.item_path))?;
        let attributes = item
            .get_attributes()
            .await
            .with_context(|| format!("read attributes for item {}", target.item_path))?
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let created = item
            .get_created()
            .await
            .with_context(|| format!("read creation timestamp for item {}", target.item_path))?;
        let modified = item.get_modified().await.with_context(|| {
            format!("read modification timestamp for item {}", target.item_path)
        })?;
        Ok(ItemInfo {
            collection_path: target.collection_path.clone(),
            path: actual_path,
            label,
            locked,
            attributes,
            created: Some(created),
            modified: Some(modified),
        })
    }

    async fn prepare_collection(&self, collection_path: &str) -> Result<Vec<StoreTarget>> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await
            .with_context(|| format!("open collection {collection_path}"))?;
        let locked = collection
            .is_locked()
            .await
            .with_context(|| format!("read lock state for collection {collection_path}"))?;
        let target = StoreTarget::Collection(collection_path.to_owned());
        if !locked {
            return Ok(if self.pending_relocks().contains(&target) {
                vec![target]
            } else {
                Vec::new()
            });
        }

        self.pending_relocks().insert(target.clone());
        let unlock_result = async {
            collection
                .unlock()
                .await
                .with_context(|| format!("temporarily unlock collection {collection_path}"))?;
            if collection
                .is_locked()
                .await
                .with_context(|| format!("verify unlock for collection {collection_path}"))?
            {
                bail!("provider left collection {collection_path} locked after unlock");
            }
            Ok(())
        }
        .await;
        if let Err(operation_error) = unlock_result {
            return Err(self
                .finish_error(vec![target], operation_error, &BTreeSet::new())
                .await);
        }
        Ok(vec![target])
    }

    async fn prepare_item(&self, target: &ItemTarget) -> Result<Vec<StoreTarget>> {
        self.verify_item_parent(target).await?;
        let collection = self
            .service
            .get_collection_by_path(Self::path(&target.collection_path)?)
            .await
            .with_context(|| format!("open collection {}", target.collection_path))?;
        let item = self
            .service
            .get_item_by_path(Self::path(&target.item_path)?)
            .await
            .with_context(|| format!("open item {}", target.item_path))?;
        let collection_locked = collection.is_locked().await.with_context(|| {
            format!("read lock state for collection {}", target.collection_path)
        })?;
        let item_locked = item
            .is_locked()
            .await
            .with_context(|| format!("read lock state for item {}", target.item_path))?;

        if collection_locked {
            let restores = self.prepare_collection(&target.collection_path).await?;
            let unlocked_item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("reopen item {} after unlock", target.item_path))?;
            let still_locked = unlocked_item
                .is_locked()
                .await
                .with_context(|| format!("verify unlock for item {}", target.item_path))?;
            if still_locked {
                let operation_error = anyhow!(
                    "item {} remained locked after temporarily unlocking collection {}",
                    target.item_path,
                    target.collection_path
                );
                return Err(self
                    .finish_error(restores, operation_error, &BTreeSet::new())
                    .await);
            }
            return Ok(restores);
        }
        let collection_target = StoreTarget::Collection(target.collection_path.clone());
        if self.pending_relocks().contains(&collection_target) {
            return Ok(vec![collection_target]);
        }
        if !item_locked {
            let item_target = StoreTarget::Item(target.clone());
            return Ok(if self.pending_relocks().contains(&item_target) {
                vec![item_target]
            } else {
                Vec::new()
            });
        }

        let store_target = StoreTarget::Item(target.clone());
        self.pending_relocks().insert(store_target.clone());
        let unlock_result = async {
            item.unlock()
                .await
                .with_context(|| format!("temporarily unlock item {}", target.item_path))?;
            if item
                .is_locked()
                .await
                .with_context(|| format!("verify unlock for item {}", target.item_path))?
            {
                bail!(
                    "provider left item {} locked after unlock",
                    target.item_path
                );
            }
            Ok(())
        }
        .await;
        if let Err(operation_error) = unlock_result {
            return Err(self
                .finish_error(vec![store_target], operation_error, &BTreeSet::new())
                .await);
        }
        Ok(vec![store_target])
    }

    async fn relock(&self, target: &StoreTarget) -> Result<()> {
        match target {
            StoreTarget::Collection(collection_path) => {
                let collection = self
                    .service
                    .get_collection_by_path(Self::path(collection_path)?)
                    .await
                    .with_context(|| format!("open collection {collection_path} for relock"))?;
                if collection
                    .is_locked()
                    .await
                    .with_context(|| format!("read lock state for collection {collection_path}"))?
                {
                    self.pending_relocks().retain(|pending| match pending {
                        StoreTarget::Collection(path) => path != collection_path,
                        StoreTarget::Item(item) => item.collection_path != collection_path.as_str(),
                    });
                    return Ok(());
                }
                collection
                    .lock()
                    .await
                    .with_context(|| format!("relock collection {collection_path}"))?;
                if !collection
                    .is_locked()
                    .await
                    .with_context(|| format!("verify relock for collection {collection_path}"))?
                {
                    bail!("provider left collection {collection_path} unlocked after relock");
                }
                self.pending_relocks().retain(|pending| match pending {
                    StoreTarget::Collection(path) => path != collection_path,
                    StoreTarget::Item(item) => item.collection_path != collection_path.as_str(),
                });
            }
            StoreTarget::Item(item_target) => {
                self.verify_item_parent(item_target).await?;
                let item = self
                    .service
                    .get_item_by_path(Self::path(&item_target.item_path)?)
                    .await
                    .with_context(|| format!("open item {} for relock", item_target.item_path))?;
                if item.is_locked().await.with_context(|| {
                    format!("read lock state for item {}", item_target.item_path)
                })? {
                    self.pending_relocks().remove(target);
                    return Ok(());
                }
                item.lock()
                    .await
                    .with_context(|| format!("relock item {}", item_target.item_path))?;
                if !item
                    .is_locked()
                    .await
                    .with_context(|| format!("verify relock for item {}", item_target.item_path))?
                {
                    bail!(
                        "provider left item {} unlocked after relock",
                        item_target.item_path
                    );
                }
            }
        }
        self.pending_relocks().remove(target);
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
                self.pending_relocks().remove(&target);
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
impl SecretStore for SecretServiceStore {
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let provider_collections = self
            .service
            .get_all_collections()
            .await
            .context("list Secret Service collections")?;
        let mut collections = Vec::with_capacity(provider_collections.len());
        for collection in provider_collections {
            let path = collection.collection_path.to_string();
            collections.push(CollectionInfo {
                path: path.clone(),
                label: collection
                    .get_label()
                    .await
                    .with_context(|| format!("read label for collection {path}"))?,
                locked: collection
                    .is_locked()
                    .await
                    .with_context(|| format!("read lock state for collection {path}"))?,
            });
        }
        collections.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(collections)
    }

    async fn list_items(&self, collection_path: &str) -> Result<Vec<ItemInfo>> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await
            .with_context(|| format!("open collection {collection_path}"))?;
        let provider_items = collection
            .get_all_items()
            .await
            .with_context(|| format!("list items in collection {collection_path}"))?;
        let mut items = Vec::with_capacity(provider_items.len());
        for provider_item in provider_items {
            let target = ItemTarget::new(collection_path, provider_item.item_path.to_string());
            items.push(self.item_info_after_parent_verified(&target).await?);
        }
        items.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(items)
    }

    async fn reveal_secret(&self, target: &ItemTarget) -> Result<StoreOutcome<SecretValue>> {
        let restores = self.prepare_item(target).await?;
        let operation = async {
            let item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("open item {}", target.item_path))?;
            let content_type = item.get_secret_content_type().await.with_context(|| {
                format!("read secret content type for item {}", target.item_path)
            })?;
            let mut secret = Zeroizing::new(
                item.get_secret()
                    .await
                    .with_context(|| format!("read secret for item {}", target.item_path))?,
            );
            Ok(SecretValue::new(std::mem::take(&mut *secret), content_type))
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
            let collection = self
                .service
                .get_collection_by_path(Self::path(collection_path)?)
                .await
                .with_context(|| format!("open collection {collection_path}"))?;
            collection
                .set_label(label)
                .await
                .with_context(|| {
                    format!(
                        "set label for collection {collection_path}; update may already have occurred"
                    )
                })?;
            let observed = collection.get_label().await.with_context(|| {
                format!(
                    "verify label for collection {collection_path}; update may already have occurred"
                )
            })?;
            if observed != label {
                bail!(
                    "provider reported a different label after updating collection {collection_path}"
                );
            }
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
            let item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("open item {}", target.item_path))?;
            item.set_label(label).await.with_context(|| {
                format!(
                    "set label for item {}; update may already have occurred",
                    target.item_path
                )
            })?;
            let observed = item.get_label().await.with_context(|| {
                format!(
                    "verify label for item {}; update may already have occurred",
                    target.item_path
                )
            })?;
            if observed != label {
                bail!(
                    "provider reported a different label after updating item {}",
                    target.item_path
                );
            }
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
            let item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("open item {}", target.item_path))?;
            item.set_attributes(Self::attrs_ref(&attributes))
                .await
                .with_context(|| {
                    format!(
                        "set attributes for item {}; update may already have occurred",
                        target.item_path
                    )
                })?;
            let observed = item.get_attributes().await.with_context(|| {
                format!(
                    "verify attributes for item {}; update may already have occurred",
                    target.item_path
                )
            })?;
            let observed = observed.into_iter().collect::<BTreeMap<_, _>>();
            if observed != attributes {
                bail!(
                    "provider reported different attributes after updating item {}",
                    target.item_path
                );
            }
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
            let item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("open item {}", target.item_path))?;
            item.set_secret(secret, content_type)
                .await
                .with_context(|| {
                    format!(
                        "replace secret for item {}; replacement may already have occurred",
                        target.item_path
                    )
                })
        }
        .await;
        match operation {
            Ok(()) => Ok(self.finish_success(restores, (), &BTreeSet::new()).await),
            Err(error) => Err(self.finish_error(restores, error, &BTreeSet::new()).await),
        }
    }

    async fn create_collection(
        &self,
        new_collection: NewCollection,
    ) -> Result<StoreOutcome<CollectionInfo>> {
        let collection = self
            .service
            .create_collection(&new_collection.label, &new_collection.alias)
            .await
            .context("create collection; creation may already have occurred")?;
        let path = collection.collection_path.to_string();
        let info = self.collection_info(&path).await.with_context(|| {
            format!("verify newly created collection {path}; creation may already have occurred")
        })?;
        if info.label != new_collection.label {
            bail!(
                "new collection {path} has an unexpected label; creation may already have occurred"
            );
        }
        Ok(StoreOutcome::new(info))
    }

    async fn create_item(&self, new_item: NewItem) -> Result<StoreOutcome<ItemInfo>> {
        let restores = self.prepare_collection(&new_item.collection_path).await?;
        let operation = async {
            let collection = self
                .service
                .get_collection_by_path(Self::path(&new_item.collection_path)?)
                .await
                .with_context(|| format!("open collection {}", new_item.collection_path))?;
            let item = collection
                .create_item(
                    &new_item.label,
                    Self::attrs_ref(&new_item.attributes),
                    new_item.secret.as_slice(),
                    false,
                    &new_item.content_type,
                )
                .await
                .with_context(|| {
                    format!(
                        "create item in collection {}; creation may already have occurred",
                        new_item.collection_path
                    )
                })?;
            let target = ItemTarget::new(&new_item.collection_path, item.item_path.to_string());
            let info = self.item_info(&target).await.with_context(|| {
                format!(
                    "verify newly created item {}; creation may already have occurred",
                    target.item_path
                )
            })?;
            if info.label != new_item.label || info.attributes != new_item.attributes {
                bail!(
                    "new item {} has unexpected metadata; creation may already have occurred",
                    target.item_path
                );
            }
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
            let collection = self
                .service
                .get_collection_by_path(Self::path(collection_path)?)
                .await
                .with_context(|| format!("open collection {collection_path}"))?;
            collection.delete().await.with_context(|| {
                format!("delete collection {collection_path}; deletion may already have occurred")
            })
        }
        .await;
        let skip = BTreeSet::from([StoreTarget::Collection(collection_path.to_owned())]);
        match operation {
            Ok(()) => {
                self.pending_relocks().retain(|target| match target {
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
            let item = self
                .service
                .get_item_by_path(Self::path(&target.item_path)?)
                .await
                .with_context(|| format!("open item {}", target.item_path))?;
            item.delete().await.with_context(|| {
                format!(
                    "delete item {}; deletion may already have occurred",
                    target.item_path
                )
            })
        }
        .await;
        let skip = BTreeSet::from([StoreTarget::Item(target.clone())]);
        match operation {
            Ok(()) => {
                self.pending_relocks()
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
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await
            .with_context(|| format!("open collection {collection_path}"))?;
        if locked {
            collection.lock().await.with_context(|| {
                format!("lock collection {collection_path}; lock may already have occurred")
            })?;
        } else {
            collection.unlock().await.with_context(|| {
                format!("unlock collection {collection_path}; unlock may already have occurred")
            })?;
        }
        let observed = collection
            .is_locked()
            .await
            .with_context(|| {
                format!(
                    "verify lock state for collection {collection_path}; requested lock change may already have occurred"
                )
            })?;
        if observed != locked {
            bail!(
                "provider reported collection {collection_path} as {} after requesting {}",
                if observed { "locked" } else { "unlocked" },
                if locked { "locked" } else { "unlocked" }
            );
        }
        self.pending_relocks().retain(|target| match target {
            StoreTarget::Collection(path) => path != collection_path,
            StoreTarget::Item(item) => item.collection_path != collection_path,
        });
        Ok(StoreOutcome::new(()))
    }

    async fn cleanup_temporary_unlocks(&self) -> Result<StoreOutcome<()>> {
        let targets = self.pending_relocks().iter().cloned().collect::<Vec<_>>();
        Ok(self.finish_success(targets, (), &BTreeSet::new()).await)
    }
}
