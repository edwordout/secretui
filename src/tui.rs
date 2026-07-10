use crate::domain::{Attributes, CollectionInfo, ItemInfo, NewCollection, NewItem};
use crate::store::SecretStore;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use zeroize::Zeroize;

const SECRET_TTL: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Collections,
    Items,
    Details,
    Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormKind {
    NewCollection,
    NewItem,
    EditItem,
    DeleteItem,
    Attributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormFieldKind {
    Text,
    Attributes,
}

struct FormField {
    label: String,
    value: String,
    cursor: usize,
    secret: bool,
    kind: FormFieldKind,
    error: Option<String>,
}

impl FormField {
    fn text(label: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.chars().count(),
            label: label.into(),
            value,
            secret: false,
            kind: FormFieldKind::Text,
            error: None,
        }
    }

    fn secret(label: &str) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            cursor: 0,
            secret: true,
            kind: FormFieldKind::Text,
            error: None,
        }
    }

    fn attributes(count: usize) -> Self {
        Self {
            label: "Attributes".into(),
            value: format!("{count} attribute(s) — Enter to edit"),
            cursor: 0,
            secret: false,
            kind: FormFieldKind::Attributes,
            error: None,
        }
    }

    fn visible_value(&self, focused: bool) -> String {
        let mut chars: Vec<char> = if self.secret {
            "•".repeat(self.value.chars().count()).chars().collect()
        } else {
            self.value.chars().collect()
        };
        if focused && self.kind == FormFieldKind::Text {
            chars.insert(self.cursor.min(chars.len()), '▌');
        }
        chars.into_iter().collect()
    }

    fn insert(&mut self, ch: char) {
        let byte_index = byte_index_at_char(&self.value, self.cursor);
        self.value.insert(byte_index, ch);
        self.cursor += 1;
        self.error = None;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = byte_index_at_char(&self.value, self.cursor - 1);
        let end = byte_index_at_char(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        let len = self.value.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = byte_index_at_char(&self.value, self.cursor);
        let end = byte_index_at_char(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = move_index(self.cursor, self.value.chars().count() + 1, delta);
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.value.chars().count();
    }
}

struct FormState {
    kind: FormKind,
    fields: Vec<FormField>,
    attributes: Attributes,
    selected_attribute: usize,
    selected_field: usize,
    selected_button: usize,
    scroll: usize,
    focus_buttons: bool,
    target_item_path: Option<String>,
    message: String,
    parent: Option<Box<FormState>>,
}

impl FormState {
    fn buttons(&self) -> &'static [&'static str] {
        match self.kind {
            FormKind::DeleteItem => &["Delete", "Cancel"],
            FormKind::Attributes => &["Add/Update", "Remove", "Done", "Cancel"],
            _ => &["Save", "Cancel"],
        }
    }

    fn title(&self) -> &'static str {
        match self.kind {
            FormKind::NewCollection => "New Collection",
            FormKind::NewItem => "New Item",
            FormKind::EditItem => "Edit Item",
            FormKind::DeleteItem => "Delete Item",
            FormKind::Attributes => "Attributes",
        }
    }

    fn cancel_page(&self) -> Page {
        match self.kind {
            FormKind::NewCollection => Page::Collections,
            FormKind::NewItem => Page::Items,
            FormKind::EditItem | FormKind::DeleteItem => Page::Details,
            FormKind::Attributes => Page::Form,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    Reveal,
    Copy,
    Edit,
    Delete,
    LockUnlock,
    Back,
}

const DETAIL_ACTIONS: [DetailAction; 6] = [
    DetailAction::Back,
    DetailAction::Reveal,
    DetailAction::Copy,
    DetailAction::Edit,
    DetailAction::Delete,
    DetailAction::LockUnlock,
];

struct RevealState {
    item_path: String,
    secret: Vec<u8>,
    expires_at: Instant,
}

impl Drop for RevealState {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

pub struct TuiApp {
    collections: Vec<CollectionInfo>,
    items: Vec<ItemInfo>,
    selected_collection: usize,
    selected_item: usize,
    collection_state: ListState,
    item_state: ListState,
    selected_action: usize,
    detail_scroll: usize,
    page: Page,
    mode: InputMode,
    filter: String,
    message: String,
    reveal: Option<RevealState>,
    form: Option<FormState>,
}

impl TuiApp {
    pub async fn new(store: &impl SecretStore) -> Result<Self> {
        let mut app = Self::from_data(store.list_collections().await?, Vec::new());
        app.refresh_items(store).await?;
        app.sync_states();
        Ok(app)
    }

    fn from_data(collections: Vec<CollectionInfo>, items: Vec<ItemInfo>) -> Self {
        let mut app = Self {
            collections,
            items,
            selected_collection: 0,
            selected_item: 0,
            collection_state: ListState::default(),
            item_state: ListState::default(),
            selected_action: 1,
            detail_scroll: 0,
            page: Page::Collections,
            mode: InputMode::Normal,
            filter: String::new(),
            message: String::new(),
            reveal: None,
            form: None,
        };
        app.sync_states();
        app
    }

    async fn refresh_all(&mut self, store: &impl SecretStore) -> Result<()> {
        self.collections = store.list_collections().await?;
        self.selected_collection = self
            .selected_collection
            .min(self.collections.len().saturating_sub(1));
        self.refresh_items(store).await
    }

    async fn refresh_items(&mut self, store: &impl SecretStore) -> Result<()> {
        self.items = match self.collections.get(self.selected_collection) {
            Some(collection) => store.list_items(&collection.path).await?,
            None => Vec::new(),
        };
        self.clamp_item_selection();
        self.sync_states();
        Ok(())
    }

    fn filtered_items(&self) -> Vec<&ItemInfo> {
        if self.filter.is_empty() {
            return self.items.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                item.label.to_lowercase().contains(&needle)
                    || item.path.to_lowercase().contains(&needle)
                    || item.attributes.iter().any(|(key, value)| {
                        key.to_lowercase().contains(&needle)
                            || value.to_lowercase().contains(&needle)
                    })
            })
            .collect()
    }

    fn selected_collection(&self) -> Option<&CollectionInfo> {
        self.collections.get(self.selected_collection)
    }

    fn selected_item(&self) -> Option<&ItemInfo> {
        self.filtered_items().get(self.selected_item).copied()
    }

    fn next_page(&mut self) {
        let previous = self.page;
        self.page = match self.page {
            Page::Collections => Page::Items,
            Page::Items => Page::Details,
            Page::Details => Page::Details,
            Page::Form => Page::Form,
        };
        if previous != Page::Details && self.page == Page::Details {
            self.selected_action = 1;
            self.detail_scroll = 0;
        }
    }

    fn move_action(&mut self, delta: isize) {
        self.selected_action = move_index(self.selected_action, DETAIL_ACTIONS.len(), delta);
    }

    fn selected_action(&self) -> DetailAction {
        DETAIL_ACTIONS[self.selected_action.min(DETAIL_ACTIONS.len() - 1)]
    }

    fn previous_page(&mut self) {
        self.page = match self.page {
            Page::Collections => Page::Collections,
            Page::Items => Page::Collections,
            Page::Details => Page::Items,
            Page::Form => {
                let page = self
                    .form
                    .as_ref()
                    .map(FormState::cancel_page)
                    .unwrap_or(Page::Items);
                self.form = None;
                page
            }
        };
    }

    fn move_selection(&mut self, delta: isize) {
        match self.page {
            Page::Collections => {
                self.selected_collection =
                    move_index(self.selected_collection, self.collections.len(), delta);
                self.selected_item = 0;
            }
            Page::Items => {
                self.selected_item =
                    move_index(self.selected_item, self.filtered_items().len(), delta);
                self.detail_scroll = 0;
            }
            Page::Details => {
                self.detail_scroll = move_index(self.detail_scroll, usize::MAX / 2, delta);
            }
            Page::Form => {}
        }
        self.sync_states();
    }

    fn jump_selection(&mut self, end: bool) {
        match self.page {
            Page::Collections => {
                self.selected_collection = if end {
                    self.collections.len().saturating_sub(1)
                } else {
                    0
                };
                self.selected_item = 0;
            }
            Page::Items => {
                self.selected_item = if end {
                    self.filtered_items().len().saturating_sub(1)
                } else {
                    0
                };
            }
            Page::Details => {
                self.detail_scroll = if end { usize::MAX / 4 } else { 0 };
            }
            Page::Form => {}
        }
        self.sync_states();
    }

    fn sync_states(&mut self) {
        self.collection_state
            .select((!self.collections.is_empty()).then_some(self.selected_collection));
        self.item_state
            .select((!self.filtered_items().is_empty()).then_some(self.selected_item));
    }

    fn clamp_item_selection(&mut self) {
        self.selected_item = self
            .selected_item
            .min(self.filtered_items().len().saturating_sub(1));
    }

    fn expire_secret(&mut self) {
        if self
            .reveal
            .as_ref()
            .is_some_and(|reveal| Instant::now() >= reveal.expires_at)
        {
            self.reveal = None;
            self.message = "revealed secret cleared".into();
        }
    }
}

pub async fn run_tui(store: &impl SecretStore) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = TuiApp::new(store).await?;
    let result = loop {
        app.expire_secret();
        terminal.draw(|frame| draw(frame, &mut app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if handle_key(store, &mut app, key).await? {
                break Ok(());
            }
        }
    };
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    result
}

async fn handle_key(store: &impl SecretStore, app: &mut TuiApp, key: KeyEvent) -> Result<bool> {
    match app.mode {
        InputMode::Search => handle_search_key(app, key),
        InputMode::Help => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                app.mode = InputMode::Normal;
            }
        }
        InputMode::Normal => match key.code {
            _ if app.page == Page::Form => {
                if handle_form_key(store, app, key).await? {
                    return Ok(true);
                }
            }
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('?') => app.mode = InputMode::Help,
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
                    if app.page == Page::Collections {
                        app.refresh_items(store).await?;
                    }
                    app.next_page();
                }
            }
            KeyCode::Char('l') => {
                if app.page == Page::Details {
                    app.move_action(1);
                } else {
                    if app.page == Page::Collections {
                        app.refresh_items(store).await?;
                    }
                    app.next_page();
                }
            }
            KeyCode::Enter => {
                if app.page == Page::Details {
                    activate_detail_action(store, app).await?;
                } else {
                    if app.page == Page::Collections {
                        app.refresh_items(store).await?;
                    }
                    app.next_page();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_selection(1);
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_selection(-1);
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::PageDown => {
                app.move_selection(PAGE_SIZE as isize);
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::PageUp => {
                app.move_selection(-(PAGE_SIZE as isize));
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::Home => {
                app.jump_selection(false);
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::End => {
                app.jump_selection(true);
                refresh_if_collections_page(store, app).await?;
            }
            KeyCode::Char('/') => {
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

fn handle_search_key(app: &mut TuiApp, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.message = if app.filter.is_empty() {
                "search cleared".into()
            } else {
                format!("filter: {}", app.filter)
            };
        }
        KeyCode::Backspace => {
            app.filter.pop();
            app.selected_item = 0;
            app.clamp_item_selection();
            app.sync_states();
        }
        KeyCode::Char(ch) => {
            app.filter.push(ch);
            app.selected_item = 0;
            app.clamp_item_selection();
            app.sync_states();
        }
        _ => {}
    }
}

async fn handle_form_key(
    store: &impl SecretStore,
    app: &mut TuiApp,
    key: KeyEvent,
) -> Result<bool> {
    if app
        .form
        .as_ref()
        .is_some_and(|form| form.kind == FormKind::Attributes)
    {
        return handle_attributes_key(app, key);
    }
    match key.code {
        KeyCode::Esc => cancel_form(app),
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Tab => form_next_focus(app),
        KeyCode::BackTab => form_prev_focus(app),
        KeyCode::Down => {
            form_next_focus(app);
            keep_form_focus_visible(app);
        }
        KeyCode::Up => {
            form_prev_focus(app);
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
                open_attributes_form(app);
            } else {
                form_next_focus(app);
                keep_form_focus_visible(app);
            }
        }
        KeyCode::Backspace => {
            if let Some(field) = current_form_field_mut(app) {
                field.backspace();
            }
        }
        KeyCode::Delete => {
            if let Some(field) = current_form_field_mut(app) {
                field.delete();
            }
        }
        KeyCode::Char(ch) => {
            if let Some(field) = current_form_field_mut(app) {
                field.insert(ch);
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_attributes_key(app: &mut TuiApp, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc => finish_attributes(app, false),
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Tab => attributes_next_focus(app),
        KeyCode::BackTab => attributes_prev_focus(app),
        KeyCode::Down => {
            attributes_down(app);
            keep_form_focus_visible(app);
        }
        KeyCode::Up => {
            attributes_up(app);
            keep_form_focus_visible(app);
        }
        KeyCode::Left => move_current_field_cursor_or_button(app, -1),
        KeyCode::Right => move_current_field_cursor_or_button(app, 1),
        KeyCode::Home => move_current_field_home(app),
        KeyCode::End => move_current_field_end(app),
        KeyCode::Enter => {
            if app.form.as_ref().is_some_and(|form| form.focus_buttons) {
                submit_attributes_action(app);
            } else if app
                .form
                .as_ref()
                .is_some_and(|form| form.selected_field == 0)
            {
                load_selected_attribute(app);
                keep_form_focus_visible(app);
            } else {
                attributes_next_focus(app);
                keep_form_focus_visible(app);
            }
        }
        KeyCode::Backspace => {
            if let Some(field) = current_form_field_mut(app) {
                field.backspace();
            }
        }
        KeyCode::Delete => {
            if let Some(field) = current_form_field_mut(app) {
                field.delete();
            }
        }
        KeyCode::Char(ch) => {
            if let Some(field) = current_form_field_mut(app) {
                field.insert(ch);
            }
        }
        _ => {}
    }
    Ok(false)
}

fn current_form_field_mut(app: &mut TuiApp) -> Option<&mut FormField> {
    let form = app.form.as_mut()?;
    if form.focus_buttons {
        return None;
    }
    let field_index = if form.kind == FormKind::Attributes {
        form.selected_field.checked_sub(1)?
    } else {
        form.selected_field
    };
    let field = form.fields.get_mut(field_index)?;
    (field.kind == FormFieldKind::Text).then_some(field)
}

fn current_form_field_kind(app: &TuiApp) -> Option<FormFieldKind> {
    let form = app.form.as_ref()?;
    if form.focus_buttons {
        return None;
    }
    form.fields.get(form.selected_field).map(|field| field.kind)
}

fn form_next_focus(app: &mut TuiApp) {
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

fn form_prev_focus(app: &mut TuiApp) {
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

fn keep_form_focus_visible(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.kind == FormKind::Attributes {
        form.scroll = match (form.focus_buttons, form.selected_field) {
            (true, _) => 17,
            (false, 0) => 3,
            (false, 1) => 9,
            (false, 2) => 13,
            _ => 0,
        };
        return;
    }
    if form.focus_buttons {
        form.scroll = form.fields.len().saturating_mul(4);
    } else {
        form.scroll = form.selected_field.saturating_mul(4);
    }
}

fn form_move_button(app: &mut TuiApp, delta: isize) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    form.focus_buttons = true;
    form.selected_button = move_index(form.selected_button, form.buttons().len(), delta);
}

fn move_current_field_cursor_or_button(app: &mut TuiApp, delta: isize) {
    if app.form.as_ref().is_some_and(|form| form.focus_buttons) {
        form_move_button(app, delta);
    } else if let Some(field) = current_form_field_mut(app) {
        field.move_cursor(delta);
    }
}

fn move_current_field_home(app: &mut TuiApp) {
    if let Some(field) = current_form_field_mut(app) {
        field.move_home();
    }
}

fn move_current_field_end(app: &mut TuiApp) {
    if let Some(field) = current_form_field_mut(app) {
        field.move_end();
    }
}

fn attributes_next_focus(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.focus_buttons {
        form.focus_buttons = false;
        form.selected_field = 0;
    } else if form.selected_field < form.fields.len() {
        form.selected_field += 1;
    } else {
        form.focus_buttons = true;
    }
}

fn attributes_prev_focus(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.focus_buttons {
        form.focus_buttons = false;
        form.selected_field = form.fields.len();
    } else if form.selected_field > 0 {
        form.selected_field -= 1;
    } else {
        form.focus_buttons = true;
    }
}

fn attributes_down(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.focus_buttons {
        return;
    }
    if form.selected_field == 0 {
        if form.attributes.is_empty() || form.selected_attribute + 1 >= form.attributes.len() {
            form.selected_field = 1;
        } else {
            form.selected_attribute += 1;
        }
    } else {
        attributes_next_focus(app);
    }
}

fn attributes_up(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if form.focus_buttons {
        form.focus_buttons = false;
        form.selected_field = form.fields.len();
        return;
    }
    if form.selected_field == 0 {
        form.selected_attribute = form.selected_attribute.saturating_sub(1);
    } else if form.selected_field == 1 {
        form.selected_field = 0;
    } else {
        attributes_prev_focus(app);
    }
}

fn cancel_form(app: &mut TuiApp) {
    let page = app
        .form
        .as_ref()
        .map(FormState::cancel_page)
        .unwrap_or(Page::Items);
    clear_form_secrets(app);
    app.form = None;
    app.page = page;
    app.message = "cancelled".into();
}

async fn submit_or_cancel_form(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(form) = app.form.as_ref() else {
        return Ok(());
    };
    if form.buttons().get(form.selected_button) == Some(&"Cancel") {
        cancel_form(app);
        return Ok(());
    }

    match form.kind {
        FormKind::NewCollection => submit_new_collection(store, app).await?,
        FormKind::NewItem => submit_new_item(store, app).await?,
        FormKind::EditItem => submit_edit_item(store, app).await?,
        FormKind::DeleteItem => submit_delete_item(store, app).await?,
        FormKind::Attributes => submit_attributes_action(app),
    }
    Ok(())
}

fn open_attributes_form(app: &mut TuiApp) {
    let Some(parent) = app.form.take() else {
        return;
    };
    app.form = Some(FormState {
        kind: FormKind::Attributes,
        fields: vec![FormField::text("Key", ""), FormField::text("Value", "")],
        attributes: parent.attributes.clone(),
        selected_attribute: 0,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        message:
            "Select an attribute, edit key/value, then Add/Update. Done returns to the item form."
                .into(),
        parent: Some(Box::new(parent)),
    });
}

fn submit_attributes_action(app: &mut TuiApp) {
    let Some(form) = app.form.as_ref() else {
        return;
    };
    match form.buttons().get(form.selected_button).copied() {
        Some("Add/Update") => add_or_update_attribute(app),
        Some("Remove") => remove_selected_attribute(app),
        Some("Done") => finish_attributes(app, true),
        Some("Cancel") => finish_attributes(app, false),
        _ => {}
    }
}

fn add_or_update_attribute(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    let key = form
        .fields
        .first()
        .map(|field| field.value.trim().to_owned())
        .unwrap_or_default();
    if key.is_empty() {
        form.message = "Key is required.".into();
        if let Some(field) = form.fields.get_mut(0) {
            field.error = Some("required".into());
        }
        return;
    }
    let value = form
        .fields
        .get(1)
        .map(|field| field.value.clone())
        .unwrap_or_default();
    form.attributes.insert(key, value);
    form.selected_attribute = form
        .selected_attribute
        .min(form.attributes.len().saturating_sub(1));
    if let Some(field) = form.fields.get_mut(0) {
        field.value.clear();
        field.cursor = 0;
    }
    if let Some(field) = form.fields.get_mut(1) {
        field.value.clear();
        field.cursor = 0;
    }
    form.selected_field = 0;
    form.message = "attribute saved".into();
}

fn remove_selected_attribute(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    let Some(key) = form.attributes.keys().nth(form.selected_attribute).cloned() else {
        form.message = "no attribute selected".into();
        return;
    };
    form.attributes.remove(&key);
    form.selected_attribute = form
        .selected_attribute
        .min(form.attributes.len().saturating_sub(1));
    form.message = "attribute removed".into();
}

fn load_selected_attribute(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    let Some((key, value)) = form
        .attributes
        .iter()
        .nth(form.selected_attribute)
        .map(|(key, value)| (key.clone(), value.clone()))
    else {
        form.selected_field = 1;
        return;
    };
    if let Some(field) = form.fields.get_mut(0) {
        field.value = key;
        field.move_end();
    }
    if let Some(field) = form.fields.get_mut(1) {
        field.value = value;
        field.move_end();
    }
    form.selected_field = 1;
}

fn finish_attributes(app: &mut TuiApp, save: bool) {
    let Some(mut attr_form) = app.form.take() else {
        return;
    };
    let Some(mut parent) = attr_form.parent.take().map(|parent| *parent) else {
        app.form = Some(attr_form);
        return;
    };
    if save {
        parent.attributes = attr_form.attributes;
        update_parent_attribute_summary(&mut parent);
    }
    parent.message = if save {
        "attributes updated".into()
    } else {
        "attributes unchanged".into()
    };
    app.form = Some(parent);
    app.page = Page::Form;
}

async fn refresh_if_collections_page(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if app.page == Page::Collections {
        app.refresh_items(store).await?;
    }
    Ok(())
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    current.saturating_add_signed(delta).min(len - 1)
}

fn byte_index_at_char(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .nth(char_index)
        .unwrap_or(text.len())
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp) {
    app.sync_states();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(header(app)).block(Block::default().borders(Borders::ALL)),
        outer[0],
    );
    match app.mode {
        InputMode::Help => draw_help(frame, outer[1]),
        _ => match app.page {
            Page::Collections => draw_collections(frame, app, outer[1]),
            Page::Items => draw_items(frame, app, outer[1]),
            Page::Details => draw_details(frame, app, outer[1]),
            Page::Form => draw_form(frame, app, outer[1]),
        },
    }
    frame.render_widget(Paragraph::new(footer(app)), outer[2]);
}

fn draw_collections(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("Actions: "),
            Span::styled(
                " New Collection ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  press n"),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let rows: Vec<_> = app
        .collections
        .iter()
        .map(|collection| {
            item_line(&format!(
                "{}{}",
                if collection.locked { "🔒 " } else { "" },
                collection.label
            ))
        })
        .collect();
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(Block::default().title("Collections").borders(Borders::ALL)),
        areas[1],
        &mut app.collection_state,
    );
    draw_scrollbar(
        frame,
        areas[1],
        app.collections.len(),
        app.selected_collection,
    );
}

fn draw_items(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("Actions: "),
            Span::styled(
                " New Item ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  press n"),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let rows: Vec<_> = app
        .filtered_items()
        .iter()
        .map(|item| {
            item_line(&format!(
                "{}{}",
                if item.locked { "🔒 " } else { "" },
                item.label
            ))
        })
        .collect();
    let title = if app.filter.is_empty() {
        "Items".to_owned()
    } else {
        format!("Items /{}", app.filter)
    };
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(Block::default().title(title).borders(Borders::ALL)),
        areas[1],
        &mut app.item_state,
    );
    draw_scrollbar(
        frame,
        areas[1],
        app.filtered_items().len(),
        app.selected_item,
    );
}

fn draw_details(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_buttons(app))
            .block(Block::default().title("Actions").borders(Borders::ALL)),
        areas[0],
    );

    let lines = detail_lines(app);
    let visible_rows = areas[1].height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_rows.max(1));
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.detail_scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().title("Details").borders(Borders::ALL)),
        areas[1],
    );
    draw_scrollbar(
        frame,
        areas[1],
        max_scroll + visible_rows,
        app.detail_scroll,
    );
}

