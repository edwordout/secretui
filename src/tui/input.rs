use super::*;

pub(super) async fn handle_key(
    store: &impl SecretStore,
    app: &mut TuiApp,
    key: KeyEvent,
) -> Result<bool> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Ok(false);
    }
    if key.kind == KeyEventKind::Repeat
        && !matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Backspace
                | KeyCode::Delete
        )
    {
        return Ok(false);
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }
    match app.mode {
        InputMode::Search => handle_search_key(app, key),
        InputMode::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                app.mode = InputMode::Normal;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.help_scroll = app.help_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(PAGE_SIZE),
            KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(PAGE_SIZE),
            KeyCode::Home => app.help_scroll = 0,
            KeyCode::End => app.help_scroll = usize::MAX / 4,
            _ => {}
        },
        InputMode::Normal => match key.code {
            _ if app.page == Page::Form => {
                if handle_form_key(store, app, key).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('?') => {
                app.mode = InputMode::Help;
                app.help_scroll = 0;
            }
            KeyCode::Char('r') if app.page == Page::Details => {
                reveal_selected(store, app).await?;
            }
            KeyCode::Esc | KeyCode::Char('h') => app.previous_page(),
            KeyCode::Left => {
                if app.page == Page::Details {
                    app.move_action(-1);
                } else {
                    app.previous_page();
                }
            }
            KeyCode::Right | KeyCode::Tab => {
                if app.page == Page::Details {
                    app.move_action(1);
                } else {
                    advance_page(store, app).await?;
                }
            }
            KeyCode::Char('l') => {
                if app.page == Page::Details {
                    app.move_action(1);
                } else {
                    advance_page(store, app).await?;
                }
            }
            KeyCode::Enter => {
                if app.page == Page::Details {
                    activate_detail_action(store, app).await?;
                } else {
                    advance_page(store, app).await?;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_selection(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_selection(-1);
            }
            KeyCode::PageDown => {
                app.move_selection(PAGE_SIZE as isize);
            }
            KeyCode::PageUp => {
                app.move_selection(-(PAGE_SIZE as isize));
            }
            KeyCode::Home => {
                app.jump_selection(false);
            }
            KeyCode::End => {
                app.jump_selection(true);
            }
            KeyCode::Char('/') => {
                app.clear_reveal();
                if app.page == Page::Collections {
                    if app.selected_collection().is_none() {
                        app.message = "create or select a collection first".into();
                        return Ok(false);
                    }
                    app.refresh_items(store).await?;
                }
                app.page = Page::Items;
                app.mode = InputMode::Search;
                app.filter.clear();
                app.selected_item = 0;
                app.sync_states();
                app.message = "type to search, Enter/Esc to finish".into();
            }
            KeyCode::Char('n') => match app.page {
                Page::Collections => start_new_collection(app),
                Page::Items => start_new_item(app),
                Page::Details | Page::Form => {}
            },

            _ => {}
        },
    }
    Ok(false)
}

pub(super) fn key_starts_backend_operation(app: &TuiApp, key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press || app.mode != InputMode::Normal {
        return false;
    }
    match (app.page, key.code) {
        (Page::Collections, KeyCode::Enter | KeyCode::Right | KeyCode::Tab)
        | (Page::Collections, KeyCode::Char('l') | KeyCode::Char('/')) => true,
        (Page::Details, KeyCode::Enter) => matches!(
            app.selected_action(),
            DetailAction::Reveal
                | DetailAction::CopyText
                | DetailAction::CopyBase64
                | DetailAction::CopyHex
                | DetailAction::LockUnlock
        ),
        (Page::Details, KeyCode::Char('r')) => true,
        (Page::Form, KeyCode::Enter) => app.form.as_ref().is_some_and(|form| {
            form.focus_buttons
                && matches!(
                    form.buttons().get(form.selected_button).copied(),
                    Some("Save" | "Save Changes" | "Delete")
                )
                && matches!(
                    form.kind,
                    FormKind::NewCollection
                        | FormKind::NewItem
                        | FormKind::EditItem
                        | FormKind::ReplaceSecret
                        | FormKind::DeleteItem
                        | FormKind::UnsavedChanges
                )
        }),
        _ => false,
    }
}

pub(super) fn handle_search_key(app: &mut TuiApp, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.clear_reveal();
            app.mode = InputMode::Normal;
            app.message = if app.filter.is_empty() {
                "search cleared".into()
            } else {
                format!("filter: {}", app.filter)
            };
        }
        KeyCode::Backspace => {
            app.clear_reveal();
            app.filter.pop();
            app.selected_item = 0;
            app.clamp_item_selection();
            app.sync_states();
        }
        KeyCode::Char(ch) => {
            app.clear_reveal();
            app.filter.push(ch);
            app.selected_item = 0;
            app.clamp_item_selection();
            app.sync_states();
        }
        _ => {}
    }
}

