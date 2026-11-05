//! `hashi install cloudflared|tailscale|crush`: the vendor's own install
//! command for this platform, run only after the owner says yes.
//!
//! Every command here is copied from the vendor's install documentation,
//! never guessed. Where a platform has no documented package-manager
//! command, or the package manager is not on `PATH`, the plan is the
//! vendor's download page and nothing is run. Hashi never downloads and
//! runs a binary itself.
//!
//! Sources, checked on 2026-11-06:
//!
//! - cloudflared on macOS: `brew install cloudflared`, from
//!   <https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/>.
//! - cloudflared on Debian and Ubuntu (`apt-get`), and on RHEL, `CentOS`,
//!   Amazon Linux and Fedora (`yum`, which Fedora provides as an alias of
//!   `dnf`): the Cloudflare Package Repository at <https://pkg.cloudflare.com/>,
//!   which the downloads page points to for Linux.
//! - cloudflared on Windows: the downloads page lists only the GitHub
//!   executables and MSI installers, and no `winget` id, so the plan is the
//!   page itself.
//! - Tailscale on Linux: `curl -fsSL https://tailscale.com/install.sh | sh`,
//!   from <https://tailscale.com/kb/1031/install-linux>.
//! - Tailscale on macOS: <https://tailscale.com/kb/1016/install-mac>
//!   recommends the standalone app and does not mention Homebrew. Tailscale's
//!   own page for the open source variant it links to,
//!   <https://github.com/tailscale/tailscale/wiki/Tailscaled-on-macOS>,
//!   gives `brew install --formula tailscale` and then
//!   `sudo brew services start tailscale`. That is the plan when `brew` is
//!   present; the app download is offered otherwise.
//! - Tailscale on Windows: <https://tailscale.com/kb/1022/install-windows>
//!   documents only the `.exe` and `.msi` installers and no `winget` id, so
//!   the plan is <https://tailscale.com/download/windows>.
//! - Crush, from its README (`README.md` in the Crush repository, read on
//!   2026-11-05): `brew install charmbracelet/tap/crush` on macOS (line 30),
//!   `winget install charmbracelet.crush` on Windows (line 49), and
//!   `npm install -g @charmland/crush` (line 33) wherever npm is on `PATH`.
//!   Otherwise the plan is the releases page the README links to.

use std::path::PathBuf;

/// A helper program Hashi can use but does not ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Helper {
    /// Cloudflare's tunnel client, for `--tunnel` and dashboard key `t`.
    Cloudflared,
    /// The Tailscale client, for `--tailscale serve|funnel`.
    Tailscale,
    /// Charmbracelet's Crush, the opt-in agent (`--enable-crush`).
    Crush,
}

impl Helper {
    /// Every helper, in display order.
    pub const ALL: [Self; 3] = [Self::Cloudflared, Self::Tailscale, Self::Crush];

    /// The executable name, which is also the `hashi install` argument.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cloudflared => "cloudflared",
            Self::Tailscale => "tailscale",
            Self::Crush => "crush",
        }
    }

    /// Parses a `hashi install` argument.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.name() == s)
    }
}

impl std::fmt::Display for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The operating system family, which picks the vendor's instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS.
    Mac,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
    /// Anything else (the BSDs, for example).
    Other,
}

impl Platform {
    /// The platform Hashi was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// One step of an install, run with the terminal attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The program and its arguments. Pipelines run as `sh -c '<line>'`.
    pub argv: Vec<String>,
    /// The line as the vendor documents it, shown to the owner and copied.
    pub shown: String,
}

impl Step {
    fn exec(args: &[&str]) -> Self {
        Self {
            argv: args.iter().map(|a| (*a).to_owned()).collect(),
            shown: args.join(" "),
        }
    }

    fn shell(line: &str) -> Self {
        Self {
            argv: vec!["sh".to_owned(), "-c".to_owned(), line.to_owned()],
            shown: line.to_owned(),
        }
    }
}