fn draw_form(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: Rect) {
    let Some(form) = &app.form else {
        return;
    };
    if form.kind == FormKind::Attributes {
        draw_attributes_form(frame, app, area);
        return;
    }

    frame.render_widget(
        Block::default().title(form.title()).borders(Borders::ALL),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });

    let help = if form.kind == FormKind::DeleteItem {
        "Choose Delete or Cancel. Esc cancels."
    } else {
        "↑↓ move fields · ←/→ move cursor · Enter next/save · Tab also works · Esc cancel"
    };
    frame.render_widget(
        Paragraph::new(vec![Line::from(form.message.clone()), Line::from(help)]),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 2,
        },
    );
    let button_area = Rect {
        x: inner.x,
        y: inner.bottom().saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    let fields_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(3),
        width: inner.width,
        height: inner.height.saturating_sub(5),
    };

    let Some(form) = app.form.as_mut() else {
        return;
    };
    let content_height = form.fields.len().saturating_mul(4);
    let max_scroll = content_height.saturating_sub(fields_area.height as usize);
    form.scroll = form.scroll.min(max_scroll);

    for (index, field) in form.fields.iter().enumerate() {
        let field_y = index.saturating_mul(4);
        let Some(field_area) = virtual_rect_with_min(fields_area, field_y, 3, form.scroll, 3)
        else {
            continue;
        };
        let focused = !form.focus_buttons && index == form.selected_field;
        draw_input_field(frame, field, focused, field_area);
    }
    draw_content_scrollbar(frame, fields_area, content_height, form.scroll);

    draw_button_row(
        frame,
        form.buttons(),
        form.selected_button,
        form.focus_buttons,
        button_area,
    );
}

