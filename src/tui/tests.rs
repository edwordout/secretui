use super::*;
use crate::store::MemorySecretStore;
use ratatui::{backend::TestBackend, Terminal};

fn collection(path: &str, label: &str) -> CollectionInfo {
    CollectionInfo {
        path: path.into(),
        label: label.into(),
        locked: false,
    }
}

fn item(path: &str, label: &str) -> ItemInfo {
    ItemInfo {
        collection_path: "collection".into(),
        path: path.into(),
        label: label.into(),
        locked: false,
        attributes: Attributes::new(),
        created: None,
        modified: None,
    }
}

fn sample_app() -> TuiApp {
    let mut attrs = Attributes::new();
    attrs.insert("server".into(), "example".into());
    TuiApp::from_data(
        vec![
            CollectionInfo {
                path: "c1".into(),
                label: "One".into(),
                locked: false,
            },
            CollectionInfo {
                path: "c2".into(),
                label: "Two".into(),
                locked: false,
            },
        ],
        vec![
            ItemInfo {
                collection_path: "c1".into(),
                path: "p1".into(),
                label: "Alpha".into(),
                locked: false,
                attributes: attrs,
                created: None,
                modified: None,
            },
            ItemInfo {
                collection_path: "c1".into(),
                path: "p2".into(),
                label: "Beta".into(),
                locked: false,
                attributes: Attributes::new(),
                created: None,
                modified: None,
            },
        ],
    )
}

fn sample_store(app: &TuiApp) -> MemorySecretStore {
    let store = MemorySecretStore::new();
    for collection in &app.collections {
        store.insert_collection(collection.clone());
    }
    for item in &app.items {
        store
            .insert_item(item.clone(), b"secret".to_vec(), "text/plain")
            .unwrap();
    }
    store
}

#[test]
fn filter_checks_label_path_and_attrs() {
    let mut app = sample_app();
    app.filter = "example".into();
    assert_eq!(app.filtered_items().len(), 1);
}

#[test]
fn listings_sort_by_case_insensitive_label_with_deterministic_ties() {
    let app = TuiApp::from_data(
        vec![
            collection("c-beta", "beta"),
            collection("c-alpha-2", "alpha"),
            collection("c-empty", ""),
            collection("c-alpha-1", "alpha"),
            collection("c-upper", "ALPHA"),
        ],
        vec![
            item("i-beta", "beta"),
            item("i-alpha-2", "alpha"),
            item("i-empty", ""),
            item("i-alpha-1", "alpha"),
            item("i-upper", "ALPHA"),
        ],
    );

    assert_eq!(
        app.collections
            .iter()
            .map(|collection| collection.path.as_str())
            .collect::<Vec<_>>(),
        vec!["c-empty", "c-upper", "c-alpha-1", "c-alpha-2", "c-beta"]
    );
    assert_eq!(
        app.items
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        vec!["i-empty", "i-upper", "i-alpha-1", "i-alpha-2", "i-beta"]
    );
}

#[test]
fn filtering_preserves_sorted_item_order() {
    let mut app = TuiApp::from_data(
        Vec::new(),
        vec![
            item("item-beta", "beta"),
            item("item-alpha", "Alpha"),
            item("item-empty", ""),
        ],
    );
    app.filter = "item-".into();

    assert_eq!(
        app.filtered_items()
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        vec!["item-empty", "item-alpha", "item-beta"]
    );
}

#[test]
fn page_navigation_is_linear() {
    let mut app = sample_app();
    assert_eq!(app.page, Page::Collections);
    app.next_page();
    assert_eq!(app.page, Page::Items);
    app.next_page();
    assert_eq!(app.page, Page::Details);
    app.previous_page();
    assert_eq!(app.page, Page::Items);
}

#[test]
fn details_default_action_is_back() {
    let mut app = sample_app();
    app.page = Page::Items;
    app.next_page();
    assert_eq!(app.page, Page::Details);
    assert_eq!(app.selected_action(), DetailAction::Back);
}

