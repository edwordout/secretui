use crate::domain::{
    Attributes, CollectionInfo, ItemInfo, MetadataFile, NewCollection, NewItem, SecretBackupFile,
};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SecretStore {
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>>;
    async fn list_items(&self, collection_path: &str) -> Result<Vec<ItemInfo>>;
    async fn reveal_secret(&self, item_path: &str) -> Result<Vec<u8>>;
    async fn edit_item(
        &self,
        item_path: &str,
        label: Option<&str>,
        attributes: Option<Attributes>,
        secret: Option<(&[u8], &str)>,
    ) -> Result<()>;
    async fn create_collection(&self, collection: NewCollection) -> Result<CollectionInfo>;
    async fn create_item(&self, item: NewItem) -> Result<ItemInfo>;
    async fn delete_item(&self, item_path: &str) -> Result<()>;
    async fn set_collection_locked(&self, collection_path: &str, locked: bool) -> Result<()>;
    async fn export_metadata(&self) -> Result<MetadataFile>;
    async fn import_metadata(&self, metadata: MetadataFile) -> Result<usize>;
    async fn export_secret_backup(&self) -> Result<SecretBackupFile>;
    async fn restore_secret_backup(&self, backup: SecretBackupFile) -> Result<usize>;
}
