//! Account usage for the dashboard: gauges per account and window with
//! reset countdowns, coloured by the rotate threshold, plus the headroom
//! figure for an agent card and the feed line for an
//! [`Event::AccountUsage`].
//!
//! Self-contained: every function here takes what it draws, or reads the
//! store through [`snapshot`], so any view can place a usage section
//! without this module knowing its layout.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app::FeedItem;
use super::theme::{self as t, Rgb};
use crate::agent::AgentKind;
use crate::events::Event;
use crate::store::Store;
use crate::usage::{Summary, UsageState, Window};
use crate::util::now_ms;

/// One account's usage, as the dashboard draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    /// Agent.
    pub agent: AgentKind,
    /// Account name.
    pub name: String,
    /// Its windows.
    pub windows: Vec<Window>,
    /// Their summary.
    pub summary: Summary,
    /// The plan, when known.
    pub plan: Option<String>,
    /// Why the last read gave no figures, when it failed.
    pub note: Option<String>,
    /// When a read last succeeded, Unix ms.
    pub read_at: Option<i64>,
}

/// Every account with usage figures or a read, from `store`, under
/// `rotate_at`. Accounts with neither are left out.
#[must_use]
pub fn snapshot(store: &Store, rotate_at: u8) -> Vec<AccountUsage> {
    let now = now_ms();
    store
        .usage_accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|(agent, name)| {
            let windows = store.usage_windows(agent, &name).unwrap_or_default();
            let summary = store
                .account_summary(agent, &name, rotate_at, now)
                .unwrap_or_else(|_| Summary::unknown());
            let poll = store.poll_status(agent, &name).ok().flatten();
            AccountUsage {
                agent,
                windows,
                summary,
                plan: poll.as_ref().and_then(|p| p.plan.clone()),
                read_at: poll.as_ref().and_then(|p| p.ok_at),
                note: poll.and_then(|p| p.note),
                name,
            }
        })
        .collect()
}

/// The colour for `used` percent under `rotate_at`: mint well under it,
/// amber from ten points below it, rose at it and beyond.
#[must_use]
pub fn level_color(used: f64, rotate_at: u8) -> Rgb {
    let at = f64::from(rotate_at);
    if used >= at {
        t::ROSE
    } else if used >= at - 10.0 {
        t::AMBER
    } else {
        t::MINT
    }
}

/// The colour of a state.
#[must_use]
pub const fn state_color(state: UsageState) -> Rgb {
    match state {
        UsageState::Ok => t::MINT,
        UsageState::Warning => t::AMBER,
        UsageState::Limited => t::ROSE,
        UsageState::Unknown => t::MIST,
    }
}