#[tokio::test]
async fn reveal_requires_explicit_r_or_deliberate_action_selection() {
    let mut app = sample_app();
    let store = sample_store(&app);
    app.page = Page::Details;
    app.selected_action = 0;

    handle_key(
        &store,
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert_eq!(app.page, Page::Items);
    assert!(app.reveal.is_none());

    app.page = Page::Details;
    handle_key(
        &store,
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(app.reveal.is_some());
}

#[test]
fn arrows_move_current_page_selection() {
    let mut app = sample_app();
    app.move_selection(1);
    assert_eq!(app.selected_collection, 1);
    app.page = Page::Items;
    app.move_selection(1);
    assert_eq!(app.selected_item, 1);
}

#[test]
fn selection_clamps_at_bounds() {
    let mut app = sample_app();
    app.page = Page::Items;
    app.move_selection(99);
    assert_eq!(app.selected_item, 1);
    app.move_selection(-99);
    assert_eq!(app.selected_item, 0);
}

#[test]
fn filter_keeps_valid_selection() {
    let mut app = sample_app();
    app.selected_item = 1;
    app.filter = "Alpha".into();
    app.clamp_item_selection();
    assert_eq!(app.selected_item, 0);
}

#[test]
fn selection_filter_and_navigation_clear_revealed_secret() {
    fn set_reveal(app: &mut TuiApp) {
        app.reveal = Some(RevealState {
            item_path: "p1".into(),
            value: secret_value(b"secret", "text/plain"),
            expires_at: Instant::now() + SECRET_TTL,
        });
    }

    let mut app = sample_app();
    app.page = Page::Items;
    set_reveal(&mut app);
    app.move_selection(1);
    assert!(app.reveal.is_none());

    set_reveal(&mut app);
    app.jump_selection(false);
    assert!(app.reveal.is_none());

    set_reveal(&mut app);
    app.mode = InputMode::Search;
    handle_search_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(app.reveal.is_none());

    set_reveal(&mut app);
    app.page = Page::Details;
    app.previous_page();
    assert!(app.reveal.is_none());
}

#[test]
fn details_page_arrows_scroll_content() {
    let mut app = sample_app();
    app.page = Page::Details;
    app.move_selection(3);
    assert_eq!(app.detail_scroll, 3);
    app.move_selection(-1);
    assert_eq!(app.detail_scroll, 2);
}

#[test]
fn form_field_edits_at_cursor() {
    let mut field = FormField::text("Label", "ac");
    field.move_cursor(-1);
    field.insert('b');
    assert_eq!(field.value, "abc");
    field.backspace();
    assert_eq!(field.value, "ac");
    field.delete();
    assert_eq!(field.value, "a");
}

#[test]
fn adding_attribute_returns_to_dirty_parent() {
    let mut app = sample_app();
    start_new_item(&mut app);
    open_attribute_form(&mut app);
    let editor = app.form.as_mut().unwrap();
    editor.fields[0].value = "username".into();
    editor.fields[1].value = "john".into();
    save_attribute(&mut app);

    let form = app.form.as_ref().unwrap();
    assert_eq!(form.kind, FormKind::NewItem);
    assert_eq!(form.attributes.get("username").unwrap(), "john");
    assert!(form.dirty);
    assert!(form.fields[1].value.contains("1 attribute"));
}

#[test]
fn attribute_editor_cancel_keeps_parent_unchanged() {
    let mut app = sample_app();
    start_new_item(&mut app);
    open_attribute_form(&mut app);
    let editor = app.form.as_mut().unwrap();
    editor.fields[0].value = "username".into();
    editor.fields[1].value = "john".into();
    restore_parent_form(&mut app, "attribute unchanged");

    let form = app.form.as_ref().unwrap();
    assert!(form.attributes.is_empty());
    assert!(!form.dirty);
}

#[test]
fn editing_attribute_key_renames_it() {
    let mut app = sample_app();
    start_new_item(&mut app);
    app.form
        .as_mut()
        .unwrap()
        .attributes
        .insert("old".into(), "value".into());
    open_attribute_form(&mut app);
    app.form.as_mut().unwrap().fields[0].value = "new".into();
    save_attribute(&mut app);

    let attributes = &app.form.as_ref().unwrap().attributes;
    assert!(!attributes.contains_key("old"));
    assert_eq!(attributes.get("new").unwrap(), "value");
}

#[test]
fn duplicate_attribute_key_stays_in_invalid_editor() {
    let mut app = sample_app();
    start_new_item(&mut app);
    let parent = app.form.as_mut().unwrap();
    parent.attributes.insert("existing".into(), "one".into());
    parent.selected_attribute = parent.attributes.len();
    open_attribute_form(&mut app);
    let editor = app.form.as_mut().unwrap();
    editor.fields[0].value = "existing".into();
    editor.fields[1].value = "two".into();
    save_attribute(&mut app);

    let editor = app.form.as_ref().unwrap();
    assert_eq!(editor.kind, FormKind::NewAttribute);
    assert_eq!(editor.fields[0].error.as_deref(), Some("already exists"));
    assert_eq!(
        editor.parent.as_ref().unwrap().attributes["existing"],
        "one"
    );
}

#[test]
fn removing_attribute_returns_to_dirty_parent() {
    let mut app = sample_app();
    start_new_item(&mut app);
    app.form
        .as_mut()
        .unwrap()
        .attributes
        .insert("old".into(), "value".into());
    open_attribute_form(&mut app);
    remove_attribute(&mut app);

    let parent = app.form.as_ref().unwrap();
    assert!(parent.attributes.is_empty());
    assert!(parent.dirty);
    assert_eq!(parent.selected_attribute, 0);
}

#[test]
fn dirty_cancel_opens_confirmation_and_escape_keeps_editing() {
    let mut app = sample_app();
    start_new_item(&mut app);
    app.form.as_mut().unwrap().dirty = true;
    app.form.as_mut().unwrap().fields[0].value = "draft".into();

    cancel_or_confirm_form(&mut app);
    let confirmation = app.form.as_ref().unwrap();
    assert_eq!(confirmation.kind, FormKind::UnsavedChanges);
    assert_eq!(confirmation.selected_button, 2);
    assert_eq!(
        confirmation.parent.as_ref().unwrap().fields[0].value,
        "draft"
    );

    cancel_or_confirm_form(&mut app);
    let parent = app.form.as_ref().unwrap();
    assert_eq!(parent.kind, FormKind::NewItem);
    assert_eq!(parent.fields[0].value, "draft");
    assert!(parent.dirty);
}

#[test]
fn untouched_form_cancels_without_confirmation() {
    let mut app = sample_app();
    start_new_item(&mut app);
    cancel_or_confirm_form(&mut app);
    assert!(app.form.is_none());
    assert_eq!(app.page, Page::Items);
}

#[test]
fn parent_form_renders_attribute_list_and_create_row() {
    let mut app = sample_app();
    start_edit_item(&mut app);
    app.form.as_mut().unwrap().selected_field = 1;
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("server = example"), "{rendered}");
    assert!(rendered.contains("+ Create new attribute"), "{rendered}");
}

#[test]
fn arrows_cross_attribute_list_boundaries() {
    let mut app = sample_app();
    start_edit_item(&mut app);
    let form = app.form.as_mut().unwrap();
    form.selected_field = 1;
    form.selected_attribute = 0;

    form_down(&mut app);
    assert_eq!(app.form.as_ref().unwrap().selected_attribute, 1);
    form_down(&mut app);
    assert!(app.form.as_ref().unwrap().focus_buttons);
    form_up(&mut app);
    let form = app.form.as_ref().unwrap();
    assert_eq!(form.selected_field, 1);
    assert_eq!(form.selected_attribute, 1);
}

#[test]
fn short_form_keeps_focus_and_scroll_feedback_visible() {
    let mut app = sample_app();
    start_new_item(&mut app);
    app.form.as_mut().unwrap().selected_field = 3;
    keep_form_focus_visible(&mut app);
    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Secret content type"), "{rendered}");
    assert!(rendered.contains('↑') || rendered.contains('↓'));
}

fn secret_value(secret: &[u8], content_type: &str) -> SecretValue {
    SecretValue::new(secret.to_vec(), content_type.into())
}

#[test]
fn new_item_and_secret_replacement_content_type_default_to_text_plain() {
    let mut app = sample_app();
    start_new_item(&mut app);
    assert_eq!(form_value(&app, 3), "text/plain");
    assert_eq!(non_empty_or("  ".into(), "text/plain"), "text/plain");

    app.form = None;
    start_replace_secret(&mut app);
    assert_eq!(form_value(&app, 1), "text/plain");
}

#[test]
fn metadata_edit_and_secret_replacement_are_separate_forms() {
    let mut app = sample_app();
    start_edit_item(&mut app);
    let edit = app.form.as_ref().unwrap();
    assert_eq!(edit.kind, FormKind::EditItem);
    assert_eq!(edit.fields.len(), 2);
    assert!(edit.fields.iter().all(|field| !field.secret));

    app.form = None;
    app.page = Page::Details;
    start_replace_secret(&mut app);
    let replacement = app.form.as_ref().unwrap();
    assert_eq!(replacement.kind, FormKind::ReplaceSecret);
    assert_eq!(replacement.fields.len(), 2);
    assert!(replacement.fields[0].secret);
    assert!(replacement.attributes.is_empty());
}

#[tokio::test]
async fn replacement_calls_only_secret_mutation_and_keeps_label_and_attributes() {
    let mut app = sample_app();
    let original = app.items[0].clone();
    let store = sample_store(&app);
    store.clear_log();
    app.page = Page::Details;
    start_replace_secret(&mut app);
    app.form.as_mut().unwrap().fields[0].value = "replacement".into();
    submit_replace_secret(&store, &mut app).await.unwrap();

    let updated = store.item("p1").unwrap();
    assert_eq!(updated.label, original.label);
    assert_eq!(updated.attributes, original.attributes);
    assert_eq!(updated.collection_path, original.collection_path);
    let mutation_operations = store
        .mutation_log()
        .into_iter()
        .map(|entry| entry.operation)
        .filter(|operation| {
            matches!(
                operation,
                crate::store::StoreOperation::SetItemLabel
                    | crate::store::StoreOperation::SetItemAttributes
                    | crate::store::StoreOperation::ReplaceItemSecret
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mutation_operations,
        [crate::store::StoreOperation::ReplaceItemSecret]
    );
}

#[tokio::test]
async fn metadata_edit_applies_label_then_attributes_and_rolls_label_back_on_failure() {
    let mut app = sample_app();
    let original = app.items[0].clone();
    let store = sample_store(&app);
    store.inject_failure(
        crate::store::StoreOperation::SetItemAttributes,
        1,
        "injected attributes failure",
    );
    app.page = Page::Details;
    start_edit_item(&mut app);
    let form = app.form.as_mut().unwrap();
    form.fields[0].value = "new label".into();
    form.attributes.insert("new".into(), "attribute".into());

    let error = submit_edit_item(&store, &mut app).await.unwrap_err();
    assert!(error.to_string().contains("label rollback completed"));
    let updated = store.item("p1").unwrap();
    assert_eq!(updated.label, original.label);
    assert_eq!(updated.attributes, original.attributes);
    let field_operations = store
        .mutation_log()
        .into_iter()
        .map(|entry| entry.operation)
        .filter(|operation| {
            matches!(
                operation,
                crate::store::StoreOperation::SetItemLabel
                    | crate::store::StoreOperation::SetItemAttributes
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        field_operations,
        [
            crate::store::StoreOperation::SetItemLabel,
            crate::store::StoreOperation::SetItemAttributes,
            crate::store::StoreOperation::SetItemLabel,
        ]
    );
}

#[tokio::test]
async fn metadata_edit_preflight_blocks_stale_form_with_zero_writes() {
    let mut app = sample_app();
    let store = sample_store(&app);
    app.page = Page::Details;
    start_edit_item(&mut app);
    app.form
        .as_mut()
        .unwrap()
        .attributes
        .insert("draft".into(), "value".into());
    let target = ItemTarget::from(&app.items[0]);
    store
        .set_item_label(&target, "changed elsewhere")
        .await
        .unwrap();
    store.clear_log();

    submit_edit_item(&store, &mut app).await.unwrap();

    assert!(app.form.as_ref().unwrap().message.contains("Edit blocked"));
    assert_eq!(store.item("p1").unwrap().label, "changed elsewhere");
    assert!(store.mutation_log().iter().all(|entry| !matches!(
        entry.operation,
        crate::store::StoreOperation::SetItemLabel
            | crate::store::StoreOperation::SetItemAttributes
    )));
}

#[tokio::test]
async fn metadata_rollback_is_skipped_when_post_label_read_has_concurrent_drift() {
    let mut app = sample_app();
    let store = sample_store(&app);
    store.set_delay(
        crate::store::StoreOperation::ListItems,
        Duration::from_millis(50),
    );
    store.inject_failure(
        crate::store::StoreOperation::SetItemAttributes,
        1,
        "injected attributes failure",
    );
    app.page = Page::Details;
    start_edit_item(&mut app);
    let form = app.form.as_mut().unwrap();
    form.fields[0].value = "new label".into();
    form.attributes.insert("draft".into(), "attribute".into());

    let concurrent_store = store.clone();
    let concurrent = tokio::spawn(async move {
        // The first delayed read is the preflight. Drift during the second, post-label read.
        tokio::time::sleep(Duration::from_millis(75)).await;
        let mut item = concurrent_store.item("p1").unwrap();
        item.attributes.insert("concurrent".into(), "change".into());
        item.modified = Some(999);
        concurrent_store.update_item_info(item).unwrap();
    });
    let error = submit_edit_item(&store, &mut app).await.unwrap_err();
    concurrent.await.unwrap();

    assert!(error
        .to_string()
        .contains("post-label concurrency check was unavailable"));
    let updated = store.item("p1").unwrap();
    assert_eq!(updated.label, "new label");
    assert_eq!(updated.attributes["concurrent"], "change");
    assert!(!store
        .mutation_log()
        .iter()
        .any(|entry| { entry.operation == crate::store::StoreOperation::SetItemAttributes }));
}

#[test]
fn preview_classification_requires_safe_text_mime_and_utf8() {
    assert_eq!(
        preview_encoding(&secret_value(b"normal text", " Text/Plain ; charset=utf-8")),
        PreviewEncoding::EscapedUtf8
    );
    assert_eq!(
        preview_encoding(&secret_value(&[0xff], "text/plain")),
        PreviewEncoding::HexDump
    );
    assert_eq!(
        preview_encoding(&secret_value(b"before\0after", "text/plain")),
        PreviewEncoding::HexDump
    );
    assert_eq!(
        preview_encoding(&secret_value(b"before\x1bafter", "text/plain")),
        PreviewEncoding::HexDump
    );
    assert_eq!(
        preview_encoding(&secret_value(b"plain ASCII", "application/octet-stream")),
        PreviewEncoding::HexDump
    );
}

#[test]
fn previews_escape_text_and_bound_binary_output() {
    assert_eq!(
        secret_preview(&secret_value(b"a\tb\nc\r", "text/plain")),
        ["a\\tb\\nc\\r"]
    );

    let bytes = (0..=255).chain(0..32).collect::<Vec<u8>>();
    let preview = secret_preview(&secret_value(&bytes, "application/octet-stream"));
    assert_eq!(
        preview[0],
        "00000000: 00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f"
    );
    assert_eq!(
        preview[15],
        "000000f0: f0 f1 f2 f3 f4 f5 f6 f7 f8 f9 fa fb fc fd fe ff"
    );
    assert_eq!(preview[16], "… 32 byte(s) omitted");
}

#[test]
fn text_preview_truncates_at_utf8_boundary() {
    let text = format!("{}érest", "a".repeat(255));
    let preview = secret_preview(&secret_value(text.as_bytes(), "text/plain"));
    assert_eq!(preview[0], "a".repeat(255));
    assert_eq!(preview[1], "… 6 byte(s) omitted");
}

#[test]
fn clipboard_encodings_use_complete_secret() {
    assert_eq!(clipboard_text(b"text").unwrap(), "text");
    assert_eq!(
        clipboard_text(&[0xff]).unwrap_err().to_string(),
        "binary secret cannot be copied as text"
    );
    assert_eq!(
        &*encode_clipboard(b"text", CopyEncoding::Text).unwrap(),
        "text"
    );
    assert_eq!(
        &*encode_clipboard(&[0, 1, 0xfe, 0xff], CopyEncoding::Base64).unwrap(),
        "AAH+/w=="
    );
    assert_eq!(
        &*encode_clipboard(&[0, 1, 0xfe, 0xff], CopyEncoding::Hex).unwrap(),
        "0001feff"
    );
}

#[test]
fn details_show_unavailable_then_cached_and_expired_diagnostics() {
    let mut app = sample_app();
    let initial = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(initial.contains("content type: unavailable until Reveal or Copy"));
    assert!(initial.contains("size: unavailable until Reveal or Copy"));

    let value = secret_value(b"hello", "text/plain");
    app.secret_metadata = Some(secret_metadata("p1", &value));
    let copied = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(copied.contains("content type: text/plain"));
    assert!(copied.contains("size: 5 bytes"));
    assert!(!copied.contains("preview:"));

    app.reveal = Some(RevealState {
        item_path: "p1".into(),
        value,
        expires_at: Instant::now() - Duration::from_secs(1),
    });
    let revealed = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(revealed.contains("preview:"));
    assert!(revealed.contains("hello"));
    app.expire_secret();
    assert!(app.reveal.is_none());
    let expired = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expired.contains("content type: text/plain"));
    assert!(!expired.contains("preview:"));
}

#[test]
fn short_attribute_editor_keeps_value_field_visible() {
    let mut app = sample_app();
    start_new_item(&mut app);
    open_attribute_form(&mut app);
    let form = app.form.as_mut().unwrap();
    form.selected_field = 1;
    keep_form_focus_visible(&mut app);
    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Value"), "{rendered}");
    assert!(rendered.contains('↑') || rendered.contains('↓'));
}

#[test]
fn interface_is_centered_at_128_columns() {
    let mut app = sample_app();
    let backend = TestBackend::new(160, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer.cell((15, 0)).unwrap().symbol(), " ");
    assert_eq!(buffer.cell((16, 0)).unwrap().symbol(), "┌");
    assert_eq!(buffer.cell((143, 0)).unwrap().symbol(), "┐");
    assert_eq!(buffer.cell((144, 0)).unwrap().symbol(), " ");
}

#[test]
fn interface_uses_full_width_below_maximum() {
    let mut app = sample_app();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "┌");
    assert_eq!(buffer.cell((79, 0)).unwrap().symbol(), "┐");
}

#[test]
fn long_list_rows_and_details_wrap_to_child_width() {
    let wrapped = wrap_text(
        "A collection name with enough words to require several visual rows",
        20,
    );
    assert!(wrapped.len() >= 3);
    assert!(wrapped.iter().all(|line| line.width() <= 20));
    let (_, row_height) = wrapped_list_item(
        "A collection name with enough words to require several visual rows",
        20,
    );
    assert_eq!(row_height, 2);

    let mut app = sample_app();
    app.items[0].path = format!("/{}", "long-path-segment".repeat(8));
    let logical_count = detail_lines(&app).len();
    let (details, _) = wrapped_detail_lines(&app, 24);
    assert!(details.len() > logical_count);
    assert!(details.iter().all(|line| line.width() <= 24));
}

#[test]
fn detail_buttons_wrap_as_complete_controls() {
    let app = sample_app();
    let lines = action_button_lines(&app, 30);
    let rendered = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(lines.len() > 1);
    for label in [
        "Back",
        "Reveal",
        "Copy Text",
        "Copy Base64",
        "Copy Hex",
        "Edit Metadata",
        "Replace Secret",
        "Delete",
        "Lock",
    ] {
        assert!(rendered.contains(label));
    }
}

#[test]
fn delete_confirmation_is_cancel_first_and_shows_safe_snapshot() {
    let mut app = sample_app();
    app.collections[0].label = "Wallet\x1b[31m".into();
    app.items[0].label = "Important\nitem\u{202e}".into();
    app.items[0]
        .attributes
        .insert("user\tname".into(), "john\\admin".into());
    app.page = Page::Details;
    start_delete_item(&mut app);

    let form = app.form.as_ref().unwrap();
    assert_eq!(form.buttons(), &["Cancel", "Delete"]);
    assert_eq!(form.selected_button, 0);
    let rendered = delete_confirmation_lines(&form.message, app.delete_snapshot.as_ref(), 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Wallet\\x1b[31m"), "{rendered}");
    assert!(rendered.contains("Important\\nitem\\u{202e}"), "{rendered}");
    assert!(rendered.contains("user\\tname=john\\\\admin"), "{rendered}");
    assert!(
        rendered.contains("cannot restore a deleted secret"),
        "{rendered}"
    );
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\u{202e}'));
}

#[tokio::test]
async fn delete_is_blocked_when_snapshot_drifts() {
    let mut app = sample_app();
    let store = sample_store(&app);
    app.page = Page::Details;
    start_delete_item(&mut app);
    let target = ItemTarget::from(&app.delete_snapshot.as_ref().unwrap().item);
    store
        .set_item_label(&target, "changed elsewhere")
        .await
        .unwrap();

    // A deliberate move from the initially selected Cancel button is required.
    form_move_button(&mut app, 1);
    submit_or_cancel_form(&store, &mut app).await.unwrap();

    assert!(store.item("p1").is_some());
    let form = app.form.as_ref().unwrap();
    assert_eq!(form.kind, FormKind::DeleteItem);
    assert!(form.message.contains("Deletion blocked"));
}

#[test]
fn details_escape_provider_errors_controls_and_bidi() {
    let mut app = sample_app();
    app.message = "provider\x1b[2J failed\nnext\u{0085}\u{202e}".into();
    app.collections[0].label = "normal 日本語 e\u{301}".into();
    app.items[0].attributes.insert(
        "control\tkey".into(),
        "right-to-left مرحبا \u{2066}value\u{2069}".into(),
    );

    let rendered = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("provider\\x1b[2J failed\\nnext\\u{85}\\u{202e}"));
    assert!(rendered.contains("normal 日本語 e\u{301}"));
    assert!(rendered.contains("control\\tkey"));
    assert!(rendered.contains("مرحبا \\u{2066}value\\u{2069}"));
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\u{0085}'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(!rendered.contains('\u{2066}'));
    assert!(!rendered.contains('\u{2069}'));
    let rendered_header = header(&app);
    assert!(rendered_header.contains("status: provider\\x1b[2J failed\\nnext"));
    assert!(!rendered_header.contains('\x1b'));
    assert!(!rendered_header.contains('\u{202e}'));
}

#[test]
fn details_bound_attribute_count_and_value_lengths() {
    let mut app = sample_app();
    app.items[0].attributes.clear();
    for index in 0..300 {
        app.items[0]
            .attributes
            .insert(format!("key-{index:03}"), "x".repeat(1_000));
    }
    let rendered = detail_lines(&app)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(rendered
        .iter()
        .any(|line| line.contains("44 attribute(s) not displayed")));
    assert!(!rendered.iter().any(|line| line.contains("key-299")));
    let first_value = rendered
        .iter()
        .find(|line| line.contains("key-000="))
        .unwrap();
    assert!(first_value.contains("1000 bytes; sha256="));
    assert!(first_value.graphemes(true).count() <= 2 + 256 + 1 + 512);
}
