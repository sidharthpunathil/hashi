//! Rendering of the management popups: token edit, the prompt popup,
//! access requests, accounts, projects, bridges, the audit log, the doctor,
//! the report, the listen address and update confirmation. Pure functions
//! of [`App`].

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap};

use super::app::{App, Popup};
use super::forms::{self, AccountForm, AskForm, EditTokenForm, ListenForm, ProjectForm};
use super::theme as t;
use super::ui::{
    SELECT, centered, form_field, form_text, form_toggle, key_hint, modal, plain, strong,
};
use crate::doctor::{self, Mark};
use crate::util::{human_age, rfc3339, truncate};

/// The error line under field `i`, when the error is about it.
fn error_under(
    error: Option<&forms::FieldError>,
    i: usize,
    indent: usize,
) -> Option<Line<'static>> {
    error.filter(|(f, _)| *f == i).map(|(_, e)| {
        Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(format!("✗ {e}"), t::fg(t::ROSE)),
        ])
    })
}

/// Field lines with the error line after the field it is about.
fn with_errors(
    fields: Vec<Line<'static>>,
    error: Option<&forms::FieldError>,
    indent: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (i, line) in fields.into_iter().enumerate() {
        out.push(line);
        if let Some(e) = error_under(error, i, indent) {
            out.push(e);
        }
    }
    out
}

fn show(frame: &mut Frame, rect: Rect, title: &str, lines: Vec<Line<'static>>) {
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(modal(title)),
        rect,
    );
}

/// A popup sized to its lines.
fn fitted(frame: &mut Frame, width: u16, title: &str, lines: Vec<Line<'static>>) {
    let height = u16::try_from(lines.len() + 4).unwrap_or(u16::MAX);
    let rect = centered(frame.area(), width, height);
    show(frame, rect, title, lines);
}

fn muted_line(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(s.into(), t::muted()))
}

