//! Project records: the folder, agents, accounts and defaults of each
//! project, which tokens and invites are bound to it, and its use. Only
//! these rows change here; nothing ever touches a project's folder.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{Result, Store, StoreError};
use crate::accounts as acct;
use crate::projects::{self as proj, Project};
use crate::token;
use crate::util::{day_start_utc, month_start_utc, now_ms};

/// A project's use in the current UTC day and month, and its bound tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStats {
    /// Prompts since the start of the UTC day.
    pub today_requests: u64,
    /// Prompts since the start of the UTC month.
    pub month_requests: u64,
    /// Reported spend since the start of the UTC day.
    pub today_cost_usd: f64,
    /// Reported spend since the start of the UTC month.
    pub month_cost_usd: f64,
    /// Tokens bound to it.
    pub tokens: u64,
}

/// What [`Store::remove_project`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedProject {
    /// No such project.
    NotFound,
    /// Refused: tokens or pending invites are still bound to it.
    Bound {
        /// Bound tokens.
        tokens: usize,
        /// Bound pending invites.
        invites: usize,
    },
    /// Removed, after taking it off these tokens and invites.
    Removed {
        /// Tokens put back on their own workspace.
        tokens: usize,
        /// Invites unbound.
        invites: usize,
    },
}

const PROJECT_COLUMNS: &str =
    "name, path, agents, accounts, default_agent, models, note, created_at";

fn row_to_project(r: &Row<'_>) -> rusqlite::Result<Result<Project>> {
    let agents: String = r.get(2)?;
    let accounts: String = r.get(3)?;
    let default_agent: Option<String> = r.get(4)?;
    let models: String = r.get(5)?;
    let build = || -> Result<Project> {
        Ok(Project {
            name: r.get(0)?,
            path: r.get(1)?,
            agents: token::decode_agents(&agents),
            accounts: acct::decode(&accounts),
            default_agent: default_agent
                .map(|a| {
                    a.parse()
                        .map_err(|e: crate::agent::UnknownAgent| StoreError::Corrupt(e.to_string()))
                })
                .transpose()?,
            models: proj::decode_models(&models),
            note: r.get(6)?,
            created_at: r.get(7)?,
        })
    };
    Ok(build())
}

