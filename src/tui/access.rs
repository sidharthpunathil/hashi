//! Dashboard pieces for access requests: the list, the accept popup and
//! the decline popup.
//!
//! Everything here is self-contained so the dashboard can place it where it
//! likes: [`render_list`], [`render_accept`] and [`render_decline`] draw into
//! a given area, and [`AcceptForm`] and [`DeclineForm`] hold their own state,
//! take keys and decide through [`crate::access::accept`] and
//! [`crate::access::decline`] with actor `dashboard`, the same functions the
//! CLI and the HTTP API use. Every string from a requester is shown through
//! [`crate::access::clean_text`] and the terminal sanitiser, so control and
//! bidi characters never reach the terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use super::theme as t;
use crate::access::{self, AcceptInput, Accepted};
use crate::agent::AgentKind;
use crate::store::{AccessKind, AccessRequest, Store};
use crate::token::Limits;
use crate::util::{human_age, truncate};

/// Actor recorded in the audit log for decisions made here.
pub const ACTOR: &str = "dashboard";

/// `s` safe to draw on one line.
fn safe(s: &str) -> String {
    crate::diagnostics::neutralize_controls(&access::clean_text(s, false), false)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn modal(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t::fg(t::PURPLE))
        .title(Span::styled(
            format!(" {} ", super::one_line(title)),
            t::bold(t::PINK),
        ))
}

fn state_style(state: &str) -> Style {
    match state {
        "pending" => t::fg(t::AMBER),
        "accepted" => t::fg(t::MINT),
        "declined" | "expired" => t::fg(t::ROSE),
        _ => t::muted(),
    }
}

/// The badge text for the tab bar: the number of pending requests, or
/// `None` when there are none.
#[must_use]
pub fn badge(pending: usize) -> Option<String> {
    (pending > 0).then(|| {
        if pending > 99 {
            "99+".to_owned()
        } else {
            pending.to_string()
        }
    })
}

/// The toast to show for a new request event.
#[must_use]
pub fn toast_text(kind: AccessKind, name: &str) -> String {
    match kind {
        AccessKind::New => format!("{} asks for access", safe(name)),
        AccessKind::More => format!("{} asks for more", safe(name)),
    }
}

/// Draws the list of requests in `area`, highlighting `selected`.
pub fn render_list(
    frame: &mut Frame,
    area: Rect,
    requests: &[AccessRequest],
    selected: Option<usize>,
    now_ms: i64,
) {
    let header = Row::new(["status", "kind", "name", "age", "what"]).style(t::muted());
    let rows: Vec<Row> = requests
        .iter()
        .map(|r| {
            let state = r.state(now_ms);
            let mut what = safe(&access::asked_text(r));
            if !r.message.is_empty() {
                what = format!("{what} \u{b7} \"{}\"", truncate(&safe(&r.message), 60));
            }
            Row::new(vec![
                Cell::from(Span::styled(state, state_style(state))),
                Cell::from(Span::styled(r.kind.as_str(), t::muted())),
                Cell::from(Span::raw(truncate(&safe(&r.name), 20))),
                Cell::from(Span::styled(human_age(r.created_at), t::muted())),
                Cell::from(Span::raw(what)),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Length(20),
            Constraint::Length(5),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED))
    .highlight_symbol(Span::styled("\u{258c}", t::fg(t::PINK)))
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(t::border())
            .title(Span::styled(" requests for access ", t::bold(t::VIOLET))),
    );
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
    if empty && area.height > 4 {
        let hint = Paragraph::new(vec![
            Line::from(Span::styled(
                "No requests",
                Style::new().add_modifier(Modifier::BOLD),
            ))
            .centered(),
            Line::from(Span::styled(
                "Open the ask-access link with `hashi requests open`",
                t::muted(),
            ))
            .centered(),
        ]);
        let inner = Rect {
            y: area.y + area.height / 2,
            height: 2,
            ..area
        };
        frame.render_widget(hint, inner);
    }
}

/// What a key did to a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    /// Still editing.
    Edit,
    /// Enter: decide now.
    Submit,
    /// Esc: close without deciding.
    Cancel,
}

fn edit_text(text: &mut String, key: KeyEvent, max: usize) {
    match key.code {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !c.is_control()
                && text.chars().count() < max =>
        {
            text.push(c);
        }
        _ => {}
    }
}

const LINK_HOURS: [u32; 3] = [1, 24, 168];