/// Draws one of the management popups.
pub fn render(app: &App, frame: &mut Frame, popup: &Popup) {
    match popup {
        Popup::EditToken(form) => edit_token(frame, form, app.tick),
        Popup::Ask(form) => ask(frame, form, app.tick),
        Popup::RemoteAnswer {
            remote,
            answer,
            scroll,
        } => remote_answer(frame, remote, answer.as_ref(), *scroll, app.tick),
        Popup::Accounts { selected } => accounts(app, frame, *selected),
        Popup::AddAccount(form) => add_account(frame, form, app.tick),
        Popup::ConfirmRemoveAccount {
            agent,
            name,
            tokens,
        } => fitted(
            frame,
            64,
            "remove account",
            vec![
                Line::from(vec![
                    Span::styled("Remove ", plain()),
                    Span::styled(format!("{agent}:{name}"), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                muted_line(format!(
                    "Its directory and key are deleted and it comes off {tokens} token(s)."
                )),
                Line::raw(""),
                key_hint(&[("y", "remove"), ("n", "keep")]),
            ],
        ),
        Popup::Projects { selected } => projects(app, frame, *selected),
        Popup::Project(form) => project(frame, form, app.tick),
        Popup::ConfirmRemoveProject { name, tokens } => fitted(
            frame,
            68,
            "remove project",
            vec![
                Line::from(vec![
                    Span::styled("Remove project ", plain()),
                    Span::styled(name.clone(), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                muted_line(if *tokens > 0 {
                    format!(
                        "{tokens} token(s) and any pending invites go back to their own workspace."
                    )
                } else {
                    "No token is bound to it.".to_owned()
                }),
                muted_line("Its folder is never touched."),
                Line::raw(""),
                key_hint(&[("y", "remove"), ("n", "keep")]),
            ],
        ),
        Popup::Remotes { selected } => remotes(app, frame, *selected),
        Popup::Connect(form) => connect(frame, form, app.tick),
        Popup::ConfirmRemoveRemote { name } => fitted(
            frame,
            64,
            "forget bridge",
            vec![
                Line::from(vec![
                    Span::styled("Forget ", plain()),
                    Span::styled(name.clone(), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                muted_line("Its token still works until its owner revokes it."),
                Line::raw(""),
                key_hint(&[("y", "forget"), ("n", "keep")]),
            ],
        ),
        Popup::Audit { scroll } => audit(app, frame, *scroll),
        Popup::Doctor => doctor_popup(app, frame),
        Popup::Report(form) => report(app, frame, form),
        Popup::ReportSaved {
            path,
            include_prompts,
        } => fitted(
            frame,
            u16::try_from(path.chars().count() + 12)
                .unwrap_or(u16::MAX)
                .max(76),
            "report saved",
            vec![
                Line::from(Span::styled(format!(" {path} "), t::bold(t::PINK))),
                Line::raw(""),
                muted_line(if *include_prompts {
                    "Secrets are redacted, but it has prompt excerpts, agent error output and logs."
                } else {
                    "Secrets are redacted and prompts are left out, but it has log lines."
                }),
                muted_line("Read it before you share it. Nothing has been uploaded."),
                muted_line(format!(
                    "To file a bug, attach it at {}",
                    crate::diagnostics::issue_url()
                )),
                Line::raw(""),
                key_hint(&[("c", "copy path"), ("enter", "done")]),
            ],
        ),
        Popup::Listen(form) => listen(app, frame, form, app.tick),
        Popup::ConfirmListen { addr, warning, .. } => fitted(
            frame,
            80,
            "listen on the network?",
            vec![
                Line::from(vec![
                    Span::styled("Move to ", plain()),
                    Span::styled(addr.to_string(), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                Line::raw(""),
                Line::from(Span::styled(warning.clone(), t::fg(t::AMBER))),
                Line::raw(""),
                key_hint(&[("y", "move anyway"), ("n", "back")]),
            ],
        ),
        Popup::ConfirmUpdate => fitted(
            frame,
            72,
            "install the update?",
            vec![
                Line::from(Span::styled(
                    match &app.update {
                        Some(v) => format!("Update hashi v{} to v{v}?", env!("CARGO_PKG_VERSION")),
                        None => "Install the latest release, if it is newer?".to_owned(),
                    },
                    strong(),
                )),
                muted_line("The dashboard steps aside while it downloads and verifies it."),
                muted_line("Hashi keeps running; quit and start it again to use the new one."),
                Line::raw(""),
                key_hint(&[("y", "update"), ("n", "not now")]),
            ],
        ),
        Popup::AccessRequests { selected, all } => access_requests(app, frame, *selected, *all),
        Popup::AcceptRequest(form) => super::access::render_accept(frame, frame.area(), form),
        Popup::DeclineRequest(form) => super::access::render_decline(frame, frame.area(), form),
        Popup::AccessLink {
            code,
            hours,
            project,
        } => access_link(app, frame, code, *hours, project.as_deref()),
        _ => {}
    }
}

fn access_requests(app: &App, frame: &mut Frame, selected: usize, all: bool) {
    let now = crate::util::now_ms();
    let rows = app.access_rows(all);
    let link = match crate::access::current_link(&app.state.store).ok().flatten() {
        Some(l) if l.status(now) == "open" => Line::from(vec![
            Span::styled("◌ ", t::fg(t::MINT)),
            Span::styled(
                format!(
                    "The ask-access link is open{} until {}.",
                    l.project
                        .as_deref()
                        .map(|p| format!(" for project {}", crate::diagnostics::terminal_safe(p)))
                        .unwrap_or_default(),
                    rfc3339(l.expires_at).unwrap_or_default()
                ),
                plain(),
            ),
        ]),
        _ => muted_line("The ask-access link is closed. Press o to open one."),
    };
    let foot = vec![
        link,
        muted_line(if all {
            "Showing every request, decided and expired ones too."
        } else {
            "Showing pending requests."
        }),
        Line::raw(""),
        key_hint(&[
            ("a", "accept"),
            ("d", "decline"),
            ("o", "open link"),
            ("x", "close link"),
            ("s", if all { "pending only" } else { "show all" }),
            ("esc", "close"),
        ]),
    ];
    let area = frame.area();
    let foot_h = u16::try_from(foot.len()).unwrap_or(0);
    let wanted = u16::try_from(rows.len()).unwrap_or(20) + 8 + foot_h;
    let rect = centered(area, 120, wanted.max(14));
    let inner = modal("").inner(rect);
    let [list, foot_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(foot_h)]).areas(inner);
    frame.render_widget(Clear, rect);
    frame.render_widget(modal("access requests"), rect);
    let selected = (!rows.is_empty()).then(|| selected.min(rows.len() - 1));
    super::access::render_list(frame, list, &rows, selected, now);
    frame.render_widget(Paragraph::new(foot).wrap(Wrap { trim: false }), foot_area);
}

fn access_link(app: &App, frame: &mut Frame, code: &str, hours: u32, project: Option<&str>) {
    let area = frame.area();
    let url = app.access_link_url(code);
    let local_only = app.local_only();
    let mut lines = vec![
        Line::from(Span::styled(
            "Anyone with this link can ask for access. You decide each request here.",
            plain(),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            format!(" {url} "),
            Style::new()
                .fg(t::PINK.into())
                .bg(SELECT.into())
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    let base = u16::try_from(lines.len()).unwrap_or(0) + 9;
    let qr = super::qr::size(&url)
        .filter(|(w, h)| area.height >= base + h + 3 && area.width >= w + 10)
        .and_then(|_| super::qr::lines(&url));
    match &qr {
        Some(rows) => {
            // Dark on light whatever the terminal's theme, as phone
            // cameras expect.
            let style = Style::new()
                .fg(ratatui::style::Color::Black)
                .bg(ratatui::style::Color::White);
            for row in rows {
                lines.push(Line::from(Span::styled(row.clone(), style)).centered());
            }
            lines.push(Line::raw(""));
        }
        None => lines.push(muted_line(
            "Make the terminal larger to see the link as a QR code.",
        )),
    }
    lines.push(muted_line(format!(
        "Closes by itself in {}{}. Opening a new one closes this one.",
        super::app::link_short(hours),
        project
            .map(|p| format!(
                "; requests are for project {}",
                crate::diagnostics::terminal_safe(p)
            ))
            .unwrap_or_default()
    )));
    if local_only {
        lines.push(Line::from(vec![
            Span::styled("! ", t::bold(t::AMBER)),
            Span::styled(
                "No public URL: this link only works on this machine. Press t to go public.",
                t::fg(t::AMBER),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("c", "copy"),
        ("t", "go public"),
        ("enter", "done"),
    ]));
    let width = u16::try_from(url.chars().count() + 10)
        .unwrap_or(u16::MAX)
        .max(84);
    fitted(frame, width, "ask-access link", lines);
}

fn edit_token(frame: &mut Frame, form: &EditTokenForm, tick: u64) {
    const W: usize = 15;
    let f = |i: usize| form.field == i;
    let text = |i: usize, value: &str, placeholder: &'static str| {
        form_field(
            f(i),
            EditTokenForm::LABELS[i],
            W,
            form_text(value, f(i), placeholder, tick),
        )
    };
    let expiry_hint = form.expiry_hint();
    let fields = vec![
        text(0, &form.name, "my-app"),
        text(1, &form.note, "who or what it is for"),
        form_field(
            f(2),
            EditTokenForm::LABELS[2],
            W,
            if form.expiry.is_empty() && !f(2) {
                vec![Span::styled(expiry_hint, t::fg(t::DUSK))]
            } else {
                form_text(&form.expiry, f(2), "", tick)
            },
        ),
        text(3, &form.daily, "none"),
        text(4, &form.monthly, "none"),
        text(5, &form.daily_budget, "none"),
        text(6, &form.monthly_budget, "none"),
        text(7, &form.max_pending, "none"),
        text(8, &form.accounts, "default (or claude=work,personal)"),
        form_field(
            f(9),
            EditTokenForm::LABELS[9],
            W,
            form_toggle(&form.project.label(), f(9)),
        ),
        form_field(
            f(EditTokenForm::SHARE_USAGE),
            EditTokenForm::LABELS[EditTokenForm::SHARE_USAGE],
            W,
            form_toggle(
                super::ui::share_usage_label(form.share_usage),
                f(EditTokenForm::SHARE_USAGE),
            ),
        ),
    ];
    let mut lines = with_errors(fields, form.error.as_ref(), W + 2);
    lines.push(Line::raw(""));
    lines.push(muted_line(
        "  Empty limits mean none. p pauses and resumes in the Tokens view.",
    ));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("←→", "change"),
        ("enter", "save"),
        ("esc", "cancel"),
    ]));
    let title = format!("edit {}", form.token.name);
    fitted(frame, 76, &title, lines);
}

fn ask(frame: &mut Frame, form: &AskForm, tick: u64) {
    const W: usize = 9;
    let f = |i: usize| form.field == i;
    let target = match form.target() {
        forms::Target::Local => "this Hashi, through the queue".to_owned(),
        forms::Target::Remote(n) => format!("bridge {n}"),
    };
    let agent = match form.agent() {
        Some(a) => a.display_name().to_owned(),
        None if form.has_accounts() => "none installed".to_owned(),
        None => "the server's default".to_owned(),
    };
    let mut fields = vec![
        form_field(f(0), "to", W, form_toggle(&target, f(0))),
        form_field(f(1), "agent", W, form_toggle(&agent, f(1))),
        if form.has_accounts() {
            let account = form.accounts.get(form.account).cloned().unwrap_or_default();
            form_field(f(2), "account", W, form_toggle(&account, f(2)))
        } else {
            form_field(
                false,
                "account",
                W,
                vec![Span::styled("the bridge's choice", t::fg(t::DUSK))],
            )
        },
        form_field(
            f(3),
            "model",
            W,
            form_text(&form.model, f(3), "default", tick),
        ),
        form_field(f(4), "prompt", W, Vec::new()),
    ];
    // The prompt's own lines, below its label, the last ones when long.
    let prompt: Vec<&str> = form.prompt.split('\n').collect();
    let shown = prompt.len().saturating_sub(8);
    let mut lines = with_errors(std::mem::take(&mut fields), form.error.as_ref(), W + 2);
    if form.prompt.is_empty() && !f(4) {
        lines.push(Line::from(Span::styled(
            "    what should the agent do?",
            t::fg(t::DUSK),
        )));
    }
    for (i, l) in prompt.iter().enumerate().skip(shown) {
        let last = i + 1 == prompt.len();
        let mut spans = vec![Span::raw("    ")];
        if form.prompt.is_empty() && !f(4) {
            continue;
        }
        spans.extend(form_text(l, f(4) && last, "", tick));
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    lines.push(muted_line(if form.has_accounts() {
        "  It runs as the owner's admin token and shows in Requests like any other."
    } else {
        "  It goes to the bridge with the token saved by hashi connect."
    }));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("tab", "field"),
        ("←→", "change"),
        ("enter", "new line"),
        ("ctrl-s", "send"),
        ("esc", "cancel"),
    ]));
    fitted(frame, 84, "ask", lines);
}

fn remote_answer(
    frame: &mut Frame,
    remote: &str,
    answer: Option<&Result<String, String>>,
    scroll: u16,
    tick: u64,
) {
    let area = frame.area();
    let rect = centered(area, 96, area.height.saturating_sub(4).max(10));
    let body: Vec<Line<'static>> = match answer {
        None => vec![Line::from(Span::styled(
            format!("{} waiting for {remote}…", t::spinner(tick)),
            t::fg(t::AMBER),
        ))],
        Some(Err(e)) => vec![Line::from(Span::styled(e.clone(), t::fg(t::ROSE)))],
        Some(Ok(text)) => text
            .lines()
            .map(|l| Line::from(Span::styled(l.to_owned(), plain())))
            .collect(),
    };
    let [text, keys] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(modal("").inner(rect));
    frame.render_widget(Clear, rect);
    frame.render_widget(modal(&format!("answer from {remote}")), rect);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        text,
    );
    frame.render_widget(
        Paragraph::new(key_hint(&[
            ("↑↓", "scroll"),
            ("c", "copy"),
            ("enter", "done"),
        ])),
        keys,
    );
}

/// A list popup: a table of `rows` with a selection, a note and keys.
fn list_popup(
    frame: &mut Frame,
    title: &str,
    header: Vec<&'static str>,
    widths: Vec<Constraint>,
    rows: Vec<Row<'static>>,
    selected: usize,
    foot: Vec<Line<'static>>,
) {
    let area = frame.area();
    let wanted =
        u16::try_from(rows.len()).unwrap_or(20) + 6 + u16::try_from(foot.len()).unwrap_or(0);
    let rect = centered(area, 110, wanted.max(12));
    let inner = modal("").inner(rect);
    let foot_h = u16::try_from(foot.len()).unwrap_or(0);
    let [table_area, foot_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(foot_h)]).areas(inner);
    frame.render_widget(Clear, rect);
    frame.render_widget(modal(title), rect);
    let empty = rows.is_empty();
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(t::bold(t::MIST)))
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(SELECT.into()).add_modifier(Modifier::BOLD))
        .highlight_symbol(Span::styled("▌", t::fg(t::PINK)))
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::default().with_selected((!empty).then_some(selected));
    frame.render_stateful_widget(table, table_area, &mut state);
    frame.render_widget(Paragraph::new(foot).wrap(Wrap { trim: false }), foot_area);
}

fn accounts(app: &App, frame: &mut Frame, selected: usize) {
    let now = crate::util::now_ms();
    let list = app.account_table();
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|row| {
            let a = &row.state;
            let state = match a.status(now) {
                crate::store::AccountStatus::Limited(until) => {
                    format!("limited until {}", rfc3339(until).unwrap_or_default())
                }
                s => s.as_str().to_owned(),
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("{} {}", t::agent_glyph(a.agent), a.agent),
                    t::fg(t::agent_color(a.agent)),
                )),
                Cell::from(Span::styled(a.name.clone(), strong())),
                Cell::from(Span::styled(a.kind.as_str(), plain())),
                Cell::from(Span::styled(row.signed_in.clone(), plain())),
                Cell::from(Span::styled(state, plain())),
                Cell::from(Span::styled(
                    a.last_used
                        .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u))),
                    t::muted(),
                )),
                Cell::from(Span::styled(row.tokens.to_string(), plain())),
            ])
        })
        .collect();
    let mut foot = Vec::new();
    if list.is_empty() {
        foot.push(muted_line(
            "No accounts yet: every agent uses your own login. Press a to add one.",
        ));
    }
    // Usage gauges for the selected account, from the poller and runs.
    let rotate_at = app.rotate_at();
    let usage = list.get(selected).and_then(|sel| {
        app.usage_snapshot()
            .iter()
            .find(|u| u.agent == sel.state.agent && u.name == sel.state.name)
            .cloned()
    });
    match usage {
        Some(u) => foot.extend(super::usage::lines(&[u], rotate_at, 24)),
        None => foot.push(Line::from(vec![
            Span::styled("usage  ", t::muted()),
            Span::styled(
                "no figures yet for this account (hashi account usage --refresh)",
                t::muted(),
            ),
        ])),
    }
    foot.push(Line::raw(""));
    foot.push(key_hint(&[
        ("a", "add"),
        ("l", "sign in"),
        ("d", "disable"),
        ("e", "enable"),
        ("x", "remove"),
        ("esc", "close"),
    ]));
    list_popup(
        frame,
        "accounts",
        vec![
            "agent",
            "name",
            "kind",
            "signed in",
            "state",
            "last used",
            "tokens",
        ],
        vec![
            Constraint::Length(10),
            Constraint::Fill(1),
            Constraint::Length(11),
            Constraint::Length(13),
            Constraint::Length(20),
            Constraint::Length(10),
            Constraint::Length(6),
        ],
        rows,
        selected,
        foot,
    );
}

