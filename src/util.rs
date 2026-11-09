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

/// Parses an RFC 3339 time into Unix ms, or `None` when it is not one.
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    OffsetDateTime::parse(s.trim(), &Rfc3339).ok().map(to_ms)
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

/// Time until `at`, short: `2h 10m`, `4d 3h`.
#[must_use]
pub fn until(at: i64, now: i64) -> String {
    let secs = (at.saturating_sub(now) / 1_000).max(0);
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, h) => format!("{h}h {m}m"),
        (d, h) => format!("{d}d {h}h"),
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

/// What went wrong with an HTTP request, in a few plain words, for a note
/// or a message a person reads. The raw error can name the URL, the OS
/// error code and library internals, so it belongs in the log, not here.
#[must_use]
pub fn http_failure(e: &ureq::Error) -> &'static str {
    use std::io::ErrorKind;
    match e {
        ureq::Error::Timeout(_) | ureq::Error::BodyStalled => "it did not answer in time",
        ureq::Error::HostNotFound => "its address could not be found; check the network",
        ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => {
            "the connection could not be made"
        }
        ureq::Error::Io(io) => match io.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => "it did not answer in time",
            ErrorKind::ConnectionRefused => "the connection was refused; is it running?",
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof => "the connection was dropped",
            ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::NotConnected
            | ErrorKind::AddrNotAvailable => "the network is unreachable",
            _ => "the connection failed",
        },
        ureq::Error::Tls(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::Pem(_)
        | ureq::Error::TlsRequired => "the secure connection failed",
        ureq::Error::RequireHttpsOnly(_) => "it needs an https address",
        ureq::Error::BodyExceedsLimit(_) | ureq::Error::LargeResponseHeader(..) => {
            "its answer was too large"
        }
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "it redirected somewhere Hashi does not follow"
        }
        ureq::Error::BadUri(_) | ureq::Error::Http(_) | ureq::Error::InvalidProxyUrl => {
            "its address is not valid"
        }
        ureq::Error::Json(_) | ureq::Error::Protocol(_) => "its answer could not be read",
        _ => "the request failed",
    }
}

/// Whether `note` still looks like raw error text: a URL, JSON, several
/// lines or a wall of text. Such a note is replaced before anyone sees it.
#[must_use]
pub fn looks_raw(note: &str) -> bool {
    const MAX: usize = 240;
    note.contains("://")
        || note.contains('{')
        || note.contains('\n')
        || note.contains('\r')
        || note.chars().count() > MAX
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

    #[test]
    fn http_failures_read_as_plain_words() {
        use std::io::{Error, ErrorKind};
        let io = |k: ErrorKind| ureq::Error::Io(Error::new(k, "os error 61 at 127.0.0.1:9"));
        assert_eq!(
            http_failure(&io(ErrorKind::ConnectionRefused)),
            "the connection was refused; is it running?"
        );
        assert_eq!(
            http_failure(&io(ErrorKind::TimedOut)),
            "it did not answer in time"
        );
        assert_eq!(
            http_failure(&io(ErrorKind::ConnectionReset)),
            "the connection was dropped"
        );
        assert_eq!(
            http_failure(&ureq::Error::HostNotFound),
            "its address could not be found; check the network"
        );
        assert_eq!(
            http_failure(&ureq::Error::BodyExceedsLimit(9)),
            "its answer was too large"
        );
        for e in [
            io(ErrorKind::Other),
            ureq::Error::BadUri("https://x.example/{y}".to_owned()),
            ureq::Error::ConnectProxyFailed("http://proxy:1 said 407".to_owned()),
        ] {
            assert!(!looks_raw(http_failure(&e)), "{e}");
        }
    }

    #[test]
    fn raw_looking_notes_are_caught() {
        assert!(looks_raw("GET https://chatgpt.com/backend-api failed"));
        assert!(looks_raw("body={\"error\":1}"));
        assert!(looks_raw("line one\nline two"));
        assert!(looks_raw(&"x".repeat(300)));
        assert!(!looks_raw(
            "unknown, the Codex sign-in was rejected; sign in again with `hashi account login codex default`"
        ));
    }
}