/// What `hashi install` would do on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Run these steps in order, stopping at the first failure.
    Run {
        /// Which helper.
        helper: Helper,
        /// The package manager or tool the steps rely on.
        via: &'static str,
        /// The steps.
        steps: Vec<Step>,
        /// The vendor page the steps come from.
        source: &'static str,
        /// Something the owner should know first, if anything.
        note: Option<&'static str>,
    },
    /// Nothing to run here: open this page and install by hand.
    Download {
        /// Which helper.
        helper: Helper,
        /// The vendor's download page for this platform.
        url: &'static str,
        /// Why there is no command to run.
        why: String,
    },
}

impl Plan {
    /// The commands as one block of text, one per line, for showing and
    /// copying. For a download plan, the page's URL.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Run { steps, .. } => steps
                .iter()
                .map(|s| s.shown.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Download { url, .. } => (*url).to_owned(),
        }
    }

    /// Which helper this plan installs.
    #[must_use]
    pub const fn helper(&self) -> Helper {
        match self {
            Self::Run { helper, .. } | Self::Download { helper, .. } => *helper,
        }
    }

    /// One line for a toast or an error: the command, or where to download.
    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Run { steps, .. } if steps.len() == 1 => steps[0].shown.clone(),
            Self::Run { helper, .. } => format!("run hashi install {helper}"),
            Self::Download { url, .. } => format!("download it from {url}"),
        }
    }
}

/// Cloudflare's download page for `cloudflared`.
pub const CLOUDFLARED_DOWNLOADS: &str =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/";
/// The Cloudflare Package Repository, the documented Linux install.
pub const CLOUDFLARE_PKG: &str = "https://pkg.cloudflare.com/";
/// Tailscale's Linux install page.
pub const TAILSCALE_LINUX: &str = "https://tailscale.com/kb/1031/install-linux";
/// Tailscale's page for the open source macOS variant, which has the
/// Homebrew commands.
pub const TAILSCALE_MAC_BREW: &str =
    "https://github.com/tailscale/tailscale/wiki/Tailscaled-on-macOS";
/// Tailscale's macOS download page (the recommended standalone app).
pub const TAILSCALE_MAC_DOWNLOAD: &str = "https://tailscale.com/download/mac";
/// Tailscale's Windows download page.
pub const TAILSCALE_WINDOWS_DOWNLOAD: &str = "https://tailscale.com/download/windows";
/// Tailscale's download page for everything else.
pub const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download";

/// Crush's source, which has the install commands in its README.
pub const CRUSH_README: &str = "https://github.com/charmbracelet/crush#installation";
/// Crush's releases page, for systems without a documented command.
pub const CRUSH_RELEASES: &str = "https://github.com/charmbracelet/crush/releases";