fn add_account(frame: &mut Frame, form: &AccountForm, tick: u64) {
    const W: usize = 8;
    let f = |i: usize| form.field == i;
    let mut fields = vec![
        form_field(
            f(0),
            "agent",
            W,
            form_toggle(form.agent.display_name(), f(0)),
        ),
        form_field(f(1), "name", W, form_text(&form.name, f(1), "work", tick)),
        form_field(f(2), "kind", W, form_toggle(form.kind.as_str(), f(2))),
    ];
    if form.kind.is_key() {
        // Masked: the key itself is never drawn.
        let masked = form.key.masked();
        fields.push(form_field(
            f(3),
            "key",
            W,
            form_text(&masked, f(3), "paste it here; it is not shown", tick),
        ));
    }
    let mut lines = with_errors(fields, form.error.as_ref(), W + 2);
    lines.push(Line::raw(""));
    lines.push(muted_line(if form.kind.is_key() {
        "  The key is stored owner-only in the account's directory, never in the database."
    } else {
        "  enter adds it, then the dashboard steps aside for the agent's own sign-in."
    }));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("←→", "change"),
        ("enter", "add"),
        ("esc", "back"),
    ]));
    fitted(frame, 84, "add account", lines);
}

fn projects(app: &App, frame: &mut Frame, selected: usize) {
    let list = app.cached_projects();
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|p| {
            let tokens = app.project_token_count(&p.name);
            let agents = match p.default_agent {
                Some(d) => format!("{} ({d})", crate::token::encode_agents(&p.agents)),
                None => crate::token::encode_agents(&p.agents),
            };
            Row::new(vec![
                Cell::from(Span::styled(p.name.clone(), strong())),
                Cell::from(Span::styled(tokens.to_string(), plain())),
                Cell::from(Span::styled(agents, plain())),
                Cell::from(Span::styled(
                    crate::accounts::format_bindings(&p.accounts),
                    plain(),
                )),
                Cell::from(Span::styled(p.path.clone(), t::muted())),
            ])
        })
        .collect();
    let mut foot = Vec::new();
    if list.is_empty() {
        foot.push(muted_line(
            "No projects yet: every token runs in its own workspace. Press a to add one.",
        ));
    }
    foot.push(Line::raw(""));
    foot.push(key_hint(&[
        ("a", "add"),
        ("e", "edit"),
        ("x", "remove"),
        ("esc", "close"),
    ]));
    list_popup(
        frame,
        "projects",
        vec!["name", "tokens", "agents", "accounts", "folder"],
        vec![
            Constraint::Length(16),
            Constraint::Length(6),
            Constraint::Length(24),
            Constraint::Length(20),
            Constraint::Fill(1),
        ],
        rows,
        selected,
        foot,
    );
}

