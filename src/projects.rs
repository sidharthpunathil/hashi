//! Projects: a folder on this machine with its own agents, accounts and
//! defaults, that tokens and invites can be bound to.
//!
//! A project has a name, a canonical absolute path to an existing directory,
//! the agents it allows, an ordered account list per agent (from
//! [`crate::accounts`]), an optional default agent and model per agent, and
//! a note. There is an implicit `default` project: the behaviour without
//! projects, where each token runs in its own workspace with its own
//! account lists.
//!
//! What a run of a bound token may do is decided by one pure function,
//! [`effective`]: the token's agents intersected with the project's, the
//! token's account list when it is a subset of the project's (else the
//! project's list), the project's model when the request names none, and
//! the project path as the working directory. Binding or editing can never
//! widen what a token may use: [`check_token_accounts`] rejects a token
//! account list outside its project's.
//!
//! Hashi never creates, changes or deletes anything inside a project
//! folder. Revoking a token deletes only its own workspace, and a project
//! path may not lie inside the Hashi home, so `hashi uninstall` (which
//! deletes the home) cannot reach one either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::accounts::{self as acct, AccountMap};
use crate::agent::AgentKind;
use crate::token::Token;
use crate::usage::Strategy;

/// Name of the implicit project that means "no project".
pub const DEFAULT: &str = "default";

/// Most projects a server keeps.
pub const MAX_PROJECTS: usize = 100;

/// Longest note, in characters.
pub const MAX_NOTE_CHARS: usize = 200;

/// Default model per agent.
pub type ModelMap = BTreeMap<AgentKind, String>;

/// A stored project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// Name, 1 to 32 characters from `[a-z0-9-]`.
    pub name: String,
    /// Canonical absolute path of the folder. Admins only.
    pub path: String,
    /// Agents the project allows.
    pub agents: Vec<AgentKind>,
    /// Ordered accounts per agent. An agent without an entry uses its
    /// default account. Admins only.
    pub accounts: AccountMap,
    /// Agent used when a request names none.
    pub default_agent: Option<AgentKind>,
    /// Model per agent, used when a request names none.
    pub models: ModelMap,
    /// Who or what the project is for.
    pub note: Option<String>,
    /// How runs choose among an agent's accounts; `None` uses the server's
    /// default.
    pub account_strategy: Option<Strategy>,
    /// Percent used at which runs move off an account; `None` uses the
    /// server's default.
    pub rotate_at: Option<u8>,
    /// Creation time, Unix ms.
    pub created_at: i64,
}

/// Checks a project name: 1 to 32 characters from `[a-z0-9-]`, not
/// starting with `-`, and not `default`.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_name(name: &str) -> Result<(), String> {
    let ok = (1..=32).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(format!(
            "project name `{}` must be 1 to 32 characters from a-z, 0-9 and -",
            crate::util::truncate(name, 40)
        ));
    }
    if name == DEFAULT {
        return Err(
            "`default` is the implicit project (each token's own workspace) and cannot be added"
                .to_owned(),
        );
    }
    Ok(())
}

/// Checks a project folder and returns its canonical path: it must be
/// absolute, exist and be a directory (after following symlinks), be valid
/// UTF-8, not be the file system root, and not lie inside the Hashi home
/// `home` (whose contents Hashi deletes on revoke and uninstall).
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_path(path: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "the project path `{}` must be absolute",
            path.display()
        ));
    }
    let canon = std::fs::canonicalize(path)
        .map_err(|_| format!("the project path `{}` does not exist", path.display()))?;
    if !canon.is_dir() {
        return Err(format!(
            "the project path `{}` is not a directory",
            path.display()
        ));
    }
    if canon.to_str().is_none() {
        return Err("the project path must be valid UTF-8".to_owned());
    }
    if canon.parent().is_none() {
        return Err("the file system root cannot be a project".to_owned());
    }
    if let Some(home) = home {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        if canon.starts_with(&home) {
            return Err(format!(
                "the project path `{}` is inside the Hashi home, which Hashi deletes on revoke \
                 and uninstall; choose a folder outside it",
                path.display()
            ));
        }
    }
    Ok(canon)
}

