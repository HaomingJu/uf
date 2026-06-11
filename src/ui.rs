use crate::matchers::fuzzy_score;
use crate::models::Entry;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use std::cmp::Ordering;
use std::io::{self, IsTerminal};
use std::process::Command;
use std::time::Duration;

pub fn run_ui(entries: Vec<Entry>) -> Result<(), String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("interactive terminal required".to_string());
    }

    let mut session = TerminalSession::enter()?;
    let mut app = AppState::new(entries);
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|err| format!("create terminal: {err}"))?;

    let result = loop {
        terminal
            .draw(|frame| render(frame, &app))
            .map_err(|err| format!("draw terminal: {err}"))?;

        if event::poll(Duration::from_millis(150)).map_err(|err| format!("poll input: {err}"))? {
            match event::read().map_err(|err| format!("read input: {err}"))? {
                Event::Key(key) => {
                    if handle_key(&mut app, key)? {
                        break Ok(());
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    };

    session.restore();
    result
}

pub fn best_entry<'a>(entries: &'a [Entry], query: &str) -> Option<&'a Entry> {
    rank_entries_all(entries, query)
        .into_iter()
        .next()
        .and_then(|(_, idx)| entries.get(idx))
}

fn handle_key(app: &mut AppState, key: KeyEvent) -> Result<bool, String> {
    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Enter => {
            if let Some(entry) = app.selected_entry() {
                open_entry(entry)?;
                return Ok(true);
            }
        }
        KeyCode::Backspace => {
            app.backspace();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.clear_query();
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            app.push_char(c);
        }
        KeyCode::Up => app.move_up(),
        KeyCode::Down => app.move_down(),
        KeyCode::PageUp => app.page_up(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::Home => app.jump_top(),
        KeyCode::End => app.jump_bottom(),
        KeyCode::Left => app.previous_tab(),
        KeyCode::Right | KeyCode::Tab => app.next_tab(),
        _ => {}
    }
    Ok(false)
}

fn render(frame: &mut Frame<'_>, app: &AppState) {
    let size = frame.area();
    let layout = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(2),
    ])
    .split(size);

    render_header(frame, layout[0], app);
    render_search(frame, layout[1], app);

    let body = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(layout[2]);

    render_results(frame, body[0], app);
    render_preview(frame, body[1], app);
    render_footer(frame, layout[3], app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let header = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

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

    let status = Line::from(vec![
        Span::styled(
            app.tab.label(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "Left/Right switch tabs",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(status).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        header[2],
    );
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

    let visible = app.visible_rows(area.height.saturating_sub(2) as usize);
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
            .map(|idx| {
                let entry = &app.entries[idx];
                let mut spans = vec![
                    Span::styled(
                        &entry.title,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("[{}]", entry.source),
                        Style::default().fg(source_color(&entry.source)),
                    ),
                ];
                if !entry.detail.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        &entry.detail,
                        Style::default().fg(Color::Gray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let mut state = ListState::default();
    state.select(Some(app.selected));

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
                Span::styled(&entry.detail, Style::default().fg(Color::Gray)),
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
    let help = if app.query.is_empty() {
        "Enter=open  Esc=quit  Up/Down=move  PageUp/PageDown=page"
    } else {
        "Type=fuzzy filter  Backspace=delete  Enter=open  Esc=quit"
    };

    let text = Line::from(vec![
        Span::styled(app.message.as_str(), Style::default().fg(Color::Gray)),
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

fn source_color(source: &str) -> Color {
    match source {
        "github" => Color::Magenta,
        "gitlab" => Color::Yellow,
        "bookmark" => Color::Blue,
        _ => Color::Green,
    }
}

fn is_browser_entry(entry: &Entry) -> bool {
    entry.source == "browser-history" || entry.source == "bookmark"
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    History,
    GitHub,
    GitLab,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::History => 0,
            Tab::GitHub => 1,
            Tab::GitLab => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::GitHub => "GitHub",
            Tab::GitLab => "GitLab",
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::History => Tab::GitHub,
            Tab::GitHub => Tab::GitLab,
            Tab::GitLab => Tab::History,
        }
    }

    fn previous(self) -> Self {
        match self {
            Tab::History => Tab::GitLab,
            Tab::GitHub => Tab::History,
            Tab::GitLab => Tab::GitHub,
        }
    }

    fn matches(self, entry: &Entry) -> bool {
        match self {
            Tab::History => is_browser_entry(entry),
            Tab::GitHub => entry.source == "github",
            Tab::GitLab => entry.source == "gitlab",
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

struct AppState {
    entries: Vec<Entry>,
    visible: Vec<usize>,
    query: String,
    selected: usize,
    scroll: usize,
    message: String,
    tab: Tab,
}

impl AppState {
    fn new(entries: Vec<Entry>) -> Self {
        let mut app = Self {
            entries,
            visible: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            message: "Type to search.".to_string(),
            tab: Tab::History,
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
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
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
        let start = self.scroll.min(self.visible.len().saturating_sub(1));
        let end = (start + height).min(self.visible.len());
        self.visible[start..end].to_vec()
    }

    fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.scroll = 0;
        self.recompute();
    }

    fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.scroll = 0;
        self.recompute();
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
        self.recompute();
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.scroll = self.selected.saturating_sub(5);
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.visible.len() {
            self.selected += 1;
        }
        self.scroll = self.selected.saturating_sub(5);
    }

    fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(10);
        self.scroll = self.selected.saturating_sub(5);
    }

    fn page_down(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 10).min(self.visible.len() - 1);
        self.scroll = self.selected.saturating_sub(5);
    }

    fn jump_top(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    fn jump_bottom(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = self.visible.len() - 1;
        self.scroll = self.selected.saturating_sub(5);
    }

    fn next_tab(&mut self) {
        self.tab = self.tab.next();
        self.selected = 0;
        self.scroll = 0;
        self.recompute();
    }

    fn previous_tab(&mut self) {
        self.tab = self.tab.previous();
        self.selected = 0;
        self.scroll = 0;
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
    use super::{best_entry, rank_entries, Tab};
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
}