fn project(frame: &mut Frame, form: &ProjectForm, tick: u64) {
    const W: usize = 17;
    let f = |i: usize| form.field == i;
    let text = |i: usize, value: &str, placeholder: &'static str| {
        form_field(
            f(i),
            ProjectForm::LABELS[i],
            W,
            form_text(value, f(i), placeholder, tick),
        )
    };
    let default_agent = form
        .default_agent
        .map_or("none".to_owned(), |a| a.as_str().to_owned());
    let fields = vec![
        if form.editing.is_some() {
            form_field(
                false,
                ProjectForm::LABELS[0],
                W,
                vec![Span::styled(form.name.clone(), strong())],
            )
        } else {
            text(0, &form.name, "api")
        },
        text(1, &form.path, "/absolute/path/to/folder"),
        text(
            2,
            &form.agents,
            if form.editing.is_some() {
                "claude,codex"
            } else {
                "the installed ones"
            },
        ),
        text(3, &form.accounts, "default (or claude=work,personal)"),
        form_field(
            f(4),
            ProjectForm::LABELS[4],
            W,
            form_toggle(&default_agent, f(4)),
        ),
        text(5, &form.models, "none (or claude=sonnet)"),
        text(6, &form.note, "who or what it is for"),
        form_field(
            f(ProjectForm::STRATEGY),
            ProjectForm::LABELS[ProjectForm::STRATEGY],
            W,
            form_toggle(form.strategy_label(), f(ProjectForm::STRATEGY)),
        ),
        text(
            ProjectForm::ROTATE_AT,
            &form.rotate_at,
            "server default (or 50 to 99)",
        ),
    ];
    let mut lines = with_errors(fields, form.error.as_ref(), W + 2);
    lines.push(Line::raw(""));
    lines.push(muted_line(
        "  Tokens bound to it run in this folder. Hashi never changes the folder itself.",
    ));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("←→", "choose"),
        ("enter", "save"),
        ("esc", "back"),
    ]));
    let title = match &form.editing {
        Some(n) => format!("edit project {n}"),
        None => "add project".to_owned(),
    };
    fitted(frame, 84, &title, lines);
}