/// Chooses the install for `helper` on `platform`, given which programs are
/// on `PATH` (`has("brew")` and so on). Pure, so every platform can be
/// tested anywhere.
#[must_use]
pub fn plan(helper: Helper, platform: Platform, has: impl Fn(&str) -> bool) -> Plan {
    let download = |url: &'static str, why: &str| Plan::Download {
        helper,
        url,
        why: why.to_owned(),
    };
    match (helper, platform) {
        (Helper::Cloudflared, Platform::Mac) if has("brew") => Plan::Run {
            helper,
            via: "Homebrew",
            steps: vec![Step::exec(&["brew", "install", "cloudflared"])],
            source: CLOUDFLARED_DOWNLOADS,
            note: None,
        },
        (Helper::Cloudflared, Platform::Mac) => download(
            CLOUDFLARED_DOWNLOADS,
            "Homebrew (brew) is not on PATH, and it is the install Cloudflare documents for macOS",
        ),
        (Helper::Cloudflared, Platform::Linux) if has("apt-get") && has("curl") && has("sudo") => {
            Plan::Run {
                helper,
                via: "apt-get",
                steps: vec![
                    Step::exec(&["sudo", "mkdir", "-p", "--mode=0755", "/usr/share/keyrings"]),
                    Step::shell(
                        "curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg | sudo tee \
                         /usr/share/keyrings/cloudflare-main.gpg >/dev/null",
                    ),
                    Step::shell(
                        "echo 'deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] \
                         https://pkg.cloudflare.com/cloudflared any main' | sudo tee \
                         /etc/apt/sources.list.d/cloudflared.list",
                    ),
                    Step::shell("sudo apt-get update && sudo apt-get install cloudflared"),
                ],
                source: CLOUDFLARE_PKG,
                note: Some("adds Cloudflare's package repository and signing key"),
            }
        }
        // The page's RPM steps also run `sudo yum update`, which upgrades
        // every package on the machine; `yum install` refreshes the new
        // repository's metadata by itself, so that step is left out. The
        // page writes `curl -fsSl`; `-L` (follow redirects) is what it means.
        (Helper::Cloudflared, Platform::Linux) if has("yum") && has("curl") && has("sudo") => {
            Plan::Run {
                helper,
                via: "yum",
                steps: vec![
                    Step::shell(
                        "curl -fsSL https://pkg.cloudflare.com/cloudflared.repo | sudo tee \
                         /etc/yum.repos.d/cloudflared.repo",
                    ),
                    Step::exec(&["sudo", "yum", "install", "cloudflared"]),
                ],
                source: CLOUDFLARE_PKG,
                note: Some("adds Cloudflare's package repository"),
            }
        }
        (Helper::Cloudflared, Platform::Linux) => download(
            CLOUDFLARED_DOWNLOADS,
            "no apt-get or yum (with curl and sudo) on PATH, the package managers Cloudflare \
             documents for Linux",
        ),
        (Helper::Cloudflared, Platform::Windows) => download(
            CLOUDFLARED_DOWNLOADS,
            "Cloudflare documents the Windows installers on this page and no winget package",
        ),
        (Helper::Cloudflared, Platform::Other) => download(
            CLOUDFLARED_DOWNLOADS,
            "Cloudflare documents no install command for this system",
        ),
        (Helper::Tailscale, Platform::Linux) if has("curl") && has("sh") => Plan::Run {
            helper,
            via: "Tailscale's install script",
            steps: vec![Step::shell(
                "curl -fsSL https://tailscale.com/install.sh | sh",
            )],
            source: TAILSCALE_LINUX,
            note: Some("then sign in with: sudo tailscale up"),
        },
        (Helper::Tailscale, Platform::Linux) => download(
            TAILSCALE_LINUX,
            "Tailscale's install script needs curl and sh on PATH",
        ),
        (Helper::Tailscale, Platform::Mac) if has("brew") => Plan::Run {
            helper,
            via: "Homebrew",
            steps: vec![
                Step::exec(&["brew", "install", "--formula", "tailscale"]),
                Step::exec(&["sudo", "brew", "services", "start", "tailscale"]),
            ],
            source: TAILSCALE_MAC_BREW,
            note: Some(
                "installs the open source command-line variant; Tailscale recommends the app \
                 from tailscale.com/download/mac. Sign in afterwards with: sudo tailscale up",
            ),
        },
        (Helper::Tailscale, Platform::Mac) => download(
            TAILSCALE_MAC_DOWNLOAD,
            "Homebrew (brew) is not on PATH; Tailscale's recommended install is the app",
        ),
        (Helper::Tailscale, Platform::Windows) => download(
            TAILSCALE_WINDOWS_DOWNLOAD,
            "Tailscale documents the Windows installer and no winget package",
        ),
        (Helper::Tailscale, Platform::Other) => download(
            TAILSCALE_DOWNLOAD,
            "Tailscale documents no install command for this system",
        ),
        (Helper::Crush, Platform::Mac) if has("brew") => Plan::Run {
            helper,
            via: "Homebrew",
            steps: vec![Step::exec(&["brew", "install", "charmbracelet/tap/crush"])],
            source: CRUSH_README,
            note: Some(CRUSH_NOTE),
        },
        (Helper::Crush, Platform::Windows) if has("winget") => Plan::Run {
            helper,
            via: "winget",
            steps: vec![Step::exec(&["winget", "install", "charmbracelet.crush"])],
            source: CRUSH_README,
            note: Some(CRUSH_NOTE),
        },
        (Helper::Crush, _) if has("npm") => Plan::Run {
            helper,
            via: "npm",
            steps: vec![npm_install_crush(platform)],
            source: CRUSH_README,
            note: Some(CRUSH_NOTE),
        },
        (Helper::Crush, _) => download(
            CRUSH_RELEASES,
            "none of the package managers the Crush README names (brew on macOS, winget on \
             Windows, npm anywhere) is on PATH",
        ),
    }
}

