//! TUI model and update logic (the "Model" and "Update" of TEA).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use super::theme::{self, Rgb};
use crate::agent::AgentKind;
use crate::api::AppState;
use crate::events::Event;
use crate::store::{NewToken, Request, Revoke, Stats};
use crate::token::{Role, SessionMode, Token};
use crate::util::{now_ms, one_line, truncate};

/// Top-level views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Live summary.
    Overview,
    /// Request history with details.
    Requests,
    /// Token management.
    Tokens,
    /// Event log.
    Logs,
}

impl Tab {
    /// Display order.
    pub const ALL: [Self; 4] = [Self::Overview, Self::Requests, Self::Tokens, Self::Logs];

    /// Tab label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Requests => "Requests",
            Self::Tokens => "Tokens",
            Self::Logs => "Logs",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn shift(self, by: isize) -> Self {
        let n = Self::ALL.len().cast_signed();
        let i = (self.index().cast_signed() + by).rem_euclid(n);
        Self::ALL[i.cast_unsigned()]
    }
}

/// Which agents a new token may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentChoice {
    /// Claude Code and Codex.
    Both,
    /// Claude Code only.
    Claude,
    /// Codex only.
    Codex,
}

impl AgentChoice {
    fn next(self) -> Self {
        match self {
            Self::Both => Self::Claude,
            Self::Claude => Self::Codex,
            Self::Codex => Self::Both,
        }
    }

    /// Label for the form.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Both => "claude + codex",
            Self::Claude => "claude only",
            Self::Codex => "codex only",
        }
    }

    fn agents(self) -> Vec<AgentKind> {
        match self {
            Self::Both => AgentKind::ALL.to_vec(),
            Self::Claude => vec![AgentKind::Claude],
            Self::Codex => vec![AgentKind::Codex],
        }
    }
}

/// State of the "new token" form.
#[derive(Debug, Clone)]
pub struct TokenForm {
    /// Name being typed.
    pub name: String,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Selected role.
    pub role: Role,
    /// Selected session mode.
    pub mode: SessionMode,
    /// Selected agents.
    pub agents: AgentChoice,
    /// Validation message shown under the name field.
    pub error: Option<String>,
}

impl TokenForm {
    /// Number of fields.
    pub const FIELDS: usize = 4;

    fn new() -> Self {
        Self {
            name: String::new(),
            field: 0,
            role: Role::User,
            mode: SessionMode::Stateless,
            agents: AgentChoice::Both,
            error: None,
        }
    }

    fn toggle(&mut self) {
        match self.field {
            1 => {
                self.role = if self.role == Role::User {
                    Role::Admin
                } else {
                    Role::User
                }
            }
            2 => {
                self.mode = match self.mode {
                    SessionMode::Stateless => SessionMode::Stateful,
                    SessionMode::Stateful => SessionMode::Stateless,
                }
            }
            3 => self.agents = self.agents.next(),
            _ => {}
        }
    }
}

/// Modal overlays.
#[derive(Debug, Clone)]
pub enum Popup {
    /// Key binding reference.
    Help,
    /// A secret shown exactly once.
    Reveal {
        /// Heading.
        title: String,
        /// The secret.
        secret: String,
    },
    /// Token creation form.
    NewToken(TokenForm),
    /// Quit confirmation, shown while requests are still in flight.
    ConfirmQuit {
        /// Requests queued or running.
        pending: u64,
    },
    /// Revoke confirmation.
    ConfirmRevoke {
        /// Token id.
        id: String,
        /// Token name.
        name: String,
    },
}

/// Public tunnel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    /// Not requested.
    Off,
    /// Waiting for cloudflared.
    Starting,
    /// Serving at this URL.
    Up(String),
    /// Failed with this reason.
    Down(String),
}

/// One line in the activity feed.
#[derive(Debug, Clone)]
pub struct FeedItem {
    /// When it happened (Unix ms).
    pub at: i64,
    /// Leading symbol.
    pub glyph: &'static str,
    /// Symbol colour.
    pub color: Rgb,
    /// Message.
    pub text: String,
}