/// Checks a note: at most [`MAX_NOTE_CHARS`] printable characters. Blank
/// is no note.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_note(note: Option<&str>) -> Result<Option<String>, String> {
    match note.map(str::trim) {
        None | Some("") => Ok(None),
        Some(n) if n.chars().count() > MAX_NOTE_CHARS || n.chars().any(char::is_control) => Err(
            format!("note must be at most {MAX_NOTE_CHARS} printable characters"),
        ),
        Some(n) => Ok(Some(n.to_owned())),
    }
}

/// Parses one `--model` value, `claude=sonnet`.
///
/// # Errors
///
/// Returns a message for an unknown agent or an unsafe model name.
pub fn parse_model(s: &str) -> Result<(AgentKind, String), String> {
    let (agent, model) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <agent>=<model>, got `{s}`"))?;
    let agent: AgentKind = agent
        .trim()
        .parse()
        .map_err(|e: crate::agent::UnknownAgent| e.to_string())?;
    let model = model.trim();
    if !crate::agent::is_safe_arg(model) {
        return Err(format!("model `{model}` contains unsupported characters"));
    }
    Ok((agent, model.to_owned()))
}

/// Checks a project's shape, apart from its path and whether its accounts
/// exist: at least one agent, no repeats; account lists and models only for
/// allowed agents; a default agent the project allows; safe model names.
/// Returns the project with agents sorted and account lists normalised.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_shape(mut p: Project) -> Result<Project, String> {
    check_name(&p.name)?;
    p.agents.sort();
    p.agents.dedup();
    if p.agents.is_empty() {
        return Err("a project must allow at least one agent".to_owned());
    }
    p.accounts = acct::normalize(p.accounts)?;
    if let Some(agent) = p.accounts.keys().find(|a| !p.agents.contains(a)) {
        return Err(format!(
            "the project lists accounts for {agent}, which it does not allow"
        ));
    }
    for (agent, model) in &p.models {
        if !p.agents.contains(agent) {
            return Err(format!(
                "the project sets a model for {agent}, which it does not allow"
            ));
        }
        if !crate::agent::is_safe_arg(model) {
            return Err(format!("model `{model}` contains unsupported characters"));
        }
    }
    if let Some(agent) = p.default_agent
        && !p.agents.contains(&agent)
    {
        return Err(format!(
            "the default agent {agent} is not one the project allows"
        ));
    }
    p.note = check_note(p.note.as_deref())?;
    if let Some(pct) = p.rotate_at {
        crate::usage::check_rotate_at(pct)?;
    }
    Ok(p)
}

/// Checks that a token's account lists stay inside its project's: for each
/// agent the token lists, every account must be on the project's list for
/// that agent (`default` when the project has none).
///
/// # Errors
///
/// Returns a message naming the first agent whose list is wider.
pub fn check_token_accounts(token: &AccountMap, project: &Project) -> Result<(), String> {
    for (agent, names) in token {
        let allowed = acct::for_agent(&project.accounts, *agent);
        if let Some(extra) = names.iter().find(|n| !allowed.contains(n)) {
            return Err(format!(
                "account {agent}:{extra} is not on project {}'s list for {agent} ({}); a token \
                 bound to a project may only narrow the project's accounts",
                project.name,
                allowed.join(",")
            ));
        }
    }
    Ok(())
}

/// Where a run starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cwd {
    /// The token's own workspace, `<home>/workspaces/<token>/<agent>`.
    Workspace,
    /// The project folder.
    Project(PathBuf),
}