fn remotes(app: &App, frame: &mut Frame, selected: usize) {
    let list = app
        .home()
        .and_then(|h| crate::remote::load(&h).ok())
        .unwrap_or_default();
    let rows: Vec<Row<'static>> = list
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(Span::styled(
                    crate::diagnostics::terminal_safe(&r.name),
                    strong(),
                )),
                // Names and URLs only: the token is never shown.
                Cell::from(Span::styled(
                    crate::diagnostics::terminal_safe(&r.base_url),
                    plain(),
                )),
            ])
        })
        .collect();
    let mut foot = Vec::new();
    if app.home().is_none() {
        foot.push(muted_line("No Hashi home here, so no bridges are saved."));
    } else if list.is_empty() {
        foot.push(muted_line(
            "No bridges saved. Press c and paste an invite link someone sent you.",
        ));
    }
    foot.extend(request_status_lines(app));
    foot.push(Line::raw(""));
    foot.push(key_hint(&[
        ("c", "connect"),
        ("a", "ask it"),
        ("x", "forget"),
        ("r", "request status"),
        ("esc", "close"),
    ]));
    list_popup(
        frame,
        "bridges I use",
        vec!["name", "url"],
        vec![Constraint::Length(24), Constraint::Fill(1)],
        rows,
        selected,
        foot,
    );
}

