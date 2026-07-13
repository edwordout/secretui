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
use std::cmp::Ordering;
use std::fmt::Write as _;
use std::io;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zeroize::{Zeroize, Zeroizing};

mod actions;
mod input;
mod rendering;
mod secret;

use actions::*;
use input::*;
use rendering::*;
use secret::*;

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

fn listing_order(
    left_label: &str,
    left_path: &str,
    right_label: &str,
    right_path: &str,
) -> Ordering {
    left_label
        .to_lowercase()
        .cmp(&right_label.to_lowercase())
        .then_with(|| left_label.cmp(right_label))
        .then_with(|| left_path.cmp(right_path))
}

fn sort_collections(collections: &mut [CollectionInfo]) {
    collections
        .sort_by(|left, right| listing_order(&left.label, &left.path, &right.label, &right.path));
}

fn sort_items(items: &mut [ItemInfo]) {
    items.sort_by(|left, right| listing_order(&left.label, &left.path, &right.label, &right.path));
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

    fn from_data(mut collections: Vec<CollectionInfo>, mut items: Vec<ItemInfo>) -> Self {
        sort_collections(&mut collections);
        sort_items(&mut items);
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
        sort_collections(&mut self.collections);
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
        sort_items(&mut self.items);
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

#[cfg(test)]
mod tests;