/// The accept popup: the prefilled values, editable, and for a "more"
/// request the token's current access next to them.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptForm {
    /// The request.
    pub request: AccessRequest,
    /// Field with the cursor.
    pub field: usize,
    /// Token name (new people).
    pub label: String,
    /// Agents, comma separated.
    pub agents: String,
    /// Prompts per day; empty for none.
    pub daily: String,
    /// Prompts per month; empty for none.
    pub monthly: String,
    /// Budget per day in USD; empty for none.
    pub daily_budget: String,
    /// Budget per month in USD; empty for none.
    pub monthly_budget: String,
    /// Token lifetime in days; empty to leave it.
    pub expiry_days: String,
    /// Invite validity, an index into 1, 24 and 168 hours (new people).
    pub link_hours: usize,
    /// The token's current access, shown for "more" requests.
    pub current: Option<access::Current>,
    /// The last error.
    pub error: Option<String>,
}

fn num<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

impl AcceptForm {
    /// Field indexes.
    const FIELDS: usize = 8;

    /// The form for `request`, filled from [`access::prefill`].
    ///
    /// # Errors
    ///
    /// Fails when the request's token is gone, or on database errors.
    pub fn new(store: &Store, request: &AccessRequest) -> Result<Self, String> {
        let pre = access::prefill(store, request).map_err(|e| e.to_string())?;
        Ok(Self {
            request: request.clone(),
            field: 0,
            label: safe(&pre.label),
            agents: crate::token::encode_agents(&pre.agents),
            daily: num(pre.limits.daily_requests),
            monthly: num(pre.limits.monthly_requests),
            daily_budget: num(pre.limits.daily_budget_usd),
            monthly_budget: num(pre.limits.monthly_budget_usd),
            expiry_days: num(pre.expires_in_days),
            link_hours: 2,
            current: pre.current,
            error: None,
        })
    }

    fn is_new(&self) -> bool {
        self.request.kind == AccessKind::New
    }

    /// Up and down move between fields, left and right change the link
    /// validity, typing edits the text fields.
    pub fn on_key(&mut self, key: KeyEvent) -> FormAction {
        if key.kind == KeyEventKind::Release {
            return FormAction::Edit;
        }
        match key.code {
            KeyCode::Esc => return FormAction::Cancel,
            KeyCode::Enter => return FormAction::Submit,
            KeyCode::Up | KeyCode::BackTab => {
                self.field = (self.field + Self::FIELDS - 1) % Self::FIELDS;
            }
            KeyCode::Down | KeyCode::Tab => self.field = (self.field + 1) % Self::FIELDS,
            KeyCode::Left if self.field == 7 => {
                self.link_hours = (self.link_hours + LINK_HOURS.len() - 1) % LINK_HOURS.len();
            }
            KeyCode::Right if self.field == 7 => {
                self.link_hours = (self.link_hours + 1) % LINK_HOURS.len();
            }
            _ => {
                let (text, max) = match self.field {
                    0 => (&mut self.label, access::MAX_NAME_CHARS),
                    1 => (&mut self.agents, 80),
                    2 => (&mut self.daily, 9),
                    3 => (&mut self.monthly, 9),
                    4 => (&mut self.daily_budget, 12),
                    5 => (&mut self.monthly_budget, 12),
                    6 => (&mut self.expiry_days, 4),
                    _ => return FormAction::Edit,
                };
                edit_text(text, key, max);
            }
        }
        FormAction::Edit
    }

    /// The owner's values, as [`access::accept`] takes them. Every field is
    /// sent, so what is applied is exactly what the form shows.
    ///
    /// # Errors
    ///
    /// Names the field that does not parse.
    pub fn to_input(&self) -> Result<AcceptInput, String> {
        fn opt<T: std::str::FromStr>(s: &str, what: &str) -> Result<Option<T>, String> {
            let s = s.trim().trim_start_matches('$');
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse()
                    .map(Some)
                    .map_err(|_| format!("{what} is not a number"))
            }
        }
        let mut agents = Vec::new();
        for a in self
            .agents
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            agents.push(
                a.parse::<AgentKind>()
                    .map_err(|_| format!("unknown agent {}", safe(a)))?,
            );
        }
        if agents.is_empty() {
            return Err("name at least one agent".to_owned());
        }
        let limits = Limits {
            daily_requests: opt(&self.daily, "prompts a day")?,
            monthly_requests: opt(&self.monthly, "prompts a month")?,
            daily_budget_usd: opt(&self.daily_budget, "budget a day")?,
            monthly_budget_usd: opt(&self.monthly_budget, "budget a month")?,
            max_pending: None,
        };
        let new = self.is_new();
        Ok(AcceptInput {
            label: new.then(|| self.label.trim().to_owned()),
            agents: Some(agents),
            expires_in_days: opt(&self.expiry_days, "lifetime")?,
            limits: Some(limits),
            link_hours: new.then(|| LINK_HOURS[self.link_hours % LINK_HOURS.len()]),
            ..AcceptInput::default()
        })
    }

    /// Accepts the request with the form's values. On failure the error is
    /// kept on the form to show.
    ///
    /// # Errors
    ///
    /// The reason it could not be accepted.
    pub fn submit(&mut self, store: &Store) -> Result<Accepted, String> {
        let result = self.to_input().and_then(|input| {
            access::accept(store, ACTOR, &self.request.id, input).map_err(|e| e.to_string())
        });
        if let Err(e) = &result {
            self.error = Some(e.clone());
        }
        result
    }
}