/// The whole UI model.
#[derive(Debug)]
pub struct App {
    /// Server handles.
    pub state: AppState,
    /// Active tab.
    pub tab: Tab,
    /// Animation counter, bumped every tick.
    pub tick: u64,
    /// Global counters.
    pub stats: Stats,
    /// Counters per agent.
    pub agent_stats: HashMap<AgentKind, Stats>,
    /// Recent requests, newest first.
    pub recent: Vec<Request>,
    /// All tokens.
    pub tokens: Vec<Token>,
    /// Activity feed, newest first.
    pub feed: VecDeque<FeedItem>,
    /// Selection in the requests table.
    pub requests_table: TableState,
    /// Selection in the tokens table.
    pub tokens_table: TableState,
    /// Scroll offset of the request detail pane.
    pub detail_scroll: u16,
    /// Active modal.
    pub popup: Option<Popup>,
    /// Transient message and when it was raised.
    pub toast: Option<(String, Instant)>,
    /// Tunnel status.
    pub tunnel: TunnelState,
    /// Set when the user asks to quit.
    pub should_quit: bool,
    /// Freeze spinners and other animation (`HASHI_REDUCED_MOTION=1`).
    pub reduced_motion: bool,
    last_refresh: Option<Instant>,
}

const FEED_CAP: usize = 500;
const RECENT_LIMIT: u32 = 300;

impl App {
    /// Creates the model. `bootstrap` is an admin secret to reveal on first run.
    #[must_use]
    pub fn new(state: AppState, bootstrap: Option<String>, tunnel: bool) -> Self {
        let mut app = Self {
            state,
            tab: Tab::Overview,
            tick: 0,
            stats: Stats::default(),
            agent_stats: HashMap::new(),
            recent: Vec::new(),
            tokens: Vec::new(),
            feed: VecDeque::new(),
            requests_table: TableState::default().with_selected(Some(0)),
            tokens_table: TableState::default().with_selected(Some(0)),
            detail_scroll: 0,
            popup: bootstrap.map(|secret| Popup::Reveal {
                title: "Your admin token".to_owned(),
                secret,
            }),
            toast: None,
            tunnel: if tunnel {
                TunnelState::Starting
            } else {
                TunnelState::Off
            },
            should_quit: false,
            reduced_motion: std::env::var("HASHI_REDUCED_MOTION").is_ok_and(|v| v != "0"),
            last_refresh: None,
        };
        app.refresh();
        app
    }

    /// Reloads data from the store.
    pub fn refresh(&mut self) {
        let store = &self.state.store;
        let loaded = (|| {
            let stats = store.stats()?;
            let mut per = HashMap::new();
            for a in AgentKind::ALL {
                per.insert(a, store.agent_stats(a)?);
            }
            Ok::<_, crate::store::StoreError>((
                stats,
                per,
                store.recent_requests(RECENT_LIMIT)?,
                store.list_tokens()?,
            ))
        })();
        match loaded {
            Ok((stats, per, recent, tokens)) => {
                self.stats = stats;
                self.agent_stats = per;
                self.recent = recent;
                self.tokens = tokens;
                clamp(&mut self.requests_table, self.recent.len());
                clamp(&mut self.tokens_table, self.tokens.len());
            }
            Err(e) => self.notify(format!("refresh failed: {e}")),
        }
        self.last_refresh = Some(Instant::now());
    }

    /// Advances animations and refreshes data periodically.
    pub fn on_tick(&mut self) {
        if !self.reduced_motion {
            self.tick = self.tick.wrapping_add(1);
        }
        if self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(750))
        {
            self.refresh();
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
        {
            self.toast = None;
        }
    }

