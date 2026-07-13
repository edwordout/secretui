use super::*;

pub(super) async fn activate_detail_action(
    store: &impl SecretStore,
    app: &mut TuiApp,
) -> Result<()> {
    match app.selected_action() {
        DetailAction::Reveal => reveal_selected(store, app).await?,
        DetailAction::CopyText => copy_selected(store, app, CopyEncoding::Text).await?,
        DetailAction::CopyBase64 => copy_selected(store, app, CopyEncoding::Base64).await?,
        DetailAction::CopyHex => copy_selected(store, app, CopyEncoding::Hex).await?,
        DetailAction::Edit => start_edit_item(app),
        DetailAction::ReplaceSecret => start_replace_secret(app),
        DetailAction::Delete => start_delete_item(app),
        DetailAction::LockUnlock => lock_toggle(store, app).await?,
        DetailAction::Back => app.previous_page(),
    }
    Ok(())
}

pub(super) fn start_new_collection(app: &mut TuiApp) {
    app.clear_reveal();
    app.delete_snapshot = None;
    app.form = Some(FormState {
        kind: FormKind::NewCollection,
        fields: vec![FormField::text("Label", ""), FormField::text("Alias", "")],
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        parent: None,
        message: "Create a Secret Service collection.".into(),
        dirty: false,
    });
    app.page = Page::Form;
}

pub(super) fn start_new_item(app: &mut TuiApp) {
    if app.selected_collection().is_none() {
        app.message = "create or select a collection first".into();
        return;
    }
    app.clear_reveal();
    app.delete_snapshot = None;
    app.form = Some(FormState {
        kind: FormKind::NewItem,
        fields: vec![
            FormField::text("Label", ""),
            FormField::attributes(0),
            FormField::secret("Secret"),
            FormField::text("Secret content type", "text/plain"),
        ],
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        parent: None,
        message: "Create an item in the selected collection.".into(),
        dirty: false,
    });
    app.page = Page::Form;
}

pub(super) fn start_edit_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    let attribute_count = item.attributes.len();
    app.clear_reveal();
    app.delete_snapshot = None;
    app.form = Some(FormState {
        kind: FormKind::EditItem,
        fields: vec![
            FormField::text("Label", item.label),
            FormField::attributes(attribute_count),
        ],
        attributes: item.attributes,
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: Some(item.path),
        parent: None,
        message: "Edit metadata only. Secret replacement is a separate Details action.".into(),
        dirty: false,
    });
    app.page = Page::Form;
}

pub(super) fn start_replace_secret(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    app.clear_reveal();
    app.delete_snapshot = None;
    app.form = Some(FormState {
        kind: FormKind::ReplaceSecret,
        fields: vec![
            FormField::secret("New secret"),
            FormField::text("Content type", "text/plain"),
        ],
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: Some(item.path),
        parent: None,
        message:
            "Replace the secret value explicitly. Metadata is not changed and replacement is never automatically rolled back."
                .into(),
        dirty: false,
    });
    app.page = Page::Form;
}

pub(super) fn start_delete_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    let Some(collection) = app.selected_collection().cloned() else {
        return;
    };
    app.clear_reveal();
    app.delete_snapshot = Some(DeleteSnapshot {
        collection,
        item: item.clone(),
    });
    app.form = Some(FormState {
        kind: FormKind::DeleteItem,
        fields: Vec::new(),
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: true,
        target_item_path: Some(item.path),
        parent: None,
        message: "Review the captured metadata before deleting.".into(),
        dirty: false,
    });
    app.page = Page::Form;
}

pub(super) async fn submit_new_collection(
    store: &impl SecretStore,
    app: &mut TuiApp,
) -> Result<()> {
    let label = form_value(app, 0).trim().to_owned();
    if label.is_empty() {
        set_form_message(app, "Label is required.");
        set_form_field_error(app, 0, "required");
        return Ok(());
    }
    let alias = form_value(app, 1).trim().to_owned();
    let outcome = store
        .create_collection(NewCollection { label, alias })
        .await?;
    let message = outcome_message("collection created", &outcome);
    let collection = outcome.value;
    clear_form_secrets(app);
    app.form = None;
    app.refresh_all(store).await?;
    if let Some(index) = app
        .collections
        .iter()
        .position(|existing| existing.path == collection.path)
    {
        app.selected_collection = index;
    }
    app.page = Page::Collections;
    app.message = message;
    app.sync_states();
    Ok(())
}