fn draw_input_field(frame: &mut ratatui::Frame<'_>, field: &FormField, focused: bool, area: Rect) {
    if area.height == 0 {
        return;
    }
    if area.height < 3 {
        let marker = if focused { "▶ " } else { "" };
        frame.render_widget(Paragraph::new(format!("{marker}{} …", field.label)), area);
        return;
    }

    let border_style = if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let title = if let Some(error) = &field.error {
        format!("{} — {}", field.label, error)
    } else if field.kind == FormFieldKind::Attributes {
        format!("{} — Enter to edit", field.label)
    } else {
        field.label.clone()
    };
    frame.render_widget(
        Paragraph::new(field.visible_value(focused)).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        ),
        area,
    );
}

fn draw_button_row(
    frame: &mut ratatui::Frame<'_>,
    buttons: &[&str],
    selected_button: usize,
    focused: bool,
    area: Rect,
) {
    let mut spans = vec![Span::raw("  ")];
    for (index, button) in buttons.iter().enumerate() {
        let selected = focused && index == selected_button;
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {button} "), style));
        spans.push(Span::raw("  "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_attributes_form(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: Rect) {
    frame.render_widget(
        Block::default().title("Attributes").borders(Borders::ALL),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if inner.height == 0 {
        return;
    }

    // Single-column virtual layout. Every interactive element keeps a minimum
    // height; the whole form scrolls when the terminal is too short.
    let help_y = 0usize;
    let help_height = 2usize;
    let list_y = help_y + help_height + 1;
    let list_height = 5usize;
    let key_y = list_y + list_height + 1;
    let input_height = 3usize;
    let value_y = key_y + input_height + 1;
    let button_y = value_y + input_height + 1;
    let button_height = 1usize;
    let content_height = button_y + button_height;
    let max_scroll = content_height.saturating_sub(inner.height as usize);
    form.scroll = form.scroll.min(max_scroll);

    if let Some(help_area) = virtual_rect_with_min(inner, help_y, help_height, form.scroll, 1) {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(form.message.clone()),
                Line::from("↑↓ move · Enter load/next · ←→ cursor/buttons · Esc cancel"),
            ]),
            help_area,
        );
    }

    if let Some(list_area) = virtual_rect_with_min(inner, list_y, list_height, form.scroll, 3) {
        let rows: Vec<_> = if form.attributes.is_empty() {
            vec![ListItem::new("<none>")]
        } else {
            form.attributes
                .iter()
                .map(|(key, value)| ListItem::new(format!("{key} = {value}")))
                .collect()
        };
        let mut state = ListState::default();
        if !form.attributes.is_empty() {
            state.select(Some(form.selected_attribute));
        }
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_symbol("› ")
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .block(
                    Block::default()
                        .title("Existing")
                        .borders(Borders::ALL)
                        .border_style(if !form.focus_buttons && form.selected_field == 0 {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        }),
                ),
            list_area,
            &mut state,
        );
        draw_scrollbar(
            frame,
            list_area,
            form.attributes.len().max(1),
            form.selected_attribute,
        );
    }

    if let Some(key_area) = virtual_rect_with_min(inner, key_y, input_height, form.scroll, 3) {
        if let Some(field) = form.fields.first() {
            draw_input_field(
                frame,
                field,
                !form.focus_buttons && form.selected_field == 1,
                key_area,
            );
        }
    }

    if let Some(value_area) = virtual_rect_with_min(inner, value_y, input_height, form.scroll, 3) {
        if let Some(field) = form.fields.get(1) {
            draw_input_field(
                frame,
                field,
                !form.focus_buttons && form.selected_field == 2,
                value_area,
            );
        }
    }

    if let Some(button_area) = virtual_rect_with_min(inner, button_y, button_height, form.scroll, 1)
    {
        draw_button_row(
            frame,
            form.buttons(),
            form.selected_button,
            form.focus_buttons,
            button_area,
        );
    }

    draw_content_scrollbar(frame, inner, content_height, form.scroll);
}

fn virtual_rect_with_min(
    container: Rect,
    y: usize,
    height: usize,
    scroll: usize,
    min_visible_height: u16,
) -> Option<Rect> {
    let top = y as isize - scroll as isize;
    let bottom = top + height as isize;
    let visible_top = top.max(0);
    let visible_bottom = bottom.min(container.height as isize);
    if visible_bottom <= visible_top {
        return None;
    }
    let visible_height = (visible_bottom - visible_top) as u16;
    if visible_height < min_visible_height {
        return None;
    }
    Some(Rect {
        x: container.x,
        y: container.y.saturating_add(visible_top as u16),
        width: container.width,
        height: visible_height,
    })
}

fn draw_content_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    content_len: usize,
    position: usize,
) {
    let visible_len = area.height as usize;
    if content_len <= visible_len || visible_len == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content_len.saturating_sub(visible_len)).position(position);
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        area,
        &mut state,
    );
}

