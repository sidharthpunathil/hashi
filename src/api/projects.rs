//! Admin project management, and the checks shared with the CLI and the
//! dashboard: a project's own validation, binding a token or invite to a
//! project, and keeping every bound token inside its project on edits.

use std::path::Path as FsPath;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Admin, ApiError, ApiResult, AppState};
use crate::accounts::{self as acct, AccountMap};
use crate::agent::AgentKind;
use crate::projects::{self as proj, ModelMap, Project};
use crate::store::{RemovedProject, Store};
use crate::usage::Strategy;
use crate::util::{now_ms, rfc3339};

/// Body of `POST /v1/projects`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectBody {
    /// Name, 1 to 32 characters from `[a-z0-9-]`.
    pub name: String,
    /// Absolute path of an existing folder.
    pub path: String,
    /// Allowed agents. Defaults to the installed ones.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// Ordered accounts per agent, as on `POST /v1/tokens`.
    #[serde(default)]
    pub accounts: Option<AccountMap>,
    /// Agent used when a request names none.
    #[serde(default)]
    pub default_agent: Option<AgentKind>,
    /// Model per agent, used when a request names none.
    #[serde(default)]
    pub models: Option<ModelMap>,
    /// Who or what the project is for.
    #[serde(default)]
    pub note: Option<String>,
    /// How runs choose among an agent's accounts. Default: the server's.
    #[serde(default)]
    pub account_strategy: Option<Strategy>,
    /// Percent used, 50 to 99, at which runs move off an account. Default:
    /// the server's.
    #[serde(default)]
    pub rotate_at: Option<u8>,
}

/// Body of `PATCH /v1/projects/{name}`. Omitted fields are left as they are.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProjectBody {
    /// New folder.
    #[serde(default)]
    pub path: Option<String>,
    /// Replaces the allowed agents.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// Replaces the accounts map.
    #[serde(default)]
    pub accounts: Option<AccountMap>,
    /// New default agent; `null` removes it.
    #[serde(default, deserialize_with = "super::tokens::present")]
    pub default_agent: Option<Option<AgentKind>>,
    /// Replaces the models map.
    #[serde(default)]
    pub models: Option<ModelMap>,
    /// New note; an empty string clears it.
    #[serde(default)]
    pub note: Option<String>,
    /// New account strategy; `null` goes back to the server's default.
    #[serde(default, deserialize_with = "super::tokens::present")]
    pub account_strategy: Option<Option<Strategy>>,
    /// New rotate threshold; `null` goes back to the server's default.
    #[serde(default, deserialize_with = "super::tokens::present")]
    pub rotate_at: Option<Option<u8>>,
}

/// Query of `DELETE /v1/projects/{name}`.
#[derive(Debug, Deserialize)]
pub(super) struct RemoveQuery {
    #[serde(default)]
    unbind: bool,
}

/// Checks a whole project before it is saved: its shape, its folder
/// (canonicalised into `path`; it may not lie inside the Hashi `home`) and
/// that its accounts exist.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] saying what is wrong, or
/// [`ApiError::Internal`] on database errors.
pub fn validate_project(
    store: &Store,
    home: Option<&FsPath>,
    project: Project,
    path: &FsPath,
) -> ApiResult<Project> {
    let mut p = proj::check_shape(project).map_err(ApiError::BadRequest)?;
    p.path = proj::check_path(path, home)
        .map_err(ApiError::BadRequest)?
        .to_string_lossy()
        .into_owned();
    p.accounts = super::check_accounts(store, p.accounts)?;
    Ok(p)
}

/// Checks that an edit of `project` keeps every token and pending invite
/// bound to it inside it: each of their account lists must stay a subset
/// of the project's.
///
/// # Errors
///
/// Returns a 409 `project_in_use` naming the first token or invite that
/// would be left outside, or [`ApiError::Internal`] on database errors.
pub fn check_bound(store: &Store, project: &Project) -> ApiResult<()> {
    let in_use = |who: String, e: String| {
        ApiError::coded(
            StatusCode::CONFLICT,
            "project_in_use",
            format!("{who} is bound to project {}: {e}", project.name),
        )
    };
    for t in store.project_tokens(&project.name)? {
        proj::check_token_accounts(&t.accounts, project)
            .map_err(|e| in_use(format!("token {}", t.name), e))?;
    }
    for i in store.project_invites(&project.name)? {
        proj::check_token_accounts(&i.accounts, project)
            .map_err(|e| in_use(format!("the invite for {}", i.label), e))?;
    }
    Ok(())
}

