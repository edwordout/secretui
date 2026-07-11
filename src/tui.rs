use crate::domain::{
    Attributes, CollectionInfo, ItemInfo, NewCollection, NewItem, SecretBytes, SecretValue,
};
use crate::store::{ItemTarget, SecretStore, StoreOutcome};
use crate::terminal as terminal_text;
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
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
use std::fmt::Write as _;
use std::io;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zeroize::{Zeroize, Zeroizing};

const SECRET_TTL: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 10;
const MAX_UI_WIDTH: u16 = 128;
const SECRET_PREVIEW_LIMIT: usize = 256;

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
    ReplaceSecret,
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
            FormKind::DeleteItem => &["Cancel", "Delete"],
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
            FormKind::EditItem => "Edit Item Metadata",
            FormKind::ReplaceSecret => "Replace Secret",
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
            FormKind::EditItem | FormKind::ReplaceSecret | FormKind::DeleteItem => Page::Details,
            FormKind::NewAttribute | FormKind::EditAttribute | FormKind::UnsavedChanges => {
                Page::Form
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    Reveal,
    CopyText,
    CopyBase64,
    CopyHex,
    Edit,
    ReplaceSecret,
    Delete,
    LockUnlock,
    Back,
}

const DETAIL_ACTIONS: [DetailAction; 9] = [
    DetailAction::Back,
    DetailAction::Reveal,
    DetailAction::CopyText,
    DetailAction::CopyBase64,
    DetailAction::CopyHex,
    DetailAction::Edit,
    DetailAction::ReplaceSecret,
    DetailAction::Delete,
    DetailAction::LockUnlock,
];

struct RevealState {
    item_path: String,
    value: SecretValue,
    expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewEncoding {
    EscapedUtf8,
    HexDump,
}

impl PreviewEncoding {
    fn label(self) -> &'static str {
        match self {
            Self::EscapedUtf8 => "escaped UTF-8",
            Self::HexDump => "hexadecimal",
        }
    }
}

struct SecretMetadata {
    item_path: String,
    content_type: String,
    size: usize,
    encoding: PreviewEncoding,
}

struct ClipboardClearState {
    expected: Zeroizing<String>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct DeleteSnapshot {
    collection: CollectionInfo,
    item: ItemInfo,
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
    secret_metadata: Option<SecretMetadata>,
    clipboard: Option<ArboardClipboard>,
    clipboard_clear: Option<ClipboardClearState>,
    form: Option<FormState>,
    delete_snapshot: Option<DeleteSnapshot>,
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
            selected_action: 0,
            detail_scroll: 0,
            reveal_scroll_pending: false,
            help_scroll: 0,
            page: Page::Collections,
            mode: InputMode::Normal,
            filter: String::new(),
            message: String::new(),
            reveal: None,
            secret_metadata: None,
            clipboard: None,
            clipboard_clear: None,
            form: None,
            delete_snapshot: None,
        };
        app.sync_states();
        app
    }

    async fn refresh_all(&mut self, store: &impl SecretStore) -> Result<()> {
        self.clear_reveal();
        self.collections = store.list_collections().await?;
        self.selected_collection = self
            .selected_collection
            .min(self.collections.len().saturating_sub(1));
        self.refresh_items(store).await
    }

    async fn refresh_items(&mut self, store: &impl SecretStore) -> Result<()> {
        self.clear_reveal();
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
        self.clear_reveal();
        self.page = match self.page {
            Page::Collections => Page::Items,
            Page::Items => Page::Details,
            Page::Details => Page::Details,
            Page::Form => Page::Form,
        };
        if previous != Page::Details && self.page == Page::Details {
            self.selected_action = 0;
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
        self.clear_reveal();
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
                self.clear_reveal();
            }
            Page::Items => {
                self.selected_item =
                    move_index(self.selected_item, self.filtered_items().len(), delta);
                self.detail_scroll = 0;
                self.clear_reveal();
            }
            Page::Details => {
                self.detail_scroll = move_index(self.detail_scroll, usize::MAX / 2, delta);
            }
            Page::Form => {}
        }
        self.sync_states();
    }

    fn jump_selection(&mut self, end: bool) {
        self.clear_reveal();
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

    fn clear_reveal(&mut self) {
        self.reveal = None;
        self.reveal_scroll_pending = false;
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
    let operation_result = loop {
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
                        app.clear_reveal();
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
    app.clear_reveal();
    clear_form_secrets(&mut app);
    let cleanup_result = store.cleanup_temporary_unlocks().await.and_then(|outcome| {
        if outcome.warnings.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "temporary-unlock cleanup remained incomplete: {}",
                warning_text(&outcome.warnings)
            ))
        }
    });
    let result = match (operation_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Err(cleanup_error)) => Err(anyhow::anyhow!(
            "TUI operation failed: {operation_error:#}; shutdown cleanup also failed: {cleanup_error:#}"
        )),
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

