//! TUI model and update logic (the "Model" and "Update" of TEA).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use super::theme::{self, Rgb};
use crate::agent::AgentKind;
use crate::api::AppState;
use crate::events::Event;
use crate::guard::ResourceSnapshot;
use crate::store::{Invite, NewInvite, NewToken, Request, Revoke, Stats, TokenUsage};
use crate::token::{Limits, Role, SessionMode, Token, TokenStatus};
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

/// Which agents a new token may use: every agent, or exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentChoice(pub Option<AgentKind>);

impl AgentChoice {
    /// Every agent Hashi supports.
    pub const EVERY: Self = Self(None);

    fn options() -> Vec<Self> {
        std::iter::once(Self::EVERY)
            .chain(AgentKind::ALL.into_iter().map(|a| Self(Some(a))))
            .collect()
    }

    fn next(self) -> Self {
        self.shift(1)
    }

    fn shift(self, by: isize) -> Self {
        let options = Self::options();
        let i = options.iter().position(|a| *a == self).unwrap_or(0);
        options[cycle(i, options.len(), by)]
    }

    /// Label for the form.
    #[must_use]
    pub fn label(self) -> String {
        match self.0 {
            None => "all agents".to_owned(),
            Some(agent) => format!("{} only", agent.as_str()),
        }
    }

    fn agents(self) -> Vec<AgentKind> {
        match self.0 {
            None => AgentKind::ALL.to_vec(),
            Some(agent) => vec![agent],
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
            agents: AgentChoice::EVERY,
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

/// Daily prompt cap choices for an invite.
const DAILY_PROMPTS: [Option<u32>; 4] = [None, Some(25), Some(100), Some(500)];
/// Daily budget choices for an invite, in USD.
const DAILY_BUDGET: [Option<f64>; 3] = [None, Some(5.0), Some(20.0)];
/// Token lifetime choices for an invite, in days.
const LIFETIME_DAYS: [Option<u32>; 4] = [None, Some(1), Some(7), Some(30)];
/// How long an invite link stays valid, in hours.
const LINK_HOURS: [u32; 3] = [1, 24, 168];
/// Prompts an invited token may have in flight at once.
const INVITE_MAX_PENDING: u32 = 3;

/// State of the "share" (invite) form.
#[derive(Debug, Clone)]
pub struct ShareForm {
    /// Label being typed: who the invite is for.
    pub label: String,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Selected agents.
    pub agents: AgentChoice,
    /// Selected session mode.
    pub mode: SessionMode,
    daily: usize,
    budget: usize,
    lifetime: usize,
    link: usize,
    /// Validation message shown under the label field.
    pub error: Option<String>,
}

impl ShareForm {
    /// Number of fields.
    pub const FIELDS: usize = 7;

    fn new() -> Self {
        Self {
            label: String::new(),
            field: 0,
            agents: AgentChoice::EVERY,
            mode: SessionMode::Stateless,
            daily: 0,
            budget: 0,
            lifetime: 0,
            link: 1,
            error: None,
        }
    }

    fn step(&mut self, by: isize) {
        match self.field {
            1 => self.agents = self.agents.shift(by),
            2 => {
                self.mode = match self.mode {
                    SessionMode::Stateless => SessionMode::Stateful,
                    SessionMode::Stateful => SessionMode::Stateless,
                }
            }
            3 => self.daily = cycle(self.daily, DAILY_PROMPTS.len(), by),
            4 => self.budget = cycle(self.budget, DAILY_BUDGET.len(), by),
            5 => self.lifetime = cycle(self.lifetime, LIFETIME_DAYS.len(), by),
            6 => self.link = cycle(self.link, LINK_HOURS.len(), by),
            _ => {}
        }
    }

    /// Daily prompt cap, as shown in the form.
    #[must_use]
    pub fn daily_label(&self) -> String {
        DAILY_PROMPTS
            .get(self.daily)
            .copied()
            .flatten()
            .map_or_else(|| "none".to_owned(), |n| format!("{n} a day"))
    }

    /// Daily budget, as shown in the form.
    #[must_use]
    pub fn budget_label(&self) -> String {
        DAILY_BUDGET
            .get(self.budget)
            .copied()
            .flatten()
            .map_or_else(|| "none".to_owned(), |b| format!("${b:.0} a day"))
    }

    /// Token lifetime, as shown in the form.
    #[must_use]
    pub fn lifetime_label(&self) -> &'static str {
        match LIFETIME_DAYS.get(self.lifetime).copied().flatten() {
            None => "never expires",
            Some(1) => "1 day",
            Some(7) => "7 days",
            Some(_) => "30 days",
        }
    }

    /// Link validity, as shown in the form.
    #[must_use]
    pub fn link_label(&self) -> &'static str {
        link_long(self.link_hours())
    }

    fn link_hours(&self) -> u32 {
        LINK_HOURS.get(self.link).copied().unwrap_or(24)
    }

    fn to_new_invite(&self, label: String) -> NewInvite {
        NewInvite {
            label,
            session_mode: self.mode,
            agents: self.agents.agents(),
            token_expires_in_days: LIFETIME_DAYS.get(self.lifetime).copied().flatten(),
            limits: Limits {
                daily_requests: DAILY_PROMPTS.get(self.daily).copied().flatten(),
                daily_budget_usd: DAILY_BUDGET.get(self.budget).copied().flatten(),
                max_pending: Some(INVITE_MAX_PENDING),
                ..Limits::default()
            },
            link_hours: self.link_hours(),
        }
    }
}

const fn link_long(hours: u32) -> &'static str {
    match hours {
        1 => "1 hour",
        24 => "24 hours",
        _ => "7 days",
    }
}