/// Checks binding a token or invite with `agents` and `accounts` to
/// `project`: the project exists, they share at least one agent, and the
/// account lists stay inside the project's. Returns the project.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] saying what is wrong, or
/// [`ApiError::Internal`] on database errors.
pub fn check_binding(
    store: &Store,
    project: Option<&str>,
    agents: &[AgentKind],
    accounts: &AccountMap,
) -> ApiResult<Option<Project>> {
    let Some(name) = project else {
        return Ok(None);
    };
    let p = store.get_project(name)?.ok_or_else(|| {
        ApiError::BadRequest(format!(
            "no project named `{}`; add it first with `hashi project add`",
            crate::util::truncate(name, 40)
        ))
    })?;
    if !agents.iter().any(|a| p.agents.contains(a)) {
        return Err(ApiError::BadRequest(format!(
            "project {} allows none of these agents (it allows {})",
            p.name,
            crate::token::encode_agents(&p.agents)
        )));
    }
    proj::check_token_accounts(accounts, &p).map_err(ApiError::BadRequest)?;
    Ok(Some(p))
}

/// The agents `token` may use: its own, intersected with its project's.
/// None when its project is gone, so nothing wider is ever offered.
#[must_use]
pub fn usable_agents(store: &Store, token: &crate::token::Token) -> Vec<AgentKind> {
    match &token.project {
        None => token.agents.clone(),
        Some(name) => match store.get_project(name) {
            Ok(Some(p)) => proj::effective(token, Some(&p)).agents,
            _ => Vec::new(),
        },
    }
}

/// Checks the shape of a project name given for a binding.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] for an invalid name.
pub fn check_project_name(name: Option<String>) -> ApiResult<Option<String>> {
    match name.map(|n| n.trim().to_owned()) {
        Some(n) if n.is_empty() || n == proj::DEFAULT => Ok(None),
        Some(n) => {
            proj::check_name(&n).map_err(ApiError::BadRequest)?;
            Ok(Some(n))
        }
        None => Ok(None),
    }
}

/// One project as admins see it: everything, with its use today and this
/// month and the number of bound tokens.
///
/// # Errors
///
/// Fails on database errors.
pub fn project_json(store: &Store, p: &Project, now: i64) -> ApiResult<Value> {
    let stats = store.project_stats(&p.name, now)?;
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("createdAt".into(), json!(rfc3339(p.created_at)));
        obj.insert("tokens".into(), json!(stats.tokens));
        obj.insert("stats".into(), json!(stats));
    }
    Ok(v)
}

/// The agents installed on this server, or every agent when none is.
/// Crush is opt-in and never part of the default; list it explicitly.
fn installed(state: &AppState) -> Vec<AgentKind> {
    let found: Vec<AgentKind> = state
        .runtime
        .agents
        .iter()
        .filter(|a| a.available && a.agent != AgentKind::Crush)
        .map(|a| a.agent)
        .collect();
    if found.is_empty() {
        AgentKind::ALL
            .into_iter()
            .filter(|a| *a != AgentKind::Crush)
            .collect()
    } else {
        found
    }
}

pub(super) async fn list(State(state): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let mut out = Vec::new();
    for p in state.store.list_projects()? {
        out.push(project_json(&state.store, &p, now)?);
    }
    Ok(Json(json!({ "projects": out })))
}

