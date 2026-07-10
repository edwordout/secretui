use crate::domain::*;
use crate::store::SecretStore;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use secret_service::{EncryptionType, SecretService};
use std::collections::{BTreeMap, HashMap};
use zbus::zvariant::OwnedObjectPath;
use zeroize::Zeroize;

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
            locked: item.is_locked().await.unwrap_or(false),
            attributes,
            content_type: item.get_secret_content_type().await.ok(),
            created: item.get_created().await.ok(),
            modified: item.get_modified().await.ok(),
        })
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
                locked: collection.is_locked().await.unwrap_or(false),
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

    async fn reveal_secret(&self, item_path: &str) -> Result<Vec<u8>> {
        let item = self
            .service
            .get_item_by_path(Self::path(item_path)?)
            .await?;
        item.unlock().await.ok();
        item.get_secret().await.context("read secret")
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

    async fn create_item(&self, mut new_item: NewItem) -> Result<ItemInfo> {
        let collection = self
            .service
            .get_collection_by_path(Self::path(&new_item.collection_path)?)
            .await?;
        let item = collection
            .create_item(
                &new_item.label,
                Self::attrs_ref(&new_item.attributes),
                &new_item.secret,
                false,
                &new_item.content_type,
            )
            .await?;
        new_item.secret.zeroize();
        self.item_info(&new_item.collection_path, item.item_path.to_string())
            .await
    }

    async fn delete_item(&self, item_path: &str) -> Result<()> {
        let item = self
            .service
            .get_item_by_path(Self::path(item_path)?)
            .await?;
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
                    content_type: item.content_type,
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
            version: 1,
            collections,
        }
        .sorted())
    }

    async fn import_metadata(&self, metadata: MetadataFile) -> Result<usize> {
        let mut changed = 0;
        for collection in metadata.collections {
            if let Ok(existing) = self
                .service
                .get_collection_by_path(Self::path(&collection.path)?)
                .await
            {
                let _ = existing.set_label(&collection.label).await;
            }
            for item in collection.items {
                if self
                    .service
                    .get_item_by_path(Self::path(&item.path)?)
                    .await
                    .is_ok()
                {
                    self.edit_item(&item.path, Some(&item.label), Some(item.attributes), None)
                        .await?;
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    async fn export_secret_backup(&self) -> Result<SecretBackupFile> {
        let mut collections = Vec::new();
        for collection in self.list_collections().await? {
            let mut items = Vec::new();
            for item in self.list_items(&collection.path).await? {
                let mut secret = self.reveal_secret(&item.path).await?;
                items.push(ItemBackup {
                    path: item.path,
                    label: item.label,
                    attributes: item.attributes,
                    content_type: item.content_type.unwrap_or_else(|| "text/plain".to_owned()),
                    secret_base64: BASE64.encode(&secret),
                });
                secret.zeroize();
            }
            collections.push(CollectionBackup {
                path: collection.path,
                label: collection.label,
                items,
            });
        }
        Ok(SecretBackupFile {
            version: 1,
            collections,
        }
        .sorted())
    }

    async fn restore_secret_backup(&self, backup: SecretBackupFile) -> Result<usize> {
        let mut changed = 0;
        for collection in backup.collections {
            let target = match self
                .service
                .get_collection_by_path(Self::path(&collection.path)?)
                .await
            {
                Ok(collection) => collection,
                Err(_) => self.service.get_any_collection().await?,
            };
            for item in collection.items {
                let mut secret = BASE64.decode(&item.secret_base64)?;
                if self
                    .service
                    .get_item_by_path(Self::path(&item.path)?)
                    .await
                    .is_ok()
                {
                    self.edit_item(
                        &item.path,
                        Some(&item.label),
                        Some(item.attributes),
                        Some((&secret, &item.content_type)),
                    )
                    .await?;
                } else {
                    target
                        .create_item(
                            &item.label,
                            Self::attrs_ref(&item.attributes),
                            &secret,
                            false,
                            &item.content_type,
                        )
                        .await?;
                }
                secret.zeroize();
                changed += 1;
            }
        }
        Ok(changed)
    }
}