pub(super) async fn submit_new_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(collection) = app.selected_collection().cloned() else {
        return Ok(());
    };
    let label = form_value(app, 0).trim().to_owned();
    if label.is_empty() {
        set_form_message(app, "Label is required.");
        set_form_field_error(app, 0, "required");
        return Ok(());
    }
    let attributes = app
        .form
        .as_ref()
        .map(|form| form.attributes.clone())
        .unwrap_or_default();
    let secret = SecretBytes::new(form_value(app, 2).into_bytes());
    let content_type = non_empty_or(form_value(app, 3), "text/plain");
    let outcome = store
        .create_item(NewItem {
            collection_path: collection.path,
            label,
            attributes,
            secret,
            content_type,
        })
        .await?;
    let message = outcome_message("item created", &outcome);
    let item = outcome.value;
    clear_form_secrets(app);
    app.form = None;
    app.refresh_items(store).await?;
    if let Some(index) = app
        .items
        .iter()
        .position(|existing| existing.path == item.path)
    {
        app.selected_item = index;
    }
    app.page = Page::Items;
    app.message = message;
    app.sync_states();
    Ok(())
}

pub(super) async fn submit_edit_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(form) = app.form.as_ref() else {
        return Ok(());
    };
    let Some(item_path) = form.target_item_path.clone() else {
        return Ok(());
    };
    let Some(original) = app
        .items
        .iter()
        .find(|item| item.path == item_path)
        .cloned()
    else {
        set_form_message(
            app,
            "The item is no longer available. Refresh before editing.",
        );
        return Ok(());
    };
    let target = ItemTarget::from(&original);
    let label = form_value(app, 0);
    if label.trim().is_empty() {
        set_form_message(app, "Label is required.");
        set_form_field_error(app, 0, "required");
        return Ok(());
    }
    let attributes = app
        .form
        .as_ref()
        .map(|form| form.attributes.clone())
        .unwrap_or_default();
    let label_changed = label != original.label;
    let attributes_changed = attributes != original.attributes;
    let mut operations = Vec::new();
    let mut warnings = Vec::new();
    let mut post_label_snapshot = None;
    let mut post_label_snapshot_issue = None;

    if label_changed || attributes_changed {
        let current = store
            .list_items(&target.collection_path)
            .await?
            .into_iter()
            .find(|item| item.path == target.item_path);
        if current.as_ref() != Some(&original) {
            set_form_message(
                app,
                "Edit blocked: the item disappeared or its metadata changed after this form opened. Cancel and review the current item.",
            );
            return Ok(());
        }
    }

    if label_changed {
        let outcome = store.set_item_label(&target, &label).await?;
        warnings.extend(outcome.warnings);
        operations.push("label");
        match store.list_items(&target.collection_path).await {
            Ok(items) => {
                post_label_snapshot = items.into_iter().find(|item| {
                    item.path == target.item_path
                        && item.collection_path == original.collection_path
                        && item.label == label
                        && item.attributes == original.attributes
                        && item.locked == original.locked
                        && item.created == original.created
                });
                if post_label_snapshot.is_none() {
                    post_label_snapshot_issue = Some(
                        "the fresh object did not retain the original attributes, lock state, and creation identity"
                            .into(),
                    );
                }
            }
            Err(error) => {
                post_label_snapshot_issue = Some(format!("the fresh read failed: {error:#}"));
            }
        }
    }

    if attributes_changed {
        if label_changed {
            let Some(post_label_snapshot) = post_label_snapshot.as_ref() else {
                let issue = post_label_snapshot_issue
                    .as_deref()
                    .unwrap_or("no matching object was returned");
                clear_form_secrets(app);
                return Err(anyhow::anyhow!(
                    "partial edit: the label changed, but attributes were not attempted because the post-label concurrency check was unavailable: {issue}"
                ));
            };
            let refreshed = store
                .list_items(&target.collection_path)
                .await?
                .into_iter()
                .find(|item| item.path == target.item_path);
            if refreshed.as_ref() != Some(post_label_snapshot) {
                clear_form_secrets(app);
                return Err(anyhow::anyhow!(
                    "partial edit: the label changed, but attributes were not attempted because a fresh read found a concurrent change"
                ));
            }
        }
        match store.set_item_attributes(&target, attributes.clone()).await {
            Ok(outcome) => {
                warnings.extend(outcome.warnings);
                operations.push("attributes");
            }
            Err(attributes_error) if label_changed => {
                let rollback = rollback_label_if_unchanged(
                    store,
                    &target,
                    &original.label,
                    post_label_snapshot.as_ref(),
                    post_label_snapshot_issue.as_deref(),
                )
                .await;
                let prior_warnings = if warnings.is_empty() {
                    String::new()
                } else {
                    format!("; warnings before failure: {}", warning_text(&warnings))
                };
                clear_form_secrets(app);
                return Err(anyhow::anyhow!(
                    "the attribute operation failed or could not be verified after the label was applied; attributes may already have changed: {attributes_error:#}; {rollback}{prior_warnings}"
                ));
            }
            Err(attributes_error) => return Err(attributes_error),
        }
    }

    let success = if operations.is_empty() {
        "item already matched the submitted metadata; no changes applied".into()
    } else {
        format!("item edited: {} changed", operations.join(", "))
    };
    let message = message_with_warnings(success, &warnings);
    clear_form_secrets(app);
    app.form = None;
    app.refresh_items(store).await?;
    if let Some(index) = app
        .items
        .iter()
        .position(|existing| existing.path == item_path)
    {
        app.selected_item = index;
    }
    app.page = Page::Details;
    app.message = message;
    app.sync_states();
    Ok(())
}