/// Time until `at` (Unix ms) from `now`, short: `4h 12m`, `2d 3h`, `now`.
#[must_use]
pub fn countdown(at: i64, now: i64) -> String {
    let secs = (at.saturating_sub(now) / 1_000).max(0);
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h, m) {
        (0, 0, 0) => "now".to_owned(),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// A gauge `width` cells wide filled to `used` percent.
#[must_use]
pub fn bar(used: f64, width: usize) -> (String, String) {
    let used = crate::usage::clamp_percent(used);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ((used / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    ("█".repeat(filled), "░".repeat(width - filled))
}

/// One gauge line: label, bar, percent used and the reset countdown.
#[must_use]
pub fn window_line(w: &Window, rotate_at: u8, width: usize, now: i64) -> Line<'static> {
    let used = w.used_at(now).unwrap_or(w.used_percent);
    let color = level_color(used, rotate_at);
    let (full, empty) = bar(used, width);
    let mut spans = vec![
        Span::styled(
            format!("  {:<14}", crate::util::truncate(&w.label, 14)),
            t::muted(),
        ),
        Span::styled(full, t::fg(color)),
        Span::styled(empty, t::fg(t::DUSK)),
        Span::styled(format!(" {used:>3.0}%"), t::bold(color)),
    ];
    if let Some(at) = w.resets_at.filter(|r| *r > now) {
        spans.push(Span::styled(
            format!("  resets in {}", countdown(at, now)),
            t::muted(),
        ));
    }
    Line::from(spans)
}

/// An account's heading: name, plan, state and headroom.
#[must_use]
pub fn account_line(a: &AccountUsage, now: i64) -> Line<'static> {
    let s = &a.summary;
    let mut spans = vec![
        Span::styled(t::agent_glyph(a.agent), t::fg(t::agent_color(a.agent))),
        Span::styled(format!(" {}:{}", a.agent, a.name), t::bold(t::PINK)),
    ];
    if let Some(plan) = &a.plan {
        spans.push(Span::styled(format!("  {plan}"), t::muted()));
    }
    let state = match (s.state, s.limited_until) {
        (UsageState::Limited, Some(until)) => {
            format!("limited, back in {}", countdown(until, now))
        }
        (state, _) => state.as_str().to_owned(),
    };
    spans.push(Span::styled(
        format!("  {state}"),
        t::bold(state_color(s.state)),
    ));
    if let Some(h) = s.headroom_pct() {
        spans.push(Span::styled(format!("  {h}% left"), t::muted()));
    }
    Line::from(spans)
}

/// Every line of the usage section: per account a heading, then a gauge
/// per window, or the note saying why there are no figures.
#[must_use]
pub fn lines(accounts: &[AccountUsage], rotate_at: u8, bar_width: usize) -> Vec<Line<'static>> {
    let now = now_ms();
    if accounts.is_empty() {
        return vec![Line::from(Span::styled(
            "no usage read yet: Claude Code and Codex accounts are read every few minutes",
            t::muted(),
        ))];
    }
    let mut out = Vec::new();
    for a in accounts {
        out.push(account_line(a, now));
        if a.windows.is_empty() {
            let note = a.note.as_deref().unwrap_or("unknown");
            out.push(Line::from(Span::styled(format!("  {note}"), t::muted())));
        } else if let Some(line) = stale_line(a) {
            out.push(line);
        }
        for w in &a.windows {
            out.push(window_line(w, rotate_at, bar_width, now));
        }
    }
    out
}

/// When the last read failed but older figures are kept, says how old
/// they are and why the newest read failed, so they are not taken as now.
fn stale_line(a: &AccountUsage) -> Option<Line<'static>> {
    let note = a.note.as_deref()?;
    let age = a.read_at.map_or_else(
        || "figures from earlier".to_owned(),
        |at| format!("figures from {} ago", crate::util::human_age(at)),
    );
    Some(Line::from(Span::styled(
        format!("  {age}; the last read failed: {note}"),
        t::fg(t::AMBER),
    )))
}

/// Draws the usage section into `area`, with bars sized to fit.
pub fn render(frame: &mut Frame, area: Rect, accounts: &[AccountUsage], rotate_at: u8) {
    let width = usize::from(area.width).saturating_sub(40).clamp(8, 40);
    frame.render_widget(Paragraph::new(lines(accounts, rotate_at, width)), area);
}

