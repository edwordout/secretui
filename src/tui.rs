use crate::domain::{Attributes, CollectionInfo, ItemInfo, NewCollection, NewItem, SecretBytes};
use crate::store::SecretStore;
use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zeroize::{Zeroize, Zeroizing};

const SECRET_TTL: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 10;
const MAX_UI_WIDTH: u16 = 128;

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
    NewAttribute,
    EditAttribute,
    UnsavedChanges,
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

impl Drop for FormField {
    fn drop(&mut self) {
        if self.secret {
            self.value.zeroize();
        }
    }
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
            value: format!("{count} attribute(s) (Enter to edit)"),
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
        self.error = None;
    }

    fn delete(&mut self) {
        let len = self.value.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = byte_index_at_char(&self.value, self.cursor);
        let end = byte_index_at_char(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
        self.error = None;
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
    editing_attribute_key: Option<String>,
    selected_field: usize,
    selected_button: usize,
    scroll: usize,
    focus_buttons: bool,
    target_item_path: Option<String>,
    message: String,
    parent: Option<Box<FormState>>,
    dirty: bool,
}

impl FormState {
    fn buttons(&self) -> &'static [&'static str] {
        match self.kind {
            FormKind::DeleteItem => &["Delete", "Cancel"],
            FormKind::NewAttribute => &["Add", "Cancel"],
            FormKind::EditAttribute => &["Update", "Cancel", "Remove"],
            FormKind::UnsavedChanges => &["Save Changes", "Discard", "Keep Editing"],
            _ => &["Save", "Cancel"],
        }
    }

    fn title(&self) -> &'static str {
        match self.kind {
            FormKind::NewCollection => "New Collection",
            FormKind::NewItem => "New Item",
            FormKind::EditItem => "Edit Item",
            FormKind::DeleteItem => "Delete Item",
            FormKind::NewAttribute => "New Attribute",
            FormKind::EditAttribute => "Edit Attribute",
            FormKind::UnsavedChanges => "Unsaved Changes",
        }
    }

    fn cancel_page(&self) -> Page {
        match self.kind {
            FormKind::NewCollection => Page::Collections,
            FormKind::NewItem => Page::Items,
            FormKind::EditItem | FormKind::DeleteItem => Page::Details,
            FormKind::NewAttribute | FormKind::EditAttribute | FormKind::UnsavedChanges => {
                Page::Form
            }
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
    secret: SecretBytes,
    expires_at: Instant,
}

struct ClipboardClearState {
    expected: Zeroizing<String>,
    expires_at: Instant,
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
    reveal_scroll_pending: bool,
    help_scroll: usize,
    page: Page,
    mode: InputMode,
    filter: String,
    message: String,
    reveal: Option<RevealState>,
    clipboard: Option<ArboardClipboard>,
    clipboard_clear: Option<ClipboardClearState>,
    form: Option<FormState>,
}

impl TuiApp {
    pub async fn new(store: &impl SecretStore) -> Result<Self> {
        let mut app = Self::from_data(store.list_collections().await?, Vec::new());
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
            reveal_scroll_pending: false,
            help_scroll: 0,
            page: Page::Collections,
            mode: InputMode::Normal,
            filter: String::new(),
            message: String::new(),
            reveal: None,
            clipboard: None,
            clipboard_clear: None,
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
        if self.page == Page::Details {
            self.reveal = None;
        }
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
                self.reveal = None;
            }
            Page::Items => {
                self.selected_item =
                    move_index(self.selected_item, self.filtered_items().len(), delta);
                self.detail_scroll = 0;
                self.reveal = None;
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
        if self
            .clipboard_clear
            .as_ref()
            .is_some_and(|clear| Instant::now() >= clear.expires_at)
        {
            if let (Some(clipboard), Some(clear)) =
                (&mut self.clipboard, self.clipboard_clear.take())
            {
                if clipboard.get_text().ok().as_deref() == Some(clear.expected.as_str()) {
                    let _ = clipboard.clear();
                }
            }
            self.message = "clipboard clear attempted".into();
        }
    }
}

pub async fn run_tui(store: &impl SecretStore) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let _cleanup = TerminalCleanup;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = TuiApp::new(store).await?;
    let result = loop {
        app.expire_secret();
        if let Err(error) = terminal.draw(|frame| draw(frame, &mut app)) {
            break Err(error.into());
        }
        let has_event = match event::poll(Duration::from_millis(250)) {
            Ok(has_event) => has_event,
            Err(error) => break Err(error.into()),
        };
        if !has_event {
            continue;
        }
        match event::read() {
            Ok(Event::Key(key)) => {
                if key_starts_backend_operation(&app, key) {
                    if let Some(form) = app.form.as_mut() {
                        form.message = "Working…".into();
                    } else {
                        app.message = "Working…".into();
                    }
                    if let Err(error) = terminal.draw(|frame| draw(frame, &mut app)) {
                        break Err(error.into());
                    }
                }
                match handle_key(store, &mut app, key).await {
                    Ok(true) => break Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        let message = format!("error: {error:#}");
                        if let Some(form) = app.form.as_mut() {
                            form.message = message;
                        } else {
                            app.message = message;
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(error) => break Err(error.into()),
        }
    };
    drop(terminal);
    result
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

async fn handle_key(store: &impl SecretStore, app: &mut TuiApp, key: KeyEvent) -> Result<bool> {
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

fn key_starts_backend_operation(app: &TuiApp, key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press || app.mode != InputMode::Normal {
        return false;
    }
    match (app.page, key.code) {
        (Page::Collections, KeyCode::Enter | KeyCode::Right | KeyCode::Tab)
        | (Page::Collections, KeyCode::Char('l') | KeyCode::Char('/')) => true,
        (Page::Details, KeyCode::Enter) => matches!(
            app.selected_action(),
            DetailAction::Reveal | DetailAction::Copy | DetailAction::LockUnlock
        ),
        (Page::Form, KeyCode::Enter) => app.form.as_ref().is_some_and(|form| {
            form.focus_buttons
                && form.selected_button == 0
                && matches!(
                    form.kind,
                    FormKind::NewCollection
                        | FormKind::NewItem
                        | FormKind::EditItem
                        | FormKind::DeleteItem
                        | FormKind::UnsavedChanges
                )
        }),
        _ => false,
    }
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

fn current_form_field_mut(app: &mut TuiApp) -> Option<&mut FormField> {
    let form = app.form.as_mut()?;
    if form.focus_buttons {
        return None;
    }
    let field = form.fields.get_mut(form.selected_field)?;
    (field.kind == FormFieldKind::Text).then_some(field)
}

fn mark_form_dirty(app: &mut TuiApp) {
    if let Some(form) = app.form.as_mut() {
        form.dirty = true;
    }
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

fn form_down(app: &mut TuiApp) {
    let Some(form) = app.form.as_mut() else {
        return;
    };
    if !form.focus_buttons
        && form.fields.get(form.selected_field).map(|field| field.kind)
            == Some(FormFieldKind::Attributes)
        && form.selected_attribute < form.attributes.len()
    {
        form.selected_attribute += 1;
        return;
    }
    form_next_focus(app);
}

fn form_up(app: &mut TuiApp) {
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
            form.selected_attribute = form.attributes.len();
        }
    }
}

fn cancel_or_confirm_form(app: &mut TuiApp) {
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

fn discard_current_form(app: &mut TuiApp) {
    let Some(mut form) = app.form.take() else {
        return;
    };
    let page = form.cancel_page();
    clear_form_state_secrets(&mut form);
    app.page = page;
    app.message = "cancelled".into();
}

fn restore_parent_form(app: &mut TuiApp, message: &str) {
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

async fn submit_or_cancel_form(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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

async fn submit_parent_form(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    match app.form.as_ref().map(|form| form.kind) {
        Some(FormKind::NewCollection) => submit_new_collection(store, app).await?,
        Some(FormKind::NewItem) => submit_new_item(store, app).await?,
        Some(FormKind::EditItem) => submit_edit_item(store, app).await?,
        Some(FormKind::DeleteItem) => submit_delete_item(store, app).await?,
        _ => {}
    }
    Ok(())
}

fn open_attribute_form(app: &mut TuiApp) {
    let Some(parent) = app.form.take() else {
        return;
    };
    let selected = parent.selected_attribute.min(parent.attributes.len());
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

fn save_attribute(app: &mut TuiApp) {
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

fn remove_attribute(app: &mut TuiApp) {
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

async fn advance_page(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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
    let area = centered_max_width(frame.area(), MAX_UI_WIDTH);
    let header_lines = wrap_text(&header(app), area.width.saturating_sub(2) as usize);
    let footer_lines = wrap_text(&footer(app), area.width as usize);
    let header_height = header_lines.len().saturating_add(2).min(u16::MAX as usize) as u16;
    let footer_height = footer_lines.len().min(u16::MAX as usize) as u16;
    let chrome_budget = area.height.saturating_sub(3);
    let footer_min = footer_height.min(1);
    let header_height = header_height.min(chrome_budget.saturating_sub(footer_min));
    let footer_height = footer_height.min(chrome_budget.saturating_sub(header_height));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(header_lines).block(Block::default().borders(Borders::ALL)),
        outer[0],
    );
    match app.mode {
        InputMode::Help => draw_help(frame, app, outer[1]),
        _ => match app.page {
            Page::Collections => draw_collections(frame, app, outer[1]),
            Page::Items => draw_items(frame, app, outer[1]),
            Page::Details => draw_details(frame, app, outer[1]),
            Page::Form => draw_form(frame, app, outer[1]),
        },
    }
    frame.render_widget(Paragraph::new(footer_lines), outer[2]);
}

fn centered_max_width(area: Rect, max_width: u16) -> Rect {
    let width = area.width.min(max_width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        width,
        ..area
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    text.split('\n')
        .flat_map(|line| wrap_text_line(line, width))
        .map(Line::from)
        .collect()
}

fn wrap_text_line(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let indent = text
        .graphemes(true)
        .take_while(|grapheme| grapheme.chars().all(char::is_whitespace))
        .collect::<String>();
    let indent = if UnicodeWidthStr::width(indent.as_str()) < width {
        indent
    } else {
        String::new()
    };
    let mut lines = Vec::new();
    let mut line = indent.clone();
    let mut has_word = false;

    for word in text.split_whitespace() {
        let separator = usize::from(has_word);
        if has_word
            && UnicodeWidthStr::width(line.as_str()) + separator + UnicodeWidthStr::width(word)
                <= width
        {
            line.push(' ');
            line.push_str(word);
            continue;
        }
        if has_word {
            lines.push(line);
            line = indent.clone();
            has_word = false;
        }

        for grapheme in word.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if has_word && UnicodeWidthStr::width(line.as_str()) + grapheme_width > width {
                lines.push(line);
                line = indent.clone();
                has_word = false;
            }
            if UnicodeWidthStr::width(line.as_str()) + grapheme_width <= width {
                line.push_str(grapheme);
                has_word = true;
            }
        }
    }

    if has_word || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn wrapped_list_item(text: &str, width: usize) -> (ListItem<'static>, usize) {
    let width = width.max(1);
    let mut wrapped = wrap_text_line(text, width);
    if wrapped.len() > 2 {
        wrapped.truncate(2);
        let last = &mut wrapped[1];
        while UnicodeWidthStr::width(last.as_str()) + 1 > width {
            let Some((index, _)) = last.grapheme_indices(true).next_back() else {
                break;
            };
            last.truncate(index);
        }
        last.push('…');
    }
    let lines: Vec<_> = wrapped.into_iter().map(Line::from).collect();
    let height = lines.len().max(1);
    (ListItem::new(lines), height)
}

fn scrollbar_position_for_rows(heights: &[usize], first_visible: usize) -> usize {
    heights.iter().take(first_visible).sum()
}

fn draw_collections(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let action_lines = new_action_lines("New Collection", "press n", area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let row_width = areas[1].width.saturating_sub(4) as usize;
    let (rows, row_heights): (Vec<_>, Vec<_>) = app
        .collections
        .iter()
        .map(|collection| {
            wrapped_list_item(
                &format!(
                    "{}{}",
                    if collection.locked { "🔒 " } else { "" },
                    collection.label
                ),
                row_width,
            )
        })
        .unzip();
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
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, app.collection_state.offset()),
    );
}

fn draw_items(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let action_lines = new_action_lines("New Item", "press n", area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().borders(Borders::ALL)),
        areas[0],
    );
    let row_width = areas[1].width.saturating_sub(4) as usize;
    let (rows, row_heights): (Vec<_>, Vec<_>) = app
        .filtered_items()
        .iter()
        .map(|item| {
            wrapped_list_item(
                &format!("{}{}", if item.locked { "🔒 " } else { "" }, item.label),
                row_width,
            )
        })
        .unzip();
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
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, app.item_state.offset()),
    );
}

fn draw_details(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let action_lines = action_button_lines(app, area.width.saturating_sub(2));
    let action_height =
        (action_lines.len().saturating_add(2) as u16).min(area.height.saturating_sub(3));
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(action_height), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(action_lines).block(Block::default().title("Actions").borders(Borders::ALL)),
        areas[0],
    );

    let (lines, revealed_row) =
        wrapped_detail_lines(app, areas[1].width.saturating_sub(2) as usize);
    let visible_rows = areas[1].height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_rows.max(1));
    if app.reveal_scroll_pending {
        app.detail_scroll = revealed_row.unwrap_or(0).saturating_sub(2);
        app.reveal_scroll_pending = false;
    }
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

    frame.render_widget(
        Block::default()
            .title(form_title(form))
            .borders(Borders::ALL),
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
    let mut help_lines = wrap_text(&form.message, inner.width as usize);
    help_lines.extend(wrap_text(help, inner.width as usize));
    let show_help = inner.height >= 8 && !help_lines.is_empty();
    let button_lines = form_button_lines(form, inner.width);
    let show_buttons = form.focus_buttons || inner.height >= 7;
    let button_height = if show_buttons {
        button_lines.len().min(u16::MAX as usize) as u16
    } else {
        0
    };
    let minimum_fields_height = if form.fields.is_empty() { 0 } else { 3 };
    let reserved_height = button_height
        .saturating_add(u16::from(show_buttons))
        .saturating_add(minimum_fields_height);
    let help_height = if show_help {
        (help_lines.len() as u16).min(inner.height.saturating_sub(reserved_height))
    } else {
        0
    };
    if show_help {
        frame.render_widget(
            Paragraph::new(help_lines),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: help_height,
            },
        );
    }
    let button_area = Rect {
        x: inner.x,
        y: inner.bottom().saturating_sub(button_height),
        width: inner.width,
        height: button_height,
    };
    let fields_offset = help_height.saturating_add(u16::from(show_help));
    let fields_bottom_gap = u16::from(show_buttons && inner.height >= 7);
    let fields_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(fields_offset),
        width: inner.width,
        height: inner
            .height
            .saturating_sub(fields_offset)
            .saturating_sub(button_height)
            .saturating_sub(fields_bottom_gap),
    };

    let Some(form) = app.form.as_mut() else {
        return;
    };
    let field_heights = form_field_heights(form, fields_area.width);
    let field_offsets: Vec<_> = field_heights
        .iter()
        .scan(0usize, |offset, height| {
            let current = *offset;
            *offset = offset.saturating_add(*height).saturating_add(1);
            Some(current)
        })
        .collect();
    let content_height = field_heights
        .iter()
        .sum::<usize>()
        .saturating_add(field_heights.len());
    let max_scroll = content_height.saturating_sub(fields_area.height as usize);
    if !form.focus_buttons {
        let field_offset = field_offsets.get(form.selected_field).copied().unwrap_or(0);
        let field_height = field_heights.get(form.selected_field).copied().unwrap_or(3);
        keep_virtual_range_visible(
            &mut form.scroll,
            fields_area.height as usize,
            field_offset,
            field_height,
        );
    }
    form.scroll = form.scroll.min(max_scroll);

    for index in 0..form.fields.len() {
        let field_y = field_offsets[index];
        let field_height = field_heights[index];
        let Some(field_area) =
            virtual_rect_with_min(fields_area, field_y, field_height, form.scroll, 3)
        else {
            continue;
        };
        let focused = !form.focus_buttons && index == form.selected_field;
        if form.fields[index].kind == FormFieldKind::Attributes {
            draw_attribute_list_field(frame, form, focused, field_area);
        } else {
            draw_input_field(frame, &form.fields[index], focused, field_area);
        }
    }
    draw_content_scrollbar(frame, fields_area, content_height, form.scroll);

    if show_buttons {
        frame.render_widget(Paragraph::new(button_lines), button_area);
    }
}

fn form_title(form: &FormState) -> String {
    if form.dirty
        && matches!(
            form.kind,
            FormKind::NewCollection | FormKind::NewItem | FormKind::EditItem
        )
    {
        format!("{} (unsaved)", form.title())
    } else {
        form.title().into()
    }
}

fn form_field_heights(form: &FormState, width: u16) -> Vec<usize> {
    form.fields
        .iter()
        .map(|field| {
            if field.kind != FormFieldKind::Attributes {
                return 3;
            }
            let row_width = width.saturating_sub(4) as usize;
            let rows_height: usize = form
                .attributes
                .iter()
                .map(|(key, value)| wrapped_list_item(&format!("{key} = {value}"), row_width).1)
                .sum::<usize>()
                .saturating_add(1);
            rows_height.saturating_add(2).clamp(5, 9)
        })
        .collect()
}

fn draw_attribute_list_field(
    frame: &mut ratatui::Frame<'_>,
    form: &mut FormState,
    focused: bool,
    area: Rect,
) {
    let row_width = area.width.saturating_sub(4) as usize;
    let mut rows = Vec::new();
    let mut row_heights = Vec::new();
    for (key, value) in &form.attributes {
        let (row, height) = wrapped_list_item(&format!("{key} = {value}"), row_width);
        rows.push(row);
        row_heights.push(height);
    }
    rows.push(
        ListItem::new("+ Create new attribute").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    );
    row_heights.push(1);

    form.selected_attribute = form.selected_attribute.min(form.attributes.len());
    let mut state = ListState::default().with_selected(Some(form.selected_attribute));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .block(
                Block::default()
                    .title("Attributes (Enter to edit)")
                    .borders(Borders::ALL)
                    .border_style(if focused {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    }),
            ),
        area,
        &mut state,
    );
    draw_scrollbar(
        frame,
        area,
        row_heights.iter().sum(),
        scrollbar_position_for_rows(&row_heights, state.offset()),
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

    let border_style = if field.error.is_some() {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let title = if let Some(error) = &field.error {
        format!("{} ({})", field.label, error)
    } else if field.kind == FormFieldKind::Attributes {
        format!("{} (Enter to edit)", field.label)
    } else {
        field.label.clone()
    };
    let inner_width = area.width.saturating_sub(2) as usize;
    let horizontal_scroll = if focused && inner_width > 0 {
        field.cursor.saturating_add(1).saturating_sub(inner_width)
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(field.visible_value(focused))
            .scroll((0, horizontal_scroll.min(u16::MAX as usize) as u16))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            ),
        area,
    );
}

fn button_row_lines(
    buttons: &[&str],
    selected_button: usize,
    focused: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    for (index, button) in buttons.iter().enumerate() {
        let selected = focused && index == selected_button;
        let destructive = matches!(*button, "Delete" | "Remove" | "Discard");
        let style = if selected && destructive {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if destructive {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut chunk = Vec::new();
        if destructive && index > 0 {
            chunk.push(Span::raw("    "));
        }
        chunk.extend([Span::styled(format!(" {button} "), style), Span::raw("  ")]);
        chunks.push(chunk);
    }
    wrap_styled_chunks(vec![Span::raw("  ")], chunks, width as usize)
}

fn form_button_lines(form: &FormState, width: u16) -> Vec<Line<'static>> {
    button_row_lines(
        form.buttons(),
        form.selected_button,
        form.focus_buttons,
        width,
    )
}

fn keep_virtual_range_visible(
    scroll: &mut usize,
    viewport_height: usize,
    target_y: usize,
    target_height: usize,
) {
    if viewport_height == 0 || target_y < *scroll {
        *scroll = target_y;
        return;
    }
    let target_bottom = target_y.saturating_add(target_height);
    let viewport_bottom = scroll.saturating_add(viewport_height);
    if target_bottom > viewport_bottom {
        *scroll = target_bottom.saturating_sub(viewport_height);
    }
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

fn draw_help(frame: &mut ratatui::Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    let lines: Vec<_> = help_lines()
        .into_iter()
        .flat_map(|line| {
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            wrap_text(&text, area.width.saturating_sub(2) as usize)
        })
        .collect();
    let visible_rows = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_rows.max(1));
    app.help_scroll = app.help_scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().title("Help").borders(Borders::ALL)),
        area,
    );
    draw_scrollbar(frame, area, max_scroll + visible_rows, app.help_scroll);
}

fn header(app: &TuiApp) -> String {
    let page = match app.page {
        Page::Collections => "Collections".into(),
        Page::Items => "Items".into(),
        Page::Details => "Details".into(),
        Page::Form => app
            .form
            .as_ref()
            .map(form_title)
            .unwrap_or_else(|| "Form".into()),
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
        InputMode::Help => "↑↓ scroll  ·  Esc / ? close help".into(),
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
                    display_secret(reveal.secret.as_slice())
                )));
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

fn wrapped_detail_lines(app: &TuiApp, width: usize) -> (Vec<Line<'static>>, Option<usize>) {
    let mut wrapped = Vec::new();
    let mut revealed_row = None;
    for line in detail_lines(app) {
        let is_revealed = line
            .spans
            .iter()
            .any(|span| span.content.contains("revealed:"));
        if is_revealed {
            revealed_row = Some(wrapped.len());
        }
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        wrapped.extend(wrap_text(&text, width));
    }
    (wrapped, revealed_row)
}

fn display_secret(secret: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(secret) else {
        return format!("<binary: {} bytes>", secret.len());
    };
    let mut chars = text.chars().flat_map(char::escape_default).peekable();
    let mut visible = chars.by_ref().take(4096).collect::<String>();
    if chars.peek().is_some() {
        visible.push('…');
    }
    visible
}

fn action_button_lines(app: &TuiApp, width: u16) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    for (index, action) in DETAIL_ACTIONS.iter().enumerate() {
        let selected = index == app.selected_action;
        let label = match action {
            DetailAction::Reveal => "Reveal",
            DetailAction::Copy => "Copy",
            DetailAction::Edit => "Edit",
            DetailAction::Delete => "Delete",
            DetailAction::LockUnlock => {
                if app
                    .selected_collection()
                    .is_some_and(|collection| collection.locked)
                {
                    "Unlock"
                } else {
                    "Lock"
                }
            }
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
        chunks.push(vec![
            Span::styled(format!(" {label} "), style),
            Span::raw(" "),
        ]);
    }
    wrap_styled_chunks(vec![Span::raw("Actions: ")], chunks, width as usize)
}

fn new_action_lines(button: &str, hint: &str, width: u16) -> Vec<Line<'static>> {
    let button_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    wrap_styled_chunks(
        vec![Span::raw("Actions: ")],
        vec![
            vec![Span::styled(format!(" {button} "), button_style)],
            vec![Span::raw(format!("  {hint}"))],
        ],
        width as usize,
    )
}

fn wrap_styled_chunks(
    prefix: Vec<Span<'static>>,
    chunks: Vec<Vec<Span<'static>>>,
    width: usize,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut spans = prefix;
    let mut line_width: usize = spans.iter().map(Span::width).sum();
    for chunk in chunks {
        let chunk_width: usize = chunk.iter().map(Span::width).sum();
        if !spans.is_empty() && line_width.saturating_add(chunk_width) > width {
            lines.push(Line::from(spans));
            spans = Vec::new();
            line_width = 0;
        }
        line_width = line_width.saturating_add(chunk_width);
        spans.extend(chunk);
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
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

fn start_new_item(app: &mut TuiApp) {
    if app.selected_collection().is_none() {
        app.message = "create or select a collection first".into();
        return;
    }
    app.form = Some(FormState {
        kind: FormKind::NewItem,
        fields: vec![
            FormField::text("Label", ""),
            FormField::attributes(0),
            FormField::secret("Secret"),
            FormField::text("Secret content type", "text/plain; charset=utf-8"),
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

fn start_edit_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    let attribute_count = item.attributes.len();
    app.reveal = None;
    app.form = Some(FormState {
        kind: FormKind::EditItem,
        fields: vec![
            FormField::text("Label", item.label),
            FormField::attributes(attribute_count),
            FormField::secret("New secret (blank keeps current)"),
            FormField::text("New secret content type", "text/plain; charset=utf-8"),
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
        message: "Secret and its content type change only when New secret is non-empty.".into(),
        dirty: false,
    });
    app.page = Page::Form;
}

fn start_delete_item(app: &mut TuiApp) {
    let Some(item) = app.selected_item().cloned() else {
        return;
    };
    app.reveal = None;
    app.form = Some(FormState {
        kind: FormKind::DeleteItem,
        fields: Vec::new(),
        attributes: Attributes::new(),
        selected_attribute: 0,
        editing_attribute_key: None,
        selected_field: 0,
        selected_button: 1,
        scroll: 0,
        focus_buttons: true,
        target_item_path: Some(item.path),
        parent: None,
        message: format!("Delete '{}' ? This cannot be undone.", item.label),
        dirty: false,
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
    let secret = SecretBytes::new(form_value(app, 2).into_bytes());
    let content_type = non_empty_or(form_value(app, 3), "text/plain; charset=utf-8");
    let item = store
        .create_item(NewItem {
            collection_path: collection.path,
            label,
            attributes,
            secret,
            content_type,
        })
        .await?;
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
    let secret = SecretBytes::new(form_value(app, 2).into_bytes());
    let content_type = non_empty_or(form_value(app, 3), "text/plain; charset=utf-8");
    let secret_ref =
        (!secret.as_slice().is_empty()).then_some((secret.as_slice(), content_type.as_str()));
    store
        .edit_item(&item_path, Some(&label), Some(attributes), secret_ref)
        .await?;
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
        clear_form_state_secrets(form);
    }
}

fn clear_form_state_secrets(form: &mut FormState) {
    for field in &mut form.fields {
        if field.secret {
            field.value.zeroize();
        }
    }
    if let Some(parent) = form.parent.as_mut() {
        clear_form_state_secrets(parent);
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
        field.value = format!("{} attribute(s) (Enter to edit)", form.attributes.len());
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
        app.reveal_scroll_pending = true;
    }
    Ok(())
}

async fn copy_selected(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let secret = store.reveal_secret(&item.path).await?;
        let text = clipboard_text(secret.as_slice())?;
        let expected = Zeroizing::new(text.to_owned());
        let clipboard = match app.clipboard.as_mut() {
            Some(clipboard) => clipboard,
            None => app.clipboard.insert(ArboardClipboard::new()?),
        };
        clipboard.set_text(expected.to_string())?;
        app.clipboard_clear = Some(ClipboardClearState {
            expected,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.message = "copied; clipboard clear scheduled for 30s".into();
    }
    Ok(())
}

fn clipboard_text(secret: &[u8]) -> Result<&str> {
    std::str::from_utf8(secret)
        .map_err(|_| anyhow::anyhow!("binary secret cannot be copied as text"))
}

struct ArboardClipboard {
    clipboard: arboard::Clipboard,
}

impl ArboardClipboard {
    fn new() -> Result<Self> {
        Ok(Self {
            clipboard: arboard::Clipboard::new()?,
        })
    }

    fn clear(&mut self) -> Result<()> {
        Ok(self.clipboard.clear()?)
    }

    fn set_text(&mut self, text: String) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use arboard::SetExtLinux;
            self.clipboard.set().exclude_from_history().text(text)?;
        }
        #[cfg(not(target_os = "linux"))]
        self.clipboard.set_text(text)?;
        Ok(())
    }

    fn get_text(&mut self) -> Result<String> {
        Ok(self.clipboard.get_text()?)
    }
}

async fn lock_toggle(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(collection) = app.selected_collection().cloned() {
        app.reveal = None;
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
    use ratatui::{backend::TestBackend, Terminal};

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
        assert_eq!(app.form.as_ref().unwrap().selected_field, 2);
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

    #[test]
    fn secret_display_escapes_control_characters_and_binary() {
        assert_eq!(display_secret(b"a\nb"), "a\\nb");
        assert_eq!(display_secret(&[0xff]), "<binary: 1 bytes>");
        assert_eq!(clipboard_text(b"text").unwrap(), "text");
        assert_eq!(
            clipboard_text(&[0xff]).unwrap_err().to_string(),
            "binary secret cannot be copied as text"
        );
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
        for label in ["Back", "Reveal", "Copy", "Edit", "Delete", "Lock"] {
            assert!(rendered.contains(label));
        }
    }
}
