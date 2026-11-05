//! Rendering (the "View" of TEA). Pure functions of [`App`].

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Offset, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, HighlightSpacing, Padding, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Shadow, Sparkline, Table, Tabs, Wrap,
};

use super::app::{App, FeedItem, Popup, ShareForm, Tab, TokenForm, TokensFocus, TunnelState};
use super::theme::{self as t, Rgb};
use crate::agent::AgentKind;
use crate::store::{Request, RequestStatus};
use crate::token::{Role, SessionMode, Token, TokenStatus};
use crate::util::{human_age, human_duration, one_line, truncate};

const PANEL: Rgb = Rgb(22, 16, 42);
const SELECT: Rgb = Rgb(46, 32, 92);
const TEXT: Rgb = Rgb(232, 228, 245);

/// Draws the whole UI.
pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    let roomy = area.height >= 32 && area.width >= 100;
    let header_h = if roomy { 8 } else { 1 };

    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if roomy {
        render_header(app, frame, header);
    } else {
        render_compact_header(app, frame, header);
    }
    render_tabs(app, frame, tabs);
    let body = body.inner(Margin::new(1, 0));
    match app.tab {
        Tab::Overview => render_overview(app, frame, body),
        Tab::Requests => render_requests(app, frame, body),
        Tab::Tokens => render_tokens(app, frame, body),
        Tab::Logs => render_logs(app, frame, body),
    }
    render_footer(app, frame, footer);

    if let Some(popup) = app.popup.clone() {
        render_popup(app, frame, &popup);
    }
}

// ── chrome ─────────────────────────────────────────────────────────────────

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let [logo, _, brand, status] = Layout::horizontal([
        Constraint::Length(u16::try_from(t::LOGO_W).unwrap_or(30) + 2),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(52),
    ])
    .areas(area.inner(Margin::new(1, 0)));

    frame.render_widget(Paragraph::new(t::logo()), logo);

    // Sparkle twinkles above the right pillar, like the logo.
    let (glyph, color) = t::sparkle(app.tick);
    let spark = Rect {
        x: logo.x + 25,
        y: logo.y + 1,
        width: 1,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Span::styled(glyph, t::bold(color))), spark);

    let mut lines = vec![Line::raw("")];
    for row in t::WORDMARK {
        lines.push(Line::from(t::gradient(
            row,
            t::VIOLET,
            t::PINK,
            Modifier::BOLD,
        )));
    }
    lines.push(Line::from(vec![
        Span::styled(
            "the bridge",
            t::bold(t::PURPLE).add_modifier(Modifier::ITALIC),
        ),
        Span::styled("  to your coding agents", t::muted()),
    ]));
    lines.push(Line::raw(""));
    let shown = app.shown_agents();
    let mut brand_line = Vec::new();
    for (i, agent) in shown.iter().take(3).enumerate() {
        if i > 0 {
            brand_line.push(Span::styled("  ⇄  ", t::fg(t::DUSK)));
        }
        brand_line.push(Span::styled(
            format!("{} ", t::agent_glyph(*agent)),
            t::fg(t::agent_color(*agent)),
        ));
        brand_line.push(Span::styled(agent.display_name(), plain()));
    }
    if shown.len() > 3 {
        brand_line.push(Span::styled(
            format!("  +{} more", shown.len() - 3),
            t::muted(),
        ));
    }
    brand_line.push(Span::styled(
        format!("   v{}", env!("CARGO_PKG_VERSION")),
        t::muted(),
    ));
    lines.push(Line::from(brand_line));
    frame.render_widget(Paragraph::new(lines), brand);

    frame.render_widget(Paragraph::new(status_lines(app)), status);
}

fn status_lines(app: &App) -> Vec<Line<'static>> {
    let rt = &app.state.runtime;
    let uptime = human_duration(Duration::from_secs(rt.started.elapsed().as_secs()));
    let pulse = if (app.tick / 6).is_multiple_of(2) {
        "●"
    } else {
        "○"
    };
    let public = match &app.tunnel {
        TunnelState::Off => Span::styled("off  (start with --tunnel)", t::muted()),
        TunnelState::Starting => Span::styled(
            format!("{} opening tunnel…", t::spinner(app.tick)),
            t::fg(t::AMBER),
        ),
        TunnelState::Up(url) => Span::styled(
            url.clone(),
            t::bold(t::PINK).add_modifier(Modifier::UNDERLINED),
        ),
        TunnelState::Down(why) => Span::styled(truncate(why, 40), t::fg(t::ROSE)),
    };
    let mut agents = vec![Span::styled("agents  ", t::muted())];
    let shown = app.shown_agents();
    // Names for up to three agents; beyond that, glyphs keep the line short.
    let compact = shown.len() > 3;
    for info in rt.agents.iter().filter(|i| shown.contains(&i.agent)) {
        let (mark, style) = if info.available {
            ("✓", t::fg(t::MINT))
        } else {
            ("✗", t::fg(t::ROSE))
        };
        agents.push(Span::styled(
            format!("{} ", t::agent_glyph(info.agent)),
            t::fg(t::agent_color(info.agent)),
        ));
        if !compact {
            agents.push(Span::styled(format!("{} ", info.agent), plain()));
        }
        agents.push(Span::styled(format!("{mark}   "), style));
    }
    vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!(" {pulse} LIVE "),
                Style::new()
                    .fg(t::INK.into())
                    .bg(t::MINT.into())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  up {uptime}"), t::muted()),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("local   ", t::muted()),
            Span::styled(rt.local_url.clone(), plain()),
        ]),
        Line::from(vec![Span::styled("public  ", t::muted()), public]),
        Line::from(agents),
    ]
}