/// The settings a run of one token actually gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    /// The project's name, or `None` for the implicit default project.
    pub project: Option<String>,
    /// Agents it may use.
    pub agents: Vec<AgentKind>,
    /// Ordered accounts per agent. An agent without an entry uses its
    /// default account, untracked, as a token without a list always has.
    pub accounts: AccountMap,
    /// Model per agent when the request names none.
    pub models: ModelMap,
    /// Agent used when the request names none, if the project sets one it
    /// may use.
    pub default_agent: Option<AgentKind>,
    /// Where runs start.
    pub cwd: Cwd,
    /// The project's account strategy, when it sets one.
    pub account_strategy: Option<Strategy>,
    /// The project's rotate threshold, when it sets one.
    pub rotate_at: Option<u8>,
}

impl Effective {
    /// Whether `agent` may run.
    #[must_use]
    pub fn allows(&self, agent: AgentKind) -> bool {
        self.agents.contains(&agent)
    }

    /// The model a request for `agent` runs with: the one it names, or the
    /// project's for that agent.
    #[must_use]
    pub fn model(&self, agent: AgentKind, requested: Option<&str>) -> Option<String> {
        requested
            .map(str::to_owned)
            .or_else(|| self.models.get(&agent).cloned())
    }

    /// The key sessions are kept under: the project name, or `default`.
    #[must_use]
    pub fn session_key(&self) -> &str {
        self.project.as_deref().unwrap_or(DEFAULT)
    }
}

/// The settings a run of `token` gets in `project` (`None` for the implicit
/// default project, which is the token's own settings and workspace).
///
/// - **Agents**: the token's, intersected with the project's.
/// - **Accounts**: for each allowed agent, the token's list when it has one
///   that is a subset of the project's list (`default` when the project has
///   none); otherwise the project's list, if either has one.
/// - **Model**: the project's per-agent model, for requests that name none.
/// - **Default agent**: the project's, when the token may use it.
/// - **Cwd**: the project folder.
/// - **Strategy and threshold**: the project's, when it sets them.
///
/// Access never widens: every agent and account here is one the project
/// allows, and every agent is one the token allows.
#[must_use]
pub fn effective(token: &Token, project: Option<&Project>) -> Effective {
    let Some(p) = project else {
        return Effective {
            project: None,
            agents: token.agents.clone(),
            accounts: token.accounts.clone(),
            models: ModelMap::new(),
            default_agent: None,
            cwd: Cwd::Workspace,
            account_strategy: None,
            rotate_at: None,
        };
    };
    let agents: Vec<AgentKind> = token
        .agents
        .iter()
        .copied()
        .filter(|a| p.agents.contains(a))
        .collect();
    let mut accounts = AccountMap::new();
    for agent in &agents {
        let own = token.accounts.get(agent).filter(|l| !l.is_empty());
        let theirs = p.accounts.get(agent).filter(|l| !l.is_empty());
        let allowed = acct::for_agent(&p.accounts, *agent);
        let chosen = match own {
            Some(own) if own.iter().all(|n| allowed.contains(n)) => Some(own.clone()),
            Some(_) => Some(allowed),
            None => theirs.cloned(),
        };
        if let Some(list) = chosen {
            accounts.insert(*agent, list);
        }
    }
    let models = p
        .models
        .iter()
        .filter(|(a, _)| agents.contains(a))
        .map(|(a, m)| (*a, m.clone()))
        .collect();
    Effective {
        project: Some(p.name.clone()),
        default_agent: p.default_agent.filter(|a| agents.contains(a)),
        agents,
        accounts,
        models,
        cwd: Cwd::Project(PathBuf::from(&p.path)),
        account_strategy: p.account_strategy,
        rotate_at: p.rotate_at,
    }
}

/// Crush's project config files. Crush can run shell code as soon as it
/// loads one of these, so Hashi refuses to run Crush in a folder that has
/// any of them.
pub const CRUSH_CONFIG_FILES: [&str; 4] = [".crushrc", "crushrc", ".crush.json", "crush.json"];