/// Short form of a link validity: `1h`, `24h`, `7d`.
#[must_use]
pub fn link_short(hours: u32) -> String {
    if hours >= 48 && hours.is_multiple_of(24) {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

/// Which table has focus in the Tokens view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokensFocus {
    /// The tokens table.
    Tokens,
    /// The pending invites table.
    Invites,
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
    /// Invite creation form.
    Share(ShareForm),
    /// A freshly created invite link.
    InviteReveal {
        /// Who the invite is for.
        label: String,
        /// The full claim URL.
        url: String,
        /// How long the link stays valid.
        link_hours: u32,
        /// Set when there is no public URL, so the link only works locally.
        local_only: bool,
    },
    /// Invite cancellation confirmation.
    ConfirmCancelInvite {
        /// Invite id.
        id: String,
        /// Invite label.
        label: String,
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
    /// Recent requests, newest first. Prompts and errors are cut short and
    /// responses left out; [`App::selected_request`] has the full row.
    pub recent: Vec<Request>,
    /// All tokens.
    pub tokens: Vec<Token>,
    /// Today's and this month's use, by token id.
    pub usage: HashMap<String, TokenUsage>,
    /// Invites not yet claimed or expired, newest first.
    pub invites: Vec<Invite>,
    /// Memory, run slots and admission state, from the resource guard.
    pub resources: ResourceSnapshot,
    /// Activity feed, newest first.
    pub feed: VecDeque<FeedItem>,
    /// Selection in the requests table.
    pub requests_table: TableState,
    /// Selection in the tokens table.
    pub tokens_table: TableState,
    /// Selection in the pending invites table.
    pub invites_table: TableState,
    /// Which table in the Tokens view has focus.
    pub tokens_focus: TokensFocus,
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
    /// The request detail pane, kept between frames.
    pub detail_cache: super::ui::DetailCache,
    last_refresh: Option<Instant>,
    /// The selected request in full, for the detail pane.
    pub(crate) detail: Option<Request>,
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
            usage: HashMap::new(),
            invites: Vec::new(),
            resources: ResourceSnapshot::default(),
            feed: VecDeque::new(),
            requests_table: TableState::default().with_selected(Some(0)),
            tokens_table: TableState::default().with_selected(Some(0)),
            invites_table: TableState::default().with_selected(Some(0)),
            tokens_focus: TokensFocus::Tokens,
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
            detail_cache: super::ui::DetailCache::default(),
            last_refresh: None,
            detail: None,
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
            let now = now_ms();
            let tokens = store.list_tokens()?;
            let mut usage = HashMap::new();
            for tk in &tokens {
                usage.insert(tk.id.clone(), store.token_usage(&tk.id, now)?);
            }
            let invites: Vec<Invite> = store
                .list_invites()?
                .into_iter()
                .filter(|i| i.status(now) == "pending")
                .collect();
            Ok::<_, crate::store::StoreError>((
                stats,
                per,
                store.recent_requests_brief(RECENT_LIMIT)?,
                tokens,
                usage,
                invites,
            ))
        })();
        match loaded {
            Ok((stats, per, recent, tokens, usage, invites)) => {
                self.stats = stats;
                self.agent_stats = per;
                self.recent = recent;
                self.tokens = tokens;
                self.usage = usage;
                self.invites = invites;
                clamp(&mut self.requests_table, self.recent.len());
                clamp(&mut self.tokens_table, self.tokens.len());
                clamp(&mut self.invites_table, self.invites.len());
                if self.invites.is_empty() {
                    self.tokens_focus = TokensFocus::Tokens;
                }
                self.load_detail();
            }
            Err(e) => self.notify(format!("refresh failed: {e}")),
        }
        self.resources = self.state.guard.snapshot();
        self.last_refresh = Some(Instant::now());
    }

    /// Whether something on screen changes by itself: a running request
    /// (spinner and elapsed time), the tunnel spinner, a toast waiting to
    /// expire or the cursor blink in a form. When this is false the
    /// dashboard only redraws on input, events and data refreshes.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.stats.running > 0
            || self
                .recent
                .iter()
                .any(|r| r.status == crate::store::RequestStatus::Running)
            || matches!(self.tunnel, TunnelState::Starting)
            || self.toast.is_some()
            || matches!(self.popup, Some(Popup::Share(_) | Popup::NewToken(_)))
    }

    /// Advances animations and refreshes data periodically. Returns whether
    /// the screen needs a redraw.
    pub fn on_tick(&mut self) -> bool {
        let animating = self.animating();
        if animating && !self.reduced_motion {
            self.tick = self.tick.wrapping_add(1);
        }
        let mut redraw = animating;
        if self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(750))
        {
            self.refresh();
            redraw = true;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
        {
            self.toast = None;
            redraw = true;
        }
        redraw
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
            Event::Held { reason, detail } => FeedItem {
                at: now_ms(),
                glyph: "!",
                color: theme::AMBER,
                text: format!("held: {reason}, queued work waits ({detail})"),
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
            KeyCode::Char('s') if self.tab == Tab::Tokens => {
                self.popup = Some(Popup::Share(ShareForm::new()));
            }
            KeyCode::Char('i') if self.tab == Tab::Tokens => {
                if self.invites.is_empty() {
                    self.notify("no pending invites");
                } else {
                    self.tokens_focus = match self.tokens_focus {
                        TokensFocus::Tokens => TokensFocus::Invites,
                        TokensFocus::Invites => TokensFocus::Tokens,
                    };
                }
            }
            KeyCode::Char('x')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Invites =>
            {
                if let Some(inv) = self.selected_invite() {
                    self.popup = Some(Popup::ConfirmCancelInvite {
                        id: inv.id.clone(),
                        label: inv.label.clone(),
                    });
                }
            }
            KeyCode::Char('p')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Tokens =>
            {
                self.toggle_pause();
            }
            KeyCode::Char('d')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Tokens =>
            {
                if let Some(t) = self.selected_token() {
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
            Popup::ConfirmCancelInvite { id, label } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.cancel_invite(&id, &label);
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmCancelInvite { id, label }),
            },
            Popup::InviteReveal {
                label,
                url,
                link_hours,
                local_only,
            } => {
                match key.code {
                    KeyCode::Char('c') => self.copy(&url, "invite link copied to clipboard"),
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => return None,
                    _ => {}
                }
                Some(Popup::InviteReveal {
                    label,
                    url,
                    link_hours,
                    local_only,
                })
            }
            Popup::Share(mut form) => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => return self.create_invite(form),
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % ShareForm::FIELDS
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = (form.field + ShareForm::FIELDS - 1) % ShareForm::FIELDS;
                    }
                    KeyCode::Backspace if form.field == 0 => {
                        form.label.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == 0 => {
                        if form.label.chars().count() < 100 && !c.is_control() {
                            form.label.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Left => form.step(-1),
                    KeyCode::Right | KeyCode::Char(' ') => form.step(1),
                    _ => {}
                }
                Some(Popup::Share(form))
            }
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
            limits: NewToken::default_limits(form.role),
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

    fn create_invite(&mut self, mut form: ShareForm) -> Option<Popup> {
        let label = form.label.trim().to_owned();
        if label.is_empty() {
            form.field = 0;
            form.error = Some("Label is required, for example alice".to_owned());
            return Some(Popup::Share(form));
        }
        let new = form.to_new_invite(label);
        match self.state.store.create_invite(&new) {
            Ok((invite, code)) => {
                self.audit("invite.create", &invite.label, link_long(new.link_hours));
                self.state
                    .bus
                    .log(format!("invite created: {}", invite.label));
                self.refresh();
                Some(Popup::InviteReveal {
                    url: format!("{}/claim/{code}", self.state.runtime.base_url()),
                    label: invite.label,
                    link_hours: new.link_hours,
                    local_only: self.state.runtime.public_url().is_none(),
                })
            }
            Err(e) => {
                self.notify(format!("could not create invite: {e}"));
                None
            }
        }
    }

    fn cancel_invite(&mut self, id: &str, label: &str) {
        match self.state.store.cancel_invite(id) {
            Ok(true) => {
                self.audit("invite.cancel", label, "");
                self.state.bus.log(format!("invite cancelled: {label}"));
                self.notify(format!("cancelled invite for {label}"));
            }
            Ok(false) => self.notify("invite already claimed or expired"),
            Err(e) => self.notify(format!("cancel failed: {e}")),
        }
        self.refresh();
    }

    fn toggle_pause(&mut self) {
        let Some(mut tk) = self.selected_token().cloned() else {
            return;
        };
        let now = now_ms();
        if !tk.paused && tk.is_admin() && tk.status(now) == TokenStatus::Active {
            let others = self
                .tokens
                .iter()
                .filter(|o| o.id != tk.id && o.is_admin() && o.status(now) == TokenStatus::Active)
                .count();
            if others == 0 {
                self.notify("cannot pause the last active admin token");
                return;
            }
        }
        tk.paused = !tk.paused;
        let detail = if tk.paused { "paused" } else { "resumed" };
        match self.state.store.save_token_policy(&tk) {
            Ok(()) => {
                if !tk.paused {
                    self.state.dispatcher.wake_all();
                }
                self.audit("token.update", &tk.name, detail);
                self.state.bus.log(format!("token {detail}: {}", tk.name));
                self.notify(format!("{detail} {}", tk.name));
            }
            Err(e) => self.notify(format!("could not update {}: {e}", tk.name)),
        }
        self.refresh();
    }

    fn audit(&mut self, action: &str, target: &str, detail: &str) {
        if let Err(e) = self.state.store.audit("dashboard", action, target, detail) {
            self.notify(format!("audit log failed: {e}"));
        }
    }

    fn selected_token(&self) -> Option<&Token> {
        self.tokens_table
            .selected()
            .and_then(|i| self.tokens.get(i))
    }

    /// The invite highlighted in the pending invites table.
    #[must_use]
    pub fn selected_invite(&self) -> Option<&Invite> {
        self.invites_table
            .selected()
            .and_then(|i| self.invites.get(i))
    }

    fn revoke(&mut self, id: &str, name: &str) {
        match self.state.store.revoke_token(id) {
            Ok(Revoke::Revoked(token_id)) => {
                self.state.bus.log(format!("token revoked: {name}"));
                self.notify(format!("revoked {name}"));
                // Stop its runs first, then delete the workspace once they
                // are gone. The dashboard must not block, so the wait runs
                // as a task.
                let stopped = self.state.dispatcher.cancel_token_runs(&token_id);
                let home = self.state.store.data_dir();
                let bus = self.state.bus.clone();
                let name = name.to_owned();
                let cleanup = move || {
                    if let Some(home) = home
                        && let Err(e) = crate::server::remove_workspace(&home, &token_id)
                    {
                        bus.log(format!("could not delete the workspace of {name}: {e}"));
                    }
                };
                match tokio::runtime::Handle::try_current() {
                    Ok(rt) => {
                        rt.spawn(async move {
                            stopped.await;
                            let _ = tokio::task::spawn_blocking(cleanup).await;
                        });
                    }
                    // No runtime means no workers either, so nothing runs.
                    Err(_) => cleanup(),
                }
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
            Tab::Tokens => match self.tokens_focus {
                TokensFocus::Tokens => (&mut self.tokens_table, self.tokens.len()),
                TokensFocus::Invites => (&mut self.invites_table, self.invites.len()),
            },
            _ => return,
        };
        if len == 0 {
            return;
        }
        let cur = table.selected().unwrap_or(0).cast_signed();
        let next = (cur + by).clamp(0, len.cast_signed() - 1);
        table.select(Some(next.cast_unsigned()));
        self.detail_scroll = 0;
        if self.tab == Tab::Requests {
            self.load_detail();
        }
    }

    /// Loads the selected request with its full prompt and response.
    fn load_detail(&mut self) {
        let id = self
            .requests_table
            .selected()
            .and_then(|i| self.recent.get(i))
            .map(|r| r.id.clone());
        self.detail = id.and_then(|id| self.state.store.get_request(&id).ok().flatten());
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

    /// The request highlighted in the requests table, in full when loaded.
    #[must_use]
    pub fn selected_request(&self) -> Option<&Request> {
        let brief = self
            .requests_table
            .selected()
            .and_then(|i| self.recent.get(i))?;
        match &self.detail {
            Some(full) if full.id == brief.id => Some(full),
            _ => Some(brief),
        }
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

    /// Agents to show on the overview: the installed ones, or Claude Code and
    /// Codex when none are, so a fresh install still shows what to expect.
    #[must_use]
    pub fn shown_agents(&self) -> Vec<AgentKind> {
        let installed: Vec<AgentKind> = AgentKind::ALL
            .into_iter()
            .filter(|a| {
                self.state
                    .runtime
                    .agents
                    .iter()
                    .any(|i| i.agent == *a && i.available)
            })
            .collect();
        if installed.is_empty() {
            vec![AgentKind::Claude, AgentKind::Codex]
        } else {
            installed
        }
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

fn cycle(i: usize, len: usize, by: isize) -> usize {
    let n = len.max(1).cast_signed();
    (i.cast_signed() + by).rem_euclid(n).cast_unsigned()
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