/// The requests for access this machine sent, as `hashi request status`
/// shows them: the last check's answers, or the ones still waiting.
fn request_status_lines(app: &App) -> Vec<Line<'static>> {
    let safe = crate::diagnostics::terminal_safe;
    let mut out = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "Requests for access you sent",
            t::bold(t::MIST),
        )),
    ];
    if app.request_checking {
        out.push(Line::from(Span::styled(
            format!("  {} asking each bridge…", t::spinner(app.tick)),
            t::fg(t::AMBER),
        )));
    }
    match &app.request_checks {
        Some(found) if !found.is_empty() => {
            for c in found {
                let (glyph, color, text) = match c.state {
                    "pending" => (
                        "◌",
                        t::AMBER,
                        format!("waiting for the owner (expires {})", c.detail),
                    ),
                    "connected" => (
                        "✓",
                        t::MINT,
                        format!("accepted, saved as remote \"{}\"", c.detail),
                    ),
                    "declined" if c.detail.is_empty() => ("✗", t::ROSE, "declined".to_owned()),
                    "declined" => ("✗", t::ROSE, format!("declined: {}", c.detail)),
                    "expired" => ("✗", t::ROSE, "expired without an answer".to_owned()),
                    other => ("!", t::AMBER, format!("{other}: {}", c.detail)),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("  {glyph} "), t::fg(color)),
                    Span::styled(format!("{}: ", c.server), strong()),
                    Span::styled(text, plain()),
                ]));
            }
            return out;
        }
        _ => {}
    }
    let waiting = app.outgoing_requests();
    if waiting.is_empty() {
        out.push(muted_line(
            "  None waiting. Ask with hashi request <link>; press r to check again.",
        ));
    }
    for p in waiting {
        out.push(Line::from(vec![
            Span::styled("  ◌ ", t::fg(t::AMBER)),
            Span::styled(format!("{}: ", safe(&p.server)), strong()),
            Span::styled(
                format!("asked as {}; press r to check", safe(&p.name)),
                plain(),
            ),
        ]));
    }
    out
}