/// Whether `dir` holds a Crush project config file (see
/// [`CRUSH_CONFIG_FILES`]). A symlink by one of those names counts, even a
/// dangling one.
///
/// TODO: Crush is not an agent yet. When the Crush backend lands, its runs
/// in a project must refuse to start when this returns `true`.
#[must_use]
pub fn crush_config_present(dir: &Path) -> bool {
    CRUSH_CONFIG_FILES
        .iter()
        .any(|name| std::fs::symlink_metadata(dir.join(name)).is_ok())
}

/// Encodes a model map for the database.
#[must_use]
pub fn encode_models(map: &ModelMap) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_owned())
}

/// Decodes a stored model map. Anything unreadable is an empty map.
#[must_use]
pub fn decode_models(s: &str) -> ModelMap {
    serde_json::from_str(s).unwrap_or_default()
}

/// A model map as `claude=sonnet codex=o4`, or `-` when empty.
#[must_use]
pub fn format_models(map: &ModelMap) -> String {
    if map.is_empty() {
        return "-".to_owned();
    }
    map.iter()
        .map(|(a, m)| format!("{a}={m}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{Limits, Role, SessionMode};

    fn token(agents: &[AgentKind], accounts: AccountMap) -> Token {
        Token {
            id: "t".into(),
            name: "t".into(),
            prefix: "hashi_t".into(),
            role: Role::User,
            session_mode: SessionMode::Stateless,
            agents: agents.to_vec(),
            expires_at: None,
            created_at: 0,
            last_used_at: None,
            paused: false,
            note: None,
            limits: Limits::default(),
            accounts,
            project: None,
            share_usage: false,
        }
    }

    fn project(agents: &[AgentKind], accounts: AccountMap) -> Project {
        Project {
            name: "api".into(),
            path: "/srv/api".into(),
            agents: agents.to_vec(),
            accounts,
            default_agent: None,
            models: ModelMap::new(),
            note: None,
            account_strategy: None,
            rotate_at: None,
            created_at: 0,
        }
    }

    fn map(s: &str) -> AccountMap {
        acct::parse_bindings(s).unwrap()
    }

    use AgentKind::{Claude, Codex, Gemini};

    #[test]
    fn no_project_is_the_token_itself() {
        let t = token(&[Claude, Codex], map("claude=work"));
        let e = effective(&t, None);
        assert_eq!(e.project, None);
        assert_eq!(e.agents, [Claude, Codex]);
        assert_eq!(e.accounts, map("claude=work"));
        assert_eq!(e.cwd, Cwd::Workspace);
        assert_eq!(e.model(Claude, None), None);
        assert_eq!(e.model(Claude, Some("opus")).as_deref(), Some("opus"));
        assert_eq!(e.default_agent, None);
        assert_eq!(e.session_key(), "default");
    }

    #[test]
    fn agents_are_intersected() {
        let t = token(&[Claude, Codex], AccountMap::new());
        let p = project(&[Codex, Gemini], AccountMap::new());
        let e = effective(&t, Some(&p));
        assert_eq!(e.agents, [Codex]);
        assert!(e.allows(Codex));
        assert!(!e.allows(Claude), "the token may not widen to the project");
        assert!(!e.allows(Gemini), "the project may not widen the token");
        assert_eq!(e.cwd, Cwd::Project(PathBuf::from("/srv/api")));
        assert_eq!(e.session_key(), "api");
    }

    #[test]
    fn a_token_list_inside_the_project_list_is_kept_in_its_order() {
        let t = token(&[Claude], map("claude=home,work"));
        let p = project(&[Claude], map("claude=work,home,spare"));
        assert_eq!(effective(&t, Some(&p)).accounts, map("claude=home,work"));
    }

    #[test]
    fn a_token_list_outside_the_project_list_falls_back_to_the_project() {
        let t = token(&[Claude], map("claude=work,personal"));
        let p = project(&[Claude], map("claude=work"));
        assert_eq!(effective(&t, Some(&p)).accounts, map("claude=work"));
        // A project with no list allows only the default login.
        let p = project(&[Claude], AccountMap::new());
        assert_eq!(effective(&t, Some(&p)).accounts, map("claude=default"));
    }

    #[test]
    fn a_token_without_a_list_uses_the_project_list() {
        let t = token(&[Claude, Codex], AccountMap::new());
        let p = project(&[Claude, Codex], map("claude=work"));
        let e = effective(&t, Some(&p));
        assert_eq!(e.accounts, map("claude=work"));
        assert_eq!(acct::for_agent(&e.accounts, Codex), ["default"]);
    }

    #[test]
    fn accounts_never_appear_for_agents_left_out() {
        let t = token(&[Claude], map("codex=ci"));
        let p = project(&[Claude, Codex], map("codex=ci claude=work"));
        let e = effective(&t, Some(&p));
        assert_eq!(e.accounts, map("claude=work"));
    }

    #[test]
    fn effective_accounts_are_always_inside_the_project() {
        // Every combination of token and project lists over three names.
        let names = ["default", "work", "home"];
        let subsets: Vec<Vec<String>> = (0..8u8)
            .map(|bits| {
                names
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| bits & (1 << i) != 0)
                    .map(|(_, n)| (*n).to_owned())
                    .collect()
            })
            .collect();
        for own in &subsets {
            for theirs in &subsets {
                let t = token(&[Claude], AccountMap::from([(Claude, own.clone())]));
                let p = project(&[Claude], AccountMap::from([(Claude, theirs.clone())]));
                let allowed = acct::for_agent(&p.accounts, Claude);
                let got = acct::for_agent(&effective(&t, Some(&p)).accounts, Claude);
                assert!(
                    got.iter().all(|n| allowed.contains(n)),
                    "{own:?} in {theirs:?} gave {got:?}"
                );
                if !own.is_empty() && own.iter().all(|n| allowed.contains(n)) {
                    assert_eq!(&got, own, "a narrower token list is kept");
                }
            }
        }
    }

    #[test]
    fn the_project_model_applies_only_when_the_request_names_none() {
        let t = token(&[Claude, Codex], AccountMap::new());
        let mut p = project(&[Claude, Codex], AccountMap::new());
        p.models.insert(Claude, "sonnet".into());
        let e = effective(&t, Some(&p));
        assert_eq!(e.model(Claude, None).as_deref(), Some("sonnet"));
        assert_eq!(e.model(Claude, Some("opus")).as_deref(), Some("opus"));
        assert_eq!(e.model(Codex, None), None);
    }

    #[test]
    fn models_and_default_agent_follow_the_intersection() {
        let t = token(&[Codex], AccountMap::new());
        let mut p = project(&[Claude, Codex], AccountMap::new());
        p.models.insert(Claude, "sonnet".into());
        p.default_agent = Some(Claude);
        let e = effective(&t, Some(&p));
        assert!(e.models.is_empty());
        assert_eq!(e.default_agent, None, "the token may not use claude");
        p.default_agent = Some(Codex);
        assert_eq!(effective(&t, Some(&p)).default_agent, Some(Codex));
    }

    #[test]
    fn binding_rejects_a_wider_account_list() {
        let p = project(&[Claude, Codex], map("claude=work,home"));
        assert!(check_token_accounts(&map("claude=home"), &p).is_ok());
        assert!(check_token_accounts(&AccountMap::new(), &p).is_ok());
        let err = check_token_accounts(&map("claude=work,personal"), &p).unwrap_err();
        assert!(
            err.contains("claude:personal") && err.contains("api"),
            "{err}"
        );
        // An agent the project has no list for allows only the default.
        assert!(check_token_accounts(&map("codex=default"), &p).is_ok());
        assert!(check_token_accounts(&map("codex=ci"), &p).is_err());
    }

    #[test]
    fn names_follow_the_rules() {
        for ok in ["api", "my-blog", "a1", &"a".repeat(32)] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Api",
            "a_b",
            "a b",
            "-x",
            "a/b",
            "..",
            "ü",
            &"a".repeat(33),
            "default",
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_must_be_existing_directories_outside_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        let folder = real.join("repo");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(real.join("file"), "x").unwrap();
        let home = real.join("home");
        std::fs::create_dir(&home).unwrap();

        assert_eq!(check_path(&folder, Some(&home)).unwrap(), folder);
        assert!(
            check_path(Path::new("relative/dir"), None)
                .unwrap_err()
                .contains("absolute")
        );
        assert!(
            check_path(&real.join("missing"), None)
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(
            check_path(&real.join("file"), None)
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(
            check_path(&home.join("."), Some(&home))
                .unwrap_err()
                .contains("inside the Hashi home")
        );
        let inner = home.join("workspaces");
        std::fs::create_dir(&inner).unwrap();
        assert!(check_path(&inner, Some(&home)).is_err());
        assert!(check_path(Path::new("/"), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_resolved_to_the_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        let folder = real.join("repo");
        std::fs::create_dir(&folder).unwrap();
        let link = real.join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        assert_eq!(check_path(&link, None).unwrap(), folder);
        assert_eq!(check_path(&folder.join("../repo"), None).unwrap(), folder);
        // A link into the home is refused like the home itself.
        let home = real.join("home");
        std::fs::create_dir(&home).unwrap();
        let sneaky = real.join("sneaky");
        std::os::unix::fs::symlink(&home, &sneaky).unwrap();
        assert!(check_path(&sneaky, Some(&home)).is_err());
        // A dangling link does not exist.
        let dangling = real.join("dangling");
        std::os::unix::fs::symlink(real.join("nowhere"), &dangling).unwrap();
        assert!(check_path(&dangling, None).is_err());
    }

    #[test]
    fn shape_checks_agents_accounts_models_and_default() {
        let mut p = project(&[Codex, Claude, Claude], map("claude=work"));
        p.models.insert(Claude, "sonnet".into());
        p.default_agent = Some(Claude);
        p.note = Some("  ".into());
        let ok = check_shape(p.clone()).unwrap();
        assert_eq!(ok.agents, [Claude, Codex]);
        assert_eq!(ok.note, None);

        let bad = |f: fn(&mut Project)| {
            let mut q = p.clone();
            f(&mut q);
            check_shape(q).unwrap_err()
        };
        assert!(bad(|q| q.agents.clear()).contains("at least one"));
        assert!(bad(|q| q.accounts = map("gemini=x")).contains("gemini"));
        assert!(bad(|q| q.default_agent = Some(Gemini)).contains("default agent"));
        assert!(
            bad(|q| {
                q.models.insert(Gemini, "pro".into());
            })
            .contains("model")
        );
        assert!(
            bad(|q| {
                q.models.insert(Claude, "-rf".into());
            })
            .contains("unsupported")
        );
        assert!(bad(|q| q.note = Some("a\nb".into())).contains("note"));
        assert!(bad(|q| q.name = "default".into()).contains("default"));
    }

    #[test]
    fn models_parse_and_roundtrip() {
        assert_eq!(
            parse_model("claude=sonnet").unwrap(),
            (Claude, "sonnet".into())
        );
        assert!(parse_model("claude").is_err());
        assert!(parse_model("nope=x").is_err());
        assert!(parse_model("claude=--yolo").is_err());
        let m = ModelMap::from([(Claude, "sonnet".to_owned())]);
        assert_eq!(decode_models(&encode_models(&m)), m);
        assert_eq!(decode_models("garbage"), ModelMap::new());
        assert_eq!(format_models(&m), "claude=sonnet");
        assert_eq!(format_models(&ModelMap::new()), "-");
    }

    #[test]
    fn crush_config_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!crush_config_present(dir.path()));
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        assert!(!crush_config_present(dir.path()));
        for name in CRUSH_CONFIG_FILES {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(name), "{}").unwrap();
            assert!(crush_config_present(d.path()), "{name}");
        }
        assert!(!crush_config_present(&dir.path().join("missing")));
    }
}
