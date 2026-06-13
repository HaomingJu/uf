use crate::matchers::fuzzy_score;
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
    events: Receiver<UiEvent>,
    refresh_requests: Sender<RefreshRequest>,
) -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("interactive terminal required".to_string());
    }

    let mut session = TerminalSession::enter()?;
    let mut app = AppState::new(entries);
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|err| format!("create terminal: {err}"))?;
    let mut last_cursor_toggle = Instant::now();
    let mut input = InputReader::default();

    let result = loop {
        if last_cursor_toggle.elapsed() >= CURSOR_BLINK_INTERVAL {
            app.cursor_visible = !app.cursor_visible;
            last_cursor_toggle = Instant::now();
        }

        drain_events(&mut app, &events);
        terminal
            .draw(|frame| render(frame, &app))
            .map_err(|err| format!("draw terminal: {err}"))?;

        let mut stdout = io::stdout();
        if app.cursor_visible {
            execute!(stdout, Show).map_err(|err| format!("show cursor: {err}"))?;
        } else {
            execute!(stdout, Hide).map_err(|err| format!("hide cursor: {err}"))?;
        }

        if let Some(key) = input.read_key()? {
            if handle_key(&mut app, key, &refresh_requests)? {
                break Ok(());
            }
            app.cursor_visible = true;
            last_cursor_toggle = Instant::now();
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
    fn read_key(&mut self) -> Result<Option<InputKey>, String> {
        let mut bytes = [0u8; 64];
        match io::stdin().read(&mut bytes) {
            Ok(0) => {}
            Ok(count) => self.buffer.extend_from_slice(&bytes[..count]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(format!("read input: {err}")),
        }

        Ok(parse_next_input_key(&mut self.buffer))
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
        b'\x7F' | b'\x08' => {
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
    match &app.mode {
        AppMode::TagList { .. } => return handle_key_tag_list(app, key),
        AppMode::ActionMenu { .. } => return handle_key_action_menu(app, key),
        AppMode::Normal => {}
    }

    match key.code {
        InputCode::Esc => return Ok(true),
        InputCode::Enter => {
            if let Some(entry) = app.selected_entry() {
                let is_dockerhub = entry.source == "public" || entry.source == "private";
                if is_dockerhub {
                    let repo = entry.title.clone();
                    let tags = dockerhub_entry_tags(entry);
                    if tags.is_empty() {
                        app.message = format!("No cached tags found for {repo}");
                    } else {
                        app.mode = AppMode::TagList {
                            repo,
                            tags,
                            selected: 0,
                        };
                    }
                    return Ok(false);
                }
                open_entry(entry)?;
                app.message = format!("Opened {}", entry.title);
                return Ok(false);
            }
        }
        InputCode::Backspace => {
            app.backspace();
        }
        InputCode::Char('u') if key.ctrl => {
            app.clear_query();
        }
        InputCode::Char('f') if key.ctrl => {
            let request = app.tab.refresh_request();
            let _ = refresh_requests.send(request);
            app.message = format!("Requested {} refresh.", app.tab.name());
        }
        InputCode::Char('j') | InputCode::Char('n') if key.ctrl => app.move_down(),
        InputCode::Char('k') | InputCode::Char('p') if key.ctrl => app.move_up(),
        InputCode::Char('h') if key.ctrl => app.previous_tab(),
        InputCode::Char('l') if key.ctrl => app.next_tab(),
        InputCode::Char(c) if !key.ctrl => app.push_char(c),
        InputCode::Up => app.move_up(),
        InputCode::Down => app.move_down(),
        InputCode::PageUp => app.page_up(),
        InputCode::PageDown => app.page_down(),
        InputCode::Home => app.jump_top(),
        InputCode::End => app.jump_bottom(),
        InputCode::Left => app.previous_tab(),
        InputCode::Right | InputCode::Tab => app.next_tab(),
        _ => {}
    }
    Ok(false)
}

fn handle_key_tag_list(app: &mut AppState, key: InputKey) -> Result<bool, String> {
    let AppMode::TagList {
        ref repo,
        ref tags,
        ref mut selected,
    } = app.mode
    else {
        return Ok(false);
    };
    match key.code {
        InputCode::Esc => {
            app.mode = AppMode::Normal;
            app.message = "Type to search.".to_string();
        }
        InputCode::Enter => {
            let tag = tags[*selected].clone();
            let repo = repo.clone();
            let tags = tags.clone();
            app.mode = AppMode::ActionMenu {
                repo,
                tag,
                tags,
                selected: 0,
            };
        }
        InputCode::Up | InputCode::Char('k') => {
            if *selected > 0 {
                *selected -= 1;
            }
        }
        InputCode::Down | InputCode::Char('j') => {
            if *selected + 1 < tags.len() {
                *selected += 1;
            }
        }
        _ => {}
    }
    Ok(false)
}

const ACTION_LABELS: [&str; 2] = ["Open in browser", "Copy docker pull command"];

fn handle_key_action_menu(app: &mut AppState, key: InputKey) -> Result<bool, String> {
    let AppMode::ActionMenu {
        ref repo,
        ref tag,
        ref tags,
        ref mut selected,
    } = app.mode
    else {
        return Ok(false);
    };
    match key.code {
        InputCode::Esc => {
            let repo = repo.clone();
            let tags = tags.clone();
            app.mode = AppMode::TagList {
                repo,
                tags,
                selected: 0,
            };
        }
        InputCode::Enter => {
            let action = *selected;
            let repo = repo.clone();
            let tag = tag.clone();
            app.mode = AppMode::Normal;
            match action {
                0 => {
                    let url = format!("https://hub.docker.com/r/{}/tags?name={}", repo, tag);
                    let _ = webbrowser::open(&url);
                    app.message = format!("Opened {repo}:{tag} in browser");
                }
                1 => {
                    let cmd = format!("docker pull {}:{}", repo, tag);
                    copy_to_clipboard(&cmd);
                    app.message = format!("Copied: {cmd}");
                }
                _ => {}
            }
        }
        InputCode::Up | InputCode::Char('k') => {
            if *selected > 0 {
                *selected -= 1;
            }
        }
        InputCode::Down | InputCode::Char('j') => {
            if *selected + 1 < ACTION_LABELS.len() {
                *selected += 1;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn render(frame: &mut Frame<'_>, app: &AppState) {
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

    match &app.mode {
        AppMode::TagList {
            repo,
            tags,
            selected,
        } => {
            render_tag_list(frame, layout[2], repo, tags, *selected);
        }
        AppMode::ActionMenu {
            repo,
            tag,
            tags: _,
            selected,
        } => {
            render_action_menu(frame, layout[2], repo, tag, *selected);
        }
        AppMode::Normal => {
            if preview_enabled() {
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

fn preview_enabled() -> bool {
    matches!(
        std::env::var("WEB_FZF_PREVIEW")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let header = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    let title = Line::from(vec![
        Span::styled(
            " web-fzf ",
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

    let tabs = vec![
        tab_label(
            "History",
            app.entries.iter().filter(|e| is_browser_entry(e)).count(),
            Color::Green,
        ),
        tab_label(
            "GitHub",
            app.entries.iter().filter(|e| e.source == "github").count(),
            Color::Magenta,
        ),
        tab_label(
            "GitLab",
            app.entries.iter().filter(|e| e.source == "gitlab").count(),
            Color::Yellow,
        ),
        tab_label(
            "DockerHub",
            tab_count(&app.entries, Tab::DockerHub),
            Color::Blue,
        ),
    ];

    let selected_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let tabs_widget = Tabs::new(tabs)
        .select(app.tab.index())
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

fn render_results(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
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
    let name_width = (inner_width / 2).min(40).max(10);
    let desc_width = inner_width
        .saturating_sub(name_width)
        .saturating_sub(type_width)
        .saturating_sub(sep * 2);

    let height = area.height.saturating_sub(2) as usize;
    let visible = app.visible_rows(height);
    let selected_in_window = app.selected_in_window(height);
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
                let desc_col = truncate_to_width(entry_detail(entry), desc_width);
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
                    Span::raw("  "),
                    Span::styled(desc_col, Style::default().fg(Color::Gray)),
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
        .highlight_symbol("  ");

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
        AppMode::TagList { .. } => "Up/Down=move  Enter=select tag  Esc=back",
        AppMode::ActionMenu { .. } => "Up/Down=move  Enter=confirm  Esc=back to tags",
        AppMode::Normal => {
            if app.query.is_empty() {
                "Enter=open  Ctrl+F=refresh tab  Esc=quit  Up/Down=move"
            } else {
                "Type=fuzzy filter  Ctrl+F=refresh tab  Enter=open  Esc=quit"
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

fn render_action_menu(frame: &mut Frame<'_>, area: Rect, repo: &str, tag: &str, selected: usize) {
    let block = Block::default()
        .title(Span::styled(
            format!(" Action: {repo}:{tag} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let items: Vec<ListItem> = ACTION_LABELS
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
    entry.source == "browser-history" || entry.source == "bookmark"
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

fn tab_count(entries: &[Entry], tab: Tab) -> usize {
    entries.iter().filter(|entry| tab.matches(entry)).count()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    History,
    GitHub,
    GitLab,
    DockerHub,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::History => 0,
            Tab::GitHub => 1,
            Tab::GitLab => 2,
            Tab::DockerHub => 3,
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::History => Tab::GitHub,
            Tab::GitHub => Tab::GitLab,
            Tab::GitLab => Tab::DockerHub,
            Tab::DockerHub => Tab::History,
        }
    }

    fn previous(self) -> Self {
        match self {
            Tab::History => Tab::DockerHub,
            Tab::GitHub => Tab::History,
            Tab::GitLab => Tab::GitHub,
            Tab::DockerHub => Tab::GitLab,
        }
    }

    fn matches(self, entry: &Entry) -> bool {
        match self {
            Tab::History => is_browser_entry(entry),
            Tab::GitHub => entry.source == "github",
            Tab::GitLab => entry.source == "gitlab",
            Tab::DockerHub => entry.source == "public" || entry.source == "private",
        }
    }

    fn refresh_request(self) -> RefreshRequest {
        match self {
            Tab::History => RefreshRequest::History,
            Tab::GitHub => RefreshRequest::GitHub,
            Tab::GitLab => RefreshRequest::GitLab,
            Tab::DockerHub => RefreshRequest::DockerHub,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::GitHub => "GitHub",
            Tab::GitLab => "GitLab",
            Tab::DockerHub => "DockerHub",
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

fn rank_entries(entries: &[Entry], query: &str, tab: Tab) -> Vec<(i64, usize)> {
    let mut ranked: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| tab.matches(entry))
        .filter_map(|(idx, entry)| {
            fuzzy_score(query, &entry.haystack()).map(|m| {
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
    let mut ranked: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            fuzzy_score(query, &entry.haystack()).map(|m| {
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
    visible: Vec<usize>,
    query: String,
    selected: usize,
    message: String,
    tab: Tab,
    cursor_visible: bool,
    mode: AppMode,
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
        tag: String,
        tags: Vec<String>,
        selected: usize,
    },
}

impl AppState {
    fn new(entries: Vec<Entry>) -> Self {
        let mut app = Self {
            entries,
            visible: Vec::new(),
            query: String::new(),
            selected: 0,
            message: "Type to search.".to_string(),
            tab: Tab::History,
            cursor_visible: true,
            mode: AppMode::Normal,
        };
        app.recompute();
        app
    }

    fn recompute(&mut self) {
        self.visible = rank_entries(&self.entries, &self.query, self.tab)
            .into_iter()
            .map(|(_, idx)| idx)
            .take(300)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.visible
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }

    fn visible_rows(&self, height: usize) -> Vec<usize> {
        if self.visible.is_empty() || height == 0 {
            return Vec::new();
        }
        let preferred_start = self.selected.saturating_sub(5);
        let max_start = self.visible.len().saturating_sub(height);
        let start = preferred_start.min(max_start);
        let end = (start + height).min(self.visible.len());
        self.visible[start..end].to_vec()
    }

    fn selected_in_window(&self, height: usize) -> usize {
        if self.visible.is_empty() || height == 0 {
            return 0;
        }
        let preferred_start = self.selected.saturating_sub(5);
        let max_start = self.visible.len().saturating_sub(height);
        let start = preferred_start.min(max_start);
        self.selected.saturating_sub(start)
    }

    fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.recompute();
    }

    fn backspace(&mut self) {
        self.pop_grapheme();
        self.selected = 0;
        self.recompute();
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.recompute();
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
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.visible.len() {
            self.selected += 1;
        }
    }

    fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(10);
    }

    fn page_down(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 10).min(self.visible.len() - 1);
    }

    fn jump_top(&mut self) {
        self.selected = 0;
    }

    fn jump_bottom(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = self.visible.len() - 1;
    }

    fn next_tab(&mut self) {
        self.tab = self.tab.next();
        self.selected = 0;
        self.recompute();
    }

    fn previous_tab(&mut self) {
        self.tab = self.tab.previous();
        self.selected = 0;
        self.recompute();
    }

    fn replace_entries_for_sources(&mut self, sources: &[String], rows: Vec<Entry>) {
        let mut merged: Vec<Entry> = std::mem::take(&mut self.entries)
            .into_iter()
            .filter(|entry| !sources.iter().any(|source| source == &entry.source))
            .collect();
        merged.extend(rows);
        self.entries = deduplicate_entries(merged);
        self.recompute();
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
            .args(["raw", "-echo", "min", "0", "time", "1"])
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
        best_entry, display_width, parse_next_input_key, rank_entries, tab_count, AppState,
        InputCode, InputKey, Tab,
    };
    use crate::models::Entry;

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
            Entry::new("Example", "https://example.com", "browser-history", ""),
            Entry::new("Repo", "https://github.com/me/repo", "github", ""),
        ];
        let ranked = rank_entries(&entries, "", Tab::History);
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
    fn chinese_input_uses_display_width_and_backspace_removes_whole_grapheme() {
        assert_eq!(display_width("中文"), 4);
        let mut app = AppState::new(Vec::new());
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
    fn replace_entries_for_sources_updates_existing_browser_entries() {
        let mut app = AppState::new(vec![
            Entry::new("Old", "https://old", "browser-history", ""),
            Entry::new("Repo", "https://github.com/me/repo", "github", ""),
        ]);
        app.replace_entries_for_sources(
            &["browser-history".to_string()],
            vec![Entry::new("New", "https://new", "browser-history", "")],
        );
        assert!(app.entries.iter().any(|entry| entry.url == "https://new"));
        assert!(!app.entries.iter().any(|entry| entry.url == "https://old"));
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.url == "https://github.com/me/repo"));
    }
}
