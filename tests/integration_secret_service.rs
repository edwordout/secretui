use secretui::domain::{
    Attributes, CollectionMetadata, ItemMetadata, MetadataFile, NewItem, SecretBytes,
};
use secretui::store::SecretStore;
use secretui::store_secret_service::SecretServiceStore;

#[tokio::test]
#[ignore = "requires SECRETUI_INTEGRATION=1 and a live Secret Service"]
async fn live_secret_service_record_lifecycle_and_metadata() {
    assert_eq!(
        std::env::var("SECRETUI_INTEGRATION").ok().as_deref(),
        Some("1"),
        "set SECRETUI_INTEGRATION=1"
    );

    let store = SecretServiceStore::connect().await.unwrap();
    let collection = store
        .list_collections()
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("collection");

    let toggled = store
        .set_collection_locked(&collection.path, !collection.locked)
        .await;
    let observed_lock = store
        .list_collections()
        .await
        .ok()
        .and_then(|collections| {
            collections
                .into_iter()
                .find(|candidate| candidate.path == collection.path)
        })
        .map(|candidate| candidate.locked);
    let restored = store
        .set_collection_locked(&collection.path, collection.locked)
        .await;
    restored.expect("restore collection lock state");
    toggled.expect("toggle collection lock state");
    assert_eq!(observed_lock, Some(!collection.locked));

    let test_id = format!("{}", std::process::id());
    let mut attributes = Attributes::new();
    attributes.insert("application".into(), "secretui:test".into());
    attributes.insert("secretui:test:id".into(), test_id);
    attributes.insert("secretui:test:change".into(), "before".into());
    attributes.insert("secretui:test:remove".into(), "remove-me".into());

    let item = store
        .create_item(NewItem {
            collection_path: collection.path.clone(),
            label: "secretui integration test".into(),
            attributes,
            secret: SecretBytes::new(b"secretui-test-secret".to_vec()),
            content_type: "text/plain".into(),
        })
        .await
        .unwrap();

    let test_result: anyhow::Result<()> = async {
        let secret = store.reveal_secret(&item.path).await?;
        assert_eq!(secret.as_slice(), b"secretui-test-secret");

        let mut edited_attributes = item.attributes.clone();
        edited_attributes.remove("secretui:test:remove");
        edited_attributes.insert("secretui:test:change".into(), "after".into());
        edited_attributes.insert("secretui:test:add".into(), "added".into());
        store
            .edit_item(
                &item.path,
                Some("secretui integration edited"),
                Some(edited_attributes.clone()),
                None,
            )
            .await?;

        let edited_item = store
            .list_items(&collection.path)
            .await?
            .into_iter()
            .find(|candidate| candidate.path == item.path)
            .expect("edited integration item");
        assert_eq!(edited_item.attributes["secretui:test:change"], "after");
        assert_eq!(edited_item.attributes["secretui:test:add"], "added");
        assert!(!edited_item.attributes.contains_key("secretui:test:remove"));

        let mut imported_attributes = edited_attributes;
        imported_attributes.insert("secretui:test:metadata".into(), "applied".into());
        let metadata = MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: collection.path.clone(),
                label: collection.label.clone(),
                locked: collection.locked,
                items: vec![ItemMetadata {
                    path: item.path.clone(),
                    label: "secretui integration imported".into(),
                    locked: false,
                    attributes: imported_attributes.clone(),
                    created: None,
                    modified: None,
                }],
            }],
        };

        let preview = store.preview_metadata_import(&metadata).await?;
        assert_eq!(preview.collections_changed, 0);
        assert_eq!(preview.items_changed, 1);
        assert_eq!(preview.paths_missing, 0);
        assert_eq!(store.import_metadata(metadata).await?, 1);

        let imported_item = store
            .list_items(&collection.path)
            .await?
            .into_iter()
            .find(|candidate| candidate.path == item.path)
            .expect("imported integration item");
        assert_eq!(imported_item.label, "secretui integration imported");
        assert_eq!(
            imported_item.attributes["secretui:test:metadata"],
            "applied"
        );
        Ok(())
    }
    .await;

    let cleanup_result = store.delete_item(&item.path).await;
    cleanup_result.expect("clean up integration item");
    test_result.unwrap();
}