/// The best headroom among an agent's accounts in `accounts` (a
/// [`snapshot`]), as the agent card shows it: the most left among those
/// that are not limited.
#[must_use]
pub fn best_headroom(accounts: &[AccountUsage], agent: AgentKind) -> Option<Summary> {
    accounts
        .iter()
        .filter(|a| a.agent == agent)
        .map(|a| &a.summary)
        .filter(|s| s.headroom.is_some() && s.state != UsageState::Limited)
        .max_by(|a, b| {
            a.headroom
                .partial_cmp(&b.headroom)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
}

/// The spans an agent card appends for the best account's headroom, or
/// none when nothing is known.
#[must_use]
pub fn headroom_spans(
    accounts: &[AccountUsage],
    agent: AgentKind,
    rotate_at: u8,
) -> Vec<Span<'static>> {
    let Some(s) = best_headroom(accounts, agent) else {
        return Vec::new();
    };
    let Some(h) = s.headroom_pct() else {
        return Vec::new();
    };
    vec![
        Span::styled("   ", t::muted()),
        Span::styled(
            format!("{h}%"),
            t::bold(level_color(100.0 - f64::from(h), rotate_at)),
        ),
        Span::styled(" left", t::muted()),
    ]
}

/// The feed line for an [`Event::AccountUsage`], and the toast to show
/// when it crossed the threshold, became limited or recovered.
#[must_use]
pub fn feed(event: &Event) -> Option<(FeedItem, Option<String>)> {
    let Event::AccountUsage {
        agent,
        account,
        state,
        headroom,
        ..
    } = event
    else {
        return None;
    };
    let alert = event.usage_alert();
    let text = alert.clone().unwrap_or_else(|| {
        format!(
            "{} account {account}: {state}{}",
            agent.display_name(),
            headroom.map(|h| format!(", {h}% left")).unwrap_or_default()
        )
    });
    let glyph = if alert.is_some() { "!" } else { "·" };
    Some((
        FeedItem {
            at: now_ms(),
            glyph,
            color: state_color(*state),
            text,
        },
        alert,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Source;

    #[test]
    fn colours_follow_the_threshold() {
        assert_eq!(level_color(10.0, 90), t::MINT);
        assert_eq!(level_color(80.0, 90), t::AMBER);
        assert_eq!(level_color(90.0, 90), t::ROSE);
        assert_eq!(level_color(60.0, 60), t::ROSE);
    }

    #[test]
    fn countdowns_are_short() {
        assert_eq!(countdown(0, 10), "now");
        assert_eq!(countdown(5 * 60_000, 0), "5m");
        assert_eq!(countdown(4 * 3_600_000 + 12 * 60_000, 0), "4h 12m");
        assert_eq!(countdown(2 * 86_400_000 + 3 * 3_600_000, 0), "2d 3h");
    }

    #[test]
    fn bars_fill_to_the_percent() {
        assert_eq!(bar(50.0, 10), ("█".repeat(5), "░".repeat(5)));
        assert_eq!(bar(150.0, 4).0.chars().count(), 4);
        assert_eq!(bar(0.0, 4).1.chars().count(), 4);
    }

    #[test]
    fn lines_show_gauges_or_the_note() {
        let now = now_ms();
        let a = AccountUsage {
            agent: AgentKind::Claude,
            name: "work".into(),
            windows: vec![Window::new(
                "five_hour",
                "5 hours",
                95.0,
                Some(now + 3_600_000),
                Some(300),
                Source::Poll,
            )],
            summary: crate::usage::summarize(&[], None, 90, now),
            plan: Some("max".into()),
            note: None,
            read_at: None,
        };
        let text: Vec<String> = lines(std::slice::from_ref(&a), 90, 10)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            text[0].contains("claude:work") && text[0].contains("max"),
            "{text:?}"
        );
        assert!(
            text[1].contains("5 hours") && text[1].contains("95%"),
            "{text:?}"
        );
        assert!(text[1].contains("resets in"), "{text:?}");
        let b = AccountUsage {
            windows: Vec::new(),
            note: Some("unknown, refreshes on its next run".into()),
            ..a.clone()
        };
        let text: Vec<String> = lines(&[b], 90, 10)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(text[1].contains("refreshes on its next run"), "{text:?}");
        // Older figures kept after a failed read say how old they are.
        let c = AccountUsage {
            note: Some("unknown, refreshes on its next run".into()),
            read_at: Some(now - 2 * 3_600_000),
            ..a
        };
        let text: Vec<String> = lines(&[c], 90, 10)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            text[1].contains("figures from 2h ago")
                && text[1].contains("refreshes on its next run"),
            "{text:?}"
        );
        assert!(text[2].contains("5 hours"), "{text:?}");
    }

    #[test]
    fn usage_events_become_feed_lines_and_alerts() {
        let ev = Event::AccountUsage {
            agent: AgentKind::Codex,
            account: "team".into(),
            state: UsageState::Warning,
            previous: Some(UsageState::Ok),
            headroom: Some(8),
            until: None,
        };
        let (item, toast) = feed(&ev).unwrap();
        assert!(
            item.text.contains("crossed its rotate threshold"),
            "{}",
            item.text
        );
        assert!(toast.is_some());
        let quiet = Event::AccountUsage {
            agent: AgentKind::Codex,
            account: "team".into(),
            state: UsageState::Ok,
            previous: Some(UsageState::Ok),
            headroom: Some(60),
            until: None,
        };
        let (item, toast) = feed(&quiet).unwrap();
        assert!(toast.is_none());
        assert!(item.text.contains("60% left"), "{}", item.text);
        assert!(feed(&Event::Log("x".into())).is_none());
    }
}
