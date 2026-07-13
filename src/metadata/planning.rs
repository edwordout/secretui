use super::format::{validate_metadata, validate_path, validate_text};
use super::{
    MAX_ATTRIBUTES, MAX_ATTRIBUTE_KEY_BYTES, MAX_ATTRIBUTE_VALUE_BYTES, MAX_LABEL_BYTES,
    MAX_METADATA_BYTES,
};
use crate::domain::{Attributes, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile};
use crate::store::SecretStore;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// Same-store metadata repair plan

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportPlan {
    pub source_version: u32,
    pub match_basis: String,
    pub requested: MetadataFile,
    pub baseline: MetadataFile,
    pub recovery: MetadataFile,
    pub changes: Vec<PlannedChange>,
    pub conflicts: Vec<ImportConflict>,
    pub field_operations_skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedChange {
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub current_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_attributes: Option<Attributes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_attributes: Option<Attributes>,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    pub reason: String,
}

impl PlannedChange {
    pub fn is_collection(&self) -> bool {
        self.item_path.is_none()
    }

    pub fn field_operation_count(&self) -> usize {
        usize::from(self.proposed_label.is_some()) + usize::from(self.proposed_attributes.is_some())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ImportConflictKind {
    MissingCollection,
    InaccessibleCollection,
    AmbiguousCollection,
    MissingItem,
    ParentMismatch,
    AmbiguousItem,
    UnrepresentableMetadata,
    ConcurrentChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportConflict {
    pub kind: ImportConflictKind,
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub message: String,
}

/// Build a deterministic exact-object-path repair plan without reading secrets.
pub async fn plan_metadata_import(
    store: &(impl SecretStore + ?Sized),
    metadata: &MetadataFile,
) -> Result<ImportPlan> {
    validate_metadata(metadata)?;
    let requested = metadata.clone().sorted();
    let current_collections = store
        .list_collections()
        .await
        .context("preflight: list collections")?;

    let mut collections_by_path = BTreeMap::<String, Vec<_>>::new();
    let mut items_by_path = BTreeMap::<String, Vec<ItemInfo>>::new();
    let mut items_by_collection = BTreeMap::<String, Result<Vec<ItemInfo>, String>>::new();
    for collection in current_collections {
        collections_by_path
            .entry(collection.path.clone())
            .or_default()
            .push(collection.clone());
        if collections_by_path[&collection.path].len() > 1 {
            continue;
        }
        match store.list_items(&collection.path).await {
            Ok(items) => {
                for item in &items {
                    items_by_path
                        .entry(item.path.clone())
                        .or_default()
                        .push(item.clone());
                }
                items_by_collection.insert(collection.path, Ok(items));
            }
            Err(error) => {
                items_by_collection.insert(collection.path, Err(format!("{error:#}")));
            }
        }
    }

    let mut baseline_collections = Vec::new();
    let mut recovery_collections = BTreeMap::<String, CollectionMetadata>::new();
    let mut changes = Vec::new();
    let mut conflicts = Vec::new();
    let mut skipped = 0usize;
    let requested_collection_paths = requested
        .collections
        .iter()
        .map(|collection| collection.path.as_str())
        .collect::<BTreeSet<_>>();

    // Exact item identity is global. If any provider collection cannot be enumerated, the
    // preflight cannot honestly prove that a requested path is unique in the same store.
    for (collection_path, items) in &items_by_collection {
        if let Err(error) = items {
            if requested_collection_paths.contains(collection_path.as_str()) {
                continue;
            }
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::InaccessibleCollection,
                collection_path: collection_path.clone(),
                item_path: None,
                message: format!(
                    "cannot prove global item-path uniqueness because this collection is inaccessible: {error}"
                ),
            });
        }
    }
    for (collection_path, matching_collections) in &collections_by_path {
        if matching_collections.len() > 1
            && !requested_collection_paths.contains(collection_path.as_str())
        {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::AmbiguousCollection,
                collection_path: collection_path.clone(),
                item_path: None,
                message: "provider returned an unrelated collection object path more than once, so global identity is ambiguous"
                    .into(),
            });
        }
    }

    for requested_collection in &requested.collections {
        let Some(matching_collections) = collections_by_path.get(&requested_collection.path) else {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::MissingCollection,
                collection_path: requested_collection.path.clone(),
                item_path: None,
                message: "exact collection object path is missing from this store".into(),
            });
            for requested_item in &requested_collection.items {
                add_unmatched_item_conflict(
                    &mut conflicts,
                    requested_collection,
                    requested_item,
                    &items_by_path,
                );
            }
            continue;
        };
        if matching_collections.len() != 1 {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::AmbiguousCollection,
                collection_path: requested_collection.path.clone(),
                item_path: None,
                message: "provider returned the collection object path more than once".into(),
            });
            continue;
        }
        let current_collection = &matching_collections[0];
        if let Err(error) = validate_text(
            "current collection label",
            &current_collection.label,
            MAX_LABEL_BYTES,
        ) {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::UnrepresentableMetadata,
                collection_path: requested_collection.path.clone(),
                item_path: None,
                message: format!(
                    "current collection metadata cannot be represented in a recovery file: {error:#}"
                ),
            });
            continue;
        }
        let mut baseline_collection = CollectionMetadata {
            path: current_collection.path.clone(),
            label: current_collection.label.clone(),
            locked: current_collection.locked,
            items: Vec::new(),
        };

        if current_collection.label != requested_collection.label {
            changes.push(PlannedChange {
                collection_path: current_collection.path.clone(),
                item_path: None,
                current_label: current_collection.label.clone(),
                proposed_label: Some(requested_collection.label.clone()),
                current_attributes: None,
                proposed_attributes: None,
                locked: current_collection.locked,
                created: None,
                modified: None,
                reason: "exact same-store collection object path".into(),
            });
            recovery_collections
                .entry(current_collection.path.clone())
                .or_insert_with(|| baseline_collection.clone());
        } else {
            skipped += 1;
        }

        let collection_items = match items_by_collection.get(&requested_collection.path) {
            Some(Ok(items)) => items,
            Some(Err(error)) => {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::InaccessibleCollection,
                    collection_path: requested_collection.path.clone(),
                    item_path: None,
                    message: format!("cannot inspect collection items: {error}"),
                });
                baseline_collections.push(baseline_collection);
                continue;
            }
            None => {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::InaccessibleCollection,
                    collection_path: requested_collection.path.clone(),
                    item_path: None,
                    message: "collection disappeared during preflight".into(),
                });
                baseline_collections.push(baseline_collection);
                continue;
            }
        };
        let mut local_items = BTreeMap::<&str, Vec<&ItemInfo>>::new();
        for item in collection_items {
            local_items.entry(&item.path).or_default().push(item);
        }

        for requested_item in &requested_collection.items {
            let Some(matching_items) = local_items.get(requested_item.path.as_str()) else {
                add_unmatched_item_conflict(
                    &mut conflicts,
                    requested_collection,
                    requested_item,
                    &items_by_path,
                );
                continue;
            };
            if matching_items.len() != 1 || items_by_path[&requested_item.path].len() != 1 {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::AmbiguousItem,
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(requested_item.path.clone()),
                    message: "provider returned the item object path more than once".into(),
                });
                continue;
            }
            let current_item = matching_items[0];
            if current_item.collection_path != requested_collection.path {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::ParentMismatch,
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(requested_item.path.clone()),
                    message: format!(
                        "item reports parent collection {} instead of the exact requested parent",
                        current_item.collection_path
                    ),
                });
                continue;
            }
            if let Err(error) = validate_current_item(current_item) {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::UnrepresentableMetadata,
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(requested_item.path.clone()),
                    message: format!(
                        "current item metadata cannot be represented in a recovery file: {error:#}"
                    ),
                });
                continue;
            }

            let current_metadata = item_metadata(current_item);
            baseline_collection.items.push(current_metadata.clone());
            let label_changed = current_item.label != requested_item.label;
            let attributes_changed = current_item.attributes != requested_item.attributes;
            skipped += usize::from(!label_changed) + usize::from(!attributes_changed);
            if label_changed || attributes_changed {
                changes.push(PlannedChange {
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(current_item.path.clone()),
                    current_label: current_item.label.clone(),
                    proposed_label: label_changed.then(|| requested_item.label.clone()),
                    current_attributes: Some(current_item.attributes.clone()),
                    proposed_attributes: attributes_changed
                        .then(|| requested_item.attributes.clone()),
                    locked: current_item.locked,
                    created: current_item.created,
                    modified: current_item.modified,
                    reason: "exact same-store item object path under exact parent collection"
                        .into(),
                });
                let recovery_collection = recovery_collections
                    .entry(current_collection.path.clone())
                    .or_insert_with(|| CollectionMetadata {
                        path: current_collection.path.clone(),
                        label: current_collection.label.clone(),
                        locked: current_collection.locked,
                        items: Vec::new(),
                    });
                recovery_collection.items.push(current_metadata);
            }
        }
        baseline_collections.push(baseline_collection);
    }

    changes.sort_by(|left, right| {
        (&left.collection_path, &left.item_path).cmp(&(&right.collection_path, &right.item_path))
    });
    let mut recovery = MetadataFile {
        version: 2,
        collections: recovery_collections.into_values().collect(),
    }
    .sorted();
    for collection in &mut recovery.collections {
        collection
            .items
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    let recovery_bytes = serde_json::to_vec_pretty(&recovery)
        .context("serialize recovery metadata during preflight")?
        .len()
        .saturating_add(1);
    if recovery_bytes as u64 > MAX_METADATA_BYTES {
        conflicts.push(ImportConflict {
            kind: ImportConflictKind::UnrepresentableMetadata,
            collection_path: String::new(),
            item_path: None,
            message: format!(
                "recovery metadata would be {recovery_bytes} bytes; maximum is {MAX_METADATA_BYTES} bytes"
            ),
        });
        // The conflict guarantees zero provider writes. Keep the journal itself reusable and
        // within the documented boundary rather than emitting an oversized file it cannot read.
        recovery.collections.clear();
    }
    conflicts.sort_by(|left, right| {
        (
            &left.collection_path,
            &left.item_path,
            &left.kind,
            &left.message,
        )
            .cmp(&(
                &right.collection_path,
                &right.item_path,
                &right.kind,
                &right.message,
            ))
    });

    Ok(ImportPlan {
        source_version: requested.version,
        match_basis: "exact object paths in the same provider database".into(),
        requested,
        baseline: MetadataFile {
            version: 2,
            collections: baseline_collections,
        }
        .sorted(),
        recovery,
        changes,
        conflicts,
        field_operations_skipped: skipped,
    })
}

