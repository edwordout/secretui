use super::*;
use crate::domain::{
    Attributes, CollectionInfo, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile,
};
use crate::store::{ItemTarget, MemorySecretStore, SecretStore, StoreOperation};
use std::collections::BTreeMap;
use std::fs;
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
    let durable: ApplyReport = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
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
