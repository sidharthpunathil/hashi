//! Recognising "this account is at its usage limit" in a failed run's error.
//!
//! Each backend has a `limit_hit` function with its CLI's own messages (see
//! the multiple accounts design, which lists them with their sources). The
//! helpers here match those messages and pull a reset time out of the text
//! when it names one, either as an RFC 3339 timestamp or as a relative time
//! such as "try again in 2 hours 5 minutes" or "retry after 30s".

use std::time::Duration;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The longest reset Hashi believes. A parsed time further out than this is
/// clamped, so a garbled message cannot shelve an account for a year.
pub const MAX_RESET: Duration = Duration::from_secs(31 * 86_400);

/// `text` lowercased, with the curly apostrophe some CLIs print (Codex's
/// "You’ve") turned into a plain one.
fn normalize(text: &str) -> String {
    text.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

/// Whether `text` contains any of `needles`, which must be lowercase.
#[must_use]
pub fn contains_any(text: &str, needles: &[&str]) -> bool {
    let text = normalize(text);
    needles.iter().any(|n| text.contains(n))
}

/// Whether `text` has `code` as a number of its own, not inside a longer one
/// (so `429` matches "HTTP 429:" but not "14290").
#[must_use]
pub fn has_status(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + code.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// The answer for a limit message: when it resets, if `text` says.
#[must_use]
pub fn hit(text: &str) -> Option<Option<OffsetDateTime>> {
    Some(reset_time(text, OffsetDateTime::now_utc()))
}

/// The reset time named in `text`, relative to `now`: the first RFC 3339
/// timestamp after `now`, or else the first "in <duration>" or
/// "after <duration>". Capped at [`MAX_RESET`] from `now`.
#[must_use]
pub fn reset_time(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let latest = now + MAX_RESET;
    let stamp = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\'' | '.')
            })
        })
        .filter_map(|w| OffsetDateTime::parse(w, &Rfc3339).ok())
        .find(|t| *t > now);
    if let Some(t) = stamp {
        return Some(t.min(latest));
    }
    let lower = normalize(text);
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().find_map(|(i, w)| {
        if !matches!(*w, "in" | "after") {
            return None;
        }
        let d = duration_at(&words[i + 1..])?;
        Some((now + d).min(latest))
    })
}

/// The text after the first (case-insensitive) `marker` in `text`,
/// lowercased, with curly apostrophes made plain.
#[must_use]
pub fn after_marker(text: &str, marker: &str) -> Option<String> {
    let lower = normalize(text);
    let at = lower.find(marker)?;
    Some(lower[at + marker.len()..].to_owned())
}

/// The words of `text` (split on whitespace) with surrounding punctuation
/// trimmed, keeping `:` and `/` inside them.
fn words(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | '(' | ')' | '"' | '\'')))
        .filter(|w| !w.is_empty())
        .collect()
}

