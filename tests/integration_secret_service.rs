use secretui::domain::{Attributes, NewItem, SecretBytes};
use secretui::store::SecretStore;
use secretui::store_secret_service::SecretServiceStore;

#[tokio::test]
#[ignore = "requires SECRETUI_INTEGRATION=1 and a live Secret Service"]
async fn live_secret_service_create_reveal_delete_namespaced_item() {
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
    let mut attrs = Attributes::new();
    attrs.insert("application".into(), "secretui:test".into());
    attrs.insert("secretui:test:id".into(), format!("{}", std::process::id()));

    let item = store
        .create_item(NewItem {
            collection_path: collection.path,
            label: "secretui integration test".into(),
            attributes: attrs,
            secret: SecretBytes::new(b"secretui-test-secret".to_vec()),
            content_type: "text/plain".into(),
        })
        .await
        .unwrap();

    let reveal_result = store.reveal_secret(&item.path).await;
    let delete_result = store.delete_item(&item.path).await;
    delete_result.expect("clean up integration item");
    let secret = reveal_result.expect("reveal integration item");
    assert_eq!(secret.as_slice(), b"secretui-test-secret");
}
