//! The upstream release each agent was last verified against.
//!
//! `docs/research/verified-versions.json` is the single source of truth. It
//! is embedded in the binary and parsed once. Doctor, discovery, the
//! dashboard and `/v1/stats` compare an installed agent's `--version` with
//! it. A newer install only produces a note: it never blocks a run, and an
//! older or unparseable version says nothing here (the minimums an agent
//! backend already enforces still apply).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::agent::AgentKind;

/// The embedded JSON, as committed.
pub const JSON: &str = include_str!("../docs/research/verified-versions.json");

/// One agent's entry in `verified-versions.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    /// Name shown to humans, such as `Codex`.
    pub name: String,
    /// Where releases come from: `npm:<pkg>`, `github:<owner>/<repo>` or
    /// `installer:<url>`.
    pub source: String,
    /// The GitHub repository, when there is one.
    pub repo: Option<String>,
    /// The verified version, as the agent's `--version` reports it.
    pub version: String,
    /// The release tag the version came from, when it differs.
    #[serde(default)]
    pub tag: Option<String>,
    /// Regex the upstream check uses to filter release tags.
    #[serde(default)]
    pub tag_pattern: Option<String>,
    /// Date of the verification, `YYYY-MM-DD`.
    pub verified: String,
}

#[derive(Debug, Deserialize)]
struct File {
    agents: BTreeMap<String, Entry>,
}

/// Every entry, keyed by agent id (`claude`, `codex`, ...).
///
/// Empty if the embedded JSON failed to parse, which a test rules out.
#[must_use]
pub fn all() -> &'static BTreeMap<String, Entry> {
    static PARSED: OnceLock<BTreeMap<String, Entry>> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str::<File>(JSON)
            .map(|f| f.agents)
            .unwrap_or_default()
    })
}

/// The entry for `kind`, if one is recorded.
#[must_use]
pub fn entry(kind: AgentKind) -> Option<&'static Entry> {
    all().get(kind.as_str())
}

/// A loosely parsed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// `major.minor.patch`; a missing patch is `0`.
    Semver(u64, u64, u64),
    /// A date-based build such as Cursor's `2026.11.05-86047df`.
    Date(u64, u64, u64),
}

impl Version {
    /// Finds the first version-like token in `text`.
    ///
    /// Accepts `2.1.318 (Claude Code)`, `codex-cli 0.166.0`, `v0.67.0`,
    /// `rust-v0.166.0`, `opencode version v0.100.0` and
    /// `2026.11.05-86047df`. Pre-release and build suffixes are ignored.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"'))
            .find_map(parse_token)
    }

    /// Orders two versions of the same kind. A date never compares with a
    /// semver.
    #[must_use]
    pub fn compare(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Semver(a, b, c), Self::Semver(x, y, z))
            | (Self::Date(a, b, c), Self::Date(x, y, z)) => Some((a, b, c).cmp(&(x, y, z))),
            _ => None,
        }
    }

    /// Whether `self` is newer than `verified` in a way worth a note.
    ///
    /// Semver compares major and minor only, so a newer patch is fine. A date
    /// build compares the whole date, which is all it has.
    #[must_use]
    pub fn is_newer_than(self, verified: Self) -> bool {
        match (self, verified) {
            (Self::Semver(a, b, _), Self::Semver(x, y, _)) => (a, b) > (x, y),
            (Self::Date(..), Self::Date(..)) => self.compare(verified) == Some(Ordering::Greater),
            _ => false,
        }
    }
}

/// Parses one token, after stripping a prefix such as `v` or `rust-v`.
fn parse_token(token: &str) -> Option<Version> {
    let start = token.find(|c: char| c.is_ascii_digit())?;
    let prefix = &token[..start];
    if !(prefix.is_empty() || prefix.ends_with('v') || prefix.ends_with('-')) {
        return None;
    }
    // Drop a pre-release or build suffix (Cursor's commit hash included).
    let core = token[start..].split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<u64> = core
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [y, m, d] if core.len() == 10 && *y >= 2000 && (1..=12).contains(m) && *d >= 1 => {
            Some(Version::Date(*y, *m, *d))
        }
        [a, b] => Some(Version::Semver(*a, *b, 0)),
        [a, b, c, ..] => Some(Version::Semver(*a, *b, *c)),
        _ => None,
    }
}

/// The version token shown back to the owner: `0.170.1` from
/// `codex-cli 0.170.1`. Falls back to the whole string.
fn shown_version(installed: &str) -> &str {
    installed
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"'))
        .find(|t| parse_token(t).is_some())
        .map_or(installed.trim(), |t| {
            t.trim_start_matches(|c: char| !c.is_ascii_digit())
        })
}

/// When `installed` is newer than the verified version for `kind`, returns
/// that verified version. `None` when it is the same, older, or either
/// side does not parse.
#[must_use]
pub fn newer_than_verified(kind: AgentKind, installed: &str) -> Option<&'static str> {
    let entry = entry(kind)?;
    let verified = Version::parse(&entry.version)?;
    Version::parse(installed)?
        .is_newer_than(verified)
        .then_some(entry.version.as_str())
}