/// `npm install -g @charmland/crush`. On Windows npm is a `.cmd` shim,
/// which only `cmd.exe` can start.
fn npm_install_crush(platform: Platform) -> Step {
    let mut step = Step::exec(&["npm", "install", "-g", "@charmland/crush"]);
    if platform == Platform::Windows {
        step.argv.splice(0..0, ["cmd".to_owned(), "/C".to_owned()]);
    }
    step
}

/// Shown with every Crush install plan.
const CRUSH_NOTE: &str = "Crush is opt-in: start Hashi with --enable-crush, then sign it in with \
                          hashi account login crush default (or set a provider key)";

/// The plan for this machine, looking programs up on `PATH` only.
#[must_use]
pub fn plan_here(helper: Helper) -> Plan {
    plan(helper, Platform::current(), |name| on_path(name).is_some())
}

/// Finds `name` in the directories of `PATH`, and nowhere else.
#[must_use]
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names = crate::agent::candidate_names(name, cfg!(windows));
    std::env::split_paths(&path)
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| crate::agent::is_executable(p))
}

/// Finds a helper: `custom` when given (and nothing else, so a wrong path
/// reads as missing instead of silently using another copy), or the usual
/// search of `PATH` and common install directories.
#[must_use]
pub fn locate(helper: Helper, custom: Option<&std::path::Path>) -> Option<PathBuf> {
    match custom {
        Some(p) => crate::agent::is_executable(p).then(|| p.to_path_buf()),
        None => crate::agent::locate(helper.name()),
    }
}

