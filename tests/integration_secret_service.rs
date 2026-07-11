use anyhow::{anyhow, bail, ensure, Context, Result};
use secretui::domain::{
    Attributes, CollectionMetadata, ItemMetadata, MetadataFile, NewCollection, NewItem, SecretBytes,
};
use secretui::metadata::{apply_metadata_plan, plan_metadata_import};
use secretui::store::{ItemTarget, SecretStore, StoreOutcome};
use secretui::store_secret_service::SecretServiceStore;
use secretui::terminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
#[ignore = "requires SECRETUI_INTEGRATION=1 and a live Secret Service"]
async fn live_secret_service_isolated_lifecycle_and_metadata_repair() -> Result<()> {
    ensure!(
        std::env::var("SECRETUI_INTEGRATION").ok().as_deref() == Some("1"),
        "set SECRETUI_INTEGRATION=1"
    );

    let store = SecretServiceStore::connect().await?;
    let test_id = unique_test_id();
    let collection_label = format!("SecretUI integration test {test_id}");
    let collection_alias = format!("secretui-integration-{test_id}");
    let collection = match store
        .create_collection(NewCollection {
            label: collection_label.clone(),
            alias: collection_alias,
        })
        .await
    {
        Ok(outcome) => outcome.value,
        Err(create_error) => {
            let cleanup = cleanup_collections_with_label(&store, &collection_label).await;
            print_manual_cleanup(&collection_label, &[]);
            return combine_results(
                Err(create_error.context(
                    "create isolated test collection; creation may already have occurred",
                )),
                cleanup,
            );
        }
    };

    let mut item_target = None;
    let test_result = async {
        let mut attributes = Attributes::new();
        attributes.insert("application".into(), "secretui:integration-test".into());
        attributes.insert("secretui:test:id".into(), test_id.clone());
        attributes.insert("secretui:test:change".into(), "before".into());
        attributes.insert("secretui:test:remove".into(), "remove-me".into());

        let item_outcome = store
            .create_item(NewItem {
                collection_path: collection.path.clone(),
                label: format!("SecretUI integration item {test_id}"),
                attributes,
                secret: SecretBytes::new(vec![0x00, 0xff, 0x10, 0x80, b'A']),
                content_type: "application/octet-stream".into(),
            })
            .await?;
        ensure_no_warnings("create item", &item_outcome)?;
        let item = item_outcome.value;
        let target = ItemTarget::from(&item);
        item_target = Some(target.clone());

        let initial_secret = store.reveal_secret(&target).await?;
        ensure_no_warnings("initial secret read", &initial_secret)?;
        ensure!(
            initial_secret.value.secret.as_slice() == [0x00, 0xff, 0x10, 0x80, b'A'],
            "binary secret bytes did not round-trip"
        );
        ensure!(
            initial_secret.value.content_type == "application/octet-stream",
            "binary secret content type did not round-trip"
        );

        let mut edited_attributes = item.attributes.clone();
        edited_attributes.remove("secretui:test:remove");
        edited_attributes.insert("secretui:test:change".into(), "after".into());
        edited_attributes.insert("secretui:test:add".into(), "added".into());
        let label_outcome = store
            .set_item_label(&target, "SecretUI integration edited")
            .await?;
        ensure_no_warnings("edit item label", &label_outcome)?;
        let attributes_outcome = store
            .set_item_attributes(&target, edited_attributes.clone())
            .await?;
        ensure_no_warnings("edit item attributes", &attributes_outcome)?;

        let edited_item = find_item(&store, &target).await?;
        ensure!(
            edited_item.label == "SecretUI integration edited",
            "item label edit was not preserved"
        );
        ensure!(
            edited_item
                .attributes
                .get("secretui:test:change")
                .map(String::as_str)
                == Some("after"),
            "changed attribute was not preserved"
        );
        ensure!(
            edited_item
                .attributes
                .get("secretui:test:add")
                .map(String::as_str)
                == Some("added"),
            "added attribute was not preserved"
        );
        ensure!(
            !edited_item.attributes.contains_key("secretui:test:remove"),
            "removed attribute was retained"
        );
        verify_secret_unchanged(&store, &target, "after direct metadata edits").await?;

        // Lock the isolated collection intentionally. Subsequent explicit operations may
        // temporarily unlock it, but must restore this captured state.
        let lock_outcome = store.set_collection_locked(&collection.path, true).await?;
        ensure_no_warnings("lock isolated collection", &lock_outcome)?;
        let locked_secret = store.reveal_secret(&target).await?;
        ensure_no_warnings("read from locked collection", &locked_secret)?;
        ensure!(
            locked_secret.value.secret.as_slice() == [0x00, 0xff, 0x10, 0x80, b'A'],
            "secret read from locked collection returned different bytes"
        );
        let observed_collection = store
            .list_collections()
            .await?
            .into_iter()
            .find(|candidate| candidate.path == collection.path)
            .context("isolated collection disappeared after temporary unlock")?;
        ensure!(
            observed_collection.locked,
            "temporary secret access did not restore the collection lock"
        );

        let current_item = find_item(&store, &target).await?;
        let mut imported_attributes = current_item.attributes.clone();
        imported_attributes.insert("secretui:test:metadata".into(), "applied".into());
        let requested = MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: observed_collection.path.clone(),
                label: observed_collection.label.clone(),
                locked: observed_collection.locked,
                items: vec![ItemMetadata {
                    path: current_item.path.clone(),
                    label: "SecretUI integration repaired".into(),
                    locked: current_item.locked,
                    attributes: imported_attributes.clone(),
                    created: current_item.created,
                    modified: current_item.modified,
                }],
            }],
        };

        let plan = plan_metadata_import(&store, &requested).await?;
        ensure!(plan.conflicts.is_empty(), "repair plan had conflicts");
        ensure!(
            plan.changes.len() == 1,
            "repair plan was not the expected item change"
        );
        let report = apply_metadata_plan(&store, &plan).await?;
        ensure!(
            report.is_complete(),
            "metadata repair did not complete: {}",
            terminal::error(&format!("{report:?}"))
        );

        let repaired_item = find_item(&store, &target).await?;
        ensure!(
            repaired_item.label == "SecretUI integration repaired",
            "metadata repair label was not applied"
        );
        ensure!(
            repaired_item
                .attributes
                .get("secretui:test:metadata")
                .map(String::as_str)
                == Some("applied"),
            "metadata repair attribute was not applied"
        );
        verify_secret_unchanged(&store, &target, "after metadata repair").await?;
        Ok(())
    }
    .await;

    let cleanup_result =
        cleanup_isolated_collection(&store, &collection.path, item_target.as_ref())
            .await
            .with_context(|| {
                print_manual_cleanup(
                    &collection_label,
                    &[
                        collection.path.as_str(),
                        item_target
                            .as_ref()
                            .map(|item| item.item_path.as_str())
                            .unwrap_or(""),
                    ],
                );
                "clean up isolated integration-test objects"
            });

    combine_results(test_result, cleanup_result)
}

