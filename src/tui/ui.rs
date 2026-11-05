//! Rendering (the "View" of TEA). Pure functions of [`App`].

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Offset, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Shadow, Sparkline, Table, Tabs, Wrap,
};

use super::app::{App, FeedItem, Popup, Tab, TokenForm, TunnelState};
use super::theme::{self as t, Rgb};
use crate::agent::AgentKind;
use crate::store::{Request, RequestStatus};
use crate::token::{Role, SessionMode};
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
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", t::agent_glyph(AgentKind::Claude)),
            t::fg(t::agent_color(AgentKind::Claude)),
        ),
        Span::styled("Claude Code", t::fg(TEXT)),
        Span::styled("  ⇄  ", t::fg(t::DUSK)),
        Span::styled(
            format!("{} ", t::agent_glyph(AgentKind::Codex)),
            t::fg(t::agent_color(AgentKind::Codex)),
        ),
        Span::styled("Codex", t::fg(TEXT)),
        Span::styled(format!("   v{}", env!("CARGO_PKG_VERSION")), t::muted()),
    ]));
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
    for info in &rt.agents {
        let (mark, style) = if info.available {
            ("✓", t::fg(t::MINT))
        } else {
            ("✗", t::fg(t::ROSE))
        };
        agents.push(Span::styled(
            format!("{} ", t::agent_glyph(info.agent)),
            t::fg(t::agent_color(info.agent)),
        ));
        agents.push(Span::styled(format!("{} ", info.agent), t::fg(TEXT)));
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
            Span::styled(rt.local_url.clone(), t::fg(TEXT)),
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
        t::fg(TEXT),
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
        Tab::Tokens => &[
            ("↑↓", "select"),
            ("n", "new token"),
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
    let [tiles, agents, bottom] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Fill(1),
    ])
    .areas(area);

    render_tiles(app, frame, tiles);

    let [claude, codex] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
        .spacing(1)
        .areas(agents);
    render_agent(app, frame, claude, AgentKind::Claude);
    render_agent(app, frame, codex, AgentKind::Codex);

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
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled("● ", t::fg(t::MINT)),
            Span::styled("ready", t::fg(TEXT)),
            Span::styled("  idle", t::muted()),
        ])
    };
    frame.render_widget(Paragraph::new(state_line), state);

    let counts_line = Line::from(vec![
        Span::styled(stats.completed.to_string(), t::bold(TEXT)),
        Span::styled(" done   ", t::muted()),
        Span::styled(
            stats.failed.to_string(),
            t::bold(if stats.failed > 0 { t::ROSE } else { TEXT }),
        ),
        Span::styled(" failed   ", t::muted()),
        Span::styled(
            stats.queued.to_string(),
            t::bold(if stats.queued > 0 { t::AMBER } else { TEXT }),
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
                Span::styled(human_duration(Duration::from_millis(avg)), t::fg(TEXT)),
                Span::styled("   last ", t::muted()),
                Span::styled(human_duration(Duration::from_millis(*last)), t::fg(TEXT)),
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
        Span::styled(format!("{:>4} ", human_age(item.at)), t::fg(t::DUSK)),
        Span::styled(format!("{} ", item.glyph), t::bold(item.color)),
        Span::styled(truncate(&item.text, width.saturating_sub(8)), t::fg(TEXT)),
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
        Line::from(Span::styled(
            "Waiting for the first prompt. Try:",
            t::fg(TEXT),
        )),
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
            let took = r.duration_ms().map_or_else(
                || "-".to_owned(),
                |d| human_duration(Duration::from_millis(u64::try_from(d).unwrap_or(0))),
            );
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
                Cell::from(Span::styled(truncate(&r.token_name, 12), t::fg(TEXT))),
                Cell::from(Span::styled(human_age(r.created_at), t::muted())),
                Cell::from(Span::styled(took, t::muted())),
                Cell::from(Span::styled(one_line(&r.message), t::fg(TEXT))),
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
    .row_highlight_style(Style::new().bg(SELECT.into()).add_modifier(Modifier::BOLD))
    .highlight_symbol(Span::styled("▌", t::fg(t::PINK)))
    .block(panel("requests", t::VIOLET));
    frame.render_stateful_widget(table, list, &mut app.requests_table);
    if empty {
        let hint = Paragraph::new(Span::styled("no requests yet", t::muted())).centered();
        frame.render_widget(hint, list.inner(Margin::new(2, 3)));
    }

    render_detail(app, frame, detail);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let Some(r) = app.selected_request() else {
        frame.render_widget(Paragraph::new("").block(panel("detail", t::PINK)), area);
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
        kv("token", Span::styled(r.token_name.clone(), t::fg(TEXT))),
        kv(
            "model",
            Span::styled(
                r.model.clone().unwrap_or_else(|| "default".to_owned()),
                t::fg(TEXT),
            ),
        ),
        kv(
            "session",
            Span::styled(
                if r.stateful { "stateful" } else { "stateless" },
                t::fg(TEXT),
            ),
        ),
        kv(
            "created",
            Span::styled(format!("{} ago", human_age(r.created_at)), t::fg(TEXT)),
        ),
    ];
    if let Some(d) = r.duration_ms() {
        lines.push(kv(
            "took",
            Span::styled(
                human_duration(Duration::from_millis(u64::try_from(d).unwrap_or(0))),
                t::fg(TEXT),
            ),
        ));
    }
    if let Some(u) = r.usage {
        let mut s = format!("{} in · {} out", u.input_tokens, u.output_tokens);
        if let Some(c) = u.cost_usd {
            s.push_str(&format!(" · ${c:.4}"));
        }
        lines.push(kv("usage", Span::styled(s, t::fg(TEXT))));
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
            .map(|l| Line::from(Span::styled(l.to_owned(), t::fg(TEXT)))),
    );
    lines.push(Line::raw(""));
    match (&r.response, &r.error) {
        (Some(resp), _) => {
            lines.push(rule("response"));
            lines.extend(
                resp.lines()
                    .map(|l| Line::from(Span::styled(l.to_owned(), t::fg(TEXT)))),
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
    let header = Row::new(
        [
            "name",
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
                Role::Admin => Span::styled(
                    " admin ",
                    Style::new()
                        .fg(t::INK.into())
                        .bg(t::ORCHID.into())
                        .add_modifier(Modifier::BOLD),
                ),
                Role::User => Span::styled(" user ", t::fg(TEXT)),
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
                Cell::from(Span::styled(tk.name.clone(), t::bold(TEXT))),
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
    .row_highlight_style(Style::new().bg(SELECT.into()))
    .highlight_symbol(Span::styled("▌", t::fg(t::PINK)))
    .block(
        panel("tokens", t::VIOLET).title_bottom(
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
    if app.feed.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("nothing yet", t::muted())),
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
            let rect = centered(area, 58, 18);
            let rows: [(&str, &str); 11] = [
                ("1 2 3 4", "jump to a view"),
                ("tab / ←→", "next / previous view"),
                ("↑↓ / j k", "move selection"),
                ("PgUp PgDn", "scroll request detail"),
                ("c", "copy the API base URL"),
                ("n", "new token (Tokens view)"),
                ("d", "revoke token (Tokens view)"),
                ("r", "refresh now"),
                ("?", "this help"),
                ("q / esc", "quit and stop the bridge"),
                ("ctrl-c", "quit immediately"),
            ];
            let mut lines: Vec<Line> = rows
                .iter()
                .map(|(k, v)| {
                    Line::from(vec![
                        Span::styled(format!("{k:<12}"), t::bold(t::ORCHID)),
                        Span::styled(*v, t::fg(TEXT)),
                    ])
                })
                .collect();
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
                    t::fg(TEXT),
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
                        &secret.get(..14).unwrap_or(secret)
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
        Popup::ConfirmRevoke { name, .. } => {
            let rect = centered(area, 56, 8);
            let lines = vec![
                Line::from(vec![
                    Span::styled("Revoke ", t::fg(TEXT)),
                    Span::styled(name.clone(), t::bold(t::PINK)),
                    Span::styled("?", t::fg(TEXT)),
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
        Popup::NewToken(form) => render_token_form(frame, area, form, app.tick),
    }
}

fn render_token_form(frame: &mut Frame, area: Rect, form: &TokenForm, tick: u64) {
    let rect = centered(area, 60, 13);
    let field = |i: usize, label: &str, value: Vec<Span<'static>>| {
        let focused = form.field == i;
        let mut spans = vec![
            Span::styled(if focused { "› " } else { "  " }, t::bold(t::PINK)),
            Span::styled(
                format!("{label:<10}"),
                if focused { t::bold(TEXT) } else { t::muted() },
            ),
        ];
        spans.extend(value);
        Line::from(spans)
    };
    let toggle = |s: &str, focused: bool| {
        let style = if focused {
            t::bold(t::ORCHID)
        } else {
            t::fg(TEXT)
        };
        vec![
            Span::styled("‹ ", t::fg(t::DUSK)),
            Span::styled(s.to_owned(), style),
            Span::styled(" ›", t::fg(t::DUSK)),
        ]
    };
    let cursor = if form.field == 0 && (tick / 5).is_multiple_of(2) {
        "▏"
    } else {
        " "
    };
    let name = if form.name.is_empty() && form.field != 0 {
        vec![Span::styled("my-app", t::fg(t::DUSK))]
    } else {
        vec![
            Span::styled(form.name.clone(), t::bold(t::PINK)),
            Span::styled(cursor, t::fg(t::PINK)),
        ]
    };
    let lines = vec![
        field(0, "name", name),
        Line::raw(""),
        field(1, "role", toggle(form.role.as_str(), form.field == 1)),
        field(2, "session", toggle(form.mode.as_str(), form.field == 2)),
        field(3, "agents", toggle(form.agents.label(), form.field == 3)),
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

fn pct(n: u64, d: u64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "display only")]
    let v = n as f64 * 100.0 / d as f64;
    v
}