fn render_compact_header(app: &App, frame: &mut Frame, area: Rect) {
    let (glyph, color) = t::sparkle(app.tick);
    let mut spans = vec![Span::styled(format!(" {glyph} "), t::bold(color))];
    spans.extend(t::gradient("hashi", t::VIOLET, t::PINK, Modifier::BOLD));
    spans.push(Span::styled(
        " the bridge   ",
        t::fg(t::PURPLE).add_modifier(Modifier::ITALIC),
    ));
    spans.push(Span::styled(
        " ● LIVE ",
        Style::new()
            .fg(t::INK.into())
            .bg(t::MINT.into())
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!("  {}", app.state.runtime.base_url()),
        plain(),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_tabs(app: &App, frame: &mut Frame, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let badge = match tab {
                Tab::Requests if app.stats.running + app.stats.queued > 0 => {
                    format!(" ·{}", app.stats.running + app.stats.queued)
                }
                Tab::Tokens => format!(" ·{}", app.tokens.len()),
                _ => String::new(),
            };
            Line::from(vec![
                Span::styled(format!("{} ", i + 1), t::fg(t::DUSK)),
                Span::raw(tab.title()),
                Span::styled(badge, t::fg(t::ORCHID)),
            ])
        })
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.tab as usize)
        .style(t::muted())
        .highlight_style(t::bold(t::PINK).add_modifier(Modifier::UNDERLINED))
        .divider(Span::styled("  ", t::muted()))
        .padding(" ", " ")
        .block(
            Block::new()
                .borders(ratatui::widgets::Borders::BOTTOM)
                .border_style(t::border()),
        );
    frame.render_widget(tabs, area.inner(Margin::new(1, 0)));
}

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let keys: &[(&str, &str)] = match app.tab {
        Tab::Overview => &[
            ("1-4", "views"),
            ("c", "copy url"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Requests => &[
            ("↑↓", "select"),
            ("PgUp/PgDn", "scroll"),
            ("c", "copy url"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Tokens if app.tokens_focus == TokensFocus::Invites => &[
            ("↑↓", "select"),
            ("x", "cancel invite"),
            ("i", "tokens"),
            ("s", "share"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Tokens if !app.invites.is_empty() => &[
            ("↑↓", "select"),
            ("n", "new"),
            ("s", "share"),
            ("p", "pause"),
            ("d", "revoke"),
            ("i", "invites"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Tokens => &[
            ("↑↓", "select"),
            ("n", "new"),
            ("s", "share"),
            ("p", "pause"),
            ("d", "revoke"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Tab::Logs => &[("1-4", "views"), ("?", "help"), ("q", "quit")],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::new()
                .fg(TEXT.into())
                .bg(SELECT.into())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}  "), t::muted()));
    }
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(48)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
    if let Some((msg, _)) = &app.toast {
        let line = Line::from(vec![
            Span::styled("✦ ", t::fg(t::SPARK)),
            Span::styled(msg.clone(), t::fg(t::PINK)),
        ]);
        frame.render_widget(
            Paragraph::new(line).alignment(Alignment::Right),
            right.inner(Margin::new(1, 0)),
        );
    }
}

fn panel<'a>(title: &'a str, accent: Rgb) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t::border())
        .title(Line::from(vec![
            Span::styled(" ", t::border()),
            Span::styled(title, t::bold(accent)),
            Span::styled(" ", t::border()),
        ]))
        .padding(Padding::horizontal(1))
}

// ── overview ───────────────────────────────────────────────────────────────

fn render_overview(app: &App, frame: &mut Frame, area: Rect) {
    let shown = app.shown_agents();
    // Up to three cards per row; a second row when more agents are installed.
    let rows = shown.len().div_ceil(3).max(1);
    let card_h: u16 = if rows == 1 { 9 } else { 7 };
    let rows_u16 = u16::try_from(rows).unwrap_or(1);
    let [tiles, agents, bottom] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(card_h * rows_u16),
        Constraint::Fill(1),
    ])
    .areas(area);

    render_tiles(app, frame, tiles);

    let per_row = shown.len().div_ceil(rows).max(1);
    let row_areas = Layout::vertical(vec![Constraint::Length(card_h); rows]).split(agents);
    for (row, chunk) in shown.chunks(per_row).enumerate() {
        let Some(row_area) = row_areas.get(row) else {
            break;
        };
        let cells = Layout::horizontal(vec![Constraint::Fill(1); chunk.len()])
            .spacing(1)
            .split(*row_area);
        for (agent, cell) in chunk.iter().zip(cells.iter()) {
            render_agent(app, frame, *cell, *agent);
        }
    }

    let [throughput, activity] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Fill(1)])
            .spacing(1)
            .areas(bottom);
    render_throughput(app, frame, throughput);
    render_activity(app, frame, activity);
}

fn render_tiles(app: &App, frame: &mut Frame, area: Rect) {
    let s = app.stats;
    let finished = s.completed + s.failed;
    let rate = if finished == 0 {
        "-".to_owned()
    } else {
        format!("{:.0}%", pct(s.completed, finished))
    };
    let mut lat: Vec<u64> = AgentKind::ALL
        .iter()
        .flat_map(|a| app.latencies(*a, 100))
        .collect();
    lat.sort_unstable();
    let avg = if lat.is_empty() {
        "-".to_owned()
    } else {
        human_duration(Duration::from_millis(
            lat.iter().sum::<u64>() / lat.len() as u64,
        ))
    };
    let p95 = lat
        .get((lat.len() * 95 / 100).min(lat.len().saturating_sub(1)))
        .map_or_else(
            || "-".to_owned(),
            |v| human_duration(Duration::from_millis(*v)),
        );

    let tiles = [
        (
            "requests",
            s.total.to_string(),
            format!("{} token{}", s.tokens, if s.tokens == 1 { "" } else { "s" }),
            t::VIOLET,
        ),
        (
            "in flight",
            s.running.to_string(),
            format!("{} queued", s.queued),
            t::AMBER,
        ),
        ("success", rate, format!("{} failed", s.failed), t::MINT),
        ("avg latency", avg, format!("p95 {p95}"), t::PINK),
    ];
    let areas = Layout::horizontal([Constraint::Fill(1); 4])
        .spacing(1)
        .split(area);
    for ((label, value, sub, color), area) in tiles.into_iter().zip(areas.iter()) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(t::border())
            .title(Span::styled(format!(" {label} "), t::muted()))
            .padding(Padding::horizontal(1));
        let text = vec![
            Line::from(Span::styled(value, t::bold(color))),
            Line::from(Span::styled(sub, t::muted())),
        ];
        frame.render_widget(Paragraph::new(text).block(block), *area);
    }
}