async fn find_item(
    store: &impl SecretStore,
    target: &ItemTarget,
) -> Result<secretui::domain::ItemInfo> {
    store
        .list_items(&target.collection_path)
        .await?
        .into_iter()
        .find(|item| item.path == target.item_path)
        .with_context(|| format!("item {} disappeared", terminal::path(&target.item_path)))
}

async fn verify_secret_unchanged(
    store: &impl SecretStore,
    target: &ItemTarget,
    stage: &str,
) -> Result<()> {
    let outcome = store.reveal_secret(target).await?;
    ensure_no_warnings(stage, &outcome)?;
    ensure!(
        outcome.value.secret.as_slice() == [0x00, 0xff, 0x10, 0x80, b'A'],
        "binary secret changed {stage}"
    );
    ensure!(
        outcome.value.content_type == "application/octet-stream",
        "secret content type changed {stage}"
    );
    Ok(())
}

fn ensure_no_warnings<T>(operation: &str, outcome: &StoreOutcome<T>) -> Result<()> {
    if outcome.warnings.is_empty() {
        return Ok(());
    }
    let warnings = outcome
        .warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "{operation} completed with provider cleanup warnings: {}",
        terminal::error(&warnings)
    )
}

async fn cleanup_isolated_collection(
    store: &impl SecretStore,
    collection_path: &str,
    item_target: Option<&ItemTarget>,
) -> Result<()> {
    let cleanup = store.cleanup_temporary_unlocks().await;
    let cleanup_warning = match cleanup {
        Ok(outcome) if outcome.warnings.is_empty() => None,
        Ok(outcome) => Some(
            outcome
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        ),
        Err(error) => Some(format!("{error:#}")),
    };

    if collection_exists(store, collection_path).await?
        && store.delete_collection(collection_path).await.is_err()
        && collection_exists(store, collection_path).await?
    {
        if let Some(target) = item_target {
            let _ = store.delete_item(target).await;
        }
        store
            .delete_collection(collection_path)
            .await
            .with_context(|| format!("delete collection {}", terminal::path(collection_path)))?;
    }
    ensure!(
        !collection_exists(store, collection_path).await?,
        "collection {} still exists after cleanup",
        terminal::path(collection_path)
    );

    // A failed relock matters during normal use. Once the isolated object is confirmed deleted,
    // there is no lock left to restore, so the cleanup warning is retained only as context.
    if let Some(warning) = cleanup_warning {
        eprintln!(
            "integration cleanup note (object was removed): {}",
            terminal::error(&warning)
        );
    }
    Ok(())
}

async fn cleanup_collections_with_label(
    store: &impl SecretStore,
    collection_label: &str,
) -> Result<()> {
    let collections = store.list_collections().await?;
    let mut failures = Vec::new();
    for collection in collections
        .into_iter()
        .filter(|collection| collection.label == collection_label)
    {
        if let Err(error) = store.delete_collection(&collection.path).await {
            failures.push(format!("{}: {error:#}", terminal::path(&collection.path)));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("cleanup failed: {}", failures.join("; "))
    }
}

async fn collection_exists(store: &impl SecretStore, collection_path: &str) -> Result<bool> {
    Ok(store
        .list_collections()
        .await?
        .iter()
        .any(|collection| collection.path == collection_path))
}

fn combine_results(operation: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Err(cleanup_error)) => Err(anyhow!(
            "integration operation failed: {operation_error:#}; cleanup also failed: {cleanup_error:#}"
        )),
    }
}

fn print_manual_cleanup(collection_label: &str, paths: &[&str]) {
    eprintln!(
        "MANUAL CLEANUP MAY BE REQUIRED: remove the test collection labelled “{}” using your wallet administration tool.",
        terminal::label(collection_label)
    );
    for path in paths.iter().copied().filter(|path| !path.is_empty()) {
        eprintln!("Object path: {}", terminal::path(path));
    }
}

fn unique_test_id() -> String {
    if let Ok(uuid) = std::fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let uuid = uuid.trim();
        if !uuid.is_empty()
            && uuid
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return uuid.to_ascii_lowercase();
        }
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:x}-{:x}-{sequence:x}", std::process::id())
}
