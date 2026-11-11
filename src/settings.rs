//! `<home>/settings.json`: choices made in the dashboard that last past a
//! restart: the port and bind address, and the names of the
//! skip-permissions commands (see [`crate::accounts::commands`]).
//!
//! At startup each value comes from the first of: its flag, its
//! environment variable, this file, the built-in default. The file is
//! owner-only, like everything else in the home.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name in the Hashi home.
pub const FILE: &str = "settings.json";
/// Port used when nothing else sets one.
pub const DEFAULT_PORT: u16 = 7878;
/// Bind address used when nothing else sets one.
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// The saved choices. A missing field means "not saved".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Port to listen on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Address to listen on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<IpAddr>,
    /// The owner's names for the skip-permissions commands (`dclaude`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dangerous_commands: Option<crate::accounts::commands::Keywords>,
}

/// Where a value in effect came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A flag or environment variable.
    Override,
    /// `settings.json`.
    Saved,
    /// The built-in default.
    Default,
}

/// Path of the file in `home`.
#[must_use]
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

/// Reads the saved settings. A missing file means none are saved.
///
/// # Errors
///
/// Fails, with a message naming the file, when it cannot be read or parsed,
/// or saves port 0 (which would listen on a random port).
pub fn load(home: &Path) -> Result<Settings, String> {
    let path = path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let s: Settings = serde_json::from_str(&text)
                .map_err(|e| format!("ignoring {}: {e}", path.display()))?;
            if s.port == Some(0) {
                return Err(format!(
                    "ignoring {}: port 0 is not a port to listen on",
                    path.display()
                ));
            }
            Ok(s)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("ignoring {}: {e}", path.display())),
    }
}

/// Saves `settings`, readable only by the owner.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn save(home: &Path, settings: &Settings) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    crate::diagnostics::write_private(&path(home), &format!("{text}\n"))
}

/// The dashboard header's warning for listening on `bind`: `None` on
/// loopback. Shown for the whole run, so an address saved in
/// `settings.json` is not only a line on stderr before the dashboard opens.
#[must_use]
pub fn exposure_note(bind: IpAddr, tls: bool) -> Option<&'static str> {
    if bind.is_loopback() {
        None
    } else if tls {
        Some("! exposed, HTTPS")
    } else {
        Some("! exposed, no TLS")
    }
}

/// The value in effect: the flag or variable when `given`, else the saved
/// one, else `default`, with where it came from.
pub fn resolve<T>(given: Option<T>, saved: Option<T>, default: T) -> (T, Source) {
    match (given, saved) {
        (Some(v), _) => (v, Source::Override),
        (None, Some(v)) => (v, Source::Saved),
        (None, None) => (default, Source::Default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_or_variable_beats_the_file_which_beats_the_default() {
        assert_eq!(
            resolve(Some(9000), Some(8000), 7878),
            (9000, Source::Override)
        );
        assert_eq!(resolve(None, Some(8000), 7878), (8000, Source::Saved));
        assert_eq!(resolve(None, None, 7878), (7878, Source::Default));
        assert_eq!(resolve(Some(9000), None, 7878), (9000, Source::Override));
    }

    #[test]
    fn settings_round_trip_owner_only_and_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), Settings::default());
        let s = Settings {
            port: Some(8080),
            bind: Some("0.0.0.0".parse().unwrap()),
            ..Settings::default()
        };
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()).unwrap(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Only what is saved is written.
        save(
            dir.path(),
            &Settings {
                port: Some(9),
                ..Settings::default()
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(!text.contains("bind"), "{text}");
    }

    #[test]
    fn port_zero_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), r#"{"port":0,"bind":"0.0.0.0"}"#).unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(err.contains("port 0"), "{err}");
        std::fs::write(path(dir.path()), r#"{"port":1}"#).unwrap();
        assert_eq!(load(dir.path()).unwrap().port, Some(1));
    }

    #[test]
    fn a_non_loopback_bind_gets_a_header_warning() {
        let any: IpAddr = "0.0.0.0".parse().unwrap();
        assert!(exposure_note(any, false).is_some_and(|n| n.contains("no TLS")));
        assert!(exposure_note(any, true).is_some_and(|n| n.contains("HTTPS")));
        assert_eq!(exposure_note(DEFAULT_BIND, false), None);
        assert_eq!(exposure_note("::1".parse().unwrap(), false), None);
    }

    #[test]
    fn a_garbled_file_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{not json").unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(err.contains("settings.json"), "{err}");
    }
}