impl Store {
    /// Records a new project. Returns `false` if the name is taken. The
    /// caller validates it first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn add_project(&self, p: &Project) -> Result<bool> {
        let n = self.conn()?.execute(
            "INSERT INTO projects (name, path, agents, accounts, default_agent, models, note,
                 created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(name) DO NOTHING",
            params![
                p.name,
                p.path,
                token::encode_agents(&p.agents),
                acct::encode(&p.accounts),
                p.default_agent.map(|a| a.as_str()),
                proj::encode_models(&p.models),
                p.note,
                p.created_at,
            ],
        )?;
        Ok(n == 1)
    }

    /// Writes a project's editable fields. Returns `false` if there is no
    /// such project.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn save_project(&self, p: &Project) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE projects SET path = ?2, agents = ?3, accounts = ?4, default_agent = ?5,
                 models = ?6, note = ?7
             WHERE name = ?1",
            params![
                p.name,
                p.path,
                token::encode_agents(&p.agents),
                acct::encode(&p.accounts),
                p.default_agent.map(|a| a.as_str()),
                proj::encode_models(&p.models),
                p.note,
            ],
        )?;
        Ok(n == 1)
    }

    /// One project.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn get_project(&self, name: &str) -> Result<Option<Project>> {
        let sql = format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE name = ?1");
        self.conn()?
            .query_row(&sql, [name], row_to_project)
            .optional()?
            .transpose()
    }

    /// Every project, by name.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {PROJECT_COLUMNS} FROM projects ORDER BY name");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_project)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|p| p))
            .collect()
    }

    /// Tokens bound to project `name`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn project_tokens(&self, name: &str) -> Result<Vec<token::Token>> {
        Ok(self
            .list_tokens()?
            .into_iter()
            .filter(|t| t.project.as_deref() == Some(name))
            .collect())
    }

    /// Pending invites bound to project `name`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn project_invites(&self, name: &str) -> Result<Vec<super::Invite>> {
        let now = now_ms();
        Ok(self
            .list_invites()?
            .into_iter()
            .filter(|i| i.project.as_deref() == Some(name) && i.status(now) == "pending")
            .collect())
    }

    /// A project's use in the UTC day and month containing `now`, and the
    /// number of tokens bound to it.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn project_stats(&self, name: &str, now: i64) -> Result<ProjectStats> {
        let conn = self.conn()?;
        let (today, month, today_cost, month_cost): (i64, i64, f64, f64) = conn.query_row(
            "SELECT
                 COUNT(CASE WHEN created_at >= ?2 THEN 1 END),
                 COUNT(*),
                 COALESCE(SUM(CASE WHEN created_at >= ?2 THEN cost_usd END), 0.0),
                 COALESCE(SUM(cost_usd), 0.0)
             FROM requests WHERE project = ?1 AND created_at >= ?3",
            params![name, day_start_utc(now), month_start_utc(now)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let tokens: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tokens WHERE project = ?1",
            [name],
            |r| r.get(0),
        )?;
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        Ok(ProjectStats {
            today_requests: n(today),
            month_requests: n(month),
            today_cost_usd: today_cost,
            month_cost_usd: month_cost,
            tokens: n(tokens),
        })
    }

    /// Deletes project `name`'s record and the sessions kept in it, in one
    /// transaction. While tokens or pending invites are bound to it, this
    /// refuses unless `unbind`, which first puts them back on their own
    /// workspace. The folder itself is never touched.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn remove_project(&self, name: &str, unbind: bool) -> Result<RemovedProject> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE name = ?1)",
            [name],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(RemovedProject::NotFound);
        }
        let tokens: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tokens WHERE project = ?1",
            [name],
            |r| r.get(0),
        )?;
        let invites: i64 = tx.query_row(
            "SELECT COUNT(*) FROM invites
             WHERE project = ?1 AND claimed_at IS NULL AND expires_at > ?2",
            params![name, now_ms()],
            |r| r.get(0),
        )?;
        let (tokens, invites) = (
            usize::try_from(tokens).unwrap_or(0),
            usize::try_from(invites).unwrap_or(0),
        );
        if (tokens > 0 || invites > 0) && !unbind {
            return Ok(RemovedProject::Bound { tokens, invites });
        }
        tx.execute(
            "UPDATE tokens SET project = NULL WHERE project = ?1",
            [name],
        )?;
        tx.execute(
            "UPDATE invites SET project = NULL WHERE project = ?1",
            [name],
        )?;
        tx.execute("DELETE FROM sessions WHERE project = ?1", [name])?;
        tx.execute("DELETE FROM projects WHERE name = ?1", [name])?;
        tx.commit()?;
        Ok(RemovedProject::Removed { tokens, invites })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountMap;
    use crate::agent::AgentKind;
    use crate::store::{NewInvite, NewRequest, NewToken};
    use crate::token::{Limits, Role, SessionMode};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: "/srv/api".into(),
            agents: vec![AgentKind::Claude, AgentKind::Codex],
            accounts: acct::parse_bindings("claude=work").unwrap(),
            default_agent: Some(AgentKind::Codex),
            models: proj::ModelMap::from([(AgentKind::Claude, "sonnet".to_owned())]),
            note: Some("the api".into()),
            created_at: 1,
        }
    }

    fn token(s: &Store, project: Option<&str>) -> crate::token::Token {
        s.create_token(&NewToken {
            name: "t".into(),
            role: Role::User,
            session_mode: SessionMode::Stateful,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: Limits::default(),
            accounts: AccountMap::new(),
            project: project.map(str::to_owned),
        })
        .unwrap()
        .0
    }

    #[test]
    fn projects_roundtrip_and_names_are_unique() {
        let s = store();
        assert!(s.add_project(&project("api")).unwrap());
        assert!(!s.add_project(&project("api")).unwrap());
        s.add_project(&project("blog")).unwrap();
        assert_eq!(s.get_project("api").unwrap().unwrap(), project("api"));
        assert!(s.get_project("nope").unwrap().is_none());
        let names: Vec<String> = s
            .list_projects()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["api", "blog"]);

        let mut p = project("api");
        p.path = "/srv/other".into();
        p.default_agent = None;
        p.models.clear();
        assert!(s.save_project(&p).unwrap());
        assert_eq!(s.get_project("api").unwrap().unwrap(), p);
        assert!(!s.save_project(&project("ghost")).unwrap());
    }

    #[test]
    fn removal_refuses_while_bound_and_unbind_clears_tokens_invites_and_sessions() {
        let s = store();
        s.add_project(&project("api")).unwrap();
        let bound = token(&s, Some("api"));
        let free = token(&s, None);
        s.create_invite(&NewInvite {
            label: "bob".into(),
            session_mode: SessionMode::Stateless,
            agents: vec![AgentKind::Claude],
            token_expires_in_days: None,
            limits: Limits::default(),
            link_hours: 24,
            accounts: AccountMap::new(),
            project: Some("api".into()),
        })
        .unwrap();
        s.set_project_session(&bound.id, AgentKind::Claude, "default", "api", "p1")
            .unwrap();
        s.set_project_session(&bound.id, AgentKind::Claude, "default", "default", "d1")
            .unwrap();
        assert_eq!(s.project_tokens("api").unwrap().len(), 1);
        assert_eq!(s.project_invites("api").unwrap().len(), 1);

        assert_eq!(
            s.remove_project("api", false).unwrap(),
            RemovedProject::Bound {
                tokens: 1,
                invites: 1
            }
        );
        assert!(s.get_project("api").unwrap().is_some());
        assert_eq!(
            s.remove_project("api", true).unwrap(),
            RemovedProject::Removed {
                tokens: 1,
                invites: 1
            }
        );
        assert!(s.get_project("api").unwrap().is_none());
        assert_eq!(s.get_token(&bound.id).unwrap().unwrap().project, None);
        assert_eq!(s.get_token(&free.id).unwrap().unwrap().project, None);
        assert_eq!(s.list_invites().unwrap()[0].project, None);
        assert!(
            s.project_session(&bound.id, AgentKind::Claude, "default", "api")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            s.session(&bound.id, AgentKind::Claude).unwrap().as_deref(),
            Some("d1")
        );
        assert_eq!(
            s.remove_project("api", true).unwrap(),
            RemovedProject::NotFound
        );
        // Nothing bound: removed without --unbind.
        s.add_project(&project("blog")).unwrap();
        assert_eq!(
            s.remove_project("blog", false).unwrap(),
            RemovedProject::Removed {
                tokens: 0,
                invites: 0
            }
        );
    }

    #[test]
    fn requests_carry_their_project_and_count_in_its_stats() {
        let s = store();
        s.add_project(&project("api")).unwrap();
        let bound = token(&s, Some("api"));
        let free = token(&s, None);
        let mut ids = Vec::new();
        for t in [&bound, &bound, &free] {
            let id = uuid::Uuid::new_v4().to_string();
            s.insert_request(NewRequest {
                id: &id,
                token: t,
                agent: AgentKind::Claude,
                model: None,
                message: "hi",
            })
            .unwrap();
            ids.push(id);
        }
        s.complete_request(
            &ids[0],
            "ok",
            Some(crate::agent::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: Some(0.25),
            }),
        )
        .unwrap();
        assert_eq!(
            s.get_request(&ids[0]).unwrap().unwrap().project.as_deref(),
            Some("api")
        );
        assert_eq!(s.get_request(&ids[2]).unwrap().unwrap().project, None);
        let st = s.project_stats("api", now_ms()).unwrap();
        assert_eq!(
            st,
            ProjectStats {
                today_requests: 2,
                month_requests: 2,
                today_cost_usd: 0.25,
                month_cost_usd: 0.25,
                tokens: 1,
            }
        );
        assert_eq!(
            s.recent_requests_brief(10)
                .unwrap()
                .iter()
                .filter(|r| r.project.as_deref() == Some("api"))
                .count(),
            2
        );
        assert_eq!(
            s.project_stats("blog", now_ms()).unwrap(),
            ProjectStats::default()
        );
    }

    #[test]
    fn sessions_are_kept_per_project() {
        let s = store();
        let t = token(&s, None);
        s.set_project_session(&t.id, AgentKind::Claude, "work", "api", "a")
            .unwrap();
        s.set_project_session(&t.id, AgentKind::Claude, "work", "blog", "b")
            .unwrap();
        s.set_account_session(&t.id, AgentKind::Claude, "work", "w")
            .unwrap();
        let get = |p: &str| {
            s.project_session(&t.id, AgentKind::Claude, "work", p)
                .unwrap()
        };
        assert_eq!(get("api").as_deref(), Some("a"));
        assert_eq!(get("blog").as_deref(), Some("b"));
        assert_eq!(get("default").as_deref(), Some("w"));
        s.clear_session(&t.id, AgentKind::Claude).unwrap();
        assert!(get("api").is_none() && get("blog").is_none() && get("default").is_none());
    }
}