fn draw_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    content_len: usize,
    position: usize,
) {
    let visible_len = area.height.saturating_sub(2) as usize;
    if content_len <= visible_len || visible_len == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content_len.saturating_sub(visible_len)).position(position);
    frame.render_stateful_widget(
        Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓")),
        area.inner(ratatui::layout::Margin {
            horizontal: 0,
            vertical: 1,
        }),
        &mut state,
    );
}

fn draw_help(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
    frame.render_widget(
        Paragraph::new(help_lines()).block(Block::default().title("Help").borders(Borders::ALL)),
        area,
    );
}

fn header(app: &TuiApp) -> String {
    let page = match app.page {
        Page::Collections => "Collections",
        Page::Items => "Items",
        Page::Details => "Details",
        Page::Form => app.form.as_ref().map(FormState::title).unwrap_or("Form"),
    };
    let collection = app
        .selected_collection()
        .map(|collection| collection.label.as_str())
        .unwrap_or("none");
    let item = app
        .selected_item()
        .map(|item| item.label.as_str())
        .unwrap_or("none");
    format!("secretui  /  {page}\ncollection: {collection}  |  item: {item}")
}

fn footer(app: &TuiApp) -> String {
    match app.mode {
        InputMode::Search => format!("Search: {}  ·  Enter/Esc finish  ·  Backspace edit", app.filter),
        InputMode::Help => "Esc / ? close help".into(),
        InputMode::Normal => match app.page {
            Page::Collections => {
                "↑↓ choose collection  ·  Enter/→ items  ·  n New Collection  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Items => {
                "↑↓ choose item  ·  Enter/→ details  ·  ←/Esc collections  ·  n New Item  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Details => {
                "↑↓ scroll  ·  ←/→ choose button  ·  Enter activate  ·  Esc back  ·  ? help  ·  q quit".into()
            }
            Page::Form => {
                "↑↓ move/scroll  ·  ←/→ cursor/buttons  ·  Enter next/activate  ·  Esc cancel".into()
            }
        },
    }
}

