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
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Terminal,
};
use std::io::{self, Write};
use std::time::{Duration, Instant};
use zeroize::Zeroize;

const SECRET_TTL: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Collections,
    Items,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Search,
    Help,
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
            if handle_key(store, &mut terminal, &mut app, key).await? {
                break Ok(());
            }
        }
    };
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    result
}

async fn handle_key(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    key: KeyEvent,
) -> Result<bool> {
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
                    activate_detail_action(store, terminal, app).await?;
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
                Page::Collections => create_collection(store, terminal, app).await?,
                Page::Items => create_item(store, terminal, app).await?,
                Page::Details => {}
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

async fn activate_detail_action(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    match app.selected_action() {
        DetailAction::Reveal => reveal_selected(store, app).await?,
        DetailAction::Copy => copy_selected(store, app).await?,
        DetailAction::Edit => edit_selected(store, terminal, app).await?,
        DetailAction::Delete => delete_selected(store, terminal, app).await?,
        DetailAction::LockUnlock => lock_toggle(store, app).await?,
        DetailAction::Back => app.previous_page(),
    }
    Ok(())
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

async fn edit_selected(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    if let Some(item) = app.selected_item().cloned() {
        suspend_terminal(terminal)?;
        let label = prompt_default("label", &item.label)?;
        let attrs = prompt_attrs(&item.attributes)?;
        let change_secret = confirm("change secret? type yes: ")?;
        let secret = if change_secret {
            let mut secret = rpassword::prompt_password("secret: ")?.into_bytes();
            let content_type = prompt_default(
                "content_type",
                item.content_type.as_deref().unwrap_or("text/plain"),
            )?;
            let owned = (secret.clone(), content_type);
            secret.zeroize();
            Some(owned)
        } else {
            None
        };
        resume_terminal(terminal)?;
        let secret_ref = secret
            .as_ref()
            .map(|(secret, content_type)| (secret.as_slice(), content_type.as_str()));
        store
            .edit_item(&item.path, Some(&label), Some(attrs), secret_ref)
            .await?;
        app.refresh_items(store).await?;
        app.page = Page::Details;
        app.message = "item edited".into();
    }
    Ok(())
}

async fn create_collection(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    suspend_terminal(terminal)?;
    let label = prompt("collection label: ")?;
    let alias = prompt_default("alias", "")?;
    resume_terminal(terminal)?;
    let collection = store
        .create_collection(NewCollection { label, alias })
        .await?;
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

async fn create_item(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    if let Some(collection) = app.selected_collection().cloned() {
        suspend_terminal(terminal)?;
        let label = prompt("item label: ")?;
        let attrs = prompt_attrs(&Attributes::new())?;
        let mut secret = rpassword::prompt_password("secret: ")?.into_bytes();
        let content_type = prompt_default("content_type", "text/plain")?;
        resume_terminal(terminal)?;
        store
            .create_item(NewItem {
                collection_path: collection.path,
                label,
                attributes: attrs,
                secret: secret.clone(),
                content_type,
            })
            .await?;
        secret.zeroize();
        app.refresh_items(store).await?;
        app.page = Page::Items;
        app.message = "item created".into();
    }
    Ok(())
}

async fn delete_selected(
    store: &impl SecretStore,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    if let Some(item) = app.selected_item().cloned() {
        suspend_terminal(terminal)?;
        let ok = confirm(&format!("delete '{}' ? type yes: ", item.label))?;
        resume_terminal(terminal)?;
        if ok {
            store.delete_item(&item.path).await?;
            app.refresh_items(store).await?;
            app.page = Page::Items;
            app.message = "item deleted".into();
        }
    }
    Ok(())
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

fn suspend_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn resume_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut text = String::new();
    io::stdin().read_line(&mut text)?;
    Ok(text.trim().to_owned())
}

fn prompt_default(label: &str, current: &str) -> Result<String> {
    let text = prompt(&format!("{label} [{current}]: "))?;
    Ok(if text.is_empty() {
        current.to_owned()
    } else {
        text
    })
}

fn confirm(label: &str) -> Result<bool> {
    Ok(prompt(label)?.eq_ignore_ascii_case("yes"))
}

fn prompt_attrs(current: &Attributes) -> Result<Attributes> {
    println!("attributes as key=value lines; blank to finish; existing shown below");
    for (key, value) in current {
        println!("{key}={value}");
    }
    let mut attrs = Attributes::new();
    loop {
        let line = prompt("attr: ")?;
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once('=') {
            attrs.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    if attrs.is_empty() {
        Ok(current.clone())
    } else {
        Ok(attrs)
    }
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
}
