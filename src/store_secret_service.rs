use crate::domain::*;
use crate::store::SecretStore;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use secret_service::{EncryptionType, SecretService};
use std::collections::{BTreeMap, HashMap};
use zbus::zvariant::OwnedObjectPath;

pub struct SecretServiceStore {
    service: SecretService<'static>,
}

impl SecretServiceStore {
    pub async fn connect() -> Result<Self> {
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("connect to Secret Service")?;
        Ok(Self { service })
    }

    fn path(path: &str) -> Result<OwnedObjectPath> {
        OwnedObjectPath::try_from(path.to_owned())
            .map_err(|err| anyhow!(err))
            .context("invalid object path")
    }

    fn attrs_ref(attrs: &Attributes) -> HashMap<&str, &str> {
        attrs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    async fn item_info(&self, collection_path: &str, item_path: String) -> Result<ItemInfo> {
        let item = self
            .service
            .get_item_by_path(Self::path(&item_path)?)
            .await?;
        let attributes = item
            .get_attributes()
            .await?
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        Ok(ItemInfo {
            collection_path: collection_path.to_owned(),
            path: item_path,
            label: item
                .get_label()
                .await
                .unwrap_or_else(|_| "<unlabeled>".to_owned()),
            locked: item.is_locked().await.context("read item lock state")?,
            attributes,
            created: item.get_created().await.ok(),
            modified: item.get_modified().await.ok(),
        })
    }

    async fn metadata_indexes(
        &self,
    ) -> Result<(
        BTreeMap<String, String>,
        BTreeMap<String, (String, Attributes)>,
    )> {
        let metadata = self.export_metadata().await?;
        let mut collections = BTreeMap::new();
        let mut items = BTreeMap::new();
        for collection in metadata.collections {
            collections.insert(collection.path, collection.label);
            for item in collection.items {
                items.insert(item.path, (item.label, item.attributes));
            }
        }
        Ok((collections, items))
    }
}

#[async_trait]
impl SecretStore for SecretServiceStore {
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let mut out = Vec::new();
        for collection in self.service.get_all_collections().await? {
            out.push(CollectionInfo {
                path: collection.collection_path.to_string(),
                label: collection
                    .get_label()
                    .await
                    .unwrap_or_else(|_| "<unlabeled>".to_owned()),
                locked: collection
                    .is_locked()
                    .await
                    .context("read collection lock state")?,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn list_items(&self, collection_path: &str) -> Result<Vec<ItemInfo>> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await?;
        let mut out = Vec::new();
        for item in collection.get_all_items().await? {
            out.push(
                self.item_info(collection_path, item.item_path.to_string())
                    .await?,
            );
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn reveal_secret(&self, item_path: &str) -> Result<SecretValue> {
        let item = self
            .service
            .get_item_by_path(Self::path(item_path)?)
            .await?;
        if item.is_locked().await? {
            item.unlock().await.context("unlock item")?;
        }
        let secret = item.get_secret().await.context("read secret")?;
        let content_type = item
            .get_secret_content_type()
            .await
            .context("read secret content type")?;
        Ok(SecretValue::new(secret, content_type))
    }

    async fn edit_item(
        &self,
        item_path: &str,
        label: Option<&str>,
        attributes: Option<Attributes>,
        secret: Option<(&[u8], &str)>,
    ) -> Result<()> {
        let item = self
            .service
            .get_item_by_path(Self::path(item_path)?)
            .await?;
        if item.is_locked().await? {
            item.unlock().await.context("unlock item")?;
        }
        if let Some(label) = label {
            item.set_label(label).await?;
        }
        if let Some(attributes) = attributes {
            item.set_attributes(Self::attrs_ref(&attributes)).await?;
        }
        if let Some((secret, content_type)) = secret {
            item.set_secret(secret, content_type).await?;
        }
        Ok(())
    }

    async fn create_collection(&self, new_collection: NewCollection) -> Result<CollectionInfo> {
        let collection = self
            .service
            .create_collection(&new_collection.label, &new_collection.alias)
            .await?;
        Ok(CollectionInfo {
            path: collection.collection_path.to_string(),
            label: collection.get_label().await.unwrap_or(new_collection.label),
            locked: collection.is_locked().await.unwrap_or(false),
        })
    }

    async fn create_item(&self, new_item: NewItem) -> Result<ItemInfo> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(&new_item.collection_path)?)
            .await?;
        let item = collection
            .create_item(
                &new_item.label,
                Self::attrs_ref(&new_item.attributes),
                new_item.secret.as_slice(),
                false,
                &new_item.content_type,
            )
            .await?;
        Ok(ItemInfo {
            collection_path: new_item.collection_path,
            path: item.item_path.to_string(),
            label: new_item.label,
            locked: false,
            attributes: new_item.attributes,
            created: None,
            modified: None,
        })
    }

    async fn delete_item(&self, item_path: &str) -> Result<()> {
        let item = self
            .service
            .get_item_by_path(Self::path(item_path)?)
            .await?;
        if item.is_locked().await? {
            item.unlock().await.context("unlock item")?;
        }
        item.delete().await.context("delete item")
    }

    async fn set_collection_locked(&self, collection_path: &str, locked: bool) -> Result<()> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(collection_path)?)
            .await?;
        if locked {
            collection.lock().await?
        } else {
            collection.unlock().await?
        }
        Ok(())
    }

    async fn export_metadata(&self) -> Result<MetadataFile> {
        let mut collections = Vec::new();
        for collection in self.list_collections().await? {
            let items = self
                .list_items(&collection.path)
                .await?
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

    async fn import_metadata(&self, metadata: MetadataFile) -> Result<usize> {
        anyhow::ensure!(
            matches!(metadata.version, 1 | 2),
            "unsupported metadata version {}",
            metadata.version
        );
        let (existing_collections, existing_items) = self.metadata_indexes().await?;
        let mut changed = 0;
        for collection in metadata.collections {
            if let Some(existing_label) = existing_collections.get(&collection.path) {
                if existing_label != &collection.label {
                    let existing = self
                        .service
                        .get_collection_by_path(Self::path(&collection.path)?)
                        .await?;
                    existing
                        .set_label(&collection.label)
                        .await
                        .context("update collection label")?;
                }
            }
            for item in collection.items {
                if let Some((existing_label, existing_attributes)) = existing_items.get(&item.path)
                {
                    let label_changed = existing_label != &item.label;
                    let attributes_changed = existing_attributes != &item.attributes;
                    if !label_changed && !attributes_changed {
                        continue;
                    }
                    self.edit_item(&item.path, Some(&item.label), Some(item.attributes), None)
                        .await?;
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    async fn preview_metadata_import(
        &self,
        metadata: &MetadataFile,
    ) -> Result<MetadataImportSummary> {
        anyhow::ensure!(
            matches!(metadata.version, 1 | 2),
            "unsupported metadata version {}",
            metadata.version
        );
        let (existing_collections, existing_items) = self.metadata_indexes().await?;
        let mut summary = MetadataImportSummary::default();
        for collection in &metadata.collections {
            match existing_collections.get(&collection.path) {
                Some(existing_label) => {
                    if existing_label != &collection.label {
                        summary.collections_changed += 1;
                    }
                }
                None => summary.paths_missing += 1,
            }
            for item in &collection.items {
                match existing_items.get(&item.path) {
                    Some((existing_label, existing_attributes)) => {
                        if existing_label != &item.label || existing_attributes != &item.attributes
                        {
                            summary.items_changed += 1;
                        }
                    }
                    None => summary.paths_missing += 1,
                }
            }
        }
        Ok(summary)
    }
}