fn connect(frame: &mut Frame, form: &forms::ConnectForm, tick: u64) {
    const W: usize = 6;
    let f = |i: usize| form.field == i;
    let fields = vec![
        form_field(
            f(0),
            "link",
            W,
            form_text(
                &truncate(&form.link, 70),
                f(0),
                "https://<host>/claim/hashi_inv_...",
                tick,
            ),
        ),
        form_field(
            f(1),
            "name",
            W,
            form_text(&form.name, f(1), "the server's name", tick),
        ),
    ];
    let mut lines = with_errors(fields, form.error.as_ref(), W + 2);
    lines.push(Line::raw(""));
    if form.busy {
        lines.push(Line::from(Span::styled(
            format!("  {} redeeming the invite…", t::spinner(tick)),
            t::fg(t::AMBER),
        )));
    } else {
        lines.push(muted_line(
            "  The token is saved owner-only in remotes.json, as hashi connect does.",
        ));
    }
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("enter", "connect"),
        ("esc", "cancel"),
    ]));
    fitted(frame, 90, "connect to a bridge", lines);
}

fn audit(app: &App, frame: &mut Frame, scroll: usize) {
    let entries = app.state.store.list_audit(500).unwrap_or_default();
    let area = frame.area();
    let rect = centered(area, 120, area.height.saturating_sub(4).max(10));
    let inner = modal("").inner(rect);
    let [list, keys] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    let visible = usize::from(list.height.saturating_sub(1)).max(1);
    let scroll = scroll.min(entries.len().saturating_sub(visible));
    let rows: Vec<Row<'static>> = entries
        .iter()
        .skip(scroll)
        .take(visible)
        .map(|e| {
            Row::new(vec![
                Cell::from(Span::styled(rfc3339(e.at).unwrap_or_default(), t::muted())),
                Cell::from(Span::styled(e.actor.clone(), plain())),
                Cell::from(Span::styled(e.action.clone(), t::fg(t::ORCHID))),
                Cell::from(Span::styled(e.target.clone(), strong())),
                Cell::from(Span::styled(e.detail.clone(), plain())),
            ])
        })
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        modal(&format!("audit log · {} entries", entries.len())),
        rect,
    );
    let empty = rows.is_empty();
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Length(16),
                Constraint::Length(20),
                Constraint::Fill(1),
            ],
        )
        .header(Row::new(["when", "actor", "action", "target", "detail"]).style(t::bold(t::MIST)))
        .column_spacing(1),
        list,
    );
    if empty {
        frame.render_widget(
            Paragraph::new(muted_line("Nothing has been shared or changed yet.")),
            Rect {
                y: list.y + 2,
                height: 1,
                ..list
            },
        );
    }
    frame.render_widget(
        Paragraph::new(key_hint(&[
            ("↑↓", "scroll"),
            ("PgUp PgDn", "page"),
            ("esc", "close"),
        ])),
        keys,
    );
}

/// The doctor's checks, from what the dashboard already knows: nothing here
/// runs an agent or asks the network.
#[must_use]
pub fn doctor_checks(app: &App) -> Vec<doctor::Check> {
    use crate::install::Helper;
    let (mut checks, _) = doctor::agents(&app.state.runtime.agents);
    if !app
        .state
        .runtime
        .agents
        .iter()
        .any(|a| a.agent == crate::agent::AgentKind::Crush)
    {
        checks.push(doctor::crush(None, app.helper_path(Helper::Crush)));
    }
    checks.push(doctor::cloudflared(app.helper_path(Helper::Cloudflared)));
    let status = app.tailscale_self.lock().ok().and_then(|s| s.clone());
    checks.push(doctor::tailscale(
        app.helper_path(Helper::Tailscale),
        status,
    ));
    checks.extend(doctor::accounts(Some(&app.state.store)));
    let update = if crate::update::opted_out() {
        doctor::UpdateState::OptedOut
    } else if let Some(v) = app
        .update
        .as_deref()
        .and_then(crate::update::Version::parse)
    {
        doctor::UpdateState::Latest(Ok(v))
    } else {
        app.home()
            .and_then(|h| crate::update::read_cache(&h))
            .and_then(|c| c.tag)
            .and_then(|tag| crate::update::Version::parse(&tag))
            .map_or(doctor::UpdateState::Unknown, |v| {
                doctor::UpdateState::Latest(Ok(v))
            })
    };
    checks.push(doctor::update(&update));
    checks
}