fn item_line(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::raw(text.to_owned())))
}

fn help_lines() -> Vec<Line<'static>> {
    vec![
        Line::from("Pages"),
        Line::from("  Collections → Items → Details"),
        Line::from(""),
        Line::from("Navigation"),
        Line::from("  ↑/↓ or j/k: move"),
        Line::from("  Enter/→/l/Tab: next page"),
        Line::from("  Esc/h: previous page or close overlay"),
        Line::from("  Details: ↑/↓ scroll, ←/→ choose button, Enter activates"),
        Line::from("  Home/End/PgUp/PgDn: jump"),
        Line::from("  Forms: ↑↓ move/scroll, ←→ cursor/buttons, Enter next/activate"),
        Line::from(""),
        Line::from("Actions"),
        Line::from("  Collections: n New Collection"),
        Line::from("  Items: n New Item"),
        Line::from("  Details buttons: reveal/copy/edit/delete/lock"),
        Line::from("  / search  ? help  q quit"),
        Line::from(""),
        Line::from("Secrets stay hidden unless explicitly revealed or copied."),
    ]
}

fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(app.message.clone()),
        Line::from(""),
    ];
    if let Some(collection) = app.selected_collection() {
        lines.push(Line::from("Collection"));
        lines.push(Line::from(format!("  label: {}", collection.label)));
        lines.push(Line::from(format!("  path: {}", collection.path)));
        lines.push(Line::from(""));
    }
    if let Some(item) = app.selected_item() {
        lines.push(Line::from("Item"));
        lines.push(Line::from(format!("  label: {}", item.label)));
        lines.push(Line::from(format!("  path: {}", item.path)));
        lines.push(Line::from(format!("  locked: {}", item.locked)));
        lines.push(Line::from(""));
        lines.push(Line::from("Attributes"));
        for (key, value) in &item.attributes {
            lines.push(Line::from(format!("  {key}={value}")));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Secret"));
        lines.push(Line::from("  <hidden>"));
        if let Some(reveal) = &app.reveal {
            if reveal.item_path == item.path {
                lines.push(Line::from(format!(
                    "  revealed: {}",
                    String::from_utf8_lossy(&reveal.secret)
                )));
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

fn revealed_line_index(app: &TuiApp) -> usize {
    detail_lines(app)
        .iter()
        .position(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("revealed:"))
        })
        .unwrap_or(0)
}

fn action_buttons(app: &TuiApp) -> Line<'static> {
    let mut spans = vec![Span::raw("Actions: ")];
    for (index, action) in DETAIL_ACTIONS.iter().enumerate() {
        let selected = index == app.selected_action;
        let label = match action {
            DetailAction::Reveal => "Reveal",
            DetailAction::Copy => "Copy",
            DetailAction::Edit => "Edit",
            DetailAction::Delete => "Delete",
            DetailAction::LockUnlock => "Lock/Unlock",
            DetailAction::Back => "Back",
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

async fn activate_detail_action(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    match app.selected_action() {
        DetailAction::Reveal => reveal_selected(store, app).await?,
        DetailAction::Copy => copy_selected(store, app).await?,
        DetailAction::Edit => start_edit_item(app),
        DetailAction::Delete => start_delete_item(app),
        DetailAction::LockUnlock => lock_toggle(store, app).await?,
        DetailAction::Back => app.previous_page(),
    }
    Ok(())
}

fn start_new_collection(app: &mut TuiApp) {
    app.form = Some(FormState {
        kind: FormKind::NewCollection,
        fields: vec![FormField::text("Label", ""), FormField::text("Alias", "")],
        attributes: Attributes::new(),
        selected_attribute: 0,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        parent: None,
        message: "Create a Secret Service collection.".into(),
    });
    app.page = Page::Form;
}

fn start_new_item(app: &mut TuiApp) {
    app.form = Some(FormState {
        kind: FormKind::NewItem,
        fields: vec![
            FormField::text("Label", ""),
            FormField::attributes(0),
            FormField::secret("Secret"),
            FormField::text("Content type", "text/plain"),
        ],
        attributes: Attributes::new(),
        selected_attribute: 0,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: None,
        parent: None,
        message: "Create an item in the selected collection.".into(),
    });
    app.page = Page::Form;
}

fn start_edit_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    let attribute_count = item.attributes.len();
    app.form = Some(FormState {
        kind: FormKind::EditItem,
        fields: vec![
            FormField::text("Label", item.label),
            FormField::attributes(attribute_count),
            FormField::secret("New secret (blank keeps current)"),
            FormField::text(
                "Content type",
                item.content_type.unwrap_or_else(|| "text/plain".into()),
            ),
        ],
        attributes: item.attributes,
        selected_attribute: 0,
        selected_field: 0,
        selected_button: 0,
        scroll: 0,
        focus_buttons: false,
        target_item_path: Some(item.path),
        parent: None,
        message: "Edit metadata. Secret is changed only if the secret field is non-empty.".into(),
    });
    app.page = Page::Form;
}

fn start_delete_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item() else {
        return;
    };
    app.form = Some(FormState {
        kind: FormKind::DeleteItem,
        fields: Vec::new(),
        attributes: Attributes::new(),
        selected_attribute: 0,
        selected_field: 0,
        selected_button: 1,
        scroll: 0,
        focus_buttons: true,
        target_item_path: Some(item.path.clone()),
        parent: None,
        message: format!("Delete '{}' ? This cannot be undone.", item.label),
    });
    app.page = Page::Form;
}

async fn submit_new_collection(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let label = form_value(app, 0).trim().to_owned();
    if label.is_empty() {
        set_form_message(app, "Label is required.");
        set_form_field_error(app, 0, "required");
        return Ok(());
    }
    let alias = form_value(app, 1).trim().to_owned();
    let collection = store
        .create_collection(NewCollection { label, alias })
        .await?;
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
    app.message = "collection created".into();
    app.sync_states();
    Ok(())
}

async fn submit_new_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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
    let mut secret = form_value(app, 2).into_bytes();
    let content_type = non_empty_or(form_value(app, 3), "text/plain");
    let item = store
        .create_item(NewItem {
            collection_path: collection.path,
            label,
            attributes,
            secret: secret.clone(),
            content_type,
        })
        .await?;
    secret.zeroize();
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
    app.message = "item created".into();
    app.sync_states();
    Ok(())
}

async fn submit_edit_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(form) = app.form.as_ref() else {
        return Ok(());
    };
    let Some(item_path) = form.target_item_path.clone() else {
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
    let mut secret = form_value(app, 2).into_bytes();
    let content_type = non_empty_or(form_value(app, 3), "text/plain");
    let secret_ref = (!secret.is_empty()).then_some((secret.as_slice(), content_type.as_str()));
    store
        .edit_item(&item_path, Some(&label), Some(attributes), secret_ref)
        .await?;
    secret.zeroize();
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
    app.message = "item edited".into();
    app.sync_states();
    Ok(())
}

async fn submit_delete_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    let Some(item_path) = app
        .form
        .as_ref()
        .and_then(|form| form.target_item_path.clone())
    else {
        return Ok(());
    };
    store.delete_item(&item_path).await?;
    clear_form_secrets(app);
    app.form = None;
    app.refresh_items(store).await?;
    app.page = Page::Items;
    app.message = "item deleted".into();
    app.sync_states();
    Ok(())
}