fn render_agent(app: &App, frame: &mut Frame, area: Rect, agent: AgentKind) {
    let info = app.state.runtime.agents.iter().find(|a| a.agent == agent);
    let available = info.is_some_and(|i| i.available);
    let accent = t::agent_color(agent);
    let stats = app.agent_stats.get(&agent).copied().unwrap_or_default();

    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t::fg(if available {
            accent.lerp(t::DUSK, 0.45)
        } else {
            t::DUSK
        }))
        .title(Line::from(vec![
            Span::styled(format!(" {} ", t::agent_glyph(agent)), t::bold(accent)),
            Span::styled(format!("{} ", agent.display_name()), t::bold(accent)),
        ]))
        .padding(Padding::horizontal(1));
    if let Some(v) = info.and_then(|i| i.version.clone()) {
        block = block.title(
            Line::from(Span::styled(format!(" {} ", truncate(&v, 28)), t::muted())).right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [state, counts, timing, _, spark] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);

    let state_line = if !available {
        Line::from(vec![
            Span::styled("○ ", t::fg(t::ROSE)),
            Span::styled(
                format!("not installed: put `{}` on PATH", agent.binary_name()),
                t::muted(),
            ),
        ])
    } else if stats.running > 0 {
        let phase = (app.tick % 24) as f32 / 24.0;
        let mut spans = vec![Span::styled(
            format!("{} ", t::spinner(app.tick)),
            t::bold(accent),
        )];
        let label = if stats.running == 1 {
            "thinking…".to_owned()
        } else {
            format!("thinking on {} prompts…", stats.running)
        };
        spans.extend(t::shimmer(&label, accent.lerp(t::DUSK, 0.2), phase));
        if let Some(ms) = app.running_for(agent) {
            spans.push(Span::styled(
                format!("  {}", human_duration(Duration::from_secs(ms / 1000))),
                t::muted(),
            ));
        }
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled("● ", t::fg(t::MINT)),
            Span::styled("ready", plain()),
            Span::styled("  idle", t::muted()),
        ])
    };
    frame.render_widget(Paragraph::new(state_line), state);

    let counts_line = Line::from(vec![
        Span::styled(stats.completed.to_string(), strong()),
        Span::styled(" done   ", t::muted()),
        Span::styled(
            stats.failed.to_string(),
            if stats.failed > 0 {
                t::bold(t::ROSE)
            } else {
                strong()
            },
        ),
        Span::styled(" failed   ", t::muted()),
        Span::styled(
            stats.queued.to_string(),
            if stats.queued > 0 {
                t::bold(t::AMBER)
            } else {
                strong()
            },
        ),
        Span::styled(" queued", t::muted()),
    ]);
    frame.render_widget(Paragraph::new(counts_line), counts);

    let lat = app.latencies(agent, 60);
    let timing_line = match (lat.last(), lat.is_empty()) {
        (Some(last), false) => {
            let avg = lat.iter().sum::<u64>() / lat.len() as u64;
            Line::from(vec![
                Span::styled("avg ", t::muted()),
                Span::styled(human_duration(Duration::from_millis(avg)), plain()),
                Span::styled("   last ", t::muted()),
                Span::styled(human_duration(Duration::from_millis(*last)), plain()),
            ])
        }
        _ => Line::from(Span::styled("no runs yet", t::muted())),
    };
    frame.render_widget(Paragraph::new(timing_line), timing);

    if !lat.is_empty() {
        let width = usize::from(spark.width);
        let data: Vec<u64> = lat.iter().rev().take(width).rev().copied().collect();
        frame.render_widget(
            Sparkline::default()
                .data(&data)
                .style(t::fg(accent.lerp(t::DUSK, 0.25))),
            spark,
        );
    }
}

