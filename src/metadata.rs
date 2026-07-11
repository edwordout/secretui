use crate::domain::{Attributes, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile};
use crate::store::{ItemTarget, SecretStore, StoreError, StoreWarning};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use zbus::zvariant::OwnedObjectPath;

pub const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_COLLECTIONS: usize = 10_000;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_ATTRIBUTES: usize = 1_024;
pub const MAX_LABEL_BYTES: usize = 16 * 1024;
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 4 * 1024;
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 64 * 1024;

// Metadata file boundary

/// Read and strictly validate a bounded metadata document.
///
/// Version 1 accepts its legacy per-item `content_type` field and discards it.
/// Version 2 rejects that field and every other unknown field.
pub fn read_metadata(path: &Path) -> Result<MetadataFile> {
    let file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("inspect {}", path.display()))?
        .len();
    anyhow::ensure!(
        length <= MAX_METADATA_BYTES,
        "metadata input is {length} bytes; maximum is {MAX_METADATA_BYTES} bytes"
    );

    // The take protects against the file growing between metadata() and read().
    let mut bytes = Vec::with_capacity(length.min(MAX_METADATA_BYTES) as usize);
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "metadata input exceeds {MAX_METADATA_BYTES} bytes"
    );

    let version = serde_json::from_slice::<MetadataVersion>(&bytes)
        .context("parse metadata version")?
        .version;
    let metadata = match version {
        1 => serde_json::from_slice::<MetadataV1>(&bytes)
            .context("parse strict version 1 metadata")?
            .into_metadata(),
        2 => serde_json::from_slice::<MetadataV2>(&bytes)
            .context("parse strict version 2 metadata")?
            .into_metadata(),
        other => anyhow::bail!("unsupported metadata version {other}"),
    };
    validate_metadata(&metadata)?;
    Ok(metadata.sorted())
}

/// Create a deterministic metadata file without replacing an existing path.
pub fn write_metadata(path: &Path, metadata: &MetadataFile) -> Result<()> {
    write_metadata_with_options(path, metadata, false)
}

/// Create deterministic metadata JSON, optionally replacing an existing path.
///
/// The write is atomic within the destination directory. The file is synced,
/// has mode 0600 on Unix, and the parent directory is synced after rename.
pub fn write_metadata_with_options(
    path: &Path,
    metadata: &MetadataFile,
    force: bool,
) -> Result<()> {
    validate_metadata(metadata)?;
    let mut text = serde_json::to_vec_pretty(&metadata.clone().sorted())?;
    text.push(b'\n');
    anyhow::ensure!(
        text.len() as u64 <= MAX_METADATA_BYTES,
        "serialized metadata is {} bytes; maximum is {MAX_METADATA_BYTES} bytes",
        text.len()
    );
    write_restricted(path, &text, force)
}

/// Create a no-clobber, mode-0600 JSON file for a recovery or report document.
pub fn create_restricted_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_restricted(path, &text, false)
}

/// Atomically replace a report file with a new mode-0600 JSON representation.
pub fn replace_restricted_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_restricted(path, &text, true)
}