fn field_line(on: bool, label: &str, value: &str, hint: &str) -> Line<'static> {
    let marker = if on { "\u{25b8} " } else { "  " };
    let value_style = if on { t::bold(t::PINK) } else { Style::new() };
    let mut spans = vec![
        Span::styled(marker.to_owned(), t::fg(t::PINK)),
        Span::styled(format!("{label:<16}"), t::muted()),
        Span::styled(
            if value.is_empty() {
                "-".to_owned()
            } else {
                value.to_owned()
            },
            value_style,
        ),
    ];
    if !hint.is_empty() {
        spans.push(Span::styled(format!("  {hint}"), t::muted()));
    }
    Line::from(spans)
}

fn limits_line(l: &Limits) -> String {
    let mut parts = Vec::new();
    if let Some(n) = l.daily_requests {
        parts.push(format!("{n}/day"));
    }
    if let Some(n) = l.monthly_requests {
        parts.push(format!("{n}/month"));
    }
    if let Some(n) = l.daily_budget_usd {
        parts.push(format!("${n}/day"));
    }
    if let Some(n) = l.monthly_budget_usd {
        parts.push(format!("${n}/month"));
    }
    if parts.is_empty() {
        "no limits".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Draws the accept popup centred in `area`.
pub fn render_accept(frame: &mut Frame, area: Rect, form: &AcceptForm) {
    let r = &form.request;
    let f = |i: usize| form.field == i;
    let new = form.is_new();
    let mut lines = vec![Line::from(vec![
        Span::styled(if new { "New person: " } else { "More for: " }, t::muted()),
        Span::styled(safe(&r.name), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(
            r.project
                .as_deref()
                .map(|p| format!("  project {}", safe(p)))
                .unwrap_or_default(),
            t::muted(),
        ),
    ])];
    if !r.message.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("\"{}\"", truncate(&safe(&r.message), 200)),
            t::fg(t::ORCHID),
        )));
    }
    if let Some(cur) = &form.current {
        lines.push(Line::from(Span::styled(
            format!(
                "now: {}, agents {}; asked: {}",
                limits_line(&cur.limits),
                crate::token::encode_agents(&cur.agents),
                safe(&access::asked_text(r))
            ),
            t::muted(),
        )));
    }
    lines.push(Line::raw(""));
    if new {
        lines.push(field_line(f(0), "token name", &form.label, ""));
    } else {
        lines.push(field_line(f(0), "token", &safe(&r.name), "(fixed)"));
    }
    lines.push(field_line(f(1), "agents", &form.agents, "comma separated"));
    lines.push(field_line(f(2), "prompts a day", &form.daily, ""));
    lines.push(field_line(f(3), "prompts a month", &form.monthly, ""));
    lines.push(field_line(f(4), "budget a day", &form.daily_budget, "USD"));
    lines.push(field_line(
        f(5),
        "budget a month",
        &form.monthly_budget,
        "USD",
    ));
    lines.push(field_line(
        f(6),
        "lifetime (days)",
        &form.expiry_days,
        if new {
            "empty: no expiry"
        } else {
            "empty: unchanged, 0: none"
        },
    ));
    if new {
        let hours = LINK_HOURS[form.link_hours % LINK_HOURS.len()];
        lines.push(field_line(
            f(7),
            "invite valid for",
            &format!("\u{2190} {hours}h \u{2192}"),
            "",
        ));
    }
    lines.push(Line::raw(""));
    if let Some(e) = &form.error {
        lines.push(Line::from(Span::styled(safe(e), t::fg(t::ROSE))));
    }
    lines.push(Line::from(Span::styled(
        if new {
            "Accepting makes a single-use invite they collect themselves."
        } else {
            "Accepting applies exactly these values to their token."
        },
        t::muted(),
    )));
    lines.push(Line::from(Span::styled(
        "\u{2191}\u{2193} field  type to edit  enter accept  esc cancel",
        t::muted(),
    )));
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = centered(area, 76, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(modal("accept request")),
        rect,
    );
}

/// The decline popup: an optional reason the requester sees.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclineForm {
    /// The request.
    pub request: AccessRequest,
    /// The reason typed so far.
    pub reason: String,
    /// The last error.
    pub error: Option<String>,
}

impl DeclineForm {
    /// An empty form for `request`.
    #[must_use]
    pub fn new(request: &AccessRequest) -> Self {
        Self {
            request: request.clone(),
            reason: String::new(),
            error: None,
        }
    }