fn render_throughput(app: &App, frame: &mut Frame, area: Rect) {
    let block = panel("throughput", t::VIOLET).title_bottom(
        Line::from(Span::styled(" last 15 min · 15s buckets ", t::muted())).right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let buckets = usize::from(inner.width).max(1);
    let data = app.throughput(Duration::from_secs(15), buckets.min(60));
    if data.iter().all(|v| *v == 0) {
        let hint = Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled("quiet so far", t::muted())).centered(),
        ]);
        frame.render_widget(hint, inner);
        return;
    }
    let peak = data.iter().copied().max().unwrap_or(1).max(4);
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(peak)
            .style(t::fg(t::ORCHID)),
        inner,
    );
}

fn feed_line(item: &FeedItem, width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:>4} ", human_age(item.at)), t::muted()),
        Span::styled(format!("{} ", item.glyph), t::bold(item.color)),
        Span::styled(truncate(&item.text, width.saturating_sub(8)), plain()),
    ])
}

fn render_activity(app: &App, frame: &mut Frame, area: Rect) {
    let block = panel("activity", t::PINK);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.stats.total == 0 {
        frame.render_widget(
            Paragraph::new(quickstart(app)).wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    let lines: Vec<Line> = app
        .feed
        .iter()
        .take(usize::from(inner.height))
        .map(|f| feed_line(f, usize::from(inner.width)))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn quickstart(app: &App) -> Vec<Line<'static>> {
    let url = app.state.runtime.base_url();
    vec![
        Line::from(Span::styled("Waiting for the first prompt. Try:", plain())),
        Line::raw(""),
        Line::from(vec![
            Span::styled("$ ", t::fg(t::DUSK)),
            Span::styled(format!("curl {url}/v1/ask \\"), t::fg(t::ORCHID)),
        ]),
        Line::from(Span::styled(
            "    -H \"Authorization: Bearer $HASHI_TOKEN\" \\",
            t::fg(t::ORCHID),
        )),
        Line::from(Span::styled(
            "    -d '{\"message\":\"hello\",\"agent\":\"claude\",\"wait\":60}'",
            t::fg(t::ORCHID),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Create tokens in the ", t::muted()),
            Span::styled("3 Tokens", t::bold(t::PINK)),
            Span::styled(" view.", t::muted()),
        ]),
    ]
}

// ── requests ───────────────────────────────────────────────────────────────

fn status_span(status: RequestStatus, tick: u64) -> Span<'static> {
    match status {
        RequestStatus::Queued => Span::styled("◌", t::fg(t::MIST)),
        RequestStatus::Running => Span::styled(t::spinner(tick), t::bold(t::AMBER)),
        RequestStatus::Completed => Span::styled("✓", t::bold(t::MINT)),
        RequestStatus::Failed => Span::styled("✗", t::bold(t::ROSE)),
    }
}

fn render_requests(app: &mut App, frame: &mut Frame, area: Rect) {
    let [list, detail] = Layout::horizontal([Constraint::Percentage(56), Constraint::Fill(1)])
        .spacing(1)
        .areas(area);

    let header = Row::new(
        ["", "id", "agent", "token", "age", "took", "prompt"]
            .map(|h| Cell::from(h).style(t::bold(t::MIST))),
    )
    .bottom_margin(0);
    let rows: Vec<Row> = app
        .recent
        .iter()
        .map(|r| {
            let took = elapsed_label(r);
            Row::new(vec![
                Cell::from(status_span(r.status, app.tick)),
                Cell::from(Span::styled(
                    r.id.get(..8).unwrap_or(&r.id).to_owned(),
                    t::fg(t::DUSK.lighten(0.3)),
                )),
                Cell::from(Span::styled(
                    format!("{} {}", t::agent_glyph(r.agent), r.agent),
                    t::fg(t::agent_color(r.agent)),
                )),
                Cell::from(Span::styled(truncate(&r.token_name, 12), plain())),
                Cell::from(Span::styled(human_age(r.created_at), t::muted())),
                Cell::from(Span::styled(took, t::muted())),
                Cell::from(Span::styled(one_line(&r.message), plain())),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(
        Style::new()
            .fg(TEXT.into())
            .bg(SELECT.into())
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol(Span::styled("▌", t::fg(t::PINK)))
    .block(panel("requests", t::VIOLET));
    frame.render_stateful_widget(table, list, &mut app.requests_table);
    if empty {
        let hint = Paragraph::new(vec![
            Line::from(Span::styled("No requests yet", strong())).centered(),
            Line::from(Span::styled(
                "Send one with POST /v1/ask. Press 1 for a curl example.",
                t::muted(),
            ))
            .centered(),
        ]);
        frame.render_widget(hint, list.inner(Margin::new(2, 3)));
    }

    render_detail(app, frame, detail);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let Some(r) = app.selected_request() else {
        let hint = Paragraph::new(Span::styled(
            "Select a request to see its prompt and answer.",
            t::muted(),
        ));
        frame.render_widget(hint.block(panel("detail", t::PINK)), area);
        return;
    };
    let title = format!("request {}", r.id.get(..8).unwrap_or(&r.id));
    let block = panel(&title, t::PINK);
    let inner = block.inner(area);
    let text = detail_text(r, app.tick);
    let width = usize::from(inner.width.max(1));
    let total = u16::try_from(
        text.lines
            .iter()
            .map(|l| l.width().max(1).div_ceil(width))
            .sum::<usize>(),
    )
    .unwrap_or(u16::MAX);
    let max_scroll = total.saturating_sub(inner.height);
    let scroll = app.detail_scroll.min(max_scroll);

    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(block),
        area,
    );
    if max_scroll > 0 {
        let mut state = ScrollbarState::new(usize::from(max_scroll)).position(usize::from(scroll));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(t::fg(t::DUSK))
                .thumb_style(t::fg(t::ORCHID)),
            area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }
}

fn detail_text(r: &Request, tick: u64) -> Text<'static> {
    let kv = |k: &str, v: Span<'static>| {
        Line::from(vec![Span::styled(format!("{k:<8}"), t::muted()), v])
    };
    let mut lines = vec![
        kv("status", {
            let mut s = status_span(r.status, tick);
            s.content = format!("{} {}", s.content, r.status.as_str()).into();
            s
        }),
        kv(
            "agent",
            Span::styled(
                format!("{} {}", t::agent_glyph(r.agent), r.agent.display_name()),
                t::fg(t::agent_color(r.agent)),
            ),
        ),
        kv("token", Span::styled(r.token_name.clone(), plain())),
        kv(
            "model",
            Span::styled(
                r.model.clone().unwrap_or_else(|| "default".to_owned()),
                plain(),
            ),
        ),
        kv(
            "session",
            Span::styled(if r.stateful { "stateful" } else { "stateless" }, plain()),
        ),
        kv(
            "created",
            Span::styled(format!("{} ago", human_age(r.created_at)), plain()),
        ),
    ];
    match r.status {
        RequestStatus::Running => lines.push(kv(
            "running",
            Span::styled(format!("{} so far", elapsed_label(r)), t::fg(t::AMBER)),
        )),
        _ if r.duration_ms().is_some() => {
            lines.push(kv("took", Span::styled(elapsed_label(r), plain())));
        }
        _ => {}
    }
    if let Some(u) = r.usage {
        let mut s = format!("{} in · {} out", u.input_tokens, u.output_tokens);
        if let Some(c) = u.cost_usd {
            s.push_str(&format!(" · ${c:.4}"));
        }
        lines.push(kv("usage", Span::styled(s, plain())));
    }
    let rule = |label: &str| {
        Line::from(vec![
            Span::styled(format!("── {label} "), t::fg(t::ORCHID)),
            Span::styled("─".repeat(40), t::fg(t::DUSK)),
        ])
    };
    lines.push(Line::raw(""));
    lines.push(rule("prompt"));
    lines.extend(
        r.message
            .lines()
            .map(|l| Line::from(Span::styled(l.to_owned(), plain()))),
    );
    lines.push(Line::raw(""));
    match (&r.response, &r.error) {
        (Some(resp), _) => {
            lines.push(rule("response"));
            lines.extend(
                resp.lines()
                    .map(|l| Line::from(Span::styled(l.to_owned(), plain()))),
            );
        }
        (None, Some(err)) => {
            lines.push(rule("error"));
            lines.extend(
                err.lines()
                    .map(|l| Line::from(Span::styled(l.to_owned(), t::fg(t::ROSE)))),
            );
        }
        (None, None) => lines.push(Line::from(Span::styled(
            "waiting for the agent…",
            t::muted(),
        ))),
    }
    Text::from(lines)
}

// ── tokens ─────────────────────────────────────────────────────────────────

fn render_tokens(app: &mut App, frame: &mut Frame, area: Rect) {
    if app.invites.is_empty() {
        render_token_table(app, frame, area);
        return;
    }
    let rows = u16::try_from(app.invites.len()).unwrap_or(u16::MAX).min(5);
    let [top, bottom] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(rows + 3)]).areas(area);
    render_token_table(app, frame, top);
    render_invites(app, frame, bottom);
}

/// Panel title that marks the focused table with a symbol as well as colour.
fn focus_panel(title: &str, focused: bool) -> Block<'_> {
    let (mark, accent) = if focused {
        ("▸ ", t::PINK)
    } else {
        ("  ", t::VIOLET)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            t::fg(t::PURPLE)
        } else {
            t::border()
        })
        .title(Line::from(vec![
            Span::styled(" ", t::border()),
            Span::styled(mark, t::bold(t::PINK)),
            Span::styled(title, t::bold(accent)),
            Span::styled(" ", t::border()),
        ]))
        .padding(Padding::horizontal(1))
}