pub(super) async fn handle_form_key(
    store: &impl SecretStore,
    app: &mut TuiApp,
    key: KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc => cancel_or_confirm_form(app),
        KeyCode::Tab => form_next_focus(app),
        KeyCode::BackTab => form_prev_focus(app),
        KeyCode::Down => {
            form_down(app);
            keep_form_focus_visible(app);
        }
        KeyCode::Up => {
            form_up(app);
            keep_form_focus_visible(app);
        }
        KeyCode::Left => move_current_field_cursor_or_button(app, -1),
        KeyCode::Right => move_current_field_cursor_or_button(app, 1),
        KeyCode::Home => move_current_field_home(app),
        KeyCode::End => move_current_field_end(app),
        KeyCode::Enter => {
            if app.form.as_ref().is_some_and(|form| form.focus_buttons) {
                submit_or_cancel_form(store, app).await?;
            } else if current_form_field_kind(app) == Some(FormFieldKind::Attributes) {
                open_attribute_form(app);
            } else {
                form_next_focus(app);
                keep_form_focus_visible(app);
            }
        }
        KeyCode::Backspace => {
            if let Some(field) = current_form_field_mut(app) {
                field.backspace();
                mark_form_dirty(app);
            }
        }
        KeyCode::Delete => {
            if let Some(field) = current_form_field_mut(app) {
                field.delete();
                mark_form_dirty(app);
            }
        }
        KeyCode::Char(ch) => {
            if let Some(field) = current_form_field_mut(app) {
                field.insert(ch);
                mark_form_dirty(app);
            }
        }
        _ => {}
    }
    Ok(false)
}

pub(super) fn current_form_field_mut(app: &mut TuiApp) -> Option<&mut FormField> {
    let form = app.form.as_mut()?;
    if form.focus_buttons {
        return None;
    }
    let field = form.fields.get_mut(form.selected_field)?;
    (field.kind == FormFieldKind::Text).then_some(field)
}

pub(super) fn mark_form_dirty(app: &mut TuiApp) {
    if let Some(form) = app.form.as_mut() {
        form.dirty = true;
    }
}

pub(super) fn current_form_field_kind(app: &TuiApp) -> Option<FormFieldKind> {
    let form = app.form.as_ref()?;
    if form.focus_buttons {
        return None;
    }
    form.fields.get(form.selected_field).map(|field| field.kind)
}

pub(super) fn form_next_focus(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.fields.is_empty() {
        form.focus_buttons = true;
        return;
    }
    if form.focus_buttons {
        form.focus_buttons = false;
        form.selected_field = 0;
    } else if form.selected_field + 1 < form.fields.len() {
        form.selected_field += 1;
    } else {
        form.focus_buttons = true;
    }
}

pub(super) fn form_prev_focus(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.fields.is_empty() {
        form.focus_buttons = true;
        return;
    }
    if form.focus_buttons {
        form.focus_buttons = false;
        form.selected_field = form.fields.len().saturating_sub(1);
    } else if form.selected_field > 0 {
        form.selected_field -= 1;
    } else {
        form.focus_buttons = true;
    }
}

pub(super) fn keep_form_focus_visible(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.focus_buttons {
        form.scroll = form.fields.len().saturating_mul(4);
    } else {
        form.scroll = form.selected_field.saturating_mul(4);
    }
}

pub(super) fn form_move_button(app: &mut TuiApp, delta: isize) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    form.focus_buttons = true;
    form.selected_button = move_index(form.selected_button, form.buttons().len(), delta);
}

pub(super) fn move_current_field_cursor_or_button(app: &mut TuiApp, delta: isize) {
    if app.form.as_ref().is_some_and(|form| form.focus_buttons) {
        form_move_button(app, delta);
    } else if let Some(field) = current_form_field_mut(app) {
        field.move_cursor(delta);
    }
}

pub(super) fn move_current_field_home(app: &mut TuiApp) {
    if let Some(field) = current_form_field_mut(app) {
        field.move_home();
    }
}

