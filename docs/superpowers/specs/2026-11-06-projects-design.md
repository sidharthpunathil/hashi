# Projects: design

## Intent

The owner thinks in projects: "my api repo, on my work Claude account",
"the blog, on my personal plan". Hashi should let them set that up once and
then share a project, instead of repeating folder and account choices on
every token. Projects replace the per-token `workdir` field from
2026-11-06-practical-additions-design.md section 2.

## Model

A project has:
- **name**: 1 to 32 characters from `[a-z0-9-]`. `default` is reserved.
- **path**: an absolute path to an existing directory, canonicalised and
  stored.
- **agents**: the agents it allows (default: all installed).
- **accounts**: for each agent, an ordered list of account names from the
  accounts feature. A missing agent uses `default`.
- **default agent** and an optional **model** for each agent, checked with
  `is_safe_arg`.
- **strategy**: the account-selection strategy from the usage-limits
  feature, once that exists.
- a **note**.

There is an implicit `default` project. It means today's behaviour: the
per-token workspace and the token's own account lists.

## Tokens and invites

- A token or invite may name one project.
- **Effective settings for a run.** The token's agent scope is intersected
  with the project's agents. The token's account list for an agent, when
  set, must be a subset of the project's list for that agent; otherwise the
  project's list is used. The project's model applies when the request
  names none. Token limits apply as they do today.
- **Validation.** Binding a token to a project, or editing a project, can
  never widen access. A token's account list that is not a subset of its
  project's list is rejected with a clear error.
- **Cwd.** Runs for a project-bound token start in the project path instead
  of the per-token workspace. Sessions are kept per token, agent, account
  and project.
- **Visibility.** Token holders see only the project name: `/v1/me` shows
  `project: "api"`, and the claim page says "works in the project api".
  Admins see everything.

## Commands, API, dashboard

- **CLI.**
  - `hashi project add <name> <path> [--agents ...] [--account agent=a,b]... [--default-agent ..] [--model agent=..]... [--note ..]`
  - `hashi project update <name> ...` and `hashi project list`
  - `hashi project remove <name>`: refuses while tokens are bound unless
    `--unbind` is passed, and never touches the folder.
  - `--project <name>` on `token create|update` and `share`, and
    `--clear-project`.
- **API (admin).** `GET|POST /v1/projects`, `PATCH|DELETE
  /v1/projects/{name}`, and a `project` field on tokens and invites.
- **Dashboard.** A Projects section (the parity builder decides where it
  sits): list, add, edit and remove, plus a project selector in the token
  and share forms. The parity table maps every `project` subcommand.
- **Stats.** `GET /v1/projects` includes, for each project, the requests
  and reported spend today and this month, and the number of bound tokens.

## Safety

- Revoking a token or removing a project never deletes or changes the
  project folder.
- Agent sandboxes are unchanged: Codex stays read-only, opencode keeps its
  read-only tools, and so on.
- Project-level config inside the folder may load for agents that read it
  (Gemini `.gemini/`, Qwen, Cursor). Claude Code is pinned to user settings.
  For Crush, Hashi refuses to run in a project that contains `.crushrc`,
  `crushrc`, `.crush.json` or `crush.json`, because those can run shell code
  as soon as Crush loads them.
- The security model says that a project changes where a run starts, not
  what an agent running as the owner can read, so share only folders you
  trust.
- `hashi uninstall` leaves project folders alone. Only Hashi's records of
  them go.

## Non-goals

- Copying or syncing project folders, git operations, or per-project file
  upload.
