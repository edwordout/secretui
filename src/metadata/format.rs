use super::{
    MAX_ATTRIBUTES, MAX_ATTRIBUTE_KEY_BYTES, MAX_ATTRIBUTE_VALUE_BYTES, MAX_COLLECTIONS, MAX_ITEMS,
    MAX_LABEL_BYTES, MAX_PATH_BYTES,
};
use crate::domain::{Attributes, CollectionMetadata, ItemMetadata, MetadataFile};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use zbus::zvariant::OwnedObjectPath;

pub fn validate_metadata(metadata: &MetadataFile) -> Result<()> {
    anyhow::ensure!(
        matches!(metadata.version, 1 | 2),
        "unsupported metadata version {}",
        metadata.version
    );
    anyhow::ensure!(
        metadata.collections.len() <= MAX_COLLECTIONS,
        "metadata has {} collections; maximum is {MAX_COLLECTIONS}",
        metadata.collections.len()
    );

    let mut collection_paths = BTreeSet::new();
    let mut item_paths = BTreeSet::new();
    let mut item_count = 0usize;
    for collection in &metadata.collections {
        validate_path("collection", &collection.path)?;
        validate_text("collection label", &collection.label, MAX_LABEL_BYTES)?;
        anyhow::ensure!(
            collection_paths.insert(collection.path.as_str()),
            "duplicate collection path {}",
            collection.path
        );

        item_count = item_count
            .checked_add(collection.items.len())
            .ok_or_else(|| anyhow!("metadata item count overflow"))?;
        anyhow::ensure!(
            item_count <= MAX_ITEMS,
            "metadata has more than {MAX_ITEMS} items"
        );
        for item in &collection.items {
            validate_path("item", &item.path)?;
            validate_text("item label", &item.label, MAX_LABEL_BYTES)?;
            anyhow::ensure!(
                item_paths.insert(item.path.as_str()),
                "duplicate item path {}",
                item.path
            );
            anyhow::ensure!(
                item.attributes.len() <= MAX_ATTRIBUTES,
                "item {} has {} attributes; maximum is {MAX_ATTRIBUTES}",
                item.path,
                item.attributes.len()
            );
            for (key, value) in &item.attributes {
                validate_text("attribute key", key, MAX_ATTRIBUTE_KEY_BYTES)?;
                validate_text("attribute value", value, MAX_ATTRIBUTE_VALUE_BYTES)?;
            }
        }
    }
    Ok(())
}

pub(super) fn validate_path(kind: &str, path: &str) -> Result<()> {
    validate_text(&format!("{kind} path"), path, MAX_PATH_BYTES)?;
    OwnedObjectPath::try_from(path.to_owned())
        .map(|_| ())
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("invalid D-Bus {kind} object path {path:?}"))
}

pub(super) fn validate_text(field: &str, text: &str, maximum: usize) -> Result<()> {
    anyhow::ensure!(
        text.len() <= maximum,
        "{field} is {} bytes; maximum is {maximum} bytes",
        text.len()
    );
    anyhow::ensure!(
        !text.contains('\0'),
        "{field} contains an interior NUL and cannot be represented as a D-Bus string"
    );
    Ok(())
}

// Strict wire formats

#[derive(Deserialize)]
struct MetadataVersion {
    version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataV1 {
    version: u32,
    collections: Vec<CollectionV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionV1 {
    path: String,
    label: String,
    locked: bool,
    items: Vec<ItemV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemV1 {
    path: String,
    label: String,
    locked: bool,
    attributes: Attributes,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    created: Option<u64>,
    #[serde(default)]
    modified: Option<u64>,
}

impl MetadataV1 {
    fn into_metadata(self) -> MetadataFile {
        debug_assert_eq!(self.version, 1);
        MetadataFile {
            version: self.version,
            collections: self
                .collections
                .into_iter()
                .map(|collection| CollectionMetadata {
                    path: collection.path,
                    label: collection.label,
                    locked: collection.locked,
                    items: collection
                        .items
                        .into_iter()
                        .map(|item| {
                            let _ = item.content_type;
                            ItemMetadata {
                                path: item.path,
                                label: item.label,
                                locked: item.locked,
                                attributes: item.attributes,
                                created: item.created,
                                modified: item.modified,
                            }
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataV2 {
    version: u32,
    collections: Vec<CollectionV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionV2 {
    path: String,
    label: String,
    locked: bool,
    items: Vec<ItemV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemV2 {
    path: String,
    label: String,
    locked: bool,
    attributes: Attributes,
    created: Option<u64>,
    modified: Option<u64>,
}

impl MetadataV2 {
    fn into_metadata(self) -> MetadataFile {
        debug_assert_eq!(self.version, 2);
        MetadataFile {
            version: self.version,
            collections: self
                .collections
                .into_iter()
                .map(|collection| CollectionMetadata {
                    path: collection.path,
                    label: collection.label,
                    locked: collection.locked,
                    items: collection
                        .items
                        .into_iter()
                        .map(|item| ItemMetadata {
                            path: item.path,
                            label: item.label,
                            locked: item.locked,
                            attributes: item.attributes,
                            created: item.created,
                            modified: item.modified,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

pub(super) fn parse_metadata(bytes: &[u8]) -> Result<MetadataFile> {
    let version = serde_json::from_slice::<MetadataVersion>(bytes)
        .context("parse metadata version")?
        .version;
    let metadata = match version {
        1 => serde_json::from_slice::<MetadataV1>(bytes)
            .context("parse strict version 1 metadata")?
            .into_metadata(),
        2 => serde_json::from_slice::<MetadataV2>(bytes)
            .context("parse strict version 2 metadata")?
            .into_metadata(),
        other => anyhow::bail!("unsupported metadata version {other}"),
    };
    validate_metadata(&metadata)?;
    Ok(metadata.sorted())
}