pub(super) fn move_current_field_end(app: &mut TuiApp) {
    if let Some(field) = current_form_field_mut(app) {
        field.move_end();
    }
}

pub(super) fn form_down(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if !form.focus_buttons
        && form.fields.get(form.selected_field).map(|field| field.kind)
            == Some(FormFieldKind::Attributes)
        && form.selected_attribute < displayed_attribute_count(form)
    {
        form.selected_attribute += 1;
        return;
    }
    form_next_focus(app);
}

pub(super) fn form_up(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if !form.focus_buttons
        && form.fields.get(form.selected_field).map(|field| field.kind)
            == Some(FormFieldKind::Attributes)
        && form.selected_attribute > 0
    {
        form.selected_attribute -= 1;
        return;
    }
    let entering_attributes = if form.focus_buttons {
        form.fields
            .last()
            .is_some_and(|field| field.kind == FormFieldKind::Attributes)
    } else {
        form.selected_field > 0
            && form.fields[form.selected_field - 1].kind == FormFieldKind::Attributes
    };
    form_prev_focus(app);
    if entering_attributes {
        if let Some(form) = app.form.as_mut() {
            form.selected_attribute = displayed_attribute_count(form);
        }
    }
}

pub(super) fn cancel_or_confirm_form(app: &mut TuiApp) {
    let Some(kind) = app.form.as_ref().map(|form| form.kind) else {
        return;
    };
    match kind {
        FormKind::NewAttribute | FormKind::EditAttribute => {
            restore_parent_form(app, "attribute unchanged")
        }
        FormKind::UnsavedChanges => restore_parent_form(app, "changes kept"),
        FormKind::DeleteItem => discard_current_form(app),
        _ if app.form.as_ref().is_some_and(|form| form.dirty) => {
            let Some(parent) = app.form.take() else {
                return;
            };
            app.form = Some(FormState {
                kind: FormKind::UnsavedChanges,
                fields: Vec::new(),
                attributes: Attributes::new(),
                selected_attribute: 0,
                editing_attribute_key: None,
                selected_field: 0,
                selected_button: 2,
                scroll: 0,
                focus_buttons: true,
                target_item_path: None,
                message: "Do you want to save your changes?".into(),
                parent: Some(Box::new(parent)),
                dirty: false,
            });
        }
        _ => discard_current_form(app),
    }
}

pub(super) fn discard_current_form(app: &mut TuiApp) {
    let Some(mut form) = app.form.take() else {
        return;
    };
    let page = form.cancel_page();
    if form.kind == FormKind::DeleteItem {
        app.delete_snapshot = None;
    }
    clear_form_state_secrets(&mut form);
    app.page = page;
    app.message = "cancelled".into();
}

pub(super) fn restore_parent_form(app: &mut TuiApp, message: &str) {
    let Some(mut form) = app.form.take() else {
        return;
    };
    let Some(mut parent) = form.parent.take().map(|parent| *parent) else {
        app.form = Some(form);
        return;
    };
    parent.message = message.into();
    app.form = Some(parent);
    app.page = Page::Form;
}

pub(super) async fn submit_or_cancel_form(
    store: &impl SecretStore,
    app: &mut TuiApp,
) -> Result<()> {
    let Some((kind, action)) = app.form.as_ref().map(|form| {
        (
            form.kind,
            form.buttons()
                .get(form.selected_button)
                .copied()
                .unwrap_or(""),
        )
    }) else {
        return Ok(());
    };

    match (kind, action) {
        (FormKind::NewAttribute | FormKind::EditAttribute, "Cancel") => {
            restore_parent_form(app, "attribute unchanged")
        }
        (FormKind::NewAttribute, "Add") | (FormKind::EditAttribute, "Update") => {
            save_attribute(app)
        }
        (FormKind::EditAttribute, "Remove") => remove_attribute(app),
        (FormKind::UnsavedChanges, "Keep Editing") => restore_parent_form(app, "changes kept"),
        (FormKind::UnsavedChanges, "Discard") => {
            restore_parent_form(app, "");
            discard_current_form(app);
        }
        (FormKind::UnsavedChanges, "Save Changes") => {
            restore_parent_form(app, "saving changes");
            submit_parent_form(store, app).await?;
        }
        (_, "Cancel") => cancel_or_confirm_form(app),
        _ => submit_parent_form(store, app).await?,
    }
    Ok(())
}