pub(super) async fn submit_replace_secret(
    store: &impl SecretStore,
    app: &mut TuiApp,
) -> Result<()> {
    let Some(item_path) = app
        .form
        .as_ref()
        .and_then(|form| form.target_item_path.clone())
    else {
        return Ok(());
    };
    let Some(item) = app
        .items
        .iter()
        .find(|item| item.path == item_path)
        .cloned()
    else {
        set_form_message(
            app,
            "The item is no longer available. Refresh before replacing its secret.",
        );
        return Ok(());
    };
    let secret = SecretBytes::new(form_value(app, 0).into_bytes());
    if secret.as_slice().is_empty() {
        set_form_message(app, "New secret is required.");
        set_form_field_error(app, 0, "required");
        return Ok(());
    }
    let content_type = non_empty_or(form_value(app, 1), "text/plain");
    let current = store
        .list_items(&item.collection_path)
        .await?
        .into_iter()
        .find(|current| current.path == item.path);
    if current.as_ref() != Some(&item) {
        set_form_message(
            app,
            "Secret replacement blocked: the item disappeared or its metadata changed after this form opened. Cancel and review the current item.",
        );
        return Ok(());
    }
    let outcome = store
        .replace_item_secret(&ItemTarget::from(&item), secret.as_slice(), &content_type)
        .await?;
    let message = outcome_message(
        "secret replaced; metadata was unchanged and no automatic rollback was attempted",
        &outcome,
    );

    clear_form_secrets(app);
    app.form = None;
    app.refresh_items(store).await?;
    if let Some(index) = app
        .items
        .iter()
        .position(|existing| existing.path == item_path)
    {
        app.selected_item = index;
    }
    app.page = Page::Details;
    app.message = message;
    app.sync_states();
    Ok(())
}