fn add_unmatched_item_conflict(
    conflicts: &mut Vec<ImportConflict>,
    requested_collection: &CollectionMetadata,
    requested_item: &ItemMetadata,
    items_by_path: &BTreeMap<String, Vec<ItemInfo>>,
) {
    let (kind, message) = match items_by_path.get(&requested_item.path) {
        Some(items) if items.len() > 1 => (
            ImportConflictKind::AmbiguousItem,
            "provider returned the item object path more than once".into(),
        ),
        Some(items) => (
            ImportConflictKind::ParentMismatch,
            format!(
                "exact item path belongs to collection {}, not requested parent {}",
                items[0].collection_path, requested_collection.path
            ),
        ),
        None => (
            ImportConflictKind::MissingItem,
            "exact item object path is missing from this store".into(),
        ),
    };
    conflicts.push(ImportConflict {
        kind,
        collection_path: requested_collection.path.clone(),
        item_path: Some(requested_item.path.clone()),
        message,
    });
}

fn item_metadata(item: &ItemInfo) -> ItemMetadata {
    ItemMetadata {
        path: item.path.clone(),
        label: item.label.clone(),
        locked: item.locked,
        attributes: item.attributes.clone(),
        created: item.created,
        modified: item.modified,
    }
}

fn validate_current_item(item: &ItemInfo) -> Result<()> {
    validate_path("current item", &item.path)?;
    validate_text("current item label", &item.label, MAX_LABEL_BYTES)?;
    anyhow::ensure!(
        item.attributes.len() <= MAX_ATTRIBUTES,
        "current item has {} attributes; maximum is {MAX_ATTRIBUTES}",
        item.attributes.len()
    );
    for (key, value) in &item.attributes {
        validate_text("current attribute key", key, MAX_ATTRIBUTE_KEY_BYTES)?;
        validate_text("current attribute value", value, MAX_ATTRIBUTE_VALUE_BYTES)?;
    }
    Ok(())
}