fn set_form_message(app: &mut TuiApp, message: &str) {
    if let Some(form) = &mut app.form {
        form.message = message.into();
    }
}

fn set_form_field_error(app: &mut TuiApp, index: usize, message: &str) {
    if let Some(field) = app
        .form
        .as_mut()
        .and_then(|form| form.fields.get_mut(index))
    {
        field.error = Some(message.into());
    }
}

fn clear_form_secrets(app: &mut TuiApp) {
    if let Some(form) = &mut app.form {
        for field in &mut form.fields {
            if field.secret {
                field.value.zeroize();
            }
        }
    }
}

fn form_value(app: &TuiApp, index: usize) -> String {
    app.form
        .as_ref()
        .and_then(|form| form.fields.get(index))
        .map(|field| field.value.clone())
        .unwrap_or_default()
}

fn update_parent_attribute_summary(form: &mut FormState) {
    if let Some(field) = form
        .fields
        .iter_mut()
        .find(|field| field.kind == FormFieldKind::Attributes)
    {
        field.value = format!("{} attribute(s) — Enter to edit", form.attributes.len());
    }
}

fn non_empty_or(value: String, fallback: &str) -> String {
    let value = value.trim().to_owned();
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}

async fn reveal_selected(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let item_path = item.path.clone();
        let secret = store.reveal_secret(&item_path).await?;
        app.reveal = Some(RevealState {
            item_path,
            secret,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.page = Page::Details;
        app.message = "secret revealed for 30s".into();
        app.detail_scroll = revealed_line_index(app).saturating_sub(2);
    }
    Ok(())
}

async fn copy_selected(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let mut secret = store.reveal_secret(&item.path).await?;
        let mut text = String::from_utf8_lossy(&secret).to_string();
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            clipboard.set_text(text.clone())?;
            best_effort_clear_clipboard(text.clone());
            app.message = "copied; clipboard clear scheduled".into();
        } else {
            app.message = "clipboard unavailable".into();
        }
        text.zeroize();
        secret.zeroize();
    }
    Ok(())
}