pub(super) async fn create(
    State(state): State<AppState>,
    Admin(admin): Admin,
    body: Result<Json<CreateProjectBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    if state.store.list_projects()?.len() >= proj::MAX_PROJECTS {
        return Err(ApiError::Conflict(
            "project limit reached, remove one first",
        ));
    }
    let raw = Project {
        name: body.name.trim().to_owned(),
        path: String::new(),
        agents: body.agents.unwrap_or_else(|| installed(&state)),
        accounts: body.accounts.unwrap_or_default(),
        default_agent: body.default_agent,
        models: body.models.unwrap_or_default(),
        note: body.note,
        account_strategy: body.account_strategy,
        rotate_at: body.rotate_at,
        created_at: now_ms(),
    };
    let home = state.store.data_dir();
    let p = validate_project(
        &state.store,
        home.as_deref(),
        raw,
        FsPath::new(body.path.trim()),
    )?;
    if !state.store.add_project(&p)? {
        return Err(ApiError::Conflict(
            "a project with that name already exists",
        ));
    }
    state
        .store
        .audit(&admin.name, "project.add", &p.name, &p.path)?;
    state.bus.log(format!("project added: {}", p.name));
    Ok((
        StatusCode::CREATED,
        Json(json!({ "project": project_json(&state.store, &p, now_ms())? })),
    ))
}

pub(super) async fn update(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(name): Path<String>,
    body: Result<Json<UpdateProjectBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let current = state
        .store
        .get_project(&name)?
        .ok_or(ApiError::NotFound("project not found"))?;
    let (p, changes) = apply_update(
        &state.store,
        state.store.data_dir().as_deref(),
        current,
        body,
    )?;
    state.store.save_project(&p)?;
    let detail = changes.join(", ");
    state
        .store
        .audit(&admin.name, "project.update", &p.name, &detail)?;
    state.bus.log(format!("project {}: {detail}", p.name));
    Ok(Json(
        json!({ "ok": true, "project": project_json(&state.store, &p, now_ms())? }),
    ))
}

/// Applies a patch to `current`, validates the result and checks that
/// every bound token stays inside it. Returns the project and a list of
/// what changed.
///
/// # Errors
///
/// As for [`validate_project`] and [`check_bound`], and
/// [`ApiError::BadRequest`] for an empty patch.
pub fn apply_update(
    store: &Store,
    home: Option<&FsPath>,
    current: Project,
    body: UpdateProjectBody,
) -> ApiResult<(Project, Vec<String>)> {
    let mut p = current;
    let mut changes = Vec::new();
    let path = match body.path {
        Some(path) => {
            changes.push("path".to_owned());
            path.trim().to_owned()
        }
        None => p.path.clone(),
    };
    if let Some(agents) = body.agents {
        p.agents = agents;
        changes.push("agents".to_owned());
    }
    if let Some(accounts) = body.accounts {
        p.accounts = accounts;
        changes.push("accounts".to_owned());
    }
    if let Some(agent) = body.default_agent {
        p.default_agent = agent;
        changes.push("default agent".to_owned());
    }
    if let Some(models) = body.models {
        p.models = models;
        changes.push("models".to_owned());
    }
    if let Some(note) = body.note {
        p.note = Some(note);
        changes.push("note".to_owned());
    }
    if let Some(strategy) = body.account_strategy {
        p.account_strategy = strategy;
        changes.push(format!(
            "account strategy {}",
            strategy.map_or("server default", Strategy::as_str)
        ));
    }
    if let Some(pct) = body.rotate_at {
        p.rotate_at = pct;
        changes.push(match pct {
            Some(n) => format!("rotate at {n}%"),
            None => "rotate at server default".to_owned(),
        });
    }
    if changes.is_empty() {
        return Err(ApiError::BadRequest("nothing to update".to_owned()));
    }
    let p = validate_project(store, home, p, FsPath::new(&path))?;
    check_bound(store, &p)?;
    if let Some(c) = changes.iter_mut().find(|c| *c == "accounts") {
        *c = format!("accounts {}", acct::format_bindings(&p.accounts));
    }
    Ok((p, changes))
}