fn doctor_popup(app: &App, frame: &mut Frame) {
    let mut lines: Vec<Line<'static>> = doctor_checks(app)
        .into_iter()
        .map(|c| {
            let color = match c.mark {
                Mark::Ok => t::MINT,
                Mark::Info => t::MIST,
                Mark::Warn => t::AMBER,
            };
            Line::from(vec![
                Span::styled(format!("{} ", c.mark.glyph()), t::bold(color)),
                Span::styled(format!("{:<13}", c.name), strong()),
                Span::styled(c.detail, plain()),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    let home = app.home().map_or_else(
        || "none (in memory)".to_owned(),
        |h| h.display().to_string(),
    );
    lines.push(Line::from(vec![
        Span::styled(format!("  {:<13}", "home"), strong()),
        Span::styled(home, plain()),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  {:<13}", "guard"), strong()),
        Span::styled(app.state.guard.describe(), plain()),
    ]));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("u", "check for updates"),
        ("r", "recheck"),
        ("enter", "done"),
    ]));
    fitted(frame, 110, "doctor", lines);
}

fn report(app: &App, frame: &mut Frame, form: &forms::ReportForm) {
    const W: usize = 16;
    let f = |i: usize| form.field == i;
    let place = if form.to_home {
        app.home().map_or_else(
            || "<home>/reports".to_owned(),
            |h| {
                h.join(crate::diagnostics::REPORTS_DIR)
                    .display()
                    .to_string()
            },
        )
    } else {
        "the current folder (like hashi report)".to_owned()
    };
    let mut lines = vec![
        form_field(
            f(0),
            "include prompts",
            W,
            form_toggle(if form.include_prompts { "yes" } else { "no" }, f(0)),
        ),
        form_field(f(1), "save in", W, form_toggle(&place, f(1))),
    ];
    if let Some(e) = &form.error {
        lines.push(Line::from(Span::styled(format!("  ✗ {e}"), t::fg(t::ROSE))));
    }
    lines.push(Line::raw(""));
    lines.push(muted_line(if form.include_prompts {
        "  With prompts: excerpts of failed prompts and agent error output are in it."
    } else {
        "  Prompts, responses and agent error output are left out. Secrets are redacted."
    }));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("←→", "change"),
        ("enter", "write it"),
        ("esc", "cancel"),
    ]));
    fitted(frame, 90, "diagnostic report", lines);
}

fn listen(app: &App, frame: &mut Frame, form: &ListenForm, tick: u64) {
    const W: usize = 14;
    let f = |i: usize| form.field == i;
    let typed = form.bind == forms::BINDS.len();
    let fields = vec![
        form_field(f(0), "port", W, form_text(&form.port, f(0), "7878", tick)),
        form_field(f(1), "bind", W, form_toggle(&form.bind_label(), f(1))),
        form_field(
            f(2),
            "address",
            W,
            if typed {
                form_text(&form.other, f(2), "100.64.0.7", tick)
            } else {
                vec![Span::styled("-", t::fg(t::DUSK))]
            },
        ),
        form_field(
            f(3),
            "keep for next",
            W,
            form_toggle(
                if form.keep {
                    "yes, in settings.json"
                } else {
                    "no, this run only"
                },
                f(3),
            ),
        ),
    ];
    let mut lines = with_errors(fields, form.error.as_ref(), W + 2);
    lines.push(Line::raw(""));
    let rt = &app.state.runtime;
    lines.push(muted_line(format!("  now {}", rt.local_url())));
    let o = rt.listen.overrides();
    let fixed: Vec<&str> = [
        (o.port, "--port or HASHI_PORT"),
        (o.bind, "--bind or HASHI_BIND"),
    ]
    .into_iter()
    .filter_map(|(on, what)| on.then_some(what))
    .collect();
    if !fixed.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} is set: it wins over the saved value at the next start.",
                fixed.join(" and ")
            ),
            t::fg(t::AMBER),
        )));
    }
    if rt
        .tunnel
        .settings()
        .is_some_and(|s| s.named_token.is_some())
    {
        lines.push(Line::from(Span::styled(
            "  A named tunnel is configured: a new port must be set on its hostname in the \
             Cloudflare dashboard too.",
            t::fg(t::AMBER),
        )));
    }
    lines.push(muted_line(
        "  Open requests finish on the old address; queued and running prompts carry on.",
    ));
    lines.push(Line::raw(""));
    lines.push(key_hint(&[
        ("↑↓", "field"),
        ("←→", "change"),
        ("enter", "move"),
        ("esc", "cancel"),
    ]));
    fitted(frame, 92, "port and bind address", lines);
}