/// Runs a plan's steps with the terminal attached (inherited stdin, stdout
/// and stderr, so `sudo` can ask for a password), stopping at the first
/// step that fails.
///
/// # Errors
///
/// Fails if a step cannot be started or exits unsuccessfully. A download
/// plan has nothing to run and fails with its page.
pub fn run(plan: &Plan) -> anyhow::Result<()> {
    let Plan::Run { steps, .. } = plan else {
        anyhow::bail!("nothing to run: {}", plan.hint());
    };
    for step in steps {
        let Some((program, args)) = step.argv.split_first() else {
            continue;
        };
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .map_err(|e| anyhow::anyhow!("could not start {program}: {e}"))?;
        if !status.success() {
            anyhow::bail!("`{}` failed ({status})", step.shown);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n| names.contains(&n)
    }

    #[test]
    fn mac_uses_homebrew_when_it_is_there() {
        let p = plan(Helper::Cloudflared, Platform::Mac, with(&["brew"]));
        assert_eq!(p.text(), "brew install cloudflared");
        assert!(matches!(
            p,
            Plan::Run {
                via: "Homebrew",
                ..
            }
        ));
        let p = plan(Helper::Tailscale, Platform::Mac, with(&["brew"]));
        assert_eq!(
            p.text(),
            "brew install --formula tailscale\nsudo brew services start tailscale"
        );
        assert!(matches!(
            p,
            Plan::Run {
                source: TAILSCALE_MAC_BREW,
                ..
            }
        ));
    }

    #[test]
    fn mac_without_homebrew_gets_the_download_page() {
        let p = plan(Helper::Cloudflared, Platform::Mac, with(&[]));
        assert!(matches!(
            p,
            Plan::Download {
                url: CLOUDFLARED_DOWNLOADS,
                ..
            }
        ));
        let p = plan(Helper::Tailscale, Platform::Mac, with(&["curl", "sh"]));
        assert!(matches!(
            p,
            Plan::Download {
                url: TAILSCALE_MAC_DOWNLOAD,
                ..
            }
        ));
    }

    #[test]
    fn windows_always_gets_the_download_page() {
        // Neither vendor documents a winget id, so none is guessed.
        for helper in [Helper::Cloudflared, Helper::Tailscale] {
            let p = plan(helper, Platform::Windows, with(&["winget", "choco"]));
            assert!(matches!(p, Plan::Download { .. }), "{helper}");
        }
        assert_eq!(
            plan(Helper::Tailscale, Platform::Windows, with(&["winget"])).text(),
            TAILSCALE_WINDOWS_DOWNLOAD
        );
    }

    #[test]
    fn linux_tailscale_uses_the_official_script() {
        let p = plan(Helper::Tailscale, Platform::Linux, with(&["curl", "sh"]));
        assert_eq!(p.text(), "curl -fsSL https://tailscale.com/install.sh | sh");
        let Plan::Run { steps, .. } = &p else {
            panic!("expected a run plan")
        };
        assert_eq!(steps[0].argv[..2], ["sh", "-c"]);
        let p = plan(Helper::Tailscale, Platform::Linux, with(&["sh"]));
        assert!(matches!(
            p,
            Plan::Download {
                url: TAILSCALE_LINUX,
                ..
            }
        ));
    }

    #[test]
    fn linux_cloudflared_uses_cloudflares_repository() {
        let p = plan(
            Helper::Cloudflared,
            Platform::Linux,
            with(&["apt-get", "curl", "sudo", "yum"]),
        );
        let text = p.text();
        assert!(matches!(p, Plan::Run { via: "apt-get", .. }));
        assert!(text.contains("https://pkg.cloudflare.com/cloudflare-main.gpg"));
        assert!(text.contains(
            "deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] \
             https://pkg.cloudflare.com/cloudflared any main"
        ));
        assert!(text.ends_with("sudo apt-get install cloudflared"));

        let p = plan(
            Helper::Cloudflared,
            Platform::Linux,
            with(&["yum", "curl", "sudo"]),
        );
        assert!(matches!(p, Plan::Run { via: "yum", .. }));
        assert!(
            p.text()
                .contains("https://pkg.cloudflare.com/cloudflared.repo")
        );
        assert!(!p.text().contains("yum update"));

        let p = plan(Helper::Cloudflared, Platform::Linux, with(&["pacman"]));
        assert!(matches!(p, Plan::Download { .. }));
    }

    #[test]
    fn other_systems_get_the_download_page() {
        for helper in [Helper::Cloudflared, Helper::Tailscale] {
            let p = plan(helper, Platform::Other, |_| true);
            assert!(matches!(p, Plan::Download { .. }), "{helper}");
        }
    }

    #[test]
    fn hints_are_one_line() {
        let p = plan(Helper::Cloudflared, Platform::Mac, with(&["brew"]));
        assert_eq!(p.hint(), "brew install cloudflared");
        let p = plan(Helper::Tailscale, Platform::Mac, with(&["brew"]));
        assert_eq!(p.hint(), "run hashi install tailscale");
        let p = plan(Helper::Tailscale, Platform::Windows, with(&[]));
        assert!(p.hint().starts_with("download it from https://"));
    }

    #[test]
    fn crush_uses_the_readme_commands() {
        let p = plan(Helper::Crush, Platform::Mac, with(&["brew", "npm"]));
        assert_eq!(p.text(), "brew install charmbracelet/tap/crush");
        let p = plan(Helper::Crush, Platform::Windows, with(&["winget", "npm"]));
        assert_eq!(p.text(), "winget install charmbracelet.crush");
        for platform in [
            Platform::Linux,
            Platform::Mac,
            Platform::Windows,
            Platform::Other,
        ] {
            let p = plan(Helper::Crush, platform, with(&["npm"]));
            assert_eq!(p.text(), "npm install -g @charmland/crush", "{platform:?}");
            let p = plan(Helper::Crush, platform, with(&[]));
            assert!(matches!(
                p,
                Plan::Download {
                    url: CRUSH_RELEASES,
                    ..
                }
            ));
        }
        assert_eq!(Helper::parse("crush"), Some(Helper::Crush));
    }

    #[test]
    fn helpers_parse_by_name() {
        assert_eq!(Helper::parse("tailscale"), Some(Helper::Tailscale));
        assert_eq!(Helper::parse("cloudflared"), Some(Helper::Cloudflared));
        assert_eq!(Helper::parse("ngrok"), None);
    }

    #[test]
    fn a_custom_path_that_is_missing_reads_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("tailscale");
        assert_eq!(locate(Helper::Tailscale, Some(&missing)), None);
    }
}