    /// Typing edits the reason; enter declines, esc cancels.
    pub fn on_key(&mut self, key: KeyEvent) -> FormAction {
        if key.kind == KeyEventKind::Release {
            return FormAction::Edit;
        }
        match key.code {
            KeyCode::Esc => FormAction::Cancel,
            KeyCode::Enter => FormAction::Submit,
            _ => {
                edit_text(&mut self.reason, key, access::MAX_REASON_CHARS);
                FormAction::Edit
            }
        }
    }

    /// Declines the request with the typed reason.
    ///
    /// # Errors
    ///
    /// The reason it could not be declined, also kept on the form.
    pub fn submit(&mut self, store: &Store) -> Result<AccessRequest, String> {
        let reason = Some(self.reason.trim()).filter(|r| !r.is_empty());
        let result =
            access::decline(store, ACTOR, &self.request.id, reason).map_err(|e| e.to_string());
        if let Err(e) = &result {
            self.error = Some(e.clone());
        }
        result
    }
}

/// Draws the decline popup centred in `area`.
pub fn render_decline(frame: &mut Frame, area: Rect, form: &DeclineForm) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Decline the request from ", t::muted()),
            Span::styled(
                safe(&form.request.name),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        field_line(true, "reason", &safe(&form.reason), "optional, they see it"),
        Line::raw(""),
    ];
    if let Some(e) = &form.error {
        lines.push(Line::from(Span::styled(safe(e), t::fg(t::ROSE))));
    }
    lines.push(Line::from(Span::styled(
        "enter decline  esc cancel",
        t::muted(),
    )));
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = centered(area, 72, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(modal("decline request")),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{NewPerson, OpenLink};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(w: u16, h: u16, f: impl FnOnce(&mut Frame)) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(f).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn request(store: &Store, name: &str) -> AccessRequest {
        let (_, code) = access::open_link(store, "cli", OpenLink::default()).unwrap();
        access::submit_new(
            store,
            NewPerson {
                code,
                name: name.into(),
                message: Some("for the \u{202e}docs".into()),
                agents: Some(vec![AgentKind::Claude]),
            },
        )
        .unwrap()
        .0
    }

    #[test]
    fn list_shows_requests_without_control_characters() {
        let s = Store::open_in_memory().unwrap();
        let r = request(&s, "alice");
        let out = draw(100, 8, |f| {
            render_list(f, f.area(), &[r], Some(0), crate::util::now_ms());
        });
        assert!(out.contains("pending") && out.contains("alice"), "{out}");
        assert!(out.contains("for the docs"), "{out}");
        assert!(!out.contains('\u{202e}'));
        let empty = draw(80, 8, |f| render_list(f, f.area(), &[], None, 0));
        assert!(empty.contains("No requests"), "{empty}");
    }

    #[test]
    fn accept_popup_edits_and_accepts() {
        let s = Store::open_in_memory().unwrap();
        let r = request(&s, "alice");
        let mut form = AcceptForm::new(&s, &r).unwrap();
        assert_eq!(form.agents, "claude");
        form.on_key(press(KeyCode::Down));
        form.on_key(press(KeyCode::Down));
        for c in "12".chars() {
            form.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(form.daily, "12");
        let out = draw(90, 24, |f| render_accept(f, f.area(), &form));
        assert!(
            out.contains("accept request") && out.contains("168h"),
            "{out}"
        );
        assert_eq!(form.on_key(press(KeyCode::Enter)), FormAction::Submit);
        let Accepted::Invite { invite, .. } = form.submit(&s).unwrap() else {
            panic!("expected an invite");
        };
        assert_eq!(invite.limits.daily_requests, Some(12));
        assert!(form.submit(&s).is_err());
        assert!(form.error.is_some());
    }

    #[test]
    fn decline_popup_sends_the_reason() {
        let s = Store::open_in_memory().unwrap();
        let r = request(&s, "bob");
        let mut form = DeclineForm::new(&r);
        for c in "full".chars() {
            form.on_key(press(KeyCode::Char(c)));
        }
        let out = draw(80, 10, |f| render_decline(f, f.area(), &form));
        assert!(
            out.contains("decline request") && out.contains("full"),
            "{out}"
        );
        assert_eq!(form.submit(&s).unwrap().reason.as_deref(), Some("full"));
        assert_eq!(form.on_key(press(KeyCode::Esc)), FormAction::Cancel);
    }

    #[test]
    fn badge_counts_pending() {
        assert_eq!(badge(0), None);
        assert_eq!(badge(3).as_deref(), Some("3"));
        assert_eq!(badge(150).as_deref(), Some("99+"));
        assert_eq!(
            toast_text(AccessKind::New, "a\u{1b}b"),
            "ab asks for access"
        );
    }
}
