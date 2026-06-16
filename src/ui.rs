use crate::config::{parse_refresh_interval, Config, RuntimeConfig};
use crate::matchers::FuzzyMatcher;
use crate::models::Entry;
use crate::sources::{dockerhub_entry_description, dockerhub_entry_tags};
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use std::cmp::Ordering;
use std::io::{self, IsTerminal, Read};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const ROW_EVEN_BG: Color = Color::Rgb(8, 13, 20);
const ROW_ODD_BG: Color = Color::Rgb(18, 24, 32);
const SELECTED_ROW_BG: Color = Color::Rgb(53, 63, 73);

pub enum UiEvent {
    ReplaceSourceEntries {
        sources: Vec<String>,
        entries: Vec<Entry>,
    },
    Status(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshRequest {
    History,
    GitHub,
    GitLab,
    DockerHub,
}

pub fn run_ui(
    entries: Vec<Entry>,
    config: Config,
    runtime_config: RuntimeConfig,
    shared_config: Arc<Mutex<Config>>,
    events: Receiver<UiEvent>,
    refresh_requests: Sender<RefreshRequest>,
) -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("interactive terminal required".to_string());
    }

    let mut session = TerminalSession::enter()?;
    let mut app = AppState::new(entries, config, runtime_config, shared_config);
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|err| format!("create terminal: {err}"))?;
    let mut last_cursor_toggle = Instant::now();
    let mut input = InputReader::default();

    // 进入循环前先渲染一帧，避免启动时白屏
    terminal
        .draw(|frame| render(frame, &mut app))
        .map_err(|err| format!("draw terminal: {err}"))?;

    let result = loop {
        // 阻塞等待输入，或 0.5s 超时（用于光标闪烁）
        let first_key = input.read_key()?;

        // 更新光标闪烁状态
        if last_cursor_toggle.elapsed() >= CURSOR_BLINK_INTERVAL {
            app.cursor_visible = !app.cursor_visible;
            last_cursor_toggle = Instant::now();
        }

        // 处理后台刷新事件
        drain_events(&mut app, &events);

        if first_key.is_none() {
            // 超时：处理后台事件，重绘（光标闪烁）
            terminal
                .draw(|frame| render(frame, &mut app))
                .map_err(|err| format!("draw terminal: {err}"))?;
            let mut stdout = io::stdout();
            if app.cursor_visible {
                execute!(stdout, Show).map_err(|err| format!("show cursor: {err}"))?;
            } else {
                execute!(stdout, Hide).map_err(|err| format!("hide cursor: {err}"))?;
            }
            continue;
        }

        // 批量处理：先处理第一个键，再把缓冲区里已有的全部键一起处理
        // 这样粘贴一段文字时只触发一次 recompute
        app.cursor_visible = true;
        last_cursor_toggle = Instant::now();

        let mut should_quit = false;
        let mut key = first_key;
        loop {
            let Some(k) = key else { break };
            if handle_key(&mut app, k, &refresh_requests)? {
                should_quit = true;
                break;
            }
            key = input.next_buffered_key();
        }

        if should_quit {
            break Ok(());
        }

        // 所有按键处理完后统一 recompute 并重绘一次
        app.flush_deferred_recompute();
        drain_events(&mut app, &events);
        terminal
            .draw(|frame| render(frame, &mut app))
            .map_err(|err| format!("draw terminal: {err}"))?;
        let mut stdout = io::stdout();
        if app.cursor_visible {
            execute!(stdout, Show).map_err(|err| format!("show cursor: {err}"))?;
        } else {
            execute!(stdout, Hide).map_err(|err| format!("hide cursor: {err}"))?;
        }
    };

    session.restore();
    result
}