fn token_status_span(tk: &Token, now: i64) -> Span<'static> {
    match tk.status(now) {
        TokenStatus::Active => Span::styled("● active", t::fg(t::MINT)),
        TokenStatus::Paused => Span::styled("‖ paused", t::bold(t::AMBER)),
        TokenStatus::Expired => Span::styled("○ expired", t::fg(t::ROSE)),
    }
}

fn today_span(app: &App, tk: &Token) -> Span<'static> {
    let used = app.usage.get(&tk.id).map_or(0, |u| u.today_requests);
    match tk.limits.daily_requests {
        Some(cap) => {
            let style = if used >= u64::from(cap) {
                t::bold(t::AMBER)
            } else {
                plain()
            };
            Span::styled(format!("{used}/{cap}"), style)
        }
        None => Span::styled(used.to_string(), plain()),
    }
}

/// Time until `ms` from now: `in 23h`, `in 6d`.
fn human_until(ms: i64) -> String {
    let secs = ((ms - crate::util::now_ms()) / 1000).max(0);
    match secs {
        0..60 => format!("in {secs}s"),
        60..3_600 => format!("in {}m", secs / 60),
        3_600..86_400 => format!("in {}h", secs / 3_600),
        _ => format!("in {}d", secs / 86_400),
    }
}