/// The note doctor and discovery print for a newer install, or `None`.
#[must_use]
pub fn newer_note(kind: AgentKind, installed: &str) -> Option<String> {
    let verified = newer_than_verified(kind, installed)?;
    Some(format!(
        "{kind} {} is newer than the version Hashi was verified with ({verified}). \
         If runs fail, run hashi report and check for an update with hashi update --check.",
        shown_version(installed)
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn embedded_json_parses_with_every_agent() {
        assert!(serde_json::from_str::<File>(JSON).is_ok());
        for kind in AgentKind::ALL {
            let e = entry(kind).unwrap();
            assert!(
                Version::parse(&e.version).is_some(),
                "{kind}: {}",
                e.version
            );
            assert_eq!(e.verified.len(), 10, "{kind}: {}", e.verified);
        }
        assert_eq!(all().len(), AgentKind::ALL.len());
    }

    #[test]
    fn parses_each_agent_format() {
        assert_eq!(v("2.1.318 (Claude Code)"), Version::Semver(2, 1, 318));
        assert_eq!(v("codex-cli 0.166.0"), Version::Semver(0, 166, 0));
        assert_eq!(v("rust-v0.166.0"), Version::Semver(0, 166, 0));
        assert_eq!(v("0.67.0"), Version::Semver(0, 67, 0));
        assert_eq!(v("v2.2.5"), Version::Semver(2, 2, 5));
        assert_eq!(v("0.26.8"), Version::Semver(0, 26, 8));
        assert_eq!(v("opencode version v0.100.0"), Version::Semver(0, 100, 0));
        assert_eq!(v("2026.11.05-86047df"), Version::Date(2026, 11, 5));
        assert_eq!(v("1.4"), Version::Semver(1, 4, 0));
        assert_eq!(v("0.170.0-alpha.2"), Version::Semver(0, 170, 0));
    }

    #[test]
    fn rejects_text_without_a_version() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("unknown"), None);
        assert_eq!(Version::parse("build 7"), None);
        // A digit inside a word is not a version.
        assert_eq!(Version::parse("gpt5.4"), None);
    }

    #[test]
    fn semver_ignores_a_newer_patch() {
        let verified = v("0.166.0");
        assert!(!v("0.166.9").is_newer_than(verified));
        assert!(v("0.167.0").is_newer_than(verified));
        assert!(v("1.0.0").is_newer_than(verified));
        assert!(!v("0.165.4").is_newer_than(verified));
    }

    #[test]
    fn cursor_compares_by_date() {
        let verified = v("2026.11.05-86047df");
        assert!(v("2026.11.06-aaaaaaa").is_newer_than(verified));
        assert!(v("2026.12.01-0000000").is_newer_than(verified));
        assert!(!v("2026.11.05-ffffff0").is_newer_than(verified));
        assert!(!v("2026.10.30-1234567").is_newer_than(verified));
        // A date and a semver never compare.
        assert!(!v("3.0.0").is_newer_than(verified));
        assert_eq!(v("3.0.0").compare(verified), None);
    }

    #[test]
    fn newer_than_verified_uses_the_json() {
        assert_eq!(
            newer_than_verified(AgentKind::Codex, "codex-cli 0.170.1"),
            Some("0.166.0")
        );
        assert_eq!(
            newer_than_verified(AgentKind::Codex, "codex-cli 0.166.4"),
            None
        );
        assert_eq!(newer_than_verified(AgentKind::Opencode, "2.1.0"), None);
        assert_eq!(newer_than_verified(AgentKind::Claude, "garbage"), None);
        assert_eq!(
            newer_than_verified(AgentKind::Cursor, "2026.11.20-abcdef1"),
            Some("2026.11.05-86047df")
        );
    }

    #[test]
    fn doctor_note_names_the_versions_and_next_steps() {
        assert_eq!(
            newer_note(AgentKind::Codex, "codex-cli 0.170.1").unwrap(),
            "codex 0.170.1 is newer than the version Hashi was verified with (0.166.0). \
             If runs fail, run hashi report and check for an update with hashi update --check."
        );
        assert!(
            newer_note(AgentKind::Claude, "2.2.0 (Claude Code)")
                .unwrap()
                .starts_with("claude 2.2.0 is newer")
        );
        assert_eq!(newer_note(AgentKind::Gemini, "0.67.3"), None);
    }

    /// The README's "Verified with" column must agree with the JSON. Rows
    /// for agents the JSON does not list are ignored, and an agent listed in
    /// the JSON but not yet supported (no `AgentKind`) may lack a row.
    #[test]
    fn readme_verified_column_matches_the_json() {
        let readme = include_str!("../README.md");
        let section = readme
            .split("## Supported agents")
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap();
        let rows: Vec<Vec<&str>> = section
            .lines()
            .filter(|l| l.starts_with('|'))
            .map(|l| l.trim_matches('|').split('|').map(str::trim).collect())
            .collect();
        let header = &rows[0];
        let agent_col = header.iter().position(|h| *h == "`agent` value").unwrap();
        let verified_col = header.iter().position(|h| *h == "Verified with").unwrap();

        let mut seen = Vec::new();
        for row in rows.iter().skip(2) {
            let agent = row[agent_col].trim_matches('`');
            let Some(e) = all().get(agent) else { continue };
            assert_eq!(
                row[verified_col],
                format!("{} ({})", e.version, e.verified),
                "README row for {agent}"
            );
            seen.push(agent.to_owned());
        }
        for kind in AgentKind::ALL {
            assert!(
                seen.iter().any(|s| s == kind.as_str()),
                "README has no row for {kind}"
            );
        }
    }
}