pub(super) async fn remove(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(name): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Json<Value>> {
    match state.store.remove_project(&name, q.unbind)? {
        RemovedProject::NotFound => Err(ApiError::NotFound("project not found")),
        RemovedProject::Bound { tokens, invites } => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "project_in_use",
            format!(
                "{tokens} token(s) and {invites} pending invite(s) are bound to {name}; \
                 pass ?unbind=true to put them back on their own workspace"
            ),
        )),
        RemovedProject::Removed { tokens, invites } => {
            state.store.audit(
                &admin.name,
                "project.remove",
                &name,
                &format!("unbound {tokens} token(s) and {invites} invite(s)"),
            )?;
            state.bus.log(format!("project removed: {name}"));
            Ok(Json(
                json!({ "ok": true, "unboundTokens": tokens, "unboundInvites": invites }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewToken;
    use crate::token::{Limits, Role, SessionMode};

    fn folder() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap();
        (dir, path)
    }

    fn raw(name: &str) -> Project {
        Project {
            name: name.into(),
            path: String::new(),
            agents: vec![AgentKind::Claude],
            accounts: AccountMap::new(),
            default_agent: None,
            models: ModelMap::new(),
            note: None,
            account_strategy: None,
            rotate_at: None,
            created_at: 0,
        }
    }

    #[test]
    fn projects_need_known_accounts_and_a_real_folder() {
        let store = Store::open_in_memory().unwrap();
        let (_d, path) = folder();
        let p = validate_project(&store, None, raw("api"), &path).unwrap();
        assert_eq!(p.path, path.to_string_lossy());
        let mut ghost = raw("api");
        ghost.accounts = acct::parse_bindings("claude=ghost").unwrap();
        let err = validate_project(&store, None, ghost, &path).unwrap_err();
        assert!(err.to_string().contains("claude:ghost"), "{err}");
        assert!(validate_project(&store, None, raw("api"), &path.join("missing")).is_err());
    }

    #[test]
    fn bindings_and_edits_never_widen_accounts() {
        let store = Store::open_in_memory().unwrap();
        for n in ["work", "home"] {
            store
                .add_account(AgentKind::Claude, n, acct::AccountKind::Login)
                .unwrap();
        }
        let (_d, path) = folder();
        let mut p = raw("api");
        p.agents = vec![AgentKind::Claude, AgentKind::Codex];
        p.accounts = acct::parse_bindings("claude=work,home").unwrap();
        let p = validate_project(&store, None, p, &path).unwrap();
        store.add_project(&p).unwrap();

        let narrow = acct::parse_bindings("claude=home").unwrap();
        let all = [AgentKind::Claude];
        assert!(check_binding(&store, Some("api"), &all, &narrow).is_ok());
        let wide = acct::parse_bindings("claude=home,default").unwrap();
        let err = check_binding(&store, Some("api"), &all, &wide).unwrap_err();
        assert!(err.to_string().contains("claude:default"), "{err}");
        assert!(check_binding(&store, Some("nope"), &all, &narrow).is_err());
        assert!(check_binding(&store, Some("api"), &[AgentKind::Gemini], &narrow).is_err());
        assert!(check_binding(&store, None, &all, &wide).unwrap().is_none());

        store
            .create_token(&NewToken {
                name: "bot".into(),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: Limits::default(),
                accounts: narrow,
                project: Some("api".into()),
            })
            .unwrap();
        // Dropping `home` from the project would leave the token outside it.
        let err = apply_update(
            &store,
            None,
            p.clone(),
            UpdateProjectBody {
                accounts: Some(acct::parse_bindings("claude=work").unwrap()),
                ..UpdateProjectBody::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("token bot"), "{err}");
        // Narrowing the agents is fine.
        let (q, changes) = apply_update(
            &store,
            None,
            p,
            UpdateProjectBody {
                agents: Some(vec![AgentKind::Claude]),
                note: Some("x".into()),
                ..UpdateProjectBody::default()
            },
        )
        .unwrap();
        assert_eq!(q.agents, [AgentKind::Claude]);
        assert_eq!(changes, ["agents", "note"]);
    }

    #[test]
    fn binding_names_are_checked_and_default_means_none() {
        assert_eq!(check_project_name(None).unwrap(), None);
        assert_eq!(check_project_name(Some("default".into())).unwrap(), None);
        assert_eq!(check_project_name(Some(" ".into())).unwrap(), None);
        assert_eq!(
            check_project_name(Some("api".into())).unwrap().as_deref(),
            Some("api")
        );
        assert!(check_project_name(Some("../x".into())).is_err());
    }
}