fn render_invites(app: &mut App, frame: &mut Frame, area: Rect) {
    let focused = app.tokens_focus == TokensFocus::Invites;
    let header =
        Row::new(["label", "expires", "created"].map(|h| Cell::from(h).style(t::bold(t::MIST))));
    let rows: Vec<Row> = app
        .invites
        .iter()
        .map(|inv| {
            Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled("◌ ", t::fg(t::ORCHID)),
                    Span::styled(inv.label.clone(), strong()),
                ])),
                Cell::from(Span::styled(human_until(inv.expires_at), plain())),
                Cell::from(Span::styled(
                    format!("{} ago", human_age(inv.created_at)),
                    t::muted(),
                )),
            ])
        })
        .collect();
    let mut table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .column_spacing(2)
    .block(
        focus_panel("pending invites", focused).title_bottom(
            Line::from(Span::styled(
                if focused {
                    " x cancel   i back to tokens "
                } else {
                    " i to select "
                },
                t::muted(),
            ))
            .right_aligned(),
        ),
    )
    .highlight_spacing(HighlightSpacing::Always);
    if focused {
        table = table
            .row_highlight_style(Style::new().bg(SELECT.into()))
            .highlight_symbol(Span::styled("▌", t::fg(t::PINK)));
        frame.render_stateful_widget(table, area, &mut app.invites_table);
    } else {
        frame.render_widget(table.highlight_symbol(" "), area);
    }
}

fn render_token_table(app: &mut App, frame: &mut Frame, area: Rect) {
    let now = crate::util::now_ms();
    let focused = app.tokens_focus == TokensFocus::Tokens;
    let header = Row::new(
        [
            "name",
            "status",
            "today",
            "prefix",
            "role",
            "session",
            "agents",
            "created",
            "last used",
            "expires",
        ]
        .map(|h| Cell::from(h).style(t::bold(t::MIST))),
    );
    let rows: Vec<Row> = app
        .tokens
        .iter()
        .map(|tk| {
            let role = match tk.role {
                Role::Admin => Span::styled("★ admin", t::bold(t::ORCHID)),
                Role::User => Span::styled(" user ", plain()),
            };
            let mode = match tk.session_mode {
                SessionMode::Stateful => Span::styled("stateful", t::fg(t::PINK)),
                SessionMode::Stateless => Span::styled("stateless", t::muted()),
            };
            let agents: Vec<Span> = tk
                .agents
                .iter()
                .map(|a| {
                    Span::styled(
                        format!("{} ", t::agent_glyph(*a)),
                        t::fg(t::agent_color(*a)),
                    )
                })
                .collect();
            let expires = tk.expires_at.map_or_else(
                || "never".to_owned(),
                |e| {
                    let left = e - crate::util::now_ms();
                    if left <= 0 {
                        "expired".to_owned()
                    } else {
                        format!("in {}d", left / 86_400_000)
                    }
                },
            );
            Row::new(vec![
                Cell::from(Span::styled(tk.name.clone(), strong())),
                Cell::from(token_status_span(tk, now)),
                Cell::from(today_span(app, tk)),
                Cell::from(Span::styled(
                    format!("{}…", tk.prefix),
                    t::fg(t::DUSK.lighten(0.3)),
                )),
                Cell::from(role),
                Cell::from(mode),
                Cell::from(Line::from(agents)),
                Cell::from(Span::styled(
                    format!("{} ago", human_age(tk.created_at)),
                    t::muted(),
                )),
                Cell::from(Span::styled(
                    tk.last_used_at
                        .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u))),
                    t::muted(),
                )),
                Cell::from(Span::styled(expires, t::muted())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Length(16),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Length(9),
        ],
    )
    .header(header.bottom_margin(1))
    .column_spacing(2)
    .row_highlight_style(if focused {
        Style::new().bg(SELECT.into())
    } else {
        Style::new()
    })
    .highlight_symbol(Span::styled(
        if focused { "▌" } else { " " },
        t::fg(t::PINK),
    ))
    .block(
        focus_panel("tokens", focused && !app.invites.is_empty()).title_bottom(
            Line::from(Span::styled(
                " secrets are stored as SHA-256 hashes only ",
                t::muted(),
            ))
            .right_aligned(),
        ),
    );
    frame.render_stateful_widget(table, area, &mut app.tokens_table);
}

// ── logs ───────────────────────────────────────────────────────────────────

fn render_logs(app: &App, frame: &mut Frame, area: Rect) {
    let block = panel("event log", t::VIOLET);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [inner, hint] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Something wrong? Run `hashi report` and attach the file to an issue.",
            t::muted(),
        )),
        hint,
    );
    if app.feed.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Events appear here as requests arrive and agents answer.",
                t::muted(),
            )),
            inner,
        );
        return;
    }
    let lines: Vec<Line> = app
        .feed
        .iter()
        .take(usize::from(inner.height))
        .map(|f| feed_line(f, usize::from(inner.width)))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

// ── popups ─────────────────────────────────────────────────────────────────

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(4));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn modal<'a>(title: &'a str) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t::fg(t::PURPLE))
        .style(Style::new().bg(PANEL.into()).fg(TEXT.into()))
        .title(Line::from(vec![
            Span::styled(" ✦ ", t::bold(t::SPARK)),
            Span::styled(title, t::bold(t::PINK)),
            Span::raw(" "),
        ]))
        .padding(Padding::new(2, 2, 1, 0))
        .shadow(
            Shadow::dark_shade()
                .style(t::fg(Rgb(12, 8, 24)))
                .offset(Offset::new(2, 1)),
        )
}