    /// Folds a bus event into the model.
    pub fn on_event(&mut self, event: Event) {
        let item = match event {
            Event::Queued { id, agent, token } => FeedItem {
                at: now_ms(),
                glyph: "◌",
                color: theme::MIST,
                text: format!(
                    "{} queued for {} by {token}",
                    short(&id),
                    agent.display_name()
                ),
            },
            Event::Started { id, agent } => FeedItem {
                at: now_ms(),
                glyph: theme::agent_glyph(agent),
                color: theme::agent_color(agent),
                text: format!("{} picked up {}", agent.display_name(), short(&id)),
            },
            Event::Finished {
                id,
                agent,
                ok: true,
                duration_ms,
                ..
            } => FeedItem {
                at: now_ms(),
                glyph: "✓",
                color: theme::MINT,
                text: format!(
                    "{} answered {} in {}",
                    agent.display_name(),
                    short(&id),
                    crate::util::human_duration(Duration::from_millis(duration_ms))
                ),
            },
            Event::Finished {
                id,
                agent,
                ok: false,
                error,
                ..
            } => FeedItem {
                at: now_ms(),
                glyph: "✗",
                color: theme::ROSE,
                text: format!(
                    "{} failed {}: {}",
                    agent.display_name(),
                    short(&id),
                    truncate(&one_line(error.as_deref().unwrap_or("unknown error")), 120)
                ),
            },
            Event::TunnelReady(url) => {
                self.tunnel = TunnelState::Up(url.clone());
                FeedItem {
                    at: now_ms(),
                    glyph: "⌁",
                    color: theme::VIOLET,
                    text: format!("tunnel live at {url}"),
                }
            }
            Event::TunnelDown(reason) => {
                self.tunnel = TunnelState::Down(reason.clone());
                FeedItem {
                    at: now_ms(),
                    glyph: "!",
                    color: theme::AMBER,
                    text: format!("tunnel: {reason}"),
                }
            }
            Event::Log(text) => FeedItem {
                at: now_ms(),
                glyph: "·",
                color: theme::MIST,
                text,
            },
        };
        self.feed.push_front(item);
        self.feed.truncate(FEED_CAP);
        // Finishing work changes counters; show it immediately.
        self.last_refresh = None;
    }

    /// Handles a key press.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if let Some(popup) = self.popup.take() {
            self.popup = self.on_popup_key(popup, key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                // Quitting stops the bridge, so ask first when work would be lost.
                let pending = self.stats.running + self.stats.queued;
                if pending > 0 {
                    self.popup = Some(Popup::ConfirmQuit { pending });
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.set_tab(self.tab.shift(1)),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.set_tab(self.tab.shift(-1))
            }
            KeyCode::Char(c @ '1'..='4') => {
                let i = usize::from(u8::try_from(c).unwrap_or(b'1') - b'1');
                self.set_tab(Tab::ALL[i]);
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown | KeyCode::Char('J') => {
                self.detail_scroll = self.detail_scroll.saturating_add(5)
            }
            KeyCode::PageUp | KeyCode::Char('K') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5)
            }
            KeyCode::Char('c') => {
                let url = self.state.runtime.base_url();
                self.copy(&url, "URL copied");
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.notify("refreshed");
            }
            KeyCode::Char('n') if self.tab == Tab::Tokens => {
                self.popup = Some(Popup::NewToken(TokenForm::new()))
            }
            KeyCode::Char('d') if self.tab == Tab::Tokens => {
                if let Some(t) = self
                    .tokens_table
                    .selected()
                    .and_then(|i| self.tokens.get(i))
                {
                    self.popup = Some(Popup::ConfirmRevoke {
                        id: t.id.clone(),
                        name: t.name.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn on_popup_key(&mut self, popup: Popup, key: KeyEvent) -> Option<Popup> {
        match popup {
            Popup::Help => None,
            Popup::Reveal { title, secret } => match key.code {
                KeyCode::Char('c') => {
                    self.copy(&secret, "token copied to clipboard");
                    Some(Popup::Reveal { title, secret })
                }
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => None,
                _ => Some(Popup::Reveal { title, secret }),
            },
            Popup::ConfirmQuit { pending } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.should_quit = true;
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmQuit { pending }),
            },
            Popup::ConfirmRevoke { id, name } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.revoke(&id, &name);
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmRevoke { id, name }),
            },
            Popup::NewToken(mut form) => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => return self.create_token(form),
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % TokenForm::FIELDS
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = (form.field + TokenForm::FIELDS - 1) % TokenForm::FIELDS;
                    }
                    KeyCode::Backspace if form.field == 0 => {
                        form.name.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == 0 => {
                        if form.name.chars().count() < 100 && !c.is_control() {
                            form.name.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => form.toggle(),
                    _ => {}
                }
                Some(Popup::NewToken(form))
            }
        }
    }

