//! Small helpers shared across modules: clocks, formatting and truncation.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Formats a millisecond timestamp as RFC 3339, or `None` if it is out of range.
#[must_use]
pub fn rfc3339(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// Human friendly duration: `840ms`, `12.4s`, `3m 05s`, `2h 10m`.
#[must_use]
pub fn human_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", d.as_secs_f64())
    } else if ms < 3_600_000 {
        let s = d.as_secs();
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        let s = d.as_secs();
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Human friendly age of a millisecond timestamp relative to now: `4s`, `3m`, `2h`, `5d`.
#[must_use]
pub fn human_age(ms: i64) -> String {
    let secs = (now_ms().saturating_sub(ms) / 1000).max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Truncates `s` to at most `max` characters, appending an ellipsis when cut.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Collapses all whitespace runs (including newlines) into single spaces.
#[must_use]
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_duration_picks_unit_by_magnitude() {
        assert_eq!(human_duration(Duration::from_millis(840)), "840ms");
        assert_eq!(human_duration(Duration::from_millis(12_400)), "12.4s");
        assert_eq!(human_duration(Duration::from_secs(185)), "3m 05s");
        assert_eq!(human_duration(Duration::from_secs(7_800)), "2h 10m");
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_cuts_on_char_boundaries() {
        assert_eq!(truncate("橋橋橋橋", 3), "橋橋…");
    }

    #[test]
    fn one_line_collapses_newlines() {
        assert_eq!(one_line("a\n  b\tc"), "a b c");
    }

    #[test]
    fn rfc3339_formats_epoch() {
        assert_eq!(rfc3339(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }
}
