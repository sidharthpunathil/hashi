//! Account operations shared by `hashi account` and the dashboard: adding,
//! signing in, disabling and removing, each audited with the caller's actor.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{self as acct, AccountKind};
use crate::agent::AgentKind;
use crate::store::{RemovedAccount, Store};

/// Checks that `name` can be added to `agent` as an account of `kind` (or
/// the kind implied by whether a key is given), and returns the kind.
/// Nothing is created.
///
/// # Errors
///
/// Fails on a bad name, an unsupported kind or a name already taken.
pub fn check_new(
    store: &Store,
    agent: AgentKind,
    name: &str,
    kind: Option<AccountKind>,
    keyed: bool,
) -> Result<AccountKind> {
    acct::check_name(name, false).map_err(|e| anyhow::anyhow!(e))?;
    let kind = match kind {
        Some(AccountKind::Default) => bail!("the default account is always there"),
        Some(AccountKind::Login) if keyed => {
            bail!("a login account has no key; drop --key-stdin and --key-env")
        }
        Some(k) => k,
        None if keyed => AccountKind::ApiKey,
        None => AccountKind::Login,
    };
    if !acct::supports(agent, kind) {
        bail!(
            "{} does not support {kind} accounts{}",
            agent.display_name(),
            match agent {
                AgentKind::Cursor => "; add an API key with --kind api-key",
                AgentKind::Opencode | AgentKind::Qwen | AgentKind::Crush => "; add a login account",
                _ => "",
            }
        );
    }
    if store
        .get_account(agent, name)?
        .is_some_and(|a| a.kind != AccountKind::Default)
    {
        bail!("{agent} already has an account named {name}");
    }
    Ok(kind)
}

/// Adds an account checked by [`check_new`]: its row, then its directory or
/// key file. A key account needs `key`, already cleaned with
/// [`acct::clean_key`]. Returns the account's directory.
///
/// # Errors
///
/// Fails when the directory or key cannot be written (the row is removed
/// again) or on database errors.
pub fn create(
    store: &Store,
    home: &Path,
    agent: AgentKind,
    name: &str,
    kind: AccountKind,
    key: Option<&str>,
    actor: &str,
) -> Result<PathBuf> {
    if kind.is_key() && key.is_none() {
        bail!("a {kind} account needs its key");
    }
    // A row left by a limit mark on a since removed account.
    store.remove_account(agent, name)?;
    store.add_account(agent, name, kind)?;
    let made = match key {
        Some(key) if kind.is_key() => {
            acct::write_key(home, agent, name, key).map(|()| acct::account_dir(home, agent, name))
        }
        _ => acct::create_dir(home, agent, name),
    };
    let dir = match made {
        Ok(dir) => dir,
        Err(e) => {
            store.remove_account(agent, name)?;
            return Err(e).context("creating the account directory");
        }
    };
    store.audit(
        actor,
        "account.add",
        &format!("{agent}:{name}"),
        kind.as_str(),
    )?;
    Ok(dir)
}

/// Gets a login account ready to sign in again: its directory exists and it
/// is a login. Crush's default account counts, since Hashi owns its
/// directory too.
///
/// # Errors
///
/// Fails when there is no such login account or the directory cannot be
/// made.
pub fn prepare_login(store: &Store, home: &Path, agent: AgentKind, name: &str) -> Result<()> {
    if agent == AgentKind::Crush && name == acct::DEFAULT {
        acct::crush_default_dir(home).context("preparing the account directory")?;
        return Ok(());
    }
    acct::check_name(name, false).map_err(|e| anyhow::anyhow!(e))?;
    match store.get_account(agent, name)? {
        Some(a) if a.kind == AccountKind::Login => {}
        Some(a) if a.kind != AccountKind::Default => {
            bail!("{agent}:{name} is a {} account, not a login", a.kind)
        }
        _ => bail!("no {agent} account named {name}"),
    }
    acct::create_dir(home, agent, name).context("preparing the account directory")?;
    Ok(())
}

/// Disables or enables an account.
///
/// # Errors
///
/// Fails for the default account, an unknown one or on database errors.
pub fn set_disabled(
    store: &Store,
    agent: AgentKind,
    name: &str,
    disabled: bool,
    actor: &str,
) -> Result<()> {
    if name == acct::DEFAULT {
        bail!("the default account cannot be disabled; leave it out of a token's list instead");
    }
    if store
        .get_account(agent, name)?
        .is_none_or(|a| a.kind == AccountKind::Default)
    {
        bail!("no {agent} account named {name}");
    }
    store.set_account_disabled(agent, name, disabled)?;
    let action = if disabled { "disable" } else { "enable" };
    store.audit(
        actor,
        &format!("account.{action}"),
        &format!("{agent}:{name}"),
        "",
    )?;
    Ok(())
}