fn key_starts_backend_operation(app: &TuiApp, key: KeyEvent) -> bool {
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

fn handle_search_key(app: &mut TuiApp, key: KeyEvent) {
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
        && form.selected_attribute < displayed_attribute_count(form)
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
            form.selected_attribute = displayed_attribute_count(form);
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
    if form.kind == FormKind::DeleteItem {
        app.delete_snapshot = None;
    }
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
        Some(FormKind::ReplaceSecret) => submit_replace_secret(store, app).await?,
        Some(FormKind::DeleteItem) => submit_delete_item(store, app).await?,
        _ => {}
    }
    Ok(())
}

fn open_attribute_form(app: &mut TuiApp) {
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
                    terminal_text::label(&collection.label)
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
                &format!(
                    "{}{}",
                    if item.locked { "🔒 " } else { "" },
                    terminal_text::label(&item.label)
                ),
                row_width,
            )
        })
        .unzip();
    let title = if app.filter.is_empty() {
        "Items".to_owned()
    } else {
        format!("Items /{}", terminal_text::attribute_value(&app.filter))
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
        "Choose Cancel or Delete. Esc cancels."
    } else {
        "↑↓ move fields · ←/→ move cursor · Enter next/save · Tab also works · Esc cancel"
    };
    let mut help_lines = if form.kind == FormKind::DeleteItem {
        delete_confirmation_lines(
            &form.message,
            app.delete_snapshot.as_ref(),
            inner.width as usize,
        )
    } else {
        wrap_text(&terminal_text::error(&form.message), inner.width as usize)
    };
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

fn delete_confirmation_lines(
    message: &str,
    snapshot: Option<&DeleteSnapshot>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut logical_lines = vec![terminal_text::error(message)];
    if let Some(snapshot) = snapshot {
        logical_lines.extend([
            format!("Item label: {}", terminal_text::label(&snapshot.item.label)),
            format!(
                "Collection: {}",
                terminal_text::label(&snapshot.collection.label)
            ),
            format!(
                "Collection path: {}",
                terminal_text::path(&snapshot.collection.path)
            ),
            format!("Item path: {}", terminal_text::path(&snapshot.item.path)),
            "WARNING: Metadata exports contain no secret values and cannot restore a deleted secret."
                .into(),
            "Attributes:".into(),
        ]);
        logical_lines.extend(
            snapshot
                .item
                .attributes
                .iter()
                .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
                .map(|(key, value)| {
                    format!(
                        "  {}={}",
                        terminal_text::attribute_key(key),
                        terminal_text::attribute_value(value)
                    )
                }),
        );
        let omitted = snapshot
            .item
            .attributes
            .len()
            .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
        if omitted > 0 {
            logical_lines.push(format!("  … {omitted} attribute(s) not displayed"));
        } else if snapshot.item.attributes.is_empty() {
            logical_lines.push("  <none>".into());
        }
    }
    logical_lines
        .into_iter()
        .flat_map(|line| wrap_text(&line, width))
        .collect()
}