/// A 12-hour clock time such as `3:45` with `pm`, as (hour 0 to 23, minute).
fn clock_12(time: &str, meridiem: &str) -> Option<(u8, u8)> {
    let (h, m) = time.split_once(':')?;
    let (h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    if !(1..=12).contains(&h) || m > 59 {
        return None;
    }
    let h = match meridiem {
        "am" => h % 12,
        "pm" => h % 12 + 12,
        _ => return None,
    };
    Some((h, m))
}

/// A month from its English abbreviation or name.
fn month(word: &str) -> Option<time::Month> {
    use time::Month::{
        April, August, December, February, January, July, June, March, May, November, October,
        September,
    };
    let months = [
        January, February, March, April, May, June, July, August, September, October, November,
        December,
    ];
    let w = word.get(..3)?;
    months
        .into_iter()
        .find(|m| m.to_string().to_ascii_lowercase().starts_with(w))
}

/// A local wall-clock time in a "try again at" suffix, as Codex prints it:
/// `3:45 PM` for today, or `Nov 7th, 2026 3:45 PM` for another day.
/// `offset` is the machine's UTC offset, which Codex formats in.
#[must_use]
pub fn local_clock(
    text: &str,
    now: OffsetDateTime,
    offset: time::UtcOffset,
) -> Option<OffsetDateTime> {
    let w = words(text);
    let (date, (h, m)) = match w.as_slice() {
        [t, mer, ..] if clock_12(t, mer).is_some() => {
            (now.to_offset(offset).date(), clock_12(t, mer)?)
        }
        [mon, day, year, t, mer, ..] => {
            let day: u8 = day.trim_end_matches(char::is_alphabetic).parse().ok()?;
            let date = time::Date::from_calendar_date(year.parse().ok()?, month(mon)?, day).ok()?;
            (date, clock_12(t, mer)?)
        }
        _ => return None,
    };
    let at = time::PrimitiveDateTime::new(date, time::Time::from_hms(h, m, 0).ok()?);
    Some(at.assume_offset(offset).to_offset(time::UtcOffset::UTC))
}

/// The next `MM-DD HH:MM:SS` UTC after `now`: Qwen Code's reset time, which
/// has no year.
#[must_use]
pub fn next_month_day_utc(text: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let w = words(text);
    let (md, hms) = (w.first()?, w.get(1)?);
    let (mo, d) = md.split_once('-')?;
    let mut parts = hms.split(':').map(str::parse::<u8>);
    let (h, mi, s) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let mo = time::Month::try_from(mo.parse::<u8>().ok()?).ok()?;
    let d: u8 = d.parse().ok()?;
    let time = time::Time::from_hms(h, mi, s).ok()?;
    let year = now.year();
    [year, year + 1].into_iter().find_map(|y| {
        let date = time::Date::from_calendar_date(y, mo, d).ok()?;
        let at = time::PrimitiveDateTime::new(date, time).assume_utc();
        (at > now).then_some(at)
    })
}

/// Midnight UTC at the start of an `M/D/YYYY` date, as Cursor prints the end
/// of its monthly cycle.
#[must_use]
pub fn us_date_utc(text: &str) -> Option<OffsetDateTime> {
    let w = words(text);
    let mut parts = w.first()?.split('/');
    let (m, d, y) = (parts.next()?, parts.next()?, parts.next()?);
    let date = time::Date::from_calendar_date(
        y.parse().ok()?,
        time::Month::try_from(m.parse::<u8>().ok()?).ok()?,
        d.parse().ok()?,
    )
    .ok()?;
    Some(date.midnight().assume_utc())
}

/// Leading seconds such as `45s` or `412.7s`, rounded up.
#[must_use]
pub fn leading_seconds(text: &str) -> Option<u64> {
    let w = text.trim_start();
    let end = w
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(w.len());
    if !w[end..].starts_with('s') {
        return None;
    }
    let secs: f64 = w[..end].parse().ok()?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (secs.is_finite() && secs >= 0.0).then(|| secs.ceil() as u64)
}

/// This machine's UTC offset, read once from `date +%z`. Parsing the offset
/// in process is not possible from a multithreaded program on Unix, which
/// Hashi is. `None` when it cannot be read, for example on Windows.
#[must_use]
pub fn local_offset() -> Option<time::UtcOffset> {
    static OFFSET: std::sync::OnceLock<Option<time::UtcOffset>> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        if !cfg!(unix) {
            return None;
        }
        let out = std::process::Command::new("date")
            .arg("+%z")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let t = text.trim();
        let (sign, digits) = match t.as_bytes().first()? {
            b'+' => (1, &t[1..]),
            b'-' => (-1, &t[1..]),
            _ => return None,
        };
        if digits.len() != 4 {
            return None;
        }
        let h: i8 = digits[..2].parse().ok()?;
        let m: i8 = digits[2..].parse().ok()?;
        time::UtcOffset::from_hms(sign * h, sign * m, 0).ok()
    })
}

/// Seconds in one `unit`, for the spellings CLIs use.
fn unit_secs(unit: &str) -> Option<u64> {
    Some(match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    })
}

/// Splits a word such as `1h20m` into `[(1, "h"), (20, "m")]`, or `30` into
/// `[(30, "")]`. `None` when it does not start with a digit.
fn segments(word: &str) -> Option<Vec<(u64, &str)>> {
    let mut out = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        let n: u64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let letters = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        out.push((n, &rest[..letters]));
        rest = &rest[letters..];
    }
    (!out.is_empty()).then_some(out)
}