/// The tokens using an account that may be removed.
///
/// # Errors
///
/// Fails when there is no such account (other than the default) or on
/// database errors.
pub fn check_removable(store: &Store, agent: AgentKind, name: &str) -> Result<usize> {
    acct::check_name(name, false).map_err(|e| anyhow::anyhow!(e))?;
    if store
        .get_account(agent, name)?
        .is_none_or(|a| a.kind == AccountKind::Default)
    {
        bail!("no {agent} account named {name}");
    }
    Ok(store.tokens_using_account(agent, name)?)
}

/// Removes an account checked by [`check_removable`]: its row, its
/// directory and key, and its place on every token and invite. The second
/// value is the result of deleting the directory.
///
/// # Errors
///
/// Fails on database errors.
pub fn remove(
    store: &Store,
    home: &Path,
    agent: AgentKind,
    name: &str,
    actor: &str,
) -> Result<(RemovedAccount, std::io::Result<bool>)> {
    let removed = store.remove_account(agent, name)?;
    let dir = acct::remove_dir(home, agent, name);
    store.audit(actor, "account.remove", &format!("{agent}:{name}"), "")?;
    Ok((removed, dir))
}

/// The account variables and the sign-in command line, as shown to the
/// owner.
#[must_use]
pub fn sign_in_line(home: &Path, agent: AgentKind, name: &str, bin: &Path) -> Option<String> {
    let sign = acct::sign_in(agent)?;
    acct::dir_var(agent)?;
    let dir = acct::account_dir(home, agent, name);
    let mut parts: Vec<String> = acct::account_vars(agent, &dir)
        .iter()
        .map(|(k, v)| format!("{k}={}", shell_quote(v)))
        .collect();
    parts.extend(sign.env.iter().map(|(k, v)| format!("{k}={v}")));
    parts.push(shell_quote(&bin.to_string_lossy()));
    parts.extend(sign.args.iter().map(|a| (*a).to_owned()));
    Some(parts.join(" "))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '~'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// A command run with the account's directory and none of the owner's
/// credential variables or Hashi's own secrets.
#[must_use]
pub fn account_command(
    home: &Path,
    agent: AgentKind,
    name: &str,
    bin: &Path,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        let removed = acct::credential_vars(agent)
            .iter()
            .chain(acct::always_removed(agent))
            .any(|p| crate::agent::runner::env_name_matches(p, &upper));
        if crate::agent::runner::is_hashi_secret(&key) || removed {
            cmd.env_remove(&key);
        }
    }
    cmd.envs(acct::account_vars(
        agent,
        &acct::account_dir(home, agent, name),
    ));
    cmd
}

/// Runs the agent's sign-in command for a login account, attached to the
/// current terminal, and returns whether it exited cleanly.
///
/// # Errors
///
/// Fails when the agent has no sign-in command or cannot be started.
pub fn run_sign_in(home: &Path, agent: AgentKind, name: &str, bin: &Path) -> Result<bool> {
    let Some(sign) = acct::sign_in(agent) else {
        bail!(
            "{} has no sign-in command Hashi knows",
            agent.display_name()
        );
    };
    let status = account_command(home, agent, name, bin)
        .args(sign.args)
        .envs(sign.env.iter().copied())
        .status()
        .with_context(|| format!("starting {}", bin.display()))?;
    Ok(status.success())
}

/// Whether an account is signed in, as far as can be checked: for a key
/// account, whether its key file is there; for a login, Claude Code answers
/// `claude auth status --json` when `probe` is set, and otherwise whether
/// the CLI wrote anything into the account directory.
#[must_use]
pub fn signed_in(
    home: &Path,
    agent: AgentKind,
    name: &str,
    kind: AccountKind,
    bin: &Path,
    probe: bool,
) -> String {
    match kind {
        AccountKind::Default => "-".to_owned(),
        AccountKind::ApiKey | AccountKind::OauthToken => {
            if acct::read_key(home, agent, name).is_ok() {
                "key set".to_owned()
            } else {
                "no key".to_owned()
            }
        }
        AccountKind::Login => match acct::status_args(agent).filter(|_| probe) {
            Some(args) => match run_status(account_command(home, agent, name, bin).args(args)) {
                Some(true) => "yes".to_owned(),
                Some(false) => "no".to_owned(),
                None => "unknown".to_owned(),
            },
            None if acct::has_files(&acct::account_dir(home, agent, name)) => {
                "files present".to_owned()
            }
            None => "no".to_owned(),
        },
    }
}

/// Runs a status command with a ten second limit. `Some(true)` when it
/// reports a login: a `loggedIn` field when its output is JSON with one,
/// else a zero exit.
fn run_status(cmd: &mut std::process::Command) -> Option<bool> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool));
    Some(field.unwrap_or(out.status.success()))
}