pub(super) async fn submit_delete_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(snapshot) = app.delete_snapshot.clone() else {
        return Ok(());
    };
    let current_collection = store
        .list_collections()
        .await?
        .into_iter()
        .find(|collection| collection.path == snapshot.collection.path);
    if current_collection.as_ref() != Some(&snapshot.collection) {
        set_form_message(
            app,
            "Deletion blocked: the collection disappeared or its metadata changed after this confirmation opened. Cancel and review the current collection.",
        );
        return Ok(());
    }
    let current = store
        .list_items(&snapshot.collection.path)
        .await?
        .into_iter()
        .find(|item| item.path == snapshot.item.path);
    if current.as_ref() != Some(&snapshot.item) {
        set_form_message(
            app,
            "Deletion blocked: the item disappeared or its metadata changed after this confirmation opened. Cancel and review the current item.",
        );
        return Ok(());
    }

    let outcome = store.delete_item(&ItemTarget::from(&snapshot.item)).await?;
    let message = outcome_message("item deleted", &outcome);
    clear_form_secrets(app);
    app.form = None;
    app.delete_snapshot = None;
    app.refresh_items(store).await?;
    app.page = Page::Items;
    app.message = message;
    app.sync_states();
    Ok(())
}

pub(super) async fn rollback_label_if_unchanged(
    store: &impl SecretStore,
    target: &ItemTarget,
    original_label: &str,
    post_label_snapshot: Option<&ItemInfo>,
    post_label_snapshot_issue: Option<&str>,
) -> String {
    let Some(post_label_snapshot) = post_label_snapshot else {
        return format!(
            "label rollback was skipped because the post-label state could not be verified: {}",
            post_label_snapshot_issue.unwrap_or("no matching object was returned")
        );
    };
    let refreshed = match store.list_items(&target.collection_path).await {
        Ok(items) => items,
        Err(error) => {
            return format!(
                "label rollback was skipped because a fresh concurrency check failed: {error:#}"
            );
        }
    };
    if refreshed
        .into_iter()
        .find(|item| item.path == target.item_path)
        .as_ref()
        != Some(post_label_snapshot)
    {
        return "label rollback was skipped because a fresh read found a concurrent change".into();
    }
    match store.set_item_label(target, original_label).await {
        Ok(outcome) if outcome.warnings.is_empty() => "the label rollback completed".into(),
        Ok(outcome) => format!(
            "the label rollback completed with warnings: {}",
            warning_text(&outcome.warnings)
        ),
        Err(error) => format!("the label rollback failed: {error:#}"),
    }
}

pub(super) fn outcome_message<T>(success: &str, outcome: &StoreOutcome<T>) -> String {
    message_with_warnings(success.into(), &outcome.warnings)
}

pub(super) fn message_with_warnings(
    success: String,
    warnings: &[crate::store::StoreWarning],
) -> String {
    if warnings.is_empty() {
        success
    } else {
        format!(
            "{success}; WARNING: operation succeeded but cleanup needs attention: {}",
            warning_text(warnings)
        )
    }
}

pub(super) fn warning_text(warnings: &[crate::store::StoreWarning]) -> String {
    warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn set_form_message(app: &mut TuiApp, message: &str) {
    if let Some(form) = &mut app.form {
        form.message = message.into();
    }
}

pub(super) fn set_form_field_error(app: &mut TuiApp, index: usize, message: &str) {
    if let Some(field) = app
        .form
        .as_mut()
        .and_then(|form| form.fields.get_mut(index))
    {
        field.error = Some(message.into());
    }
}

pub(super) fn clear_form_secrets(app: &mut TuiApp) {
    if let Some(form) = &mut app.form {
        clear_form_state_secrets(form);
    }
}

pub(super) fn clear_form_state_secrets(form: &mut FormState) {
    for field in &mut form.fields {
        if field.secret {
            field.value.zeroize();
        }
    }
    if let Some(parent) = form.parent.as_mut() {
        clear_form_state_secrets(parent);
    }
}

pub(super) fn form_value(app: &TuiApp, index: usize) -> String {
    app.form
        .as_ref()
        .and_then(|form| form.fields.get(index))
        .map(|field| field.value.clone())
        .unwrap_or_default()
}

pub(super) fn update_parent_attribute_summary(form: &mut FormState) {
    if let Some(field) = form
        .fields
        .iter_mut()
        .find(|field| field.kind == FormFieldKind::Attributes)
    {
        field.value = format!("{} attribute(s) (Enter to edit)", form.attributes.len());
    }
}

pub(super) fn non_empty_or(value: String, fallback: &str) -> String {
    let value = value.trim().to_owned();
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}