fn best_effort_clear_clipboard(expected: String) {
    std::thread::spawn(move || {
        std::thread::sleep(SECRET_TTL);
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if clipboard.get_text().ok().as_deref() == Some(expected.as_str()) {
                let _ = clipboard.set_text(String::new());
            }
        }
    });
}

async fn lock_toggle(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(collection) = app.selected_collection().cloned() {
        store
            .set_collection_locked(&collection.path, !collection.locked)
            .await?;
        app.refresh_all(store).await?;
        app.message = "collection lock state changed".into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    content_type: None,
                    created: None,
                    modified: None,
                },
                ItemInfo {
                    collection_path: "c1".into(),
                    path: "p2".into(),
                    label: "Beta".into(),
                    locked: false,
                    attributes: Attributes::new(),
                    content_type: None,
                    created: None,
                    modified: None,
                },
            ],
        )
    }

    #[test]
    fn filter_checks_label_path_and_attrs() {
        let mut app = sample_app();
        app.filter = "example".into();
        assert_eq!(app.filtered_items().len(), 1);
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
    fn attributes_editor_done_updates_parent_summary() {
        let mut app = sample_app();
        start_new_item(&mut app);
        open_attributes_form(&mut app);
        if let Some(form) = &mut app.form {
            form.fields[0].value = "username".into();
            form.fields[0].move_end();
            form.fields[1].value = "john".into();
            form.fields[1].move_end();
        }
        add_or_update_attribute(&mut app);
        finish_attributes(&mut app, true);
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.attributes.get("username").unwrap(), "john");
        assert!(form.fields[1].value.contains("1 attribute"));
    }

    #[test]
    fn attributes_editor_cancel_keeps_parent_unchanged() {
        let mut app = sample_app();
        start_new_item(&mut app);
        open_attributes_form(&mut app);
        if let Some(form) = &mut app.form {
            form.fields[0].value = "username".into();
            form.fields[1].value = "john".into();
        }
        add_or_update_attribute(&mut app);
        finish_attributes(&mut app, false);
        let form = app.form.as_ref().unwrap();
        assert!(form.attributes.is_empty());
        assert!(form.fields[1].value.contains("0 attribute"));
    }
}