pub(super) async fn submit_parent_form(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    match app.form.as_ref().map(|form| form.kind) {
        Some(FormKind::NewCollection) => submit_new_collection(store, app).await?,
        Some(FormKind::NewItem) => submit_new_item(store, app).await?,
        Some(FormKind::EditItem) => submit_edit_item(store, app).await?,
        Some(FormKind::ReplaceSecret) => submit_replace_secret(store, app).await?,
        Some(FormKind::DeleteItem) => submit_delete_item(store, app).await?,
        _ => {}
    }
    Ok(())
}

pub(super) fn open_attribute_form(app: &mut TuiApp) {
    let Some(parent) = app.form.take() else {
        return;
    };
    let selected = parent
        .selected_attribute
        .min(displayed_attribute_count(&parent));
    let existing = parent
        .attributes
        .iter()
        .nth(selected)
        .map(|(key, value)| (key.clone(), value.clone()));
    let (kind, key, value, editing_attribute_key, message) = match existing {
        Some((key, value)) => (
            FormKind::EditAttribute,
            key.clone(),
            value,
            Some(key),
            "Update or remove this attribute.",
        ),
        None => (
            FormKind::NewAttribute,
            String::new(),
            String::new(),
            None,
            "Add an attribute to the item draft.",
        ),
    };
    app.form = Some(FormState {
        kind,
        fields: vec![FormField::text("Key", key), FormField::text("Value", value)],
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        message: message.into(),
        parent: Some(Box::new(parent)),
        dirty: false,
    });
}

pub(super) fn save_attribute(app: &mut TuiApp) {
    let Some(form) = app.form.as_ref() else {
        return;
    };
    let key = form
        .fields
        .first()
        .map(|field| field.value.trim().to_owned())
        .unwrap_or_default();
    if key.is_empty() {
        set_form_message(app, "Key is required.");
        set_form_field_error(app, 0, "required");
        return;
    }
    let previous_key = form.editing_attribute_key.as_deref();
    let duplicate = form.parent.as_ref().is_some_and(|parent| {
        parent.attributes.contains_key(&key) && previous_key != Some(key.as_str())
    });
    if duplicate {
        set_form_message(app, "An attribute with this key already exists.");
        set_form_field_error(app, 0, "already exists");
        return;
    }

    let Some(mut editor) = app.form.take() else {
        return;
    };
    let value = editor
        .fields
        .get(1)
        .map(|field| field.value.clone())
        .unwrap_or_default();
    let Some(mut parent) = editor.parent.take().map(|parent| *parent) else {
        app.form = Some(editor);
        return;
    };
    if let Some(previous_key) = editor.editing_attribute_key.take() {
        if previous_key != key {
            parent.attributes.remove(&previous_key);
        }
    }
    parent.attributes.insert(key.clone(), value);
    parent.selected_attribute = parent
        .attributes
        .keys()
        .position(|existing| existing == &key)
        .unwrap_or(0);
    parent.dirty = true;
    parent.message = "attribute saved (unsaved item changes)".into();
    update_parent_attribute_summary(&mut parent);
    app.form = Some(parent);
}

pub(super) fn remove_attribute(app: &mut TuiApp) {
    let Some(mut editor) = app.form.take() else {
        return;
    };
    let Some(mut parent) = editor.parent.take().map(|parent| *parent) else {
        app.form = Some(editor);
        return;
    };
    let Some(key) = editor.editing_attribute_key.take() else {
        app.form = Some(parent);
        return;
    };
    let old_index = parent.selected_attribute;
    parent.attributes.remove(&key);
    parent.selected_attribute = old_index.min(parent.attributes.len());
    parent.dirty = true;
    parent.message = "attribute removed (unsaved item changes)".into();
    update_parent_attribute_summary(&mut parent);
    app.form = Some(parent);
}

pub(super) async fn advance_page(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    match app.page {
        Page::Collections => {
            if app.selected_collection().is_none() {
                app.message = "create or select a collection first".into();
                return Ok(());
            }
            app.refresh_items(store).await?;
            app.next_page();
        }
        Page::Items => {
            if app.selected_item().is_none() {
                app.message = "create or select an item first".into();
                return Ok(());
            }
            app.next_page();
        }
        Page::Details | Page::Form => {}
    }
    Ok(())
}

pub(super) fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    current.saturating_add_signed(delta).min(len - 1)
}

pub(super) fn byte_index_at_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .nth(char_index)
        .unwrap_or(text.len())
}