    fn create_token(&mut self, mut form: TokenForm) -> Option<Popup> {
        let name = form.name.trim().to_owned();
        if name.is_empty() {
            form.field = 0;
            form.error = Some("Name is required, for example ci-bot".to_owned());
            return Some(Popup::NewToken(form));
        }
        let new = NewToken {
            name,
            role: form.role,
            session_mode: form.mode,
            agents: form.agents.agents(),
            expires_in_days: None,
            note: None,
            limits: crate::token::Limits::default(),
        };
        match self.state.store.create_token(&new) {
            Ok((token, secret)) => {
                self.state
                    .bus
                    .log(format!("token created: {} ({})", token.name, token.prefix));
                self.refresh();
                Some(Popup::Reveal {
                    title: format!("Token for {}", token.name),
                    secret: secret.raw,
                })
            }
            Err(e) => {
                self.notify(format!("could not create token: {e}"));
                None
            }
        }
    }

    fn revoke(&mut self, id: &str, name: &str) {
        match self.state.store.revoke_token(id) {
            Ok(Revoke::Revoked) => {
                self.state.bus.log(format!("token revoked: {name}"));
                self.notify(format!("revoked {name}"));
            }
            Ok(Revoke::LastAdmin) => self.notify("cannot revoke the last admin token"),
            Ok(Revoke::NotFound) => self.notify("token already gone"),
            Err(e) => self.notify(format!("revoke failed: {e}")),
        }
        self.refresh();
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.detail_scroll = 0;
    }

    fn move_selection(&mut self, by: isize) {
        let (table, len) = match self.tab {
            Tab::Requests => (&mut self.requests_table, self.recent.len()),
            Tab::Tokens => (&mut self.tokens_table, self.tokens.len()),
            _ => return,
        };
        if len == 0 {
            return;
        }
        let cur = table.selected().unwrap_or(0).cast_signed();
        let next = (cur + by).clamp(0, len.cast_signed() - 1);
        table.select(Some(next.cast_unsigned()));
        self.detail_scroll = 0;
    }

    /// Shows a transient message in the footer.
    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn copy(&mut self, text: &str, done: &str) {
        match super::clipboard::copy(text) {
            Ok(()) => self.notify(done),
            Err(e) => self.notify(format!("copy failed: {e}")),
        }
    }

    /// The request highlighted in the requests table.
    #[must_use]
    pub fn selected_request(&self) -> Option<&Request> {
        self.requests_table
            .selected()
            .and_then(|i| self.recent.get(i))
    }

    /// How long the oldest running request for `agent` has been running, in ms.
    #[must_use]
    pub fn running_for(&self, agent: AgentKind) -> Option<u64> {
        let now = now_ms();
        self.recent
            .iter()
            .filter(|r| r.agent == agent && r.status == crate::store::RequestStatus::Running)
            .filter_map(|r| r.started_at)
            .min()
            .and_then(|s| u64::try_from(now - s).ok())
    }

    /// Recent completed durations for `agent`, oldest first, for sparklines.
    #[must_use]
    pub fn latencies(&self, agent: AgentKind, n: usize) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .recent
            .iter()
            .filter(|r| r.agent == agent)
            .filter_map(Request::duration_ms)
            .filter_map(|d| u64::try_from(d).ok())
            .take(n)
            .collect();
        v.reverse();
        v
    }

    /// Finished requests per `bucket` over the last `buckets` windows, oldest first.
    #[must_use]
    pub fn throughput(&self, bucket: Duration, buckets: usize) -> Vec<u64> {
        let mut out = vec![0_u64; buckets];
        let now = now_ms();
        let width = i64::try_from(bucket.as_millis()).unwrap_or(1).max(1);
        for done in self.recent.iter().filter_map(|r| r.completed_at) {
            let age = (now - done) / width;
            if let Ok(age) = usize::try_from(age)
                && age < buckets
            {
                out[buckets - 1 - age] += 1;
            }
        }
        out
    }
}

fn clamp(table: &mut TableState, len: usize) {
    match (table.selected(), len) {
        (_, 0) => table.select(None),
        (None, _) => table.select(Some(0)),
        (Some(i), n) if i >= n => table.select(Some(n - 1)),
        _ => {}
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