fn key_hint(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, label) in keys {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::new()
                .fg(t::INK.into())
                .bg(t::ORCHID.into())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}   "), t::muted()));
    }
    Line::from(spans)
}

fn render_popup(app: &App, frame: &mut Frame, popup: &Popup) {
    let area = frame.area();
    match popup {
        Popup::Help => {
            let rows: [(&str, &str); 16] = [
                ("1 2 3 4", "jump to a view"),
                ("tab / ←→", "next / previous view"),
                ("↑↓ / j k", "move selection"),
                ("PgUp PgDn", "scroll request detail"),
                ("c", "copy the API base URL"),
                ("n", "new token (Tokens view)"),
                ("d", "revoke token (Tokens view)"),
                ("p", "pause or resume token (Tokens view)"),
                ("s", "share: create an invite link (Tokens view)"),
                ("i", "switch between tokens and invites"),
                ("x", "cancel pending invite (Tokens view)"),
                ("r", "refresh now"),
                ("?", "this help"),
                ("q", "quit (asks first if work is running)"),
                ("esc", "close a popup"),
                ("ctrl-c", "quit immediately"),
            ];
            let mut lines: Vec<Line> = rows
                .iter()
                .map(|(k, v)| {
                    Line::from(vec![
                        Span::styled(format!("{k:<12}"), t::bold(t::ORCHID)),
                        Span::styled(*v, plain()),
                    ])
                })
                .collect();
            let height = u16::try_from(rows.len()).unwrap_or(16) + 7;
            let rect = centered(area, 62, height);
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "press any key to close",
                t::muted(),
            )));
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(lines).block(modal("keys")), rect);
        }
        Popup::Reveal { title, secret } => {
            let rect = centered(area, 84, 14);
            let url = app.state.runtime.base_url();
            let lines = vec![
                Line::from(Span::styled(
                    "Copy it now. Hashi keeps only a hash, so it cannot be shown again.",
                    plain(),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    format!(" {secret} "),
                    Style::new()
                        .fg(t::PINK.into())
                        .bg(SELECT.into())
                        .add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(Span::styled("Use it as a bearer token:", t::muted())),
                Line::from(Span::styled(
                    format!(
                        "curl {url}/v1/ask -H \"Authorization: Bearer {}…\" \\",
                        secret.get(..14).unwrap_or(secret)
                    ),
                    t::fg(t::ORCHID),
                )),
                Line::from(Span::styled(
                    "     -d '{\"message\":\"hello\",\"wait\":60}'",
                    t::fg(t::ORCHID),
                )),
                Line::raw(""),
                key_hint(&[("c", "copy"), ("enter", "done")]),
            ];
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(modal(title)),
                rect,
            );
        }
        Popup::ConfirmQuit { pending } => {
            let rect = centered(area, 68, 8);
            let noun = if *pending == 1 {
                "request is"
            } else {
                "requests are"
            };
            let lines = vec![
                Line::from(Span::styled(
                    format!("{pending} {noun} still in flight."),
                    strong(),
                )),
                Line::from(Span::styled(
                    "Quitting stops the bridge. They resume on the next start.",
                    t::muted(),
                )),
                Line::raw(""),
                key_hint(&[("y", "quit"), ("n", "keep running")]),
            ];
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(lines).block(modal("quit hashi?")), rect);
        }
        Popup::ConfirmRevoke { name, .. } => {
            let rect = centered(area, 56, 8);
            let lines = vec![
                Line::from(vec![
                    Span::styled("Revoke ", plain()),
                    Span::styled(name.clone(), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                Line::from(Span::styled(
                    "Apps using it will start getting 401s.",
                    t::muted(),
                )),
                Line::raw(""),
                key_hint(&[("y", "revoke"), ("n", "keep")]),
            ];
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(lines).block(modal("revoke token")), rect);
        }
        Popup::ConfirmCancelInvite { label, .. } => {
            let rect = centered(area, 56, 8);
            let lines = vec![
                Line::from(vec![
                    Span::styled("Cancel the invite for ", plain()),
                    Span::styled(label.clone(), t::bold(t::PINK)),
                    Span::styled("?", plain()),
                ]),
                Line::from(Span::styled("The link will stop working.", t::muted())),
                Line::raw(""),
                key_hint(&[("y", "cancel invite"), ("n", "keep")]),
            ];
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(lines).block(modal("cancel invite")), rect);
        }
        Popup::InviteReveal {
            label,
            url,
            link_hours,
            local_only,
        } => {
            let height = if *local_only { 15 } else { 12 };
            let width = u16::try_from(url.chars().count() + 10)
                .unwrap_or(u16::MAX)
                .max(80);
            let rect = centered(area, width, height);
            let mut lines = vec![
                Line::from(Span::styled(
                    "Send this link to the person you are sharing with:",
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
                Line::from(vec![
                    Span::styled("◌ ", t::fg(t::ORCHID)),
                    Span::styled(
                        format!(
                            "Works once, expires in {}. Opening it gives them their own token.",
                            super::app::link_short(*link_hours)
                        ),
                        t::muted(),
                    ),
                ]),
            ];
            if *local_only {
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled("! ", t::bold(t::AMBER)),
                    Span::styled(
                        "No public URL: this link only works on this machine.",
                        t::fg(t::AMBER),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    "  Restart with --tunnel or --public-url to share it.",
                    t::muted(),
                )));
            }
            lines.push(Line::raw(""));
            lines.push(key_hint(&[("c", "copy"), ("enter", "done")]));
            let title = format!("Invite for {label}");
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(modal(&title)),
                rect,
            );
        }
        Popup::Share(form) => render_share_form(frame, area, form, app.tick),
        Popup::NewToken(form) => render_token_form(frame, area, form, app.tick),
    }
}