fn form_title(form: &FormState) -> String {
    if form.dirty
        && matches!(
            form.kind,
            FormKind::NewCollection
                | FormKind::NewItem
                | FormKind::EditItem
                | FormKind::ReplaceSecret
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
                .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
                .map(|(key, value)| {
                    wrapped_list_item(
                        &format!(
                            "{} = {}",
                            terminal_text::attribute_key(key),
                            terminal_text::attribute_value(value)
                        ),
                        row_width,
                    )
                    .1
                })
                .sum::<usize>()
                .saturating_add(1)
                .saturating_add(usize::from(
                    form.attributes.len() > terminal_text::DISPLAYED_ATTRIBUTE_LIMIT,
                ));
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
    for (key, value) in form
        .attributes
        .iter()
        .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
    {
        let (row, height) = wrapped_list_item(
            &format!(
                "{} = {}",
                terminal_text::attribute_key(key),
                terminal_text::attribute_value(value)
            ),
            row_width,
        );
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

    let omitted = form
        .attributes
        .len()
        .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
    if omitted > 0 {
        rows.push(ListItem::new(format!(
            "… {omitted} additional attribute(s) not displayed"
        )));
        row_heights.push(1);
    }

    form.selected_attribute = form.selected_attribute.min(displayed_attribute_count(form));
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

fn displayed_attribute_count(form: &FormState) -> usize {
    form.attributes
        .len()
        .min(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
}

fn draw_input_field(frame: &mut ratatui::Frame<'_>, field: &FormField, focused: bool, area: Rect) {
    if area.height == 0 {
        return;
    }
    if area.height < 3 {
        let marker = if focused { "▶ " } else { "" };
        frame.render_widget(
            Paragraph::new(format!("{marker}{} …", terminal_text::label(&field.label))),
            area,
        );
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
        format!(
            "{} ({})",
            terminal_text::label(&field.label),
            terminal_text::error(error)
        )
    } else if field.kind == FormFieldKind::Attributes {
        format!("{} (Enter to edit)", terminal_text::label(&field.label))
    } else {
        terminal_text::label(&field.label)
    };
    let inner_width = area.width.saturating_sub(2) as usize;
    let horizontal_scroll = if focused && inner_width > 0 {
        field.cursor.saturating_add(1).saturating_sub(inner_width)
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(terminal_text::terminal_safe(
            &field.visible_value(focused),
            form_field_render_limit(field),
        ))
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

fn form_field_render_limit(field: &FormField) -> usize {
    match field.label.as_str() {
        "Key" => terminal_text::ATTRIBUTE_KEY_GRAPHEME_LIMIT,
        "Label" | "Alias" => terminal_text::LABEL_GRAPHEME_LIMIT,
        _ => terminal_text::ATTRIBUTE_VALUE_GRAPHEME_LIMIT,
    }
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
        .map(|collection| terminal_text::label(&collection.label))
        .unwrap_or_else(|| "none".into());
    let item = app
        .selected_item()
        .map(|item| terminal_text::label(&item.label))
        .unwrap_or_else(|| "none".into());
    let mut header = format!("secretui  /  {page}\ncollection: {collection}  |  item: {item}");
    if !app.message.is_empty() {
        write!(header, "\nstatus: {}", terminal_text::error(&app.message))
            .expect("write to string");
    }
    header
}

fn footer(app: &TuiApp) -> String {
    match app.mode {
        InputMode::Search => format!(
            "Search: {}  ·  Enter/Esc finish  ·  Backspace edit",
            terminal_text::attribute_value(&app.filter)
        ),
        InputMode::Help => "↑↓ scroll  ·  Esc / ? close help".into(),
        InputMode::Normal => match app.page {
            Page::Collections => {
                "↑↓ choose collection  ·  Enter/→ items  ·  n New Collection  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Items => {
                "↑↓ choose item  ·  Enter/→ details  ·  ←/Esc collections  ·  n New Item  ·  / search  ·  ? help  ·  q quit".into()
            }
            Page::Details => {
                "↑↓ scroll  ·  ←/→ choose button  ·  Enter activate  ·  r reveal  ·  Esc back  ·  ? help  ·  q quit".into()
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
        Line::from("  Details: r explicitly reveals the selected secret"),
        Line::from(
            "  Details buttons: Back/reveal/copy text/copy Base64/copy hex/edit metadata/replace secret/delete/lock",
        ),
        Line::from(
            "  Reveal/Copy/Edit/Replace/Delete may temporarily unlock the needed collection or item; SecretUI restores its original lock state.",
        ),
        Line::from("  / search  ? help  q quit"),
        Line::from(""),
        Line::from("Secrets stay hidden unless explicitly revealed or copied."),
    ]
}

fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(terminal_text::error(&app.message)),
        Line::from(
            "Explicit Reveal/Copy/Edit/Replace/Delete authorizes provider prompts and any temporary unlock of the needed collection or item; SecretUI restores its original lock state.",
        ),
        Line::from(""),
    ];
    if let Some(collection) = app.selected_collection() {
        lines.push(Line::from("Collection"));
        lines.push(Line::from(format!(
            "  label: {}",
            terminal_text::label(&collection.label)
        )));
        lines.push(Line::from(format!(
            "  path: {}",
            terminal_text::path(&collection.path)
        )));
        lines.push(Line::from(""));
    }
    if let Some(item) = app.selected_item() {
        lines.push(Line::from("Item"));
        lines.push(Line::from(format!(
            "  label: {}",
            terminal_text::label(&item.label)
        )));
        lines.push(Line::from(format!(
            "  path: {}",
            terminal_text::path(&item.path)
        )));
        lines.push(Line::from(format!("  locked: {}", item.locked)));
        lines.push(Line::from(""));
        lines.push(Line::from("Attributes"));
        for (key, value) in item
            .attributes
            .iter()
            .take(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT)
        {
            lines.push(Line::from(format!(
                "  {}={}",
                terminal_text::attribute_key(key),
                terminal_text::attribute_value(value)
            )));
        }
        let omitted = item
            .attributes
            .len()
            .saturating_sub(terminal_text::DISPLAYED_ATTRIBUTE_LIMIT);
        if omitted > 0 {
            lines.push(Line::from(format!(
                "  … {omitted} attribute(s) not displayed"
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Secret"));
        lines.push(Line::from("  <hidden>"));
        let metadata = app
            .secret_metadata
            .as_ref()
            .filter(|metadata| metadata.item_path == item.path);
        lines.push(Line::from(format!(
            "  content type: {}",
            metadata
                .map(|metadata| terminal_text::attribute_value(&metadata.content_type))
                .as_deref()
                .unwrap_or("unavailable until Reveal or Copy")
        )));
        lines.push(Line::from(format!(
            "  size: {}",
            metadata
                .map(|metadata| format!("{} bytes", metadata.size))
                .unwrap_or_else(|| "unavailable until Reveal or Copy".into())
        )));
        lines.push(Line::from(format!(
            "  encoding: {}",
            metadata
                .map(|metadata| metadata.encoding.label())
                .unwrap_or("unavailable until Reveal or Copy")
        )));
        if let Some(reveal) = &app.reveal {
            if reveal.item_path == item.path {
                lines.push(Line::from("  preview:"));
                lines.extend(
                    secret_preview(&reveal.value)
                        .into_iter()
                        .map(|line| Line::from(format!("    {line}"))),
                );
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
            .any(|span| span.content.contains("preview:"));
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

fn preview_encoding(value: &SecretValue) -> PreviewEncoding {
    let mime_type = value
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    let safe_text = std::str::from_utf8(value.secret.as_slice())
        .ok()
        .is_some_and(|text| {
            text.chars()
                .all(|ch| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r'))
        });
    if mime_type.to_ascii_lowercase().starts_with("text/") && safe_text {
        PreviewEncoding::EscapedUtf8
    } else {
        PreviewEncoding::HexDump
    }
}

fn secret_preview(value: &SecretValue) -> Vec<String> {
    let secret = value.secret.as_slice();
    match preview_encoding(value) {
        PreviewEncoding::EscapedUtf8 => {
            let mut preview_len = secret.len().min(SECRET_PREVIEW_LIMIT);
            while !std::str::from_utf8(&secret[..preview_len]).is_ok() {
                preview_len -= 1;
            }
            let text = std::str::from_utf8(&secret[..preview_len]).expect("valid UTF-8 boundary");
            let escaped = terminal_text::attribute_value(text);
            let mut lines = vec![if escaped.is_empty() {
                "<empty>".into()
            } else {
                escaped
            }];
            append_omitted_bytes(&mut lines, secret.len() - preview_len);
            lines
        }
        PreviewEncoding::HexDump => {
            let preview_len = secret.len().min(SECRET_PREVIEW_LIMIT);
            let mut lines = secret[..preview_len]
                .chunks(16)
                .enumerate()
                .map(|(row, bytes)| {
                    let mut line = format!("{:08x}:", row * 16);
                    for byte in bytes {
                        write!(line, " {byte:02x}").expect("write to string");
                    }
                    line
                })
                .collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push("<empty>".into());
            }
            append_omitted_bytes(&mut lines, secret.len() - preview_len);
            lines
        }
    }
}

fn append_omitted_bytes(lines: &mut Vec<String>, omitted: usize) {
    if omitted > 0 {
        lines.push(format!("… {omitted} byte(s) omitted"));
    }
}

fn action_button_lines(app: &TuiApp, width: u16) -> Vec<Line<'static>> {
    let mut chunks = Vec::new();
    for (index, action) in DETAIL_ACTIONS.iter().enumerate() {
        let selected = index == app.selected_action;
        let label = match action {
            DetailAction::Reveal => "Reveal",
            DetailAction::CopyText => "Copy Text",
            DetailAction::CopyBase64 => "Copy Base64",
            DetailAction::CopyHex => "Copy Hex",
            DetailAction::Edit => "Edit Metadata",
            DetailAction::ReplaceSecret => "Replace Secret",
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

fn start_new_collection(app: &mut TuiApp) {
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

fn start_new_item(app: &mut TuiApp) {
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

fn start_edit_item(app: &mut TuiApp) {
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

fn start_replace_secret(app: &mut TuiApp) {
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

fn start_delete_item(app: &mut TuiApp) {
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

async fn submit_new_collection(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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

async fn submit_edit_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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

async fn submit_replace_secret(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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

async fn submit_delete_item(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
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

async fn rollback_label_if_unchanged(
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

fn outcome_message<T>(success: &str, outcome: &StoreOutcome<T>) -> String {
    message_with_warnings(success.into(), &outcome.warnings)
}

fn message_with_warnings(success: String, warnings: &[crate::store::StoreWarning]) -> String {
    if warnings.is_empty() {
        success
    } else {
        format!(
            "{success}; WARNING: operation succeeded but cleanup needs attention: {}",
            warning_text(warnings)
        )
    }
}

fn warning_text(warnings: &[crate::store::StoreWarning]) -> String {
    warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
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
        let target = ItemTarget::from(item);
        let outcome = store.reveal_secret(&target).await?;
        let message = outcome_message("secret revealed for 30s", &outcome);
        let value = outcome.value;
        app.secret_metadata = Some(secret_metadata(&item_path, &value));
        app.reveal = Some(RevealState {
            item_path,
            value,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.page = Page::Details;
        app.message = message;
        app.reveal_scroll_pending = true;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyEncoding {
    Text,
    Base64,
    Hex,
}

impl CopyEncoding {
    fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Base64 => "Base64",
            Self::Hex => "hex",
        }
    }
}

fn secret_metadata(item_path: &str, value: &SecretValue) -> SecretMetadata {
    SecretMetadata {
        item_path: item_path.into(),
        content_type: value.content_type.clone(),
        size: value.secret.as_slice().len(),
        encoding: preview_encoding(value),
    }
}

async fn copy_selected(
    store: &impl SecretStore,
    app: &mut TuiApp,
    encoding: CopyEncoding,
) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let item_path = item.path.clone();
        let target = ItemTarget::from(item);
        let outcome = store.reveal_secret(&target).await?;
        let warning_suffix = if outcome.warnings.is_empty() {
            String::new()
        } else {
            format!("; WARNING: {}", warning_text(&outcome.warnings))
        };
        let value = outcome.value;
        app.secret_metadata = Some(secret_metadata(&item_path, &value));
        let expected = encode_clipboard(value.secret.as_slice(), encoding)?;
        let clipboard = match app.clipboard.as_mut() {
            Some(clipboard) => clipboard,
            None => app.clipboard.insert(ArboardClipboard::new()?),
        };
        clipboard.set_text(expected.to_string())?;
        app.clipboard_clear = Some(ClipboardClearState {
            expected,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.message = format!(
            "copied as {}; clipboard clear scheduled for 30s{}",
            encoding.label(),
            warning_suffix
        );
    }
    Ok(())
}

fn clipboard_text(secret: &[u8]) -> Result<&str> {
    std::str::from_utf8(secret)
        .map_err(|_| anyhow::anyhow!("binary secret cannot be copied as text"))
}

fn encode_clipboard(secret: &[u8], encoding: CopyEncoding) -> Result<Zeroizing<String>> {
    let encoded = match encoding {
        CopyEncoding::Text => clipboard_text(secret)?.to_owned(),
        CopyEncoding::Base64 => BASE64_STANDARD.encode(secret),
        CopyEncoding::Hex => {
            let mut encoded = String::with_capacity(secret.len().saturating_mul(2));
            for byte in secret {
                write!(encoded, "{byte:02x}").expect("write to string");
            }
            encoded
        }
    };
    Ok(Zeroizing::new(encoded))
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
        app.clear_reveal();
        let outcome = store
            .set_collection_locked(&collection.path, !collection.locked)
            .await?;
        let message = outcome_message("collection lock state changed", &outcome);
        app.refresh_all(store).await?;
        app.message = message;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemorySecretStore;
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
}
