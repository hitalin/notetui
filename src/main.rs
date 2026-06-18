//! notetui — a terminal Misskey client.
//!
//! This is a thin TUI frontend over the `notecli` library (the same crate
//! NoteDeck consumes). It reads the shared account DB, fetches a timeline via
//! `MisskeyClient`, and renders notes with ratatui. All Misskey logic lives in
//! notecli; this binary only owns terminal state and rendering.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use notecli::api::MisskeyClient;
use notecli::db::Database;
use notecli::models::{Account, NormalizedNote, TimelineOptions, TimelineType};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

/// Selectable timelines: (notecli timeline key, display label).
const TIMELINES: &[(&str, &str)] = &[
    ("home", "Home"),
    ("social", "Social"),
    ("local", "Local"),
    ("global", "Global"),
];

#[tokio::main]
async fn main() -> Result<()> {
    let db_path = data_dir().join("notecli").join("notecli.db");
    let db = Database::open(&db_path)
        .with_context(|| format!("failed to open account DB at {}", db_path.display()))?;

    let account = db
        .load_accounts()?
        .into_iter()
        .next()
        .context("no accounts found — run `notecli login <HOST>` first")?;
    let (host, token) = notecli::get_credentials(&db, &account.id)?;
    let client = MisskeyClient::new()?;

    let mut app = App::new(account, client, host, token);
    let mut terminal = ratatui::init();
    app.refresh().await;
    let result = app.run(&mut terminal).await;
    ratatui::restore();
    result
}

/// Same data dir notecli uses (see `dirs_data_dir` in notecli's commands/mod.rs).
fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".local/share")
    })
}

struct App {
    account: Account,
    client: MisskeyClient,
    host: String,
    token: String,
    tl_idx: usize,
    notes: Vec<NormalizedNote>,
    list_state: ListState,
    status: String,
    should_quit: bool,
}

impl App {
    fn new(account: Account, client: MisskeyClient, host: String, token: String) -> Self {
        Self {
            account,
            client,
            host,
            token,
            tl_idx: 0,
            notes: Vec::new(),
            list_state: ListState::default(),
            status: String::from("loading…"),
            should_quit: false,
        }
    }

    fn timeline(&self) -> TimelineType {
        TimelineType::new(TIMELINES[self.tl_idx].0)
    }

    /// Fetch the current timeline and reset the selection to the top.
    async fn refresh(&mut self) {
        self.status = format!("fetching {}…", TIMELINES[self.tl_idx].1);
        let opts = TimelineOptions::new(40, None, None);
        match self
            .client
            .get_timeline(
                &self.host,
                &self.token,
                &self.account.id,
                self.timeline(),
                opts,
            )
            .await
        {
            Ok(notes) => {
                self.notes = notes;
                self.list_state
                    .select(if self.notes.is_empty() { None } else { Some(0) });
                self.status = format!("{} notes", self.notes.len());
            }
            Err(e) => {
                // notecli scrubs tokens from messages via safe_message().
                self.status = format!("error: {}", e.safe_message());
            }
        }
    }