/// One form row: focus marker, label column and value.
fn form_field(
    focused: bool,
    label: &str,
    width: usize,
    value: Vec<Span<'static>>,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(if focused { "› " } else { "  " }, t::bold(t::PINK)),
        Span::styled(
            format!("{label:<width$}"),
            if focused { strong() } else { t::muted() },
        ),
    ];
    spans.extend(value);
    Line::from(spans)
}

/// A `‹ value ›` choice.
fn form_toggle(s: &str, focused: bool) -> Vec<Span<'static>> {
    let style = if focused { t::bold(t::ORCHID) } else { plain() };
    vec![
        Span::styled("‹ ", t::fg(t::DUSK)),
        Span::styled(s.to_owned(), style),
        Span::styled(" ›", t::fg(t::DUSK)),
    ]
}

/// A text input with a blinking cursor, or a placeholder when empty and unfocused.
fn form_text(
    value: &str,
    focused: bool,
    placeholder: &'static str,
    tick: u64,
) -> Vec<Span<'static>> {
    if value.is_empty() && !focused {
        return vec![Span::styled(placeholder, t::fg(t::DUSK))];
    }
    let cursor = if focused && (tick / 5).is_multiple_of(2) {
        "▏"
    } else {
        " "
    };
    vec![
        Span::styled(value.to_owned(), t::bold(t::PINK)),
        Span::styled(cursor, t::fg(t::PINK)),
    ]
}

fn form_error(error: Option<&String>, indent: usize) -> Line<'static> {
    match error {
        Some(err) => Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(format!("✗ {err}"), t::fg(t::ROSE)),
        ]),
        None => Line::raw(""),
    }
}

fn render_share_form(frame: &mut Frame, area: Rect, form: &ShareForm, tick: u64) {
    const W: usize = 16;
    let rect = centered(area, 72, 17);
    let f = |i: usize| form.field == i;
    let lines = vec![
        form_field(
            f(0),
            "label",
            W,
            form_text(&form.label, f(0), "alice", tick),
        ),
        form_error(form.error.as_ref(), W + 2),
        form_field(f(1), "agents", W, form_toggle(&form.agents.label(), f(1))),
        form_field(f(2), "session", W, form_toggle(form.mode.as_str(), f(2))),
        form_field(
            f(3),
            "daily prompts",
            W,
            form_toggle(&form.daily_label(), f(3)),
        ),
        form_field(
            f(4),
            "daily budget",
            W,
            form_toggle(&form.budget_label(), f(4)),
        ),
        form_field(
            f(5),
            "token lifetime",
            W,
            form_toggle(form.lifetime_label(), f(5)),
        ),
        form_field(
            f(6),
            "link valid for",
            W,
            form_toggle(form.link_label(), f(6)),
        ),
        Line::raw(""),
        Line::from(Span::styled(
            "  The link works once and creates a user token for them.",
            t::muted(),
        )),
        Line::from(Span::styled(
            "  At most 3 of their prompts run at a time.",
            t::muted(),
        )),
        Line::raw(""),
        key_hint(&[
            ("↑↓", "field"),
            ("←→", "change"),
            ("enter", "create link"),
            ("esc", "cancel"),
        ]),
    ];
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(modal("share")), rect);
}

fn render_token_form(frame: &mut Frame, area: Rect, form: &TokenForm, tick: u64) {
    const W: usize = 10;
    let rect = centered(area, 60, 14);
    let f = |i: usize| form.field == i;
    let lines = vec![
        form_field(f(0), "name", W, form_text(&form.name, f(0), "my-app", tick)),
        form_error(form.error.as_ref(), W + 2),
        form_field(f(1), "role", W, form_toggle(form.role.as_str(), f(1))),
        form_field(f(2), "session", W, form_toggle(form.mode.as_str(), f(2))),
        form_field(f(3), "agents", W, form_toggle(&form.agents.label(), f(3))),
        Line::raw(""),
        Line::from(Span::styled(
            match form.mode {
                SessionMode::Stateful => {
                    "  stateful: each agent remembers this token's conversation"
                }
                SessionMode::Stateless => "  stateless: every prompt starts a fresh conversation",
            },
            t::muted(),
        )),
        Line::raw(""),
        key_hint(&[
            ("↑↓", "field"),
            ("←→", "toggle"),
            ("enter", "create"),
            ("esc", "cancel"),
        ]),
    ];
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(modal("new token")), rect);
}

/// Body text: inherits the terminal (or popup) foreground so it stays
/// readable on both light and dark themes.
fn plain() -> Style {
    Style::new()
}

/// Emphasised body text.
fn strong() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// Run time for finished requests, live elapsed time for running ones.
fn elapsed_label(r: &Request) -> String {
    let ms = match (r.status, r.started_at, r.duration_ms()) {
        (_, _, Some(d)) => d,
        (RequestStatus::Running, Some(start), None) => crate::util::now_ms() - start,
        _ => return "-".to_owned(),
    };
    human_duration(Duration::from_millis(u64::try_from(ms).unwrap_or(0)))
}

fn pct(n: u64, d: u64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "display only")]
    let v = n as f64 * 100.0 / d as f64;
    v
}
