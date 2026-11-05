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

fn to_ms(dt: OffsetDateTime) -> i64 {
    i64::try_from(dt.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

fn from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Start of the UTC day containing `ms`.
#[must_use]
pub fn day_start_utc(ms: i64) -> i64 {
    to_ms(from_ms(ms).replace_time(time::Time::MIDNIGHT))
}

/// Start of the next UTC day after `ms`.
#[must_use]
pub fn next_day_utc(ms: i64) -> i64 {
    day_start_utc(ms) + 86_400_000
}

/// Start of the UTC month containing `ms`.
#[must_use]
pub fn month_start_utc(ms: i64) -> i64 {
    let dt = from_ms(ms).replace_time(time::Time::MIDNIGHT);
    to_ms(dt.replace_day(1).unwrap_or(dt))
}

/// Start of the next UTC month after `ms`.
#[must_use]
pub fn next_month_utc(ms: i64) -> i64 {
    let start = from_ms(month_start_utc(ms));
    let (year, month) = match start.month() {
        time::Month::December => (start.year() + 1, time::Month::January),
        m => (start.year(), m.next()),
    };
    time::Date::from_calendar_date(year, month, 1)
        .map(|d| to_ms(d.midnight().assume_utc()))
        .unwrap_or(i64::MAX)
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

/// Human friendly byte count in binary units: `512 B`, `640 KiB`, `512 MiB`, `3.2 GiB`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if n < KIB {
        format!("{n} B")
    } else if n < MIB {
        format!("{} KiB", n / KIB)
    } else if n < GIB {
        format!("{} MiB", n / MIB)
    } else if n < TIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else {
        format!("{:.1} TiB", n as f64 / TIB as f64)
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
    fn human_bytes_picks_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(640 * 1024), "640 KiB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "512 MiB");
        assert_eq!(human_bytes(16 * 1024 * 1024 * 1024 / 5), "3.2 GiB");
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
    fn day_start_truncates() {
        // 2026-11-05T09:35:31Z
        let ms = 1_793_871_331_000;
        assert_eq!(
            rfc3339(day_start_utc(ms)).as_deref(),
            Some("2026-11-05T00:00:00Z")
        );
        assert_eq!(
            rfc3339(next_day_utc(ms)).as_deref(),
            Some("2026-11-06T00:00:00Z")
        );
    }

    #[test]
    fn month_start_handles_december() {
        // 2026-12-31T23:00:00Z
        let ms = 1_798_758_000_000;
        assert_eq!(
            rfc3339(month_start_utc(ms)).as_deref(),
            Some("2026-12-01T00:00:00Z")
        );
        assert_eq!(
            rfc3339(next_month_utc(ms)).as_deref(),
            Some("2027-01-01T00:00:00Z")
        );
    }

    #[test]
    fn rfc3339_formats_epoch() {
        assert_eq!(rfc3339(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }
}