    async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;
            self.handle_events().await?;
        }
        Ok(())
    }

    async fn handle_events(&mut self) -> Result<()> {
        // Short poll keeps the loop responsive without busy-waiting.
        if !event::poll(Duration::from_millis(150))? {
            return Ok(());
        }
        let Event::Key(key) = event::read()? else {
            return Ok(());
        };
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => {
                if !self.notes.is_empty() {
                    self.select(self.notes.len() - 1);
                }
            }
            KeyCode::Char('r') => self.refresh().await,
            KeyCode::Tab | KeyCode::Char('l') => {
                self.tl_idx = (self.tl_idx + 1) % TIMELINES.len();
                self.refresh().await;
            }
            KeyCode::BackTab | KeyCode::Char('h') => {
                self.tl_idx = (self.tl_idx + TIMELINES.len() - 1) % TIMELINES.len();
                self.refresh().await;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.notes.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.notes.len() as isize - 1);
        self.list_state.select(Some(next as usize));
    }

    fn select(&mut self, idx: usize) {
        if !self.notes.is_empty() {
            self.list_state.select(Some(idx.min(self.notes.len() - 1)));
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(1), // header
            Constraint::Min(1),    // timeline
            Constraint::Length(1), // footer
        ])
        .split(f.area());

        self.draw_header(f, chunks[0]);
        self.draw_timeline(f, chunks[1]);
        self.draw_footer(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let mut tabs: Vec<Span> = Vec::new();
        for (i, (_, label)) in TIMELINES.iter().enumerate() {
            let style = if i == self.tl_idx {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            tabs.push(Span::styled(format!(" {label} "), style));
            tabs.push(Span::raw(" "));
        }
        let acct = format!("@{}@{} ", self.account.username, self.host);
        let mut spans = vec![Span::styled(acct, Style::default().fg(Color::Yellow))];
        spans.extend(tabs);
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_timeline(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let width = area.width.saturating_sub(2) as usize; // borders
        let items: Vec<ListItem> = self
            .notes
            .iter()
            .map(|n| ListItem::new(note_lines(n, width.max(8))))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", TIMELINES[self.tl_idx].1)),
            )
            .highlight_style(Style::default().bg(Color::Rgb(40, 40, 60)))
            .highlight_symbol("▌");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let help = "q quit  j/k move  r refresh  Tab/h/l switch  g/G top/bottom";
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", self.status),
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::raw("  "),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }
}

/// Render a single note as a block of lines: header, body (wrapped), footer.
fn note_lines(note: &NormalizedNote, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // A pure renote (boost) carries no text of its own.
    if let Some(renote) = note.renote.as_deref() {
        if note.text.as_deref().unwrap_or("").is_empty() {
            lines.push(Line::from(Span::styled(
                format!("🔁 {} renoted", display_handle(note)),
                Style::default().fg(Color::Green),
            )));
            lines.extend(note_lines(renote, width.saturating_sub(2)));
            lines.push(Line::raw(""));
            return lines;
        }
    }

    lines.push(Line::from(vec![
        Span::styled(
            display_handle(note),
            Style::default().fg(Color::Cyan).bold(),
        ),
        Span::raw("  "),
        Span::styled(short_time(&note.created_at), Style::default().fg(Color::DarkGray)),
    ]));

    if let Some(cw) = note.cw.as_deref().filter(|s| !s.is_empty()) {
        lines.push(Line::from(Span::styled(
            format!("⚠ {cw}"),
            Style::default().fg(Color::Yellow),
        )));
    }

    let body = note.text.as_deref().unwrap_or("");
    for raw_line in body.split('\n') {
        for wrapped in wrap(raw_line, width) {
            lines.push(Line::raw(wrapped));
        }
    }

    let reactions: i64 = note.reactions.values().sum();
    lines.push(Line::from(Span::styled(
        format!(
            "↩ {}  🔁 {}  ❤ {}",
            note.replies_count, note.renote_count, reactions
        ),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));
    lines
}

/// "Display Name @user@host" for a note's author.
fn display_handle(note: &NormalizedNote) -> String {
    let u = &note.user;
    let name = u.name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&u.username);
    match u.host.as_deref() {
        Some(h) if !h.is_empty() => format!("{name} @{}@{}", u.username, h),
        _ => format!("{name} @{}", u.username),
    }
}

/// ISO-8601 ("2026-06-18T12:34:56.789Z") → "06-18 12:34". Best-effort slicing,
/// no date library needed for a display hint.
fn short_time(iso: &str) -> String {
    let date = iso.get(5..10).unwrap_or("");
    let time = iso.get(11..16).unwrap_or("");
    format!("{date} {time}").trim().to_string()
}

/// Greedy word-wrap on whitespace; falls back to hard splits for long tokens.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    out.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            line = chunk;
            continue;
        }
        let extra = if line.is_empty() { 0 } else { 1 };
        if line.chars().count() + extra + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