/// A duration spelled at the start of `words`: `2 hours 5 minutes`,
/// `1h 20m`, `30 seconds`, `3 days and 4 hours`.
fn duration_at(words: &[&str]) -> Option<Duration> {
    let mut total: u64 = 0;
    let mut any = false;
    let mut i = 0;
    while let Some(word) = words.get(i) {
        if any && *word == "and" {
            i += 1;
            continue;
        }
        let Some(parts) = segments(word) else {
            break;
        };
        let mut used = 1;
        let mut ok = true;
        for (n, unit) in parts {
            let unit = if unit.is_empty() {
                used = 2;
                words.get(i + 1).copied().unwrap_or("")
            } else {
                unit
            };
            match unit_secs(unit) {
                Some(secs) => total = total.saturating_add(n.saturating_mul(secs)),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        any = true;
        i += used;
    }
    (any && total > 0).then(|| Duration::from_secs(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    fn after(text: &str) -> Option<i64> {
        reset_time(text, now()).map(|t| (t - now()).whole_seconds())
    }

    #[test]
    fn relative_times_are_read() {
        assert_eq!(after("try again in 2 hours 5 minutes."), Some(7_500));
        assert_eq!(after("Try again in 1h 20m"), Some(4_800));
        assert_eq!(after("retry after 30 seconds"), Some(30));
        assert_eq!(
            after("resets in 3 days and 4 hours"),
            Some(3 * 86_400 + 4 * 3_600)
        );
        assert_eq!(after("quota resets in 45s"), Some(45));
        assert_eq!(after("try again in 1h20m"), Some(4_800));
    }

    #[test]
    fn timestamps_are_read_and_past_ones_ignored() {
        // `now()` is 2027-01-15T08:00:00Z.
        assert_eq!(after("resets at (2027-01-15T10:00:00Z)."), Some(7_200));
        assert_eq!(after("limited since 2027-01-15T08:00:00Z"), None);
        assert_eq!(after("was limited at 2020-01-01T00:00:00Z"), None);
    }

    #[test]
    fn unknown_or_absent_times_give_none() {
        assert_eq!(after("You've hit your session limit · resets 3pm"), None);
        assert_eq!(after("in the workspace"), None);
        assert_eq!(after("weekly limit reached"), None);
    }

    #[test]
    fn far_resets_are_clamped() {
        assert_eq!(after("try again in 400 days"), Some(31 * 86_400));
    }

    #[test]
    fn status_codes_match_whole_numbers_only() {
        assert!(has_status("HTTP 429: too many", "429"));
        assert!(has_status("(429)", "429"));
        assert!(!has_status("id 14290", "429"));
    }

    #[test]
    fn wall_clock_and_dated_resets_are_read() {
        let plus2 = time::UtcOffset::from_hms(2, 0, 0).unwrap();
        // 08:00 UTC is 10:00 at +02:00, so 3:45 PM there is 13:45 UTC.
        let t = local_clock("3:45 pm.", now(), plus2).unwrap();
        assert_eq!((t - now()).whole_minutes(), 5 * 60 + 45);
        let t = local_clock("jan 17th, 2027 12:05 am.", now(), plus2).unwrap();
        // 00:05 on the 17th at +02:00 is 22:05 UTC on the 16th.
        assert_eq!((t - now()).whole_minutes(), 38 * 60 + 5);
        assert!(local_clock("later.", now(), plus2).is_none());

        let t = next_month_day_utc("01-15 09:30:00 utc.", now()).unwrap();
        assert_eq!((t - now()).whole_minutes(), 90);
        let t = next_month_day_utc("01-14 09:30:00 utc.", now()).unwrap();
        assert_eq!(t.year(), 2028, "a date already past is next year's");

        let t = us_date_utc("2/1/2027.").unwrap();
        assert_eq!(
            (t.month(), t.day(), t.year()),
            (time::Month::February, 1, 2027)
        );
        assert_eq!(leading_seconds(" 412.2s."), Some(413));
        assert_eq!(leading_seconds("soon"), None);
    }

    #[test]
    fn curly_apostrophes_are_normalized() {
        assert!(contains_any("You\u{2019}ve hit", &["you've hit"]));
    }
}