fn drain_events(app: &mut AppState, events: &Receiver<UiEvent>) {
    loop {
        match events.try_recv() {
            Ok(UiEvent::ReplaceSourceEntries { sources, entries }) => {
                let count = entries.len();
                app.replace_entries_for_sources(&sources, entries);
                app.message = format!("Browser data refreshed ({count} items).");
            }
            Ok(UiEvent::Status(message)) => {
                app.message = message;
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
}

pub fn best_entry<'a>(entries: &'a [Entry], query: &str) -> Option<&'a Entry> {
    rank_entries_all(entries, query)
        .into_iter()
        .next()
        .and_then(|(_, idx)| entries.get(idx))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputKey {
    code: InputCode,
    ctrl: bool,
}

impl InputKey {
    fn new(code: InputCode) -> Self {
        Self { code, ctrl: false }
    }

    fn ctrl(ch: char) -> Self {
        Self {
            code: InputCode::Char(ch),
            ctrl: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputCode {
    Esc,
    Enter,
    Backspace,
    Char(char),
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Tab,
}

#[derive(Default)]
struct InputReader {
    buffer: Vec<u8>,
}

impl InputReader {
    // 阻塞读：等待至少一个字节（stty min 1），超时返回 None（stty time N）
    fn read_key(&mut self) -> Result<Option<InputKey>, String> {
        let mut bytes = [0u8; 256];
        match io::stdin().read(&mut bytes) {
            Ok(0) => {}
            Ok(count) => self.buffer.extend_from_slice(&bytes[..count]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(format!("read input: {err}")),
        }
        Ok(parse_next_input_key(&mut self.buffer))
    }

    // 从内部缓冲区解析出下一个按键（不做任何 IO）
    fn next_buffered_key(&mut self) -> Option<InputKey> {
        parse_next_input_key(&mut self.buffer)
    }
}

fn parse_next_input_key(buffer: &mut Vec<u8>) -> Option<InputKey> {
    let first = *buffer.first()?;
    match first {
        b'\x1B' => parse_escape_input_key(buffer),
        b'\r' => {
            buffer.drain(..1);
            Some(InputKey::new(InputCode::Enter))
        }
        b'\n' => {
            buffer.drain(..1);
            Some(InputKey::ctrl('j'))
        }
        b'\t' => {
            buffer.drain(..1);
            Some(InputKey::new(InputCode::Tab))
        }
        b'\x7F' => {
            buffer.drain(..1);
            Some(InputKey::new(InputCode::Backspace))
        }
        byte @ b'\x01'..=b'\x1A' => {
            buffer.drain(..1);
            let ch = (byte - 1 + b'a') as char;
            Some(InputKey::ctrl(ch))
        }
        _ => parse_text_input_key(buffer),
    }
}

fn parse_escape_input_key(buffer: &mut Vec<u8>) -> Option<InputKey> {
    if buffer.len() >= 3 && buffer[1] == b'[' {
        let key = match buffer[2] {
            b'A' => Some((InputCode::Up, 3)),
            b'B' => Some((InputCode::Down, 3)),
            b'C' => Some((InputCode::Right, 3)),
            b'D' => Some((InputCode::Left, 3)),
            b'H' => Some((InputCode::Home, 3)),
            b'F' => Some((InputCode::End, 3)),
            b'1' | b'7' if buffer.get(3) == Some(&b'~') => Some((InputCode::Home, 4)),
            b'4' | b'8' if buffer.get(3) == Some(&b'~') => Some((InputCode::End, 4)),
            b'5' if buffer.get(3) == Some(&b'~') => Some((InputCode::PageUp, 4)),
            b'6' if buffer.get(3) == Some(&b'~') => Some((InputCode::PageDown, 4)),
            _ => None,
        };
        if let Some((code, consumed)) = key {
            buffer.drain(..consumed);
            return Some(InputKey::new(code));
        }
    }

    buffer.drain(..1);
    Some(InputKey::new(InputCode::Esc))
}

fn parse_text_input_key(buffer: &mut Vec<u8>) -> Option<InputKey> {
    let first = *buffer.first()?;
    let width = utf8_char_width(first);
    if width == 0 {
        buffer.drain(..1);
        return None;
    }
    if buffer.len() < width {
        return None;
    }

    let Ok(text) = std::str::from_utf8(&buffer[..width]) else {
        buffer.drain(..1);
        return None;
    };
    let ch = text.chars().next()?;
    buffer.drain(..width);
    Some(InputKey::new(InputCode::Char(ch)))
}

fn utf8_char_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    }
}

fn handle_key(
    app: &mut AppState,
    key: InputKey,
    refresh_requests: &Sender<RefreshRequest>,
) -> Result<bool, String> {
    let Some(action) = resolve_action(app.mode.kind(), key) else {
        return Ok(false);
    };
    apply_action(app, action, refresh_requests)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppModeKind {
    Normal,
    TagList,
    ActionMenu,
    RepoMenu,
    ConfigEdit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Quit,
    OpenSelected,
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    JumpTop,
    JumpBottom,
    PreviousTab,
    NextTab,
    ToggleConfigTab,
    RefreshCurrentTab,
    Backspace,
    InsertChar(char),
    BackToNormal,
    SelectTag,
    BackToTags,
    ConfirmDockerAction,
    ConfirmRepoAction,
    CommitConfigEdit,
    CancelConfigEdit,
}

fn resolve_action(mode: AppModeKind, key: InputKey) -> Option<Action> {
    match mode {
        AppModeKind::Normal => resolve_normal_action(key),
        AppModeKind::TagList => resolve_tag_list_action(key),
        AppModeKind::ActionMenu => resolve_action_menu_action(key),
        AppModeKind::RepoMenu => resolve_repo_menu_action(key),
        AppModeKind::ConfigEdit => resolve_config_edit_action(key),
    }
}

fn resolve_normal_action(key: InputKey) -> Option<Action> {
    match key.code {
        InputCode::Esc => Some(Action::Quit),
        InputCode::Enter => Some(Action::OpenSelected),
        InputCode::Backspace => Some(Action::Backspace),
        InputCode::Char('u') if key.ctrl => Some(Action::PageUp),
        InputCode::Char('d') if key.ctrl => Some(Action::PageDown),
        InputCode::Char('r') if key.ctrl => Some(Action::RefreshCurrentTab),
        InputCode::Char('b') if key.ctrl => Some(Action::ToggleConfigTab),
        InputCode::Char('j') | InputCode::Char('n') if key.ctrl => Some(Action::MoveDown),
        InputCode::Char('k') | InputCode::Char('p') if key.ctrl => Some(Action::MoveUp),
        InputCode::Char('h') if key.ctrl => Some(Action::PreviousTab),
        InputCode::Char('l') if key.ctrl => Some(Action::NextTab),
        InputCode::Char(c) if !key.ctrl => Some(Action::InsertChar(c)),
        InputCode::Up => Some(Action::MoveUp),
        InputCode::Down => Some(Action::MoveDown),
        InputCode::PageUp => Some(Action::PageUp),
        InputCode::PageDown => Some(Action::PageDown),
        InputCode::Home => Some(Action::JumpTop),
        InputCode::End => Some(Action::JumpBottom),
        InputCode::Left => Some(Action::PreviousTab),
        InputCode::Right | InputCode::Tab => Some(Action::NextTab),
        _ => None,
    }
}

fn resolve_tag_list_action(key: InputKey) -> Option<Action> {
    match key.code {
        InputCode::Esc | InputCode::Backspace => Some(Action::BackToNormal),
        InputCode::Enter => Some(Action::SelectTag),
        InputCode::Up | InputCode::Char('k') if !key.ctrl => Some(Action::MoveUp),
        InputCode::Down | InputCode::Char('j') if !key.ctrl => Some(Action::MoveDown),
        InputCode::Char('p') | InputCode::Char('k') if key.ctrl => Some(Action::MoveUp),
        InputCode::Char('n') | InputCode::Char('j') if key.ctrl => Some(Action::MoveDown),
        InputCode::PageUp => Some(Action::PageUp),
        InputCode::PageDown => Some(Action::PageDown),
        InputCode::Char('u') if key.ctrl => Some(Action::PageUp),
        InputCode::Char('d') if key.ctrl => Some(Action::PageDown),
        InputCode::Left => Some(Action::PreviousTab),
        InputCode::Right | InputCode::Tab => Some(Action::NextTab),
        InputCode::Char('h') if key.ctrl => Some(Action::PreviousTab),
        InputCode::Char('l') if key.ctrl => Some(Action::NextTab),
        _ => None,
    }
}

fn resolve_action_menu_action(key: InputKey) -> Option<Action> {
    match key.code {
        InputCode::Esc | InputCode::Backspace => Some(Action::BackToTags),
        InputCode::Enter => Some(Action::ConfirmDockerAction),
        InputCode::Up | InputCode::Char('k') if !key.ctrl => Some(Action::MoveUp),
        InputCode::Down | InputCode::Char('j') if !key.ctrl => Some(Action::MoveDown),
        InputCode::Char('p') | InputCode::Char('k') if key.ctrl => Some(Action::MoveUp),
        InputCode::Char('n') | InputCode::Char('j') if key.ctrl => Some(Action::MoveDown),
        InputCode::PageUp => Some(Action::PageUp),
        InputCode::PageDown => Some(Action::PageDown),
        InputCode::Char('u') if key.ctrl => Some(Action::PageUp),
        InputCode::Char('d') if key.ctrl => Some(Action::PageDown),
        InputCode::Left => Some(Action::PreviousTab),
        InputCode::Right | InputCode::Tab => Some(Action::NextTab),
        InputCode::Char('h') if key.ctrl => Some(Action::PreviousTab),
        InputCode::Char('l') if key.ctrl => Some(Action::NextTab),
        _ => None,
    }
}

fn resolve_repo_menu_action(key: InputKey) -> Option<Action> {
    match key.code {
        InputCode::Esc | InputCode::Backspace => Some(Action::BackToNormal),
        InputCode::Enter => Some(Action::ConfirmRepoAction),
        InputCode::Up | InputCode::Char('k') if !key.ctrl => Some(Action::MoveUp),
        InputCode::Down | InputCode::Char('j') if !key.ctrl => Some(Action::MoveDown),
        InputCode::Char('p') | InputCode::Char('k') if key.ctrl => Some(Action::MoveUp),
        InputCode::Char('n') | InputCode::Char('j') if key.ctrl => Some(Action::MoveDown),
        InputCode::PageUp => Some(Action::PageUp),
        InputCode::PageDown => Some(Action::PageDown),
        InputCode::Char('u') if key.ctrl => Some(Action::PageUp),
        InputCode::Char('d') if key.ctrl => Some(Action::PageDown),
        InputCode::Left => Some(Action::PreviousTab),
        InputCode::Right | InputCode::Tab => Some(Action::NextTab),
        InputCode::Char('h') if key.ctrl => Some(Action::PreviousTab),
        InputCode::Char('l') if key.ctrl => Some(Action::NextTab),
        _ => None,
    }
}

fn resolve_config_edit_action(key: InputKey) -> Option<Action> {
    match key.code {
        InputCode::Esc => Some(Action::CancelConfigEdit),
        InputCode::Enter => Some(Action::CommitConfigEdit),
        InputCode::Backspace => Some(Action::Backspace),
        InputCode::Char(c) if !key.ctrl => Some(Action::InsertChar(c)),
        _ => None,
    }
}

fn apply_action(
    app: &mut AppState,
    action: Action,
    refresh_requests: &Sender<RefreshRequest>,
) -> Result<bool, String> {
    match action {
        Action::Quit => return Ok(true),
        Action::OpenSelected if app.tab == Tab::Config => start_config_edit(app),
        Action::OpenSelected => open_selected_entry(app)?,
        Action::MoveUp => move_selection_up(app),
        Action::MoveDown => move_selection_down(app),
        Action::PageUp => page_selection_up(app),
        Action::PageDown => page_selection_down(app),
        Action::JumpTop => app.jump_top(),
        Action::JumpBottom => app.jump_bottom(),
        Action::PreviousTab => {
            app.mode = AppMode::Normal;
            app.previous_tab();
        }
        Action::NextTab => {
            app.mode = AppMode::Normal;
            app.next_tab();
        }
        Action::ToggleConfigTab => app.toggle_config_tab(),
        Action::RefreshCurrentTab => {
            if let Some(request) = app.tab.refresh_request() {
                let _ = refresh_requests.send(request);
                app.message = format!("Requested {} refresh.", app.tab.name());
            } else {
                app.message = format!("{} tab has no refresh.", app.tab.name());
            }
        }
        Action::Backspace if matches!(app.mode, AppMode::ConfigEdit { .. }) => {
            app.pop_config_edit_char()
        }
        Action::Backspace => app.backspace(),
        Action::InsertChar(ch) if matches!(app.mode, AppMode::ConfigEdit { .. }) => {
            app.push_config_edit_char(ch)
        }
        Action::InsertChar(ch) => app.push_char(ch),
        Action::BackToNormal => {
            app.mode = AppMode::Normal;
            app.message = "Type to search.".to_string();
        }
        Action::SelectTag => select_docker_tag(app),
        Action::BackToTags => back_to_docker_tags(app),
        Action::ConfirmDockerAction => confirm_docker_action(app),
        Action::ConfirmRepoAction => confirm_repo_action(app)?,
        Action::CommitConfigEdit => commit_config_edit(app),
        Action::CancelConfigEdit => cancel_config_edit(app),
    }
    Ok(false)
}

fn open_selected_entry(app: &mut AppState) -> Result<(), String> {
    if let Some(entry) = app.selected_entry() {
        if entry.source == "github" || entry.source == "gitlab" {
            app.mode = AppMode::RepoMenu { selected: 0 };
            return Ok(());
        }
        let is_dockerhub = entry.source == "public" || entry.source == "private";
        if is_dockerhub {
            let repo = entry.title.clone();
            let tags = dockerhub_entry_tags(entry);
            if tags.is_empty() {
                app.mode = AppMode::ActionMenu {
                    repo,
                    tag: None,
                    tags,
                    selected: 0,
                };
            } else {
                app.mode = AppMode::TagList {
                    repo,
                    tags,
                    selected: 0,
                };
            }
            return Ok(());
        }

        let title = entry.title.clone();
        open_entry(entry)?;
        app.message = format!("Opened {title}");
    }
    Ok(())
}

fn start_config_edit(app: &mut AppState) {
    let Some(item_index) = app.selected_config_item_index() else {
        app.message = "Select an indented config item to edit.".to_string();
        return;
    };
    let edit_kind = app.config_items[item_index].edit_kind;
    match edit_kind {
        ConfigEditKind::Toggle => {
            let (level1, level2, level3, enabled) = {
                let item = &app.config_items[item_index];
                (
                    item.level1.clone(),
                    item.level2.clone(),
                    item.level3.clone(),
                    item.current != "enabled",
                )
            };
            if let Err(err) = apply_config_toggle(app, &level1, &level2, &level3, enabled) {
                app.message = err;
                return;
            }
            app.message = format!("Updated {} / {} / {}.", level1, level2, level3);
            app.recompute();
        }
        ConfigEditKind::Text => {
            let current = app.config_items[item_index].current.clone();
            let buffer = if current == "not set" {
                String::new()
            } else {
                current
            };
            app.mode = AppMode::ConfigEdit { item_index, buffer };
        }
        ConfigEditKind::Secret => {
            app.mode = AppMode::ConfigEdit {
                item_index,
                buffer: String::new(),
            };
            app.message = "Enter a token value; it will only be shown as present.".to_string();
        }
        ConfigEditKind::ReadOnly => {
            app.message = "This config item is informational.".to_string();
        }
    }
}

fn commit_config_edit(app: &mut AppState) {
    let AppMode::ConfigEdit { item_index, buffer } = &app.mode else {
        return;
    };
    let item_index = *item_index;
    let buffer = buffer.trim().to_string();
    let Some(item) = app.config_items.get(item_index) else {
        app.mode = AppMode::Normal;
        return;
    };
    let level1 = item.level1.clone();
    let level2 = item.level2.clone();
    let level3 = item.level3.clone();
    let edit_kind = item.edit_kind;

    let result = match edit_kind {
        ConfigEditKind::Text | ConfigEditKind::Secret => {
            apply_config_value(app, &level1, &level2, &level3, &buffer)
        }
        ConfigEditKind::Toggle | ConfigEditKind::ReadOnly => Ok(()),
    };
    if let Err(err) = result {
        app.message = err;
        return;
    }
    app.message = format!("Updated {} / {} / {}.", level1, level2, level3);
    app.mode = AppMode::Normal;
}

fn apply_config_toggle(
    app: &mut AppState,
    level1: &str,
    level2: &str,
    level3: &str,
    enabled: bool,
) -> Result<(), String> {
    match (level1, level2, level3) {
        ("Browser", "Source", "Enabled")
        | ("GitHub", "Source", "Enabled")
        | ("GitLab", "Source", "Enabled")
        | ("DockerHub", "Source", "Enabled") => app.set_source_enabled(level1, enabled),
        ("System", "Display", "Preview pane") => app.config.preview_enabled = enabled,
        ("System", "Diagnostics", "Debug logging") => app.config.debug = enabled,
        _ => {}
    }
    app.sync_config_items();
    app.persist_config()
}

fn apply_config_value(
    app: &mut AppState,
    level1: &str,
    level2: &str,
    level3: &str,
    value: &str,
) -> Result<(), String> {
    match (level1, level2, level3) {
        ("GitHub", "Auth", "Token") => {
            app.config.github_token = optional_config_value(value);
        }
        ("GitHub", "Auth", "User") => {
            app.config.github_user = optional_config_value(value);
        }
        ("GitLab", "Auth", "Token") => {
            app.config.gitlab_token = optional_config_value(value);
        }
        ("DockerHub", "Auth", "Token") => {
            app.config.dockerhub_token = optional_config_value(value);
        }
        ("DockerHub", "Auth", "Username") => {
            app.config.dockerhub_username = optional_config_value(value);
        }
        ("GitHub", "API", "Base URL") => {
            app.config.github_api = value.trim().to_string();
        }
        ("GitLab", "API", "Base URL") => {
            app.config.gitlab_api = value.trim().to_string();
        }
        ("Browser", "Refresh", "Interval") => {
            app.config.history_refresh_interval = parse_refresh_interval(value)
                .ok_or_else(|| "Invalid browser refresh interval.".to_string())?;
        }
        ("GitHub", "Refresh", "Interval") => {
            app.config.github_refresh_interval = parse_refresh_interval(value)
                .ok_or_else(|| "Invalid GitHub refresh interval.".to_string())?;
        }
        ("GitLab", "Refresh", "Interval") => {
            app.config.gitlab_refresh_interval = parse_refresh_interval(value)
                .ok_or_else(|| "Invalid GitLab refresh interval.".to_string())?;
        }
        ("DockerHub", "Refresh", "Interval") => {
            app.config.dockerhub_refresh_interval = parse_refresh_interval(value)
                .ok_or_else(|| "Invalid DockerHub refresh interval.".to_string())?;
        }
        _ => {}
    }
    app.sync_config_items();
    app.persist_config()
}

fn optional_config_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn cancel_config_edit(app: &mut AppState) {
    app.mode = AppMode::Normal;
    app.message = "Config edit cancelled.".to_string();
}

fn move_selection_up(app: &mut AppState) {
    match app.mode {
        AppMode::Normal if app.tab == Tab::Config => app.move_config_up(),
        AppMode::Normal => app.move_up(),
        AppMode::TagList {
            ref mut selected, ..
        }
        | AppMode::ActionMenu {
            ref mut selected, ..
        }
        | AppMode::RepoMenu {
            ref mut selected, ..
        } => {
            if *selected > 0 {
                *selected -= 1;
            }
        }
        AppMode::ConfigEdit { .. } => {}
    }
}

fn move_selection_down(app: &mut AppState) {
    let repo_source = if matches!(app.mode, AppMode::RepoMenu { .. }) {
        app.selected_entry()
            .map(|e| e.source.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    match app.mode {
        AppMode::Normal if app.tab == Tab::Config => app.move_config_down(),
        AppMode::Normal => app.move_down(),
        AppMode::TagList {
            ref tags,
            ref mut selected,
            ..
        } => {
            if *selected + 1 < tags.len() {
                *selected += 1;
            }
        }
        AppMode::ActionMenu {
            ref mut selected, ..
        } => {
            let limit = ACTION_LABELS.len();
            if *selected + 1 < limit {
                *selected += 1;
            }
        }
        AppMode::RepoMenu {
            ref mut selected, ..
        } => {
            let limit = repo_action_labels(&repo_source).len();
            if *selected + 1 < limit {
                *selected += 1;
            }
        }
        AppMode::ConfigEdit { .. } => {}
    }
}

fn page_selection_up(app: &mut AppState) {
    match app.mode {
        AppMode::Normal if app.tab == Tab::Config => app.page_config_up(),
        AppMode::Normal => app.page_up(),
        AppMode::TagList {
            ref mut selected, ..
        }
        | AppMode::ActionMenu {
            ref mut selected, ..
        }
        | AppMode::RepoMenu {
            ref mut selected, ..
        } => {
            *selected = selected.saturating_sub(10);
        }
        AppMode::ConfigEdit { .. } => {}
    }
}

fn page_selection_down(app: &mut AppState) {
    let repo_source = if matches!(app.mode, AppMode::RepoMenu { .. }) {
        app.selected_entry()
            .map(|e| e.source.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    match app.mode {
        AppMode::Normal if app.tab == Tab::Config => app.page_config_down(),
        AppMode::Normal => app.page_down(),
        AppMode::TagList {
            ref tags,
            ref mut selected,
            ..
        } => {
            if !tags.is_empty() {
                *selected = (*selected + 10).min(tags.len() - 1);
            }
        }
        AppMode::ActionMenu {
            ref mut selected, ..
        } => {
            let limit = ACTION_LABELS.len();
            if limit > 0 {
                *selected = (*selected + 10).min(limit - 1);
            }
        }
        AppMode::RepoMenu {
            ref mut selected, ..
        } => {
            let limit = repo_action_labels(&repo_source).len();
            if limit > 0 {
                *selected = (*selected + 10).min(limit - 1);
            }
        }
        AppMode::ConfigEdit { .. } => {}
    }
}

fn select_docker_tag(app: &mut AppState) {
    let AppMode::TagList {
        ref repo,
        ref tags,
        selected,
    } = app.mode
    else {
        return;
    };

    let tag = tags[selected].clone();
    let repo = repo.clone();
    let tags = tags.clone();
    app.mode = AppMode::ActionMenu {
        repo,
        tag: Some(tag),
        tags,
        selected: 0,
    };
}

fn back_to_docker_tags(app: &mut AppState) {
    let AppMode::ActionMenu {
        ref repo, ref tags, ..
    } = app.mode
    else {
        return;
    };

    if tags.is_empty() {
        app.mode = AppMode::Normal;
        app.message = "Type to search.".to_string();
        return;
    }

    let repo = repo.clone();
    let tags = tags.clone();
    app.mode = AppMode::TagList {
        repo,
        tags,
        selected: 0,
    };
}

const ACTION_LABELS: [&str; 2] = ["Open in browser", "Copy docker pull command"];

fn confirm_docker_action(app: &mut AppState) {
    let AppMode::ActionMenu {
        ref repo,
        ref tag,
        selected,
        ..
    } = app.mode
    else {
        return;
    };

    let action = selected;
    let repo = repo.clone();
    let tag = tag.clone();
    app.mode = AppMode::Normal;
    match action {
        0 => {
            let url = match &tag {
                Some(tag) => format!("https://hub.docker.com/r/{}/tags?name={}", repo, tag),
                None => format!("https://hub.docker.com/r/{repo}"),
            };
            let _ = webbrowser::open(&url);
            app.message = match tag {
                Some(tag) => format!("Opened {repo}:{tag} in browser"),
                None => format!("Opened {repo} in browser"),
            };
        }
        1 => {
            let cmd = match tag {
                Some(tag) => format!("docker pull {}:{}", repo, tag),
                None => format!("docker pull {repo}"),
            };
            copy_to_clipboard(&cmd);
            app.message = format!("Copied: {cmd}");
        }
        _ => {}
    }
}

const REPO_ACTION_LABELS: [&str; 3] = ["Open in browser", "Copy HTTPS address", "Copy SSH address"];

const GITHUB_ACTION_LABELS: [&str; 4] = [
    "Open in browser",
    "Copy HTTPS address",
    "Copy SSH address",
    "Copy gh CLI command",
];

fn repo_action_labels(source: &str) -> &'static [&'static str] {
    if source == "github" {
        &GITHUB_ACTION_LABELS
    } else {
        &REPO_ACTION_LABELS
    }
}

fn repo_ssh_url(https_url: &str) -> Option<String> {
    let url = https_url.trim_end_matches('/');
    let without_scheme = url.strip_prefix("https://")?;
    let slash = without_scheme.find('/')?;
    let host = &without_scheme[..slash];
    let path = without_scheme[slash + 1..].trim_end_matches(".git");
    Some(format!("git@{}:{}.git", host, path))
}

fn confirm_repo_action(app: &mut AppState) -> Result<(), String> {
    let AppMode::RepoMenu { selected } = app.mode else {
        return Ok(());
    };

    let Some(entry) = app.selected_entry().cloned() else {
        return Ok(());
    };

    app.mode = AppMode::Normal;
    match selected {
        0 => {
            open_entry(&entry)?;
            app.message = format!("Opened {}", entry.title);
        }
        1 => {
            copy_to_clipboard(&entry.url);
            app.message = format!("Copied HTTPS: {}", entry.url);
        }
        2 => {
            let ssh = repo_ssh_url(&entry.url).unwrap_or_else(|| entry.url.clone());
            copy_to_clipboard(&ssh);
            app.message = format!("Copied SSH: {ssh}");
        }
        3 if entry.source == "github" => {
            let cmd = format!("gh repo clone {}", entry.title);
            copy_to_clipboard(&cmd);
            app.message = format!("Copied: {cmd}");
        }
        _ => {}
    }
    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    let size = frame.area();
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(2),
    ])
    .split(size);

    render_header(frame, layout[0], app);
    render_search(frame, layout[1], app);

    match app.mode.kind() {
        AppModeKind::TagList => {
            if let AppMode::TagList {
                repo,
                tags,
                selected,
            } = &app.mode
            {
                render_tag_list(frame, layout[2], repo, tags, *selected);
            }
        }
        AppModeKind::ActionMenu => {
            if app.selected_entry().is_some() {
                let body =
                    Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
                        .split(layout[2]);
                if let AppMode::ActionMenu {
                    repo,
                    tag,
                    tags: _,
                    selected,
                } = &app.mode
                {
                    render_action_menu(
                        frame,
                        body[0],
                        &format!(
                            "Action: {repo}{}",
                            tag.as_ref()
                                .map(|tag| format!(":{tag}"))
                                .unwrap_or_default()
                        ),
                        &ACTION_LABELS,
                        *selected,
                    );
                }
                render_preview(frame, body[1], app);
            }
        }
        AppModeKind::RepoMenu => {
            if let Some(entry) = app.selected_entry() {
                let body =
                    Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
                        .split(layout[2]);
                let labels = repo_action_labels(&entry.source.clone());
                render_action_menu(
                    frame,
                    body[0],
                    &format!("Action: {}", entry.title),
                    labels,
                    app.repo_menu_selected(),
                );
                render_preview(frame, body[1], app);
            }
        }
        AppModeKind::ConfigEdit => {
            render_config(frame, layout[2], app);
            render_config_editor(frame, size, app);
        }
        AppModeKind::Normal => {
            if app.tab == Tab::Config {
                render_config(frame, layout[2], app);
            } else if app.config.preview_enabled {
                let body =
                    Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
                        .split(layout[2]);
                render_results(frame, body[0], app);
                render_preview(frame, body[1], app);
            } else {
                render_results(frame, layout[2], app);
            }
        }
    }
    render_footer(frame, layout[3], app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let header = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    let title = Line::from(vec![
        Span::styled(
            " uf ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{} results", app.visible.len()),
            Style::default().fg(Color::Gray),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(title).style(Style::default()).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        header[0],
    );

    let tabs: Vec<Line<'static>> = app
        .visible_tabs()
        .iter()
        .map(|tab| tab_label_for(*tab, &app.entries))
        .collect();

    let selected_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let tabs_widget = Tabs::new(tabs)
        .select(app.selected_tab_index())
        .block(
            Block::default()
                .borders(Borders::NONE)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::Gray))
        .highlight_style(selected_style);

    frame.render_widget(tabs_widget, header[1]);
}

fn render_search(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Search ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let content = if app.query.is_empty() {
        Span::styled(" type to search ", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(app.query.as_str(), Style::default().fg(Color::White))
    };

    frame.render_widget(Paragraph::new(Line::from(content)).block(block), area);

    let query_width = display_width(&app.query) as u16;
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(query_width.min(area.width.saturating_sub(2)));
    let cursor_y = area.y.saturating_add(1);
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn pad_or_truncate(text: &str, width: usize) -> String {
    let w = display_width(text);
    if w >= width {
        truncate_to_width(text, width)
    } else {
        let mut s = text.to_string();
        for _ in 0..width - w {
            s.push(' ');
        }
        s
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width + 1 > max_width {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out.push('…');
    out
}

fn render_results(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Results ",
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if app.entries.is_empty() {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new("No entries loaded. Check browser data, GitHub/GitLab credentials, or network access.")
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    // inner width minus highlight symbol "❯ " (2 chars)
    let inner_width = area.width.saturating_sub(2 + 2) as usize;
    let type_width = 14usize;
    let sep = 2usize; // spaces between columns
    let repo_no_description = app.tab == Tab::GitHub
        || app.tab == Tab::GitLab
        || app.tab == Tab::DockerHub
        || app.tab == Tab::History;
    let (name_width, desc_width) = if repo_no_description {
        (
            inner_width.saturating_sub(type_width).saturating_sub(sep),
            0,
        )
    } else {
        let name_width = (inner_width / 2).min(40).max(10);
        let desc_width = inner_width
            .saturating_sub(name_width)
            .saturating_sub(type_width)
            .saturating_sub(sep * 2);
        (name_width, desc_width)
    };

    let height = area.height.saturating_sub(2) as usize;
    app.set_result_viewport_height(height);
    let visible = app.visible_rows();
    let selected_in_window = app.selected_in_window();
    let items: Vec<ListItem> = if visible.is_empty() {
        let text = if app.query.is_empty() {
            "Type to search."
        } else {
            "No matches."
        };
        vec![ListItem::new(text)]
    } else {
        visible
            .into_iter()
            .enumerate()
            .map(|(row, idx)| {
                let entry = &app.entries[idx];
                let name_col = pad_or_truncate(&entry.title, name_width);
                let type_col = pad_or_truncate(&entry.source, type_width);
                let desc_col = if desc_width == 0 {
                    String::new()
                } else {
                    truncate_to_width(entry_detail(entry), desc_width)
                };
                let row_style = Style::default().bg(if row % 2 == 0 {
                    ROW_EVEN_BG
                } else {
                    ROW_ODD_BG
                });
                let spans = vec![
                    Span::styled(
                        name_col,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(type_col, Style::default().fg(source_color(&entry.source))),
                    if desc_width == 0 {
                        Span::raw("")
                    } else {
                        Span::raw("  ")
                    },
                    if desc_width == 0 {
                        Span::raw("")
                    } else {
                        Span::styled(desc_col, Style::default().fg(Color::Gray))
                    },
                ];
                ListItem::new(Line::from(spans)).style(row_style)
            })
            .collect()
    };

    let mut state = ListState::default();
    state.select(Some(selected_in_window));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(SELECTED_ROW_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_preview(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Preview ",
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let lines = if let Some(entry) = app.selected_entry() {
        vec![
            Line::from(vec![
                Span::styled("Title", Style::default().fg(Color::Cyan)),
                Span::raw(": "),
                Span::styled(&entry.title, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("URL", Style::default().fg(Color::Cyan)),
                Span::raw(": "),
                Span::styled(&entry.url, Style::default().fg(Color::Blue)),
            ]),
            Line::from(vec![
                Span::styled("Source", Style::default().fg(Color::Cyan)),
                Span::raw(": "),
                Span::styled(
                    &entry.source,
                    Style::default().fg(source_color(&entry.source)),
                ),
            ]),
            Line::from(vec![
                Span::styled("Detail", Style::default().fg(Color::Cyan)),
                Span::raw(": "),
                Span::styled(entry_detail(entry), Style::default().fg(Color::Gray)),
            ]),
        ]
    } else {
        vec![Line::from(Span::styled(
            "Select an item to preview it here.",
            Style::default().fg(Color::DarkGray),
        ))]
    };

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let help = match &app.mode {
        AppMode::TagList { .. } => "Up/Down=move  Ctrl+U/D=page  Enter=select  Backspace=back",
        AppMode::ActionMenu { .. } | AppMode::RepoMenu { .. } => {
            "Up/Down=move  Ctrl+U/D=page  Enter=confirm  Backspace=back"
        }
        AppMode::ConfigEdit { .. } => "Type=value  Enter=save  Esc=cancel  Backspace=delete",
        AppMode::Normal => {
            if app.tab == Tab::Config {
                "Up/Down=select  Enter=edit/toggle  Type=filter  Ctrl+B=hide Config  Esc=quit"
            } else if app.query.is_empty() {
                "Enter=open  Ctrl+U/D=page  Ctrl+R=refresh tab  Ctrl+B=toggle Config  Esc=quit"
            } else {
                "Type=fuzzy filter  Ctrl+U/D=page  Enter=open  Ctrl+B=toggle Config  Esc=quit"
            }
        }
    };
    let available = area.width.saturating_sub(2) as usize;
    let message = truncate_to_width(app.message.as_str(), available);
    let help = truncate_to_width(help, available.saturating_sub(display_width(&message) + 2));

    let text = Line::from(vec![
        Span::styled(message, Style::default().fg(Color::Gray)),
        Span::raw("  "),
        Span::styled(help, Style::default().fg(Color::DarkGray)),
    ]);

    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

fn render_tag_list(
    frame: &mut Frame<'_>,
    area: Rect,
    repo: &str,
    tags: &[String],
    selected: usize,
) {
    let block = Block::default()
        .title(Span::styled(
            format!(" Tags: {repo} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let height = area.height.saturating_sub(2) as usize;
    let start = selected
        .saturating_sub(5)
        .min(tags.len().saturating_sub(height));
    let end = (start + height).min(tags.len());
    let selected_in_window = selected.saturating_sub(start);

    let items: Vec<ListItem> = tags[start..end]
        .iter()
        .map(|tag| {
            ListItem::new(Span::styled(
                tag.as_str(),
                Style::default().fg(Color::White),
            ))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected_in_window));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_action_menu(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    labels: &[&str],
    selected: usize,
) {
    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let items: Vec<ListItem> = labels
        .iter()
        .map(|label| ListItem::new(Span::styled(*label, Style::default().fg(Color::White))))
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_config(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Config: Source / Feature / Setting ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner_width = area.width.saturating_sub(2) as usize;
    let level1_width = (inner_width / 7).clamp(12, 18);
    let level3_width = (inner_width / 6).clamp(10, 18);
    let value_width = (inner_width / 7).clamp(9, 18);
    let detail_width = inner_width
        .saturating_sub(level1_width)
        .saturating_sub(level3_width)
        .saturating_sub(value_width)
        .saturating_sub(6);

    let rows = config_display_rows(&app.config_items, &app.query);
    let height = area.height.saturating_sub(2) as usize;
    let row_count = rows.len();
    let selected = app.selected.min(row_count.saturating_sub(1));
    let start = selected
        .saturating_sub(height.saturating_sub(1))
        .min(row_count.saturating_sub(height));
    let selected_in_window = selected.saturating_sub(start);
    let items: Vec<ListItem> = if rows.is_empty() {
        let text = if app.query.is_empty() {
            "No config items."
        } else {
            "No matching config items."
        };
        vec![ListItem::new(text)]
    } else {
        rows.into_iter()
            .skip(start)
            .take(height.max(1))
            .enumerate()
            .map(|(row, idx)| {
                let row_style = Style::default().bg(if row % 2 == 0 {
                    ROW_EVEN_BG
                } else {
                    ROW_ODD_BG
                });
                match idx {
                    ConfigDisplayRow::Group(group) => ListItem::new(Line::from(vec![
                        Span::styled(
                            pad_or_truncate(&format!("▼ {group}"), level1_width),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            "contains configurable items",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                    .style(row_style),
                    ConfigDisplayRow::Item(idx) => {
                        let item = &app.config_items[idx];
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                pad_or_truncate(&format!("    {}", item.level2), level1_width),
                                Style::default().fg(Color::Yellow),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                pad_or_truncate(&item.level3, level3_width),
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                pad_or_truncate(&item.current, value_width),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                truncate_to_width(&item.detail, detail_width),
                                Style::default().fg(Color::Gray),
                            ),
                        ]))
                        .style(row_style)
                    }
                }
            })
            .collect()
    };

    let mut state = ListState::default();
    if row_count > 0 {
        state.select(Some(selected_in_window));
    }

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(SELECTED_ROW_BG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_config_editor(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let AppMode::ConfigEdit { item_index, buffer } = &app.mode else {
        return;
    };
    let Some(item) = app.config_items.get(*item_index) else {
        return;
    };

    let width = area.width.min(90).max(40);
    let height = 7;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect::new(x, y, width, height);
    let value = if item.edit_kind == ConfigEditKind::Secret {
        "*".repeat(buffer.chars().count())
    } else {
        buffer.clone()
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Path", Style::default().fg(Color::Cyan)),
            Span::raw(": "),
            Span::styled(
                format!("{} / {} / {}", item.level1, item.level2, item.level3),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Value", Style::default().fg(Color::Cyan)),
            Span::raw(": "),
            Span::styled(value, Style::default().fg(Color::White)),
        ]),
        Line::from(Span::styled(
            "Enter=save  Esc=cancel  Backspace=delete",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(Span::styled(
                        " Edit Config ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn source_color(source: &str) -> Color {
    match source {
        "github" => Color::Magenta,
        "gitlab" => Color::Yellow,
        "bookmark" => Color::Blue,
        "public" => Color::Cyan,
        "private" => Color::Red,
        _ => Color::Green,
    }
}

fn is_browser_entry(entry: &Entry) -> bool {
    entry.source == "history" || entry.source == "browser-history" || entry.source == "bookmark"
}

fn entry_detail(entry: &Entry) -> &str {
    if entry.source == "public" || entry.source == "private" {
        dockerhub_entry_description(entry)
    } else {
        &entry.detail
    }
}

fn tab_label(name: &str, count: usize, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            name.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(format!("({count})"), Style::default().fg(Color::DarkGray)),
    ])
}

fn tab_label_for(tab: Tab, entries: &[Entry]) -> Line<'static> {
    match tab {
        Tab::History => tab_label(
            "History",
            entries.iter().filter(|e| is_browser_entry(e)).count(),
            Color::Green,
        ),
        Tab::GitHub => tab_label(
            "GitHub",
            entries.iter().filter(|e| e.source == "github").count(),
            Color::Magenta,
        ),
        Tab::GitLab => tab_label(
            "GitLab",
            entries.iter().filter(|e| e.source == "gitlab").count(),
            Color::Yellow,
        ),
        Tab::DockerHub => tab_label("DockerHub", tab_count(entries, Tab::DockerHub), Color::Blue),
        Tab::Config => tab_label("Config", 0, Color::Cyan),
    }
}

fn tab_count(entries: &[Entry], tab: Tab) -> usize {
    entries.iter().filter(|entry| tab.matches(entry)).count()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    History,
    GitHub,
    GitLab,
    DockerHub,
    Config,
}

const BASE_TABS: [Tab; 4] = [Tab::History, Tab::GitHub, Tab::GitLab, Tab::DockerHub];

impl Tab {
    fn matches(self, entry: &Entry) -> bool {
        match self {
            Tab::History => is_browser_entry(entry),
            Tab::GitHub => entry.source == "github",
            Tab::GitLab => entry.source == "gitlab",
            Tab::DockerHub => entry.source == "public" || entry.source == "private",
            Tab::Config => false,
        }
    }

    fn refresh_request(self) -> Option<RefreshRequest> {
        match self {
            Tab::History => Some(RefreshRequest::History),
            Tab::GitHub => Some(RefreshRequest::GitHub),
            Tab::GitLab => Some(RefreshRequest::GitLab),
            Tab::DockerHub => Some(RefreshRequest::DockerHub),
            Tab::Config => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::GitHub => "GitHub",
            Tab::GitLab => "GitLab",
            Tab::DockerHub => "DockerHub",
            Tab::Config => "Config",
        }
    }
}

fn open_entry(entry: &Entry) -> Result<(), String> {
    if webbrowser::open(&entry.url).is_ok() {
        return Ok(());
    }

    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    Command::new(command)
        .arg(&entry.url)
        .status()
        .map_err(|err| format!("open browser: {err}"))?;

    Ok(())
}

fn copy_to_clipboard(text: &str) {
    let command = if cfg!(target_os = "macos") {
        "pbcopy"
    } else {
        "xclip"
    };
    let mut child = Command::new(command)
        .stdin(std::process::Stdio::piped())
        .spawn();
    if let Ok(ref mut c) = child {
        if let Some(mut stdin) = c.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = c.wait();
    }
}

fn rank_entries(
    entries: &[Entry],
    haystacks: &[String],
    query: &str,
    tab: Tab,
) -> Vec<(i64, usize)> {
    let mut matcher = FuzzyMatcher::new(query);
    let mut ranked: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| tab.matches(entry))
        .filter_map(|(idx, entry)| {
            matcher.score(&haystacks[idx]).map(|m| {
                let boost = match entry.source.as_str() {
                    "github" | "gitlab" => 20,
                    "bookmark" => 10,
                    _ => 0,
                };
                (m.score + boost, idx)
            })
        })
        .collect();

    ranked.sort_by(|a, b| match b.0.cmp(&a.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    ranked
}

fn rank_entries_all(entries: &[Entry], query: &str) -> Vec<(i64, usize)> {
    let haystacks = entry_haystacks(entries);
    rank_entries_all_with_haystacks(entries, &haystacks, query)
}

fn rank_entries_all_with_haystacks(
    entries: &[Entry],
    haystacks: &[String],
    query: &str,
) -> Vec<(i64, usize)> {
    let mut matcher = FuzzyMatcher::new(query);
    let mut ranked: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            matcher.score(&haystacks[idx]).map(|m| {
                let boost = match entry.source.as_str() {
                    "github" | "gitlab" => 20,
                    "bookmark" => 10,
                    _ => 0,
                };
                (m.score + boost, idx)
            })
        })
        .collect();

    ranked.sort_by(|a, b| match b.0.cmp(&a.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    ranked
}

fn ranked_config_items(items: &[ConfigItem], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }

    let normalized_query = query.to_ascii_lowercase();
    let mut matcher = FuzzyMatcher::new(query);
    let mut ranked: Vec<(i64, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            matcher.score(&item.haystack).map(|m| {
                let path =
                    format!("{} {} {}", item.level1, item.level2, item.level3).to_ascii_lowercase();
                let query_terms_match_path = normalized_query
                    .split_whitespace()
                    .all(|term| path.contains(term));
                let name_boost = if path.contains(&normalized_query) || query_terms_match_path {
                    10_000
                } else {
                    0
                };
                (m.score + name_boost, idx)
            })
        })
        .collect();

    ranked.sort_by(|a, b| match b.0.cmp(&a.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    ranked.into_iter().map(|(_, idx)| idx).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfigDisplayRow {
    Group(String),
    Item(usize),
}

const CONFIG_GROUP_ORDER: [&str; 5] = ["Browser", "GitHub", "GitLab", "DockerHub", "System"];

fn config_display_rows(items: &[ConfigItem], query: &str) -> Vec<ConfigDisplayRow> {
    let ranked = ranked_config_items(items, query);
    let mut rows = Vec::new();

    for group in CONFIG_GROUP_ORDER {
        let group_items: Vec<usize> = ranked
            .iter()
            .copied()
            .filter(|idx| items[*idx].level1 == group)
            .collect();
        if group_items.is_empty() {
            continue;
        }
        rows.push(ConfigDisplayRow::Group(group.to_string()));
        rows.extend(group_items.into_iter().map(ConfigDisplayRow::Item));
    }

    for idx in ranked {
        if CONFIG_GROUP_ORDER
            .iter()
            .any(|group| items[idx].level1 == *group)
        {
            continue;
        }
        rows.push(ConfigDisplayRow::Group(items[idx].level1.clone()));
        rows.push(ConfigDisplayRow::Item(idx));
    }

    rows
}

fn entry_haystacks(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(Entry::haystack).collect()
}

fn deduplicate_entries(entries: Vec<Entry>) -> Vec<Entry> {
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for entry in entries {
        let key = (entry.title.clone(), entry.url.clone(), entry.source.clone());
        if entry.url.is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(entry);
    }
    deduped
}

struct AppState {
    entries: Vec<Entry>,
    haystacks: Vec<String>,
    config: Config,
    config_items: Vec<ConfigItem>,
    runtime_config: RuntimeConfig,
    shared_config: Arc<Mutex<Config>>,
    visible: Vec<usize>,
    query: String,
    selected: usize,
    scroll_start: usize,
    result_viewport_height: usize,
    message: String,
    tab: Tab,
    config_tab_visible: bool,
    cursor_visible: bool,
    mode: AppMode,
    dirty: bool,
}

struct ConfigItem {
    level1: String,
    level2: String,
    level3: String,
    current: String,
    detail: String,
    haystack: String,
    edit_kind: ConfigEditKind,
}

impl ConfigItem {
    fn new(
        level1: impl Into<String>,
        level2: impl Into<String>,
        level3: impl Into<String>,
        current: impl Into<String>,
        configure: impl Into<String>,
        detail: impl Into<String>,
        edit_kind: ConfigEditKind,
    ) -> Self {
        let level1 = level1.into();
        let level2 = level2.into();
        let level3 = level3.into();
        let current = current.into();
        let configure = configure.into();
        let detail = detail.into();
        let haystack = format!("{level1} {level2} {level3} {current} {configure} {detail}");
        Self {
            level1,
            level2,
            level3,
            current,
            detail,
            haystack,
            edit_kind,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigEditKind {
    Toggle,
    Text,
    Secret,
    ReadOnly,
}

enum AppMode {
    Normal,
    TagList {
        repo: String,
        tags: Vec<String>,
        selected: usize,
    },
    ActionMenu {
        repo: String,
        tag: Option<String>,
        tags: Vec<String>,
        selected: usize,
    },
    RepoMenu {
        selected: usize,
    },
    ConfigEdit {
        item_index: usize,
        buffer: String,
    },
}

impl AppMode {
    fn kind(&self) -> AppModeKind {
        match self {
            AppMode::Normal => AppModeKind::Normal,
            AppMode::TagList { .. } => AppModeKind::TagList,
            AppMode::ActionMenu { .. } => AppModeKind::ActionMenu,
            AppMode::RepoMenu { .. } => AppModeKind::RepoMenu,
            AppMode::ConfigEdit { .. } => AppModeKind::ConfigEdit,
        }
    }
}

impl AppState {
    fn repo_menu_selected(&self) -> usize {
        match self.mode {
            AppMode::RepoMenu { selected } => selected,
            _ => 0,
        }
    }

    fn new(
        entries: Vec<Entry>,
        config: Config,
        runtime_config: RuntimeConfig,
        shared_config: Arc<Mutex<Config>>,
    ) -> Self {
        let haystacks = entry_haystacks(&entries);
        let config_items = build_config_items(&config);
        let mut app = Self {
            entries,
            haystacks,
            config: config.clone(),
            config_items,
            runtime_config,
            shared_config,
            visible: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll_start: 0,
            result_viewport_height: 0,
            message: "Type to search.".to_string(),
            tab: Tab::History,
            config_tab_visible: true,
            cursor_visible: true,
            mode: AppMode::Normal,
            dirty: false,
        };
        app.recompute();
        app
    }

    fn flush_deferred_recompute(&mut self) {
        if self.dirty {
            self.recompute();
            self.dirty = false;
        }
    }

    fn recompute(&mut self) {
        self.ensure_current_tab_visible();
        if self.tab == Tab::Config {
            self.visible.clear();
            self.selected = self
                .selected
                .min(self.config_result_count().saturating_sub(1));
            self.scroll_start = 0;
            return;
        }
        self.visible = rank_entries(&self.entries, &self.haystacks, &self.query, self.tab)
            .into_iter()
            .map(|(_, idx)| idx)
            .take(300)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.clamp_scroll_start();
        self.ensure_selected_visible();
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.visible
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }

    fn selected_config_item_index(&self) -> Option<usize> {
        config_display_rows(&self.config_items, &self.query)
            .get(self.selected)
            .and_then(|row| match row {
                ConfigDisplayRow::Group(_) => None,
                ConfigDisplayRow::Item(idx) => Some(*idx),
            })
    }

    fn source_tabs(&self) -> Vec<Tab> {
        let mut tabs = Vec::new();
        for tab in BASE_TABS {
            if self.tab_enabled(tab) {
                tabs.push(tab);
            }
        }
        if self.config_tab_visible {
            tabs.push(Tab::Config);
        }
        tabs
    }

    fn visible_tabs(&self) -> Vec<Tab> {
        self.source_tabs()
    }

    fn tab_enabled(&self, tab: Tab) -> bool {
        match tab {
            Tab::History => self.runtime_config.include_browser(),
            Tab::GitHub => self.runtime_config.include_github(),
            Tab::GitLab => self.runtime_config.include_gitlab(),
            Tab::DockerHub => self.runtime_config.include_dockerhub(),
            Tab::Config => self.config_tab_visible,
        }
    }

    fn selected_tab_index(&self) -> usize {
        self.visible_tabs()
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0)
    }

    fn set_result_viewport_height(&mut self, height: usize) {
        self.result_viewport_height = height;
        self.clamp_scroll_start();
        self.ensure_selected_visible();
    }

    fn visible_rows(&self) -> Vec<usize> {
        if self.visible.is_empty() || self.result_viewport_height == 0 {
            return Vec::new();
        }
        let start = self.scroll_start.min(self.max_scroll_start());
        let end = (start + self.result_viewport_height).min(self.visible.len());
        self.visible[start..end].to_vec()
    }

    fn selected_in_window(&self) -> usize {
        if self.visible.is_empty() || self.result_viewport_height == 0 {
            return 0;
        }
        self.selected.saturating_sub(self.scroll_start)
    }

    fn max_scroll_start(&self) -> usize {
        self.visible
            .len()
            .saturating_sub(self.result_viewport_height)
    }

    fn clamp_scroll_start(&mut self) {
        self.scroll_start = self.scroll_start.min(self.max_scroll_start());
    }

    fn ensure_selected_visible(&mut self) {
        if self.visible.is_empty() || self.result_viewport_height == 0 {
            self.scroll_start = 0;
            return;
        }

        if self.selected < self.scroll_start {
            self.scroll_start = self.selected;
        } else {
            let viewport_end = self.scroll_start + self.result_viewport_height;
            if self.selected >= viewport_end {
                self.scroll_start = self.selected + 1 - self.result_viewport_height;
            }
        }
        self.clamp_scroll_start();
    }

    fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.scroll_start = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        self.pop_grapheme();
        self.selected = 0;
        self.scroll_start = 0;
        self.dirty = true;
    }

    fn pop_grapheme(&mut self) {
        if let Some(grapheme) = self.query.graphemes(true).next_back() {
            let new_len = self.query.len().saturating_sub(grapheme.len());
            self.query.truncate(new_len);
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.ensure_selected_visible();
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.visible.len() {
            self.selected += 1;
            self.ensure_selected_visible();
        }
    }

    fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(10);
        self.ensure_selected_visible();
    }

    fn page_down(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 10).min(self.visible.len() - 1);
        self.ensure_selected_visible();
    }

    fn config_result_count(&self) -> usize {
        config_display_rows(&self.config_items, &self.query).len()
    }

    fn move_config_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_config_down(&mut self) {
        let count = self.config_result_count();
        if self.selected + 1 < count {
            self.selected += 1;
        }
    }

    fn page_config_up(&mut self) {
        self.selected = self.selected.saturating_sub(10);
    }

    fn page_config_down(&mut self) {
        let count = self.config_result_count();
        if count > 0 {
            self.selected = (self.selected + 10).min(count - 1);
        }
    }

    fn push_config_edit_char(&mut self, ch: char) {
        if let AppMode::ConfigEdit { ref mut buffer, .. } = self.mode {
            buffer.push(ch);
        }
    }

    fn pop_config_edit_char(&mut self) {
        if let AppMode::ConfigEdit { ref mut buffer, .. } = self.mode {
            if let Some(grapheme) = buffer.graphemes(true).next_back() {
                let new_len = buffer.len().saturating_sub(grapheme.len());
                buffer.truncate(new_len);
            }
        }
    }

    fn jump_top(&mut self) {
        self.selected = 0;
        self.ensure_selected_visible();
    }

    fn jump_bottom(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = self.visible.len() - 1;
        self.ensure_selected_visible();
    }

    fn next_tab(&mut self) {
        let tabs = self.visible_tabs();
        if tabs.is_empty() {
            return;
        }
        let current = self.selected_tab_index();
        self.tab = tabs[(current + 1) % tabs.len()];
        self.selected = 0;
        self.scroll_start = 0;
        self.recompute();
    }

    fn previous_tab(&mut self) {
        let tabs = self.visible_tabs();
        if tabs.is_empty() {
            return;
        }
        let current = self.selected_tab_index();
        self.tab = tabs[(current + tabs.len() - 1) % tabs.len()];
        self.selected = 0;
        self.scroll_start = 0;
        self.recompute();
    }

    fn toggle_config_tab(&mut self) {
        self.config_tab_visible = !self.config_tab_visible;
        self.mode = AppMode::Normal;
        if self.config_tab_visible {
            self.tab = Tab::Config;
            self.message = "Config tab shown.".to_string();
        } else {
            if self.tab == Tab::Config {
                self.tab = Tab::History;
            }
            self.message = "Config tab hidden.".to_string();
        }
        self.selected = 0;
        self.scroll_start = 0;
        self.ensure_current_tab_visible();
        self.recompute();
    }

    fn set_source_enabled(&mut self, level1: &str, enabled: bool) {
        match level1 {
            "Browser" => {
                self.config.include_browser = enabled;
                self.runtime_config.set_include_browser(enabled);
            }
            "GitHub" => {
                self.config.include_github = enabled;
                self.runtime_config.set_include_github(enabled);
            }
            "GitLab" => {
                self.config.include_gitlab = enabled;
                self.runtime_config.set_include_gitlab(enabled);
            }
            "DockerHub" => {
                self.config.include_dockerhub = enabled;
                self.runtime_config.set_include_dockerhub(enabled);
            }
            _ => {}
        }
        self.ensure_current_tab_visible();
    }

    fn ensure_current_tab_visible(&mut self) {
        if self.tab_enabled(self.tab) {
            return;
        }
        self.tab = self
            .visible_tabs()
            .into_iter()
            .find(|tab| *tab != Tab::Config)
            .unwrap_or(Tab::Config);
        self.selected = 0;
        self.scroll_start = 0;
    }

    fn replace_entries_for_sources(&mut self, sources: &[String], rows: Vec<Entry>) {
        let mut merged: Vec<Entry> = std::mem::take(&mut self.entries)
            .into_iter()
            .filter(|entry| !sources.iter().any(|source| source == &entry.source))
            .collect();
        merged.extend(rows);
        self.entries = deduplicate_entries(merged);
        self.haystacks = entry_haystacks(&self.entries);
        self.recompute();
    }

    fn sync_config_items(&mut self) {
        self.config_items = build_config_items(&self.config);
    }

    fn persist_config(&mut self) -> Result<(), String> {
        if let Ok(mut shared) = self.shared_config.lock() {
            *shared = self.config.clone();
        }
        self.config.save_to_disk()
    }
}

fn build_config_items(config: &Config) -> Vec<ConfigItem> {
    vec![
        ConfigItem::new(
            "Browser",
            "Source",
            "Enabled",
            enabled_text(config.include_browser),
            "--no-browser",
            "Local browser history and bookmarks. Safari on macOS may require Full Disk Access for the terminal.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "GitHub",
            "Source",
            "Enabled",
            enabled_text(config.include_github),
            "--no-github",
            "Search repositories visible to GitHub. Use GITHUB_TOKEN for private or organization repositories, or GITHUB_USER for public user repositories.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "GitLab",
            "Source",
            "Enabled",
            enabled_text(config.include_gitlab),
            "--no-gitlab",
            "Search GitLab projects. Use GITLAB_TOKEN for membership projects; otherwise only public projects are queried.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "DockerHub",
            "Source",
            "Enabled",
            enabled_text(config.include_dockerhub),
            "--no-dockerhub",
            "Search DockerHub repositories for DOCKERHUB_USERNAME. A token enables private repositories.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "GitHub",
            "Auth",
            "Token",
            secret_status(config.github_token.as_deref()),
            "--github-token <token> or GITHUB_TOKEN",
            "Recommended acquisition: install gh, run gh auth login, then export GITHUB_TOKEN=$(gh auth token). Token needs repository read access for private repos.",
            ConfigEditKind::Secret,
        ),
        ConfigItem::new(
            "GitHub",
            "Auth",
            "User",
            optional_value(config.github_user.as_deref()),
            "--github-user <user> or GITHUB_USER",
            "Used when no token is provided. Fetches public owner repositories for the configured user.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "GitLab",
            "Auth",
            "Token",
            secret_status(config.gitlab_token.as_deref()),
            "--gitlab-token <token> or GITLAB_TOKEN",
            "Recommended acquisition when glab is available: glab auth login, then export GITLAB_TOKEN=$(glab auth token). Token should have read_api scope.",
            ConfigEditKind::Secret,
        ),
        ConfigItem::new(
            "DockerHub",
            "Auth",
            "Token",
            secret_status(config.dockerhub_token.as_deref()),
            "--dockerhub-token <token> or DOCKERHUB_TOKEN",
            "Create a DockerHub Personal Access Token and export DOCKERHUB_TOKEN. Future automatic flow can read docker login credentials or request a PAT interactively.",
            ConfigEditKind::Secret,
        ),
        ConfigItem::new(
            "DockerHub",
            "Auth",
            "Username",
            optional_value(config.dockerhub_username.as_deref()),
            "--dockerhub-user <user> or DOCKERHUB_USERNAME",
            "Required for DockerHub repository search.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "GitHub",
            "API",
            "Base URL",
            config.github_api.clone(),
            "--github-api <url> or GITHUB_API",
            "Set this for GitHub Enterprise, for example https://github.example.com/api/v3.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "GitLab",
            "API",
            "Base URL",
            config.gitlab_api.clone(),
            "--gitlab-api <url> or GITLAB_API",
            "Set this for self-hosted GitLab, for example https://gitlab.example.com/api/v4.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "Browser",
            "Refresh",
            "Interval",
            format_duration(config.history_refresh_interval),
            "WEB_FZF_HISTORY_REFRESH",
            "Accepts plain seconds, seconds such as 5s, or minutes such as 1min.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "GitHub",
            "Refresh",
            "Interval",
            format_duration(config.github_refresh_interval),
            "WEB_FZF_GITHUB_REFRESH",
            "Remote cache is used immediately; refresh runs in the background when this interval has elapsed.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "GitLab",
            "Refresh",
            "Interval",
            format_duration(config.gitlab_refresh_interval),
            "WEB_FZF_GITLAB_REFRESH",
            "Remote cache is kept if refresh fails or returns no rows.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "DockerHub",
            "Refresh",
            "Interval",
            format_duration(config.dockerhub_refresh_interval),
            "WEB_FZF_DOCKERHUB_REFRESH",
            "Controls DockerHub repository cache refresh frequency.",
            ConfigEditKind::Text,
        ),
        ConfigItem::new(
            "System",
            "Display",
            "Preview pane",
            enabled_text(config.preview_enabled),
            "WEB_FZF_PREVIEW or config file",
            "Enables the preview pane in normal result views.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "System",
            "Diagnostics",
            "Debug logging",
            enabled_text(config.debug),
            "--debug",
            "Writes diagnostics to stderr. Redirect stderr to a file so it does not interfere with the TUI.",
            ConfigEditKind::Toggle,
        ),
        ConfigItem::new(
            "System",
            "Automation",
            "Token plan",
            "planned",
            "Config tab action menu",
            "GitHub can shell out to gh auth token, GitLab can shell out to glab auth token, and DockerHub can import existing docker login credentials or guide PAT creation. Tokens should be shown as present/missing only.",
            ConfigEditKind::ReadOnly,
        ),
    ]
}

fn enabled_text(value: bool) -> &'static str {
    if value {
        "enabled"
    } else {
        "disabled"
    }
}

fn secret_status(value: Option<&str>) -> &'static str {
    match value {
        Some(value) if !value.is_empty() => "present",
        _ => "missing",
    }
}

fn optional_value(value: Option<&str>) -> String {
    value
        .filter(|value| !value.is_empty())
        .unwrap_or("not set")
        .to_string()
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 && seconds % 60 == 0 {
        format!("{}min", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

struct TerminalSession {
    previous_state: Option<String>,
}

impl TerminalSession {
    fn enter() -> Result<Self, String> {
        let previous_state = Command::new("stty")
            .arg("-g")
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    None
                }
            });

        Command::new("stty")
            .args(["raw", "-echo", "min", "1", "time", "5"])
            .status()
            .map_err(|err| format!("enable raw mode: {err}"))?;

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)
            .map_err(|err| format!("enter alternate screen: {err}"))?;

        Ok(Self { previous_state })
    }

    fn restore(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, LeaveAlternateScreen);
        if let Some(state) = &self.previous_state {
            let _ = Command::new("stty").arg(state).status();
        } else {
            let _ = Command::new("stty").args(["sane"]).status();
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        best_entry, commit_config_edit, config_display_rows, display_width, entry_haystacks,
        open_selected_entry, parse_next_input_key, rank_entries, ranked_config_items,
        resolve_action, start_config_edit, tab_count, Action, AppMode, AppModeKind, AppState,
        ConfigDisplayRow, InputCode, InputKey, Tab,
    };
    use crate::config::{Config, RuntimeConfig};
    use crate::models::Entry;
    use std::sync::{Arc, Mutex};

    fn app_state(entries: Vec<Entry>) -> AppState {
        let config = Config::default();
        let runtime_config = RuntimeConfig::new(&config);
        let shared_config = Arc::new(Mutex::new(config.clone()));
        AppState::new(entries, config, runtime_config, shared_config)
    }

    #[test]
    fn best_entry_prefers_stronger_match() {
        let entries = vec![
            Entry::new("GitHub Home", "https://github.com", "github", ""),
            Entry::new("GitLab Docs", "https://docs.gitlab.com", "gitlab", ""),
        ];
        let best = best_entry(&entries, "gh").unwrap();
        assert_eq!(best.url, "https://github.com");
    }

    #[test]
    fn history_tab_filters_to_browser_entries() {
        let entries = vec![
            Entry::new("Example", "https://example.com", "history", ""),
            Entry::new("Repo", "https://github.com/me/repo", "github", ""),
        ];
        let haystacks = entry_haystacks(&entries);
        let ranked = rank_entries(&entries, &haystacks, "", Tab::History);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1, 0);
    }

    #[test]
    fn dockerhub_tab_count_includes_public_and_private_entries() {
        let entries = vec![
            Entry::new(
                "me/public",
                "https://hub.docker.com/r/me/public",
                "public",
                "",
            ),
            Entry::new(
                "me/private",
                "https://hub.docker.com/r/me/private",
                "private",
                "",
            ),
            Entry::new("Repo", "https://github.com/me/repo", "github", ""),
        ];

        assert_eq!(tab_count(&entries, Tab::DockerHub), 2);
    }

    #[test]
    fn dockerhub_entry_without_tags_opens_action_menu() {
        let mut app = app_state(vec![Entry::new(
            "juhaoming/ubuntu-dev",
            "https://hub.docker.com/r/juhaoming/ubuntu-dev",
            "public",
            "Ubuntu dev image",
        )]);
        app.tab = Tab::DockerHub;
        app.recompute();

        open_selected_entry(&mut app).unwrap();

        match app.mode {
            AppMode::ActionMenu {
                ref repo,
                ref tag,
                ref tags,
                selected,
            } => {
                assert_eq!(repo, "juhaoming/ubuntu-dev");
                assert_eq!(tag, &None);
                assert!(tags.is_empty());
                assert_eq!(selected, 0);
            }
            _ => panic!("expected action menu for dockerhub entry without tags"),
        }
    }

    #[test]
    fn github_entry_opens_action_menu() {
        let mut app = app_state(vec![Entry::new(
            "juhaoming/web-fzf",
            "https://github.com/juhaoming/web-fzf",
            "github",
            "terminal search launcher",
        )]);
        app.tab = Tab::GitHub;
        app.recompute();

        open_selected_entry(&mut app).unwrap();

        match app.mode {
            AppMode::RepoMenu { selected } => {
                assert_eq!(selected, 0);
            }
            _ => panic!("expected repository action menu"),
        }
    }

    #[test]
    fn gitlab_entry_opens_action_menu() {
        let mut app = app_state(vec![Entry::new(
            "platform/psd/auto-server/parking_fusion",
            "https://gitlab.example.com/platform/psd/auto-server/parking_fusion",
            "gitlab",
            "freespace fusion and parking static fusion merge process",
        )]);
        app.tab = Tab::GitLab;
        app.recompute();

        open_selected_entry(&mut app).unwrap();

        match app.mode {
            AppMode::RepoMenu { selected } => {
                assert_eq!(selected, 0);
            }
            _ => panic!("expected repository action menu"),
        }
    }

    #[test]
    fn result_selection_scrolls_only_at_viewport_edges() {
        let entries: Vec<Entry> = (0..8)
            .map(|idx| {
                Entry::new(
                    format!("Item {idx}"),
                    format!("https://example.com/{idx}"),
                    "history",
                    "",
                )
            })
            .collect();
        let mut app = app_state(entries);
        app.set_result_viewport_height(3);

        assert_eq!(app.selected, 0);
        assert_eq!(app.scroll_start, 0);
        assert_eq!(app.selected_in_window(), 0);

        app.move_down();
        assert_eq!(app.selected_in_window(), 1);
        assert_eq!(app.scroll_start, 0);

        app.move_down();
        assert_eq!(app.selected_in_window(), 2);
        assert_eq!(app.scroll_start, 0);

        app.move_down();
        assert_eq!(app.selected_in_window(), 2);
        assert_eq!(app.scroll_start, 1);

        app.move_up();
        assert_eq!(app.selected_in_window(), 1);
        assert_eq!(app.scroll_start, 1);

        app.move_up();
        assert_eq!(app.selected_in_window(), 0);
        assert_eq!(app.scroll_start, 1);

        app.move_up();
        assert_eq!(app.selected_in_window(), 0);
        assert_eq!(app.scroll_start, 0);
    }

    #[test]
    fn chinese_input_uses_display_width_and_backspace_removes_whole_grapheme() {
        assert_eq!(display_width("中文"), 4);
        let mut app = app_state(Vec::new());
        app.push_char('中');
        app.push_char('文');
        assert_eq!(app.query, "中文");
        app.backspace();
        assert_eq!(app.query, "中");
    }

    #[test]
    fn raw_input_distinguishes_enter_from_ctrl_j() {
        let mut enter = b"\r".to_vec();
        assert_eq!(
            parse_next_input_key(&mut enter),
            Some(InputKey::new(InputCode::Enter))
        );

        let mut ctrl_j = b"\n".to_vec();
        assert_eq!(parse_next_input_key(&mut ctrl_j), Some(InputKey::ctrl('j')));
    }

    #[test]
    fn raw_input_parses_arrows_and_utf8_text() {
        let mut down = b"\x1B[B".to_vec();
        assert_eq!(
            parse_next_input_key(&mut down),
            Some(InputKey::new(InputCode::Down))
        );

        let mut text = "中".as_bytes().to_vec();
        assert_eq!(
            parse_next_input_key(&mut text),
            Some(InputKey::new(InputCode::Char('中')))
        );
    }

    #[test]
    fn normal_keymap_distinguishes_enter_from_ctrl_j() {
        assert_eq!(
            resolve_action(AppModeKind::Normal, InputKey::new(InputCode::Enter)),
            Some(Action::OpenSelected)
        );
        assert_eq!(
            resolve_action(AppModeKind::Normal, InputKey::ctrl('j')),
            Some(Action::MoveDown)
        );
        assert_eq!(
            resolve_action(AppModeKind::Normal, InputKey::ctrl('b')),
            Some(Action::ToggleConfigTab)
        );
    }

    #[test]
    fn modal_keymaps_keep_enter_mode_specific() {
        assert_eq!(
            resolve_action(AppModeKind::TagList, InputKey::new(InputCode::Enter)),
            Some(Action::SelectTag)
        );
        assert_eq!(
            resolve_action(AppModeKind::ActionMenu, InputKey::new(InputCode::Enter)),
            Some(Action::ConfirmDockerAction)
        );
        assert_eq!(
            resolve_action(AppModeKind::RepoMenu, InputKey::new(InputCode::Enter)),
            Some(Action::ConfirmRepoAction)
        );
        assert_eq!(
            resolve_action(AppModeKind::RepoMenu, InputKey::new(InputCode::Backspace)),
            Some(Action::BackToNormal)
        );
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_in_modal_modes() {
        for mode in [AppModeKind::TagList, AppModeKind::ActionMenu] {
            assert_eq!(
                resolve_action(mode, InputKey::ctrl('n')),
                Some(Action::MoveDown)
            );
            assert_eq!(
                resolve_action(mode, InputKey::ctrl('p')),
                Some(Action::MoveUp)
            );
        }
    }

    #[test]
    fn ctrl_u_and_ctrl_d_page_in_all_modes() {
        for mode in [
            AppModeKind::Normal,
            AppModeKind::TagList,
            AppModeKind::ActionMenu,
        ] {
            assert_eq!(
                resolve_action(mode, InputKey::ctrl('u')),
                Some(Action::PageUp)
            );
            assert_eq!(
                resolve_action(mode, InputKey::ctrl('d')),
                Some(Action::PageDown)
            );
        }
    }

    #[test]
    fn backspace_goes_back_only_in_modal_modes() {
        assert_eq!(
            resolve_action(AppModeKind::Normal, InputKey::new(InputCode::Backspace)),
            Some(Action::Backspace)
        );
        assert_eq!(
            resolve_action(AppModeKind::TagList, InputKey::new(InputCode::Backspace)),
            Some(Action::BackToNormal)
        );
        assert_eq!(
            resolve_action(AppModeKind::ActionMenu, InputKey::new(InputCode::Backspace)),
            Some(Action::BackToTags)
        );
    }

    #[test]
    fn config_tab_is_visible_by_default_and_can_be_toggled() {
        let mut app = app_state(Vec::new());
        assert_eq!(
            app.visible_tabs(),
            vec![
                Tab::History,
                Tab::GitHub,
                Tab::GitLab,
                Tab::DockerHub,
                Tab::Config,
            ]
        );
        assert_eq!(app.tab, Tab::History);

        app.toggle_config_tab();
        assert_eq!(
            app.visible_tabs(),
            vec![Tab::History, Tab::GitHub, Tab::GitLab, Tab::DockerHub]
        );
        assert_eq!(app.tab, Tab::History);

        app.toggle_config_tab();
        assert_eq!(
            app.visible_tabs(),
            vec![
                Tab::History,
                Tab::GitHub,
                Tab::GitLab,
                Tab::DockerHub,
                Tab::Config,
            ]
        );
        assert_eq!(app.tab, Tab::Config);
        assert_eq!(app.selected_tab_index(), 4);
        assert!(app.visible.is_empty());
    }

    #[test]
    fn config_items_are_searchable() {
        let app = app_state(Vec::new());
        let rows = ranked_config_items(&app.config_items, "github token");
        assert!(!rows.is_empty());
        assert_eq!(app.config_items[rows[0]].level1, "GitHub");
        assert_eq!(app.config_items[rows[0]].level2, "Auth");
        assert_eq!(app.config_items[rows[0]].level3, "Token");
    }

    #[test]
    fn config_items_use_source_grouped_hierarchy() {
        let app = app_state(Vec::new());
        assert!(app.config_items.iter().any(|item| {
            item.level1 == "DockerHub" && item.level2 == "Auth" && item.level3 == "Username"
        }));
        assert!(app.config_items.iter().any(|item| {
            item.level1 == "Browser" && item.level2 == "Refresh" && item.level3 == "Interval"
        }));
        assert!(app.config_items.iter().any(|item| {
            item.level1 == "GitHub" && item.level2 == "Source" && item.level3 == "Enabled"
        }));
    }

    #[test]
    fn config_display_rows_include_expanded_function_headers() {
        let app = app_state(Vec::new());
        let rows = config_display_rows(&app.config_items, "github token");
        assert!(matches!(
            rows.first(),
            Some(ConfigDisplayRow::Group(group)) if group == "GitHub"
        ));
        assert!(matches!(rows.get(1), Some(ConfigDisplayRow::Item(_))));
    }

    #[test]
    fn config_display_rows_group_all_items_under_source_header() {
        let app = app_state(Vec::new());
        let rows = config_display_rows(&app.config_items, "");
        let github_start = rows
            .iter()
            .position(|row| matches!(row, ConfigDisplayRow::Group(group) if group == "GitHub"))
            .unwrap();
        let next_group = rows[github_start + 1..]
            .iter()
            .position(|row| matches!(row, ConfigDisplayRow::Group(_)))
            .map(|offset| github_start + 1 + offset)
            .unwrap_or(rows.len());
        let github_settings: Vec<_> = rows[github_start + 1..next_group]
            .iter()
            .filter_map(|row| match row {
                ConfigDisplayRow::Item(idx) => Some(app.config_items[*idx].level3.as_str()),
                ConfigDisplayRow::Group(_) => None,
            })
            .collect();

        assert_eq!(
            github_settings,
            vec!["Enabled", "Token", "User", "Base URL", "Interval"]
        );
    }

    #[test]
    fn selected_config_item_can_change_value() {
        let mut app = app_state(Vec::new());
        app.query = "browser sources".to_string();
        app.tab = Tab::Config;
        app.recompute();
        app.move_config_down();

        start_config_edit(&mut app);
        assert_eq!(
            app.config_items[app.selected_config_item_index().unwrap()].current,
            "disabled"
        );

        app.query = "github user".to_string();
        app.recompute();
        app.selected = 0;
        app.move_config_down();
        start_config_edit(&mut app);
        app.push_config_edit_char('m');
        app.push_config_edit_char('e');
        commit_config_edit(&mut app);
        let item = &app.config_items[app.selected_config_item_index().unwrap()];
        assert_eq!(item.level1, "GitHub");
        assert_eq!(item.level2, "Auth");
        assert_eq!(item.level3, "User");
        assert_eq!(item.current, "me");
    }

    #[test]
    fn tab_navigation_includes_config_only_when_visible() {
        let mut app = app_state(Vec::new());
        app.previous_tab();
        assert_eq!(app.tab, Tab::Config);

        app.toggle_config_tab();
        app.previous_tab();
        assert_eq!(app.tab, Tab::DockerHub);

        app.toggle_config_tab();
        assert_eq!(app.tab, Tab::Config);
    }

    #[test]
    fn disabling_source_hides_tab_and_moves_selection() {
        let mut app = app_state(Vec::new());
        app.toggle_config_tab();
        app.tab = Tab::GitHub;
        app.query = "github enabled".to_string();
        app.recompute();
        app.move_config_down();

        start_config_edit(&mut app);

        assert!(!app.runtime_config.include_github());
        assert!(!app.visible_tabs().contains(&Tab::GitHub));
        assert_eq!(app.tab, Tab::History);
    }

    #[test]
    fn replace_entries_for_sources_updates_existing_browser_entries() {
        let mut app = app_state(vec![
            Entry::new("Old", "https://old", "history", ""),
            Entry::new("Repo", "https://github.com/me/repo", "github", ""),
        ]);
        app.replace_entries_for_sources(
            &["history".to_string()],
            vec![Entry::new("New", "https://new", "history", "")],
        );
        assert!(app.entries.iter().any(|entry| entry.url == "https://new"));
        assert!(!app.entries.iter().any(|entry| entry.url == "https://old"));
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.url == "https://github.com/me/repo"));
    }

    #[test]
    fn history_tab_still_accepts_old_browser_history_source() {
        let entry = Entry::new("Old", "https://old", "browser-history", "");
        assert!(Tab::History.matches(&entry));
    }
}
