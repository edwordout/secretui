use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use secretui::domain::{Attributes, NewItem};
use secretui::store::SecretStore;
use secretui::store_secret_service::SecretServiceStore;

#[tokio::test]
async fn live_secret_service_create_reveal_delete_namespaced_item() {
    if std::env::var("SECRETUI_INTEGRATION").ok().as_deref() != Some("1") {
        eprintln!("skipped: set SECRETUI_INTEGRATION=1 to run live Secret Service tests");
        return;
    }

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
            secret: b"secretui-test-secret".to_vec(),
            content_type: "text/plain".into(),
        })
        .await
        .unwrap();

    let secret = store.reveal_secret(&item.path).await.unwrap();
    assert_eq!(
        BASE64.encode(secret),
        BASE64.encode(b"secretui-test-secret")
    );

    store.delete_item(&item.path).await.unwrap();
}