fn write_restricted(path: &Path, bytes: &[u8], force: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    if !force {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("refusing existing symlink {}", path.display())
            }
            Ok(_) => anyhow::bail!(
                "refusing to replace existing path {}; use --force when appropriate",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    } else if let Ok(metadata) = fs::symlink_metadata(path) {
        anyhow::ensure!(
            !metadata.file_type().is_dir(),
            "refusing to replace directory {}",
            path.display()
        );
    }

    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    set_private_permissions(temporary.as_file())?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("write temporary file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync temporary file for {}", path.display()))?;

    if force {
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace {}", path.display()))?;
    } else {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .with_context(|| format!("create {} without replacing it", path.display()))?;
    }

    // Re-open without following a stale pre-rename handle and enforce the mode.
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open completed file {}", path.display()))?;
    set_private_permissions(&output)?;
    output
        .sync_all()
        .with_context(|| format!("sync completed file {}", path.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync directory {}", parent.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .context("set file mode 0600")
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> Result<()> {
    Ok(())
}

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

fn validate_path(kind: &str, path: &str) -> Result<()> {
    validate_text(&format!("{kind} path"), path, MAX_PATH_BYTES)?;
    OwnedObjectPath::try_from(path.to_owned())
        .map(|_| ())
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("invalid D-Bus {kind} object path {path:?}"))
}

fn validate_text(field: &str, text: &str, maximum: usize) -> Result<()> {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    InProgress,
    Complete,
    Blocked,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedOperation {
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyFailure {
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub field: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyReport {
    pub status: ApplyStatus,
    pub collections_changed: usize,
    pub items_changed: usize,
    pub field_operations_attempted: usize,
    pub field_operations_applied: usize,
    pub field_operations_skipped: usize,
    pub conflicts: Vec<ImportConflict>,
    pub failures: Vec<ApplyFailure>,
    pub relock_failures: Vec<String>,
    pub operations: Vec<AppliedOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_file: Option<PathBuf>,
}

impl ApplyReport {
    pub fn is_complete(&self) -> bool {
        self.status == ApplyStatus::Complete
    }

    pub fn is_partial(&self) -> bool {
        self.status == ApplyStatus::Partial
    }

    fn new(plan: &ImportPlan) -> Self {
        Self {
            status: ApplyStatus::InProgress,
            collections_changed: 0,
            items_changed: 0,
            field_operations_attempted: 0,
            field_operations_applied: 0,
            field_operations_skipped: plan.field_operations_skipped,
            conflicts: Vec::new(),
            failures: Vec::new(),
            relock_failures: Vec::new(),
            operations: Vec::new(),
            recovery_file: None,
            report_file: None,
        }
    }

    fn finish_failed(&mut self) {
        self.status = if self.field_operations_attempted == 0 {
            ApplyStatus::Blocked
        } else {
            ApplyStatus::Partial
        };
    }
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

/// Apply an exact plan after repeating the full preflight.
pub async fn apply_metadata_plan(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
) -> Result<ApplyReport> {
    apply_metadata_plan_inner(store, plan, None, None).await
}

/// Apply a plan while durably replacing a JSON report after every field write.
pub async fn apply_metadata_plan_with_report(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
    report_path: &Path,
    recovery_path: Option<&Path>,
) -> Result<ApplyReport> {
    apply_metadata_plan_inner(store, plan, Some(report_path), recovery_path).await
}

async fn apply_metadata_plan_inner(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
    report_path: Option<&Path>,
    recovery_path: Option<&Path>,
) -> Result<ApplyReport> {
    if let Some(path) = recovery_path {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect recovery metadata {}", path.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "recovery metadata must be an existing regular, non-symlink file: {}",
            path.display()
        );
        let recovery = read_metadata(path)
            .with_context(|| format!("verify recovery metadata {}", path.display()))?;
        anyhow::ensure!(
            recovery == plan.recovery.clone().sorted(),
            "recovery metadata does not contain this exact plan's original values"
        );
    }
    let mut report = ApplyReport::new(plan);
    report.report_file = report_path.map(Path::to_path_buf);
    report.recovery_file = recovery_path.map(Path::to_path_buf);
    if let Some(path) = report_path {
        create_restricted_json(path, &report)
            .with_context(|| format!("create apply report {}", path.display()))?;
    }

    if !plan.conflicts.is_empty() {
        report.conflicts = plan.conflicts.clone();
        report.finish_failed();
        persist_report(report_path, &mut report);
        return Ok(report);
    }

    let fresh_plan = match plan_metadata_import(store, &plan.requested).await {
        Ok(fresh_plan) => fresh_plan,
        Err(error) => {
            report.failures.push(ApplyFailure {
                collection_path: String::new(),
                item_path: None,
                field: "preflight".into(),
                error: format!("second full preflight failed: {error:#}"),
            });
            report.finish_failed();
            persist_report(report_path, &mut report);
            return Ok(report);
        }
    };
    if fresh_plan != *plan {
        report.conflicts = second_preflight_conflicts(plan, &fresh_plan);
        report.finish_failed();
        persist_report(report_path, &mut report);
        return Ok(report);
    }

    for change in &plan.changes {
        if let Err(error) = verify_change_target(store, change).await {
            report.conflicts.push(ImportConflict {
                kind: ImportConflictKind::ConcurrentChange,
                collection_path: change.collection_path.clone(),
                item_path: change.item_path.clone(),
                message: error.to_string(),
            });
            report.finish_failed();
            persist_report(report_path, &mut report);
            return Ok(report);
        }

        if change.is_collection() {
            let proposed_label = change
                .proposed_label
                .as_deref()
                .expect("collection changes always contain a proposed label");
            report.field_operations_attempted += 1;
            match store
                .set_collection_label(&change.collection_path, proposed_label)
                .await
            {
                Ok(outcome) => {
                    report.collections_changed += 1;
                    record_applied(&mut report, change, "label");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "label", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
            continue;
        }

        let target = ItemTarget {
            collection_path: change.collection_path.clone(),
            item_path: change.item_path.clone().expect("item path was checked"),
        };
        let mut item_counted = false;
        let mut post_label_snapshot = None;
        if let Some(proposed_label) = change.proposed_label.as_deref() {
            report.field_operations_attempted += 1;
            match store.set_item_label(&target, proposed_label).await {
                Ok(outcome) => {
                    item_counted = true;
                    report.items_changed += 1;
                    record_applied(&mut report, change, "label");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "label", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
            if change.proposed_attributes.is_some() {
                match read_after_own_label(store, change).await {
                    Ok(snapshot) => post_label_snapshot = Some(snapshot),
                    Err(error) => {
                        record_failure(&mut report, change, "post_label_verification", error);
                        report.finish_failed();
                        persist_report(report_path, &mut report);
                        return Ok(report);
                    }
                }
            }
        }

        if let Some(proposed_attributes) = &change.proposed_attributes {
            if let Some(post_label_snapshot) = post_label_snapshot.as_ref() {
                if let Err(error) = verify_unchanged_item(store, change, post_label_snapshot).await
                {
                    report.conflicts.push(ImportConflict {
                        kind: ImportConflictKind::ConcurrentChange,
                        collection_path: change.collection_path.clone(),
                        item_path: change.item_path.clone(),
                        message: error.to_string(),
                    });
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            report.field_operations_attempted += 1;
            match store
                .set_item_attributes(&target, proposed_attributes.clone())
                .await
            {
                Ok(outcome) => {
                    if !item_counted {
                        report.items_changed += 1;
                    }
                    record_applied(&mut report, change, "attributes");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "attributes", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
        }
    }

    report.status = ApplyStatus::Complete;
    persist_report(report_path, &mut report);
    Ok(report)
}

fn second_preflight_conflicts(plan: &ImportPlan, fresh_plan: &ImportPlan) -> Vec<ImportConflict> {
    if !fresh_plan.conflicts.is_empty() {
        return fresh_plan
            .conflicts
            .iter()
            .cloned()
            .map(|mut conflict| {
                conflict.message = format!("second preflight: {}", conflict.message);
                conflict
            })
            .collect();
    }

    let old_collections = plan
        .baseline
        .collections
        .iter()
        .map(|collection| (collection.path.as_str(), collection))
        .collect::<BTreeMap<_, _>>();
    let fresh_collections = fresh_plan
        .baseline
        .collections
        .iter()
        .map(|collection| (collection.path.as_str(), collection))
        .collect::<BTreeMap<_, _>>();
    let mut conflicts = Vec::new();
    for requested_collection in &plan.requested.collections {
        let old_collection = old_collections
            .get(requested_collection.path.as_str())
            .copied();
        let fresh_collection = fresh_collections
            .get(requested_collection.path.as_str())
            .copied();
        if old_collection.map(|collection| (&collection.label, collection.locked))
            != fresh_collection.map(|collection| (&collection.label, collection.locked))
        {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::ConcurrentChange,
                collection_path: requested_collection.path.clone(),
                item_path: None,
                message: "collection label, lock state, or identity changed after planning".into(),
            });
        }

        let old_items = old_collection
            .map(|collection| {
                collection
                    .items
                    .iter()
                    .map(|item| (item.path.as_str(), item))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let fresh_items = fresh_collection
            .map(|collection| {
                collection
                    .items
                    .iter()
                    .map(|item| (item.path.as_str(), item))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        for requested_item in &requested_collection.items {
            if old_items.get(requested_item.path.as_str())
                != fresh_items.get(requested_item.path.as_str())
            {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::ConcurrentChange,
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(requested_item.path.clone()),
                    message: "item label, attributes, lock state, identity, or timestamp changed after planning"
                        .into(),
                });
            }
        }
    }
    if conflicts.is_empty() {
        conflicts.push(ImportConflict {
            kind: ImportConflictKind::ConcurrentChange,
            collection_path: String::new(),
            item_path: None,
            message: "the exact plan changed during the second full preflight".into(),
        });
    }
    conflicts
}

async fn verify_change_target(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<()> {
    if change.is_collection() {
        let matching = store
            .list_collections()
            .await
            .context("re-read collections immediately before write")?
            .into_iter()
            .filter(|collection| collection.path == change.collection_path)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matching.len() == 1,
            "collection identity is no longer unique"
        );
        let current = &matching[0];
        anyhow::ensure!(
            current.label == change.current_label && current.locked == change.locked,
            "collection label or lock state changed after preflight"
        );
        return Ok(());
    }

    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        current.label == change.current_label
            && Some(&current.attributes) == change.current_attributes.as_ref()
            && current.locked == change.locked
            && current.created == change.created
            && current.modified == change.modified,
        "item identity, label, attributes, lock state, creation, or modification timestamp changed after preflight"
    );
    Ok(())
}

async fn read_after_own_label(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<ItemInfo> {
    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        Some(current.label.as_str()) == change.proposed_label.as_deref()
            && Some(&current.attributes) == change.current_attributes.as_ref()
            && current.locked == change.locked
            && current.created == change.created,
        "item changed concurrently between its label and attribute operations"
    );
    Ok(current)
}

async fn verify_unchanged_item(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
    expected: &ItemInfo,
) -> Result<()> {
    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        &current == expected,
        "item label, attributes, lock state, identity, or timestamp changed between field operations"
    );
    Ok(())
}

async fn read_exact_item(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<ItemInfo> {
    let item_path = change.item_path.as_deref().context("missing item path")?;
    let matching = store
        .list_items(&change.collection_path)
        .await
        .context("re-read collection immediately before item write")?
        .into_iter()
        .filter(|item| item.path == item_path && item.collection_path == change.collection_path)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matching.len() == 1,
        "item identity or parent collection changed"
    );
    Ok(matching.into_iter().next().expect("length checked"))
}

fn record_applied(report: &mut ApplyReport, change: &PlannedChange, field: &str) {
    report.field_operations_applied += 1;
    report.operations.push(AppliedOperation {
        collection_path: change.collection_path.clone(),
        item_path: change.item_path.clone(),
        field: field.into(),
    });
}

fn record_failure(
    report: &mut ApplyReport,
    change: &PlannedChange,
    field: &str,
    error: anyhow::Error,
) {
    let error_text = if let Some(store_error) = error.downcast_ref::<StoreError>() {
        record_warnings(report, &store_error.warnings);
        store_error.operation_error.clone()
    } else {
        format!("{error:#}")
    };
    report.failures.push(ApplyFailure {
        collection_path: change.collection_path.clone(),
        item_path: change.item_path.clone(),
        field: field.into(),
        error: error_text,
    });
}

fn record_warnings(report: &mut ApplyReport, warnings: &[StoreWarning]) {
    report
        .relock_failures
        .extend(warnings.iter().map(ToString::to_string));
}

fn persist_report(path: Option<&Path>, report: &mut ApplyReport) -> bool {
    let Some(path) = path else {
        return true;
    };
    if let Err(error) = replace_restricted_json(path, report) {
        report.failures.push(ApplyFailure {
            collection_path: String::new(),
            item_path: None,
            field: "report_journal".into(),
            error: format!("durable apply report update failed: {error:#}"),
        });
        report.finish_failed();
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CollectionInfo, ItemInfo};
    use crate::store::{MemorySecretStore, StoreOperation};
    use std::os::unix::fs::PermissionsExt;

    fn metadata() -> MetadataFile {
        MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: "/collection/b".into(),
                label: "B".into(),
                locked: false,
                items: vec![
                    ItemMetadata {
                        path: "/collection/b/item/z".into(),
                        label: "Z".into(),
                        locked: false,
                        attributes: BTreeMap::new(),
                        created: None,
                        modified: None,
                    },
                    ItemMetadata {
                        path: "/collection/b/item/a".into(),
                        label: "A".into(),
                        locked: false,
                        attributes: BTreeMap::new(),
                        created: None,
                        modified: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn metadata_json_is_deterministic_private_and_no_clobber() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.json");
        write_metadata(&path, &metadata()).unwrap();
        let json = fs::read_to_string(&path).unwrap();
        assert!(json.find("/item/a").unwrap() < json.find("/item/z").unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_metadata(&path, &metadata()).is_err());
        write_metadata_with_options(&path, &metadata(), true).unwrap();
    }

    #[test]
    fn export_refuses_symlinks_by_default_and_force_replaces_only_the_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.json");
        let link = directory.path().join("metadata.json");
        fs::write(&target, b"do not replace through link").unwrap();
        symlink(&target, &link).unwrap();

        assert!(write_metadata(&link, &metadata()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"do not replace through link");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());

        write_metadata_with_options(&link, &metadata(), true).unwrap();
        assert!(!fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"do not replace through link");
        assert_eq!(read_metadata(&link).unwrap(), metadata().sorted());
    }

    #[test]
    fn reads_v1_and_ignores_content_type() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            r#"{"version":1,"collections":[{"path":"/c","label":"C","locked":false,"items":[{"path":"/c/i","label":"I","locked":false,"attributes":{},"content_type":"text/plain","created":null,"modified":null}]}]}"#,
        )
        .unwrap();
        assert_eq!(read_metadata(file.path()).unwrap().version, 1);
    }

    #[test]
    fn v2_rejects_content_type_and_unknown_fields() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            r#"{"version":2,"collections":[{"path":"/c","label":"C","locked":false,"items":[{"path":"/c/i","label":"I","locked":false,"attributes":{},"content_type":"text/plain","created":null,"modified":null}]}]}"#,
        )
        .unwrap();
        assert!(read_metadata(file.path()).is_err());

        fs::write(file.path(), r#"{"version":2,"collections":[],"extra":1}"#).unwrap();
        assert!(read_metadata(file.path()).is_err());
    }

    #[test]
    fn v2_accepts_omitted_optional_timestamps() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            r#"{"version":2,"collections":[{"path":"/c","label":"C","locked":false,"items":[{"path":"/c/i","label":"I","locked":false,"attributes":{}}]}]}"#,
        )
        .unwrap();
        let metadata = read_metadata(file.path()).unwrap();
        assert_eq!(metadata.collections[0].items[0].created, None);
        assert_eq!(metadata.collections[0].items[0].modified, None);
    }

    #[test]
    fn rejects_unknown_version_invalid_paths_and_global_item_duplicates() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), r#"{"version":99,"collections":[]}"#).unwrap();
        assert!(read_metadata(file.path()).is_err());

        let mut invalid = metadata();
        invalid.collections[0].path = "not/a/path".into();
        assert!(validate_metadata(&invalid).is_err());

        let mut duplicate = metadata();
        let mut second = duplicate.collections[0].clone();
        second.path = "/collection/c".into();
        duplicate.collections.push(second);
        assert!(validate_metadata(&duplicate).is_err());
    }

    #[test]
    fn rejects_oversized_input_before_json_parsing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_METADATA_BYTES + 1).unwrap();
        let error = read_metadata(file.path()).unwrap_err().to_string();
        assert!(error.contains("maximum"), "{error}");
    }

    #[test]
    fn refuses_to_write_metadata_larger_than_the_read_boundary() {
        let mut oversized = metadata();
        oversized.collections[0].items.truncate(1);
        oversized.collections[0].items[0].attributes = (0..257)
            .map(|index| {
                (
                    format!("key-{index}"),
                    "x".repeat(MAX_ATTRIBUTE_VALUE_BYTES),
                )
            })
            .collect();
        validate_metadata(&oversized).unwrap();

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("too-large.json");
        let error = write_metadata(&path, &oversized).unwrap_err().to_string();
        assert!(error.contains("serialized metadata"), "{error}");
        assert!(!path.exists());
    }

    fn memory_store() -> MemorySecretStore {
        let store = MemorySecretStore::new();
        store.insert_collection(CollectionInfo {
            path: "/collection/a".into(),
            label: "Current collection".into(),
            locked: false,
        });
        store.insert_collection(CollectionInfo {
            path: "/collection/b".into(),
            label: "Other collection".into(),
            locked: false,
        });
        let mut attributes = Attributes::new();
        attributes.insert("account".into(), "current".into());
        store
            .insert_item(
                ItemInfo {
                    collection_path: "/collection/a".into(),
                    path: "/collection/a/item/one".into(),
                    label: "Current item".into(),
                    locked: false,
                    attributes,
                    created: Some(10),
                    modified: Some(11),
                },
                vec![0, 1, 0xfe, 0xff],
                "application/octet-stream",
            )
            .unwrap();
        store
    }

    fn requested_metadata() -> MetadataFile {
        let mut attributes = Attributes::new();
        attributes.insert("account".into(), "proposed".into());
        MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: "/collection/a".into(),
                label: "Proposed collection".into(),
                locked: false,
                items: vec![ItemMetadata {
                    path: "/collection/a/item/one".into(),
                    label: "Proposed item".into(),
                    locked: false,
                    attributes,
                    created: Some(10),
                    modified: Some(11),
                }],
            }],
        }
    }

    #[tokio::test]
    async fn plans_exact_paths_in_deterministic_field_order() {
        let store = memory_store();
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.changes.len(), 2);
        assert!(plan.changes[0].is_collection());
        assert_eq!(
            plan.changes[1].item_path.as_deref(),
            Some("/collection/a/item/one")
        );
        assert!(plan.changes[1].proposed_label.is_some());
        assert!(plan.changes[1].proposed_attributes.is_some());
        assert_eq!(plan.recovery.version, 2);
        assert_eq!(plan.recovery.collections.len(), 1);
        assert_eq!(plan.recovery.collections[0].items.len(), 1);
    }

    #[tokio::test]
    async fn plans_label_only_attributes_only_and_no_op_fields() {
        let label_store = memory_store();
        let mut label_request = requested_metadata();
        label_request.collections[0].label = "Current collection".into();
        label_request.collections[0].items[0].attributes = label_store
            .item("/collection/a/item/one")
            .unwrap()
            .attributes;
        let label_plan = plan_metadata_import(&label_store, &label_request)
            .await
            .unwrap();
        assert_eq!(label_plan.changes.len(), 1);
        assert!(label_plan.changes[0].proposed_label.is_some());
        assert!(label_plan.changes[0].proposed_attributes.is_none());
        assert!(apply_metadata_plan(&label_store, &label_plan)
            .await
            .unwrap()
            .is_complete());

        let attributes_store = memory_store();
        let mut attributes_request = requested_metadata();
        attributes_request.collections[0].label = "Current collection".into();
        attributes_request.collections[0].items[0].label = "Current item".into();
        let attributes_plan = plan_metadata_import(&attributes_store, &attributes_request)
            .await
            .unwrap();
        assert_eq!(attributes_plan.changes.len(), 1);
        assert!(attributes_plan.changes[0].proposed_label.is_none());
        assert!(attributes_plan.changes[0].proposed_attributes.is_some());
        assert!(apply_metadata_plan(&attributes_store, &attributes_plan)
            .await
            .unwrap()
            .is_complete());

        let no_op_store = memory_store();
        let no_op = no_op_store.export_metadata().await.unwrap();
        no_op_store.clear_log();
        let no_op_plan = plan_metadata_import(&no_op_store, &no_op).await.unwrap();
        assert!(no_op_plan.changes.is_empty());
        assert!(no_op_plan.conflicts.is_empty());
        let report = apply_metadata_plan(&no_op_store, &no_op_plan)
            .await
            .unwrap();
        assert!(report.is_complete());
        assert_eq!(report.field_operations_applied, 0);
        assert!(no_op_store.mutation_log().iter().all(|entry| !matches!(
            entry.operation,
            StoreOperation::SetCollectionLabel
                | StoreOperation::SetItemLabel
                | StoreOperation::SetItemAttributes
        )));
    }

    #[tokio::test]
    async fn missing_and_parent_mismatched_targets_block_all_writes() {
        let store = memory_store();
        let mut requested = requested_metadata();
        requested.collections[0].items[0].path = "/missing/item".into();
        requested.collections[0].items.push(ItemMetadata {
            path: "/collection/a/item/one".into(),
            label: "Wrong parent".into(),
            locked: false,
            attributes: Attributes::new(),
            created: None,
            modified: None,
        });
        // Put both item requests under collection b while keeping their globally unique paths.
        requested.collections[0].items.remove(1);
        requested.collections.push(CollectionMetadata {
            path: "/collection/b".into(),
            label: "Other collection".into(),
            locked: false,
            items: vec![ItemMetadata {
                path: "/collection/a/item/one".into(),
                label: "Wrong parent".into(),
                locked: false,
                attributes: Attributes::new(),
                created: None,
                modified: None,
            }],
        });

        let plan = plan_metadata_import(&store, &requested).await.unwrap();
        assert!(plan
            .conflicts
            .iter()
            .any(|conflict| conflict.kind == ImportConflictKind::MissingItem));
        assert!(plan
            .conflicts
            .iter()
            .any(|conflict| conflict.kind == ImportConflictKind::ParentMismatch));
        store.clear_log();
        let report = apply_metadata_plan(&store, &plan).await.unwrap();
        assert_eq!(report.status, ApplyStatus::Blocked);
        assert!(store.mutation_log().iter().all(|entry| !matches!(
            entry.operation,
            StoreOperation::SetCollectionLabel
                | StoreOperation::SetItemLabel
                | StoreOperation::SetItemAttributes
        )));
    }

    #[tokio::test]
    async fn inaccessible_unrelated_collection_blocks_global_identity_preflight() {
        let store = memory_store();
        store.inject_failure(
            StoreOperation::ListItems,
            2,
            "unrelated collection is inaccessible",
        );
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        assert!(plan.conflicts.iter().any(|conflict| {
            conflict.kind == ImportConflictKind::InaccessibleCollection
                && conflict.collection_path == "/collection/b"
        }));
        assert!(apply_metadata_plan(&store, &plan).await.unwrap().status == ApplyStatus::Blocked);
    }

    #[tokio::test]
    async fn unrepresentable_current_metadata_blocks_recovery_and_writes() {
        let store = memory_store();
        store.insert_collection(CollectionInfo {
            path: "/collection/a".into(),
            label: "x".repeat(MAX_LABEL_BYTES + 1),
            locked: false,
        });
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        assert!(plan.conflicts.iter().any(|conflict| {
            conflict.kind == ImportConflictKind::UnrepresentableMetadata
                && conflict.collection_path == "/collection/a"
        }));
        store.clear_log();
        let report = apply_metadata_plan(&store, &plan).await.unwrap();
        assert_eq!(report.status, ApplyStatus::Blocked);
        assert!(store.mutation_log().is_empty());
    }

    #[tokio::test]
    async fn second_preflight_rejects_concurrent_change_with_zero_writes() {
        let store = memory_store();
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        let target = ItemTarget::new("/collection/a", "/collection/a/item/one");
        store
            .set_item_label(&target, "Concurrent label")
            .await
            .unwrap();
        store.clear_log();

        let report = apply_metadata_plan(&store, &plan).await.unwrap();
        assert_eq!(report.status, ApplyStatus::Blocked);
        assert_eq!(report.field_operations_applied, 0);
        assert!(report
            .conflicts
            .iter()
            .any(|conflict| conflict.kind == ImportConflictKind::ConcurrentChange));
        assert_eq!(
            report.conflicts[0].item_path.as_deref(),
            Some("/collection/a/item/one")
        );
        assert!(store.mutation_log().iter().all(|entry| !matches!(
            entry.operation,
            StoreOperation::SetCollectionLabel
                | StoreOperation::SetItemLabel
                | StoreOperation::SetItemAttributes
        )));
    }

    #[tokio::test]
    async fn apply_stops_on_first_field_failure_and_writes_partial_report() {
        let store = memory_store();
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        store.inject_failure(
            StoreOperation::SetItemAttributes,
            1,
            "injected attribute failure",
        );
        let directory = tempfile::tempdir().unwrap();
        let report_path = directory.path().join("report.json");
        let report = apply_metadata_plan_with_report(&store, &plan, &report_path, None)
            .await
            .unwrap();

        assert_eq!(report.status, ApplyStatus::Partial);
        assert_eq!(report.collections_changed, 1);
        assert_eq!(report.items_changed, 1);
        assert_eq!(report.field_operations_applied, 2);
        assert_eq!(report.failures.len(), 1);
        let durable: ApplyReport =
            serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        assert_eq!(durable, report);
        assert_eq!(
            fs::metadata(&report_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            store.item("/collection/a/item/one").unwrap().attributes["account"],
            "current"
        );
    }

    #[tokio::test]
    async fn first_and_middle_failures_stop_later_writes() {
        let first_store = memory_store();
        let first_plan = plan_metadata_import(&first_store, &requested_metadata())
            .await
            .unwrap();
        first_store.inject_failure(
            StoreOperation::SetCollectionLabel,
            1,
            "first operation failed",
        );
        let first_report = apply_metadata_plan(&first_store, &first_plan)
            .await
            .unwrap();
        assert_eq!(first_report.status, ApplyStatus::Partial);
        assert_eq!(first_report.field_operations_attempted, 1);
        assert_eq!(first_report.field_operations_applied, 0);
        assert_eq!(
            first_store.item("/collection/a/item/one").unwrap().label,
            "Current item"
        );

        let middle_store = memory_store();
        let middle_plan = plan_metadata_import(&middle_store, &requested_metadata())
            .await
            .unwrap();
        middle_store.inject_failure(StoreOperation::SetItemLabel, 1, "middle operation failed");
        let middle_report = apply_metadata_plan(&middle_store, &middle_plan)
            .await
            .unwrap();
        assert_eq!(middle_report.status, ApplyStatus::Partial);
        assert_eq!(middle_report.field_operations_applied, 1);
        assert_eq!(
            middle_store.collection("/collection/a").unwrap().label,
            "Proposed collection"
        );
        let item = middle_store.item("/collection/a/item/one").unwrap();
        assert_eq!(item.label, "Current item");
        assert_eq!(item.attributes["account"], "current");
    }

    #[tokio::test]
    async fn operation_and_relock_failures_remain_separate_in_apply_report() {
        let store = memory_store();
        store
            .set_collection_locked("/collection/a", true)
            .await
            .unwrap();
        let plan = plan_metadata_import(&store, &requested_metadata())
            .await
            .unwrap();
        store.inject_failure(
            StoreOperation::SetCollectionLabel,
            1,
            "provider write failed",
        );
        store.inject_failure(
            StoreOperation::RelockCollection,
            1,
            "provider relock failed",
        );

        let report = apply_metadata_plan(&store, &plan).await.unwrap();
        assert_eq!(report.status, ApplyStatus::Partial);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("provider write failed"));
        assert_eq!(report.relock_failures.len(), 1);
        assert!(report.relock_failures[0].contains("provider relock failed"));
        assert_eq!(store.pending_temporary_unlocks().len(), 1);
    }

    #[tokio::test]
    async fn recovery_metadata_is_directly_reusable() {
        let store = memory_store();
        let requested = requested_metadata();
        let plan = plan_metadata_import(&store, &requested).await.unwrap();
        let applied = apply_metadata_plan(&store, &plan).await.unwrap();
        assert!(applied.is_complete());

        let recovery_plan = plan_metadata_import(&store, &plan.recovery).await.unwrap();
        assert!(recovery_plan.conflicts.is_empty());
        let recovered = apply_metadata_plan(&store, &recovery_plan).await.unwrap();
        assert!(recovered.is_complete());
        assert_eq!(
            store.collection("/collection/a").unwrap().label,
            "Current collection"
        );
        let item = store.item("/collection/a/item/one").unwrap();
        assert_eq!(item.label, "Current item");
        assert_eq!(item.attributes["account"], "current");
        let secret = store
            .reveal_secret(&ItemTarget::new("/collection/a", "/collection/a/item/one"))
            .await
            .unwrap();
        assert_eq!(secret.value.secret.as_slice(), &[0, 1, 0xfe, 0xff]);
        assert_eq!(secret.value.content_type, "application/octet-stream");
    }
}
