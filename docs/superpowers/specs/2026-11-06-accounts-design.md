# Multiple accounts per agent: design

## Intent

The owner often has more than one login for an agent: a personal and a work
Claude plan, two Codex accounts, an API key on top of a subscription. They
want to choose which account each person or app they share with uses, and
when one account runs into its limit, carry on with the next instead of
failing.

## Research

Each agent CLI keeps its login inside a directory it can be pointed at, so
an account can be a directory owned by Hashi. The CLI signs in and refreshes
tokens there itself. Hashi never copies, refreshes or swaps credentials, and
the owner's normal login is never touched. This is safer than switching the
default login, which races the CLI's own token refresh.

| Agent | Account directory variable | Sign-in command (run with that variable set) | API-key variable |
|---|---|---|---|
| Claude Code | `CLAUDE_CONFIG_DIR` (on macOS the Keychain entry is tied to the exact string, so always pass the same absolute path) | `claude auth login` | `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) or `ANTHROPIC_API_KEY` |
| Codex | `CODEX_HOME` (must exist) | `codex login`, or `codex login --device-auth` | `CODEX_API_KEY` |
| Gemini CLI | `GEMINI_CLI_HOME` (`GEMINI_FORCE_ENCRYPTED_FILE_STORAGE` must be unset: it uses one global keychain entry) | `NO_BROWSER=true gemini`, then Sign in with Google | `GEMINI_API_KEY` |
| opencode | `XDG_DATA_HOME` (its SQLite database holds the credentials) | `opencode auth login --standalone` | provider variables (not supported in this design) |
| Qwen Code | `QWEN_HOME` | `qwen`, then `/auth` (accounts are API-key plans since the OAuth free tier ended) | not supported in this design (several provider-specific variables) |
| Cursor CLI | none documented for the login | not supported | `CURSOR_API_KEY` |

Agent runs today inherit the owner's environment. Variables such as
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` outrank a directory login, so a run
under a chosen account must remove them.

Limit messages to recognise (from the CLIs' sources and docs):
- Claude: "hit your session limit", "weekly limit", "Opus limit", "Credit
  balance is too low", "Request rejected (429)".
- Codex: "hit your usage limit" (the apostrophe in "You’ve" is curly), "out
  of credits", "spend cap".
- Gemini: "exhausted your daily quota", "exhausted your capacity", "quota
  exceeded".
- Qwen: "Quota exceeded for quota metric".
- Cursor: unknown, so treat a 429 or "quota" as a limit.

## Goals

### 1. Accounts

An account is `(agent, name)`. It comes in three kinds:

- **default**: implicit, always present, the owner's normal login. Nothing
  is overridden and the run behaves as it does today.
- **login**: a directory `<home>/accounts/<agent>/<name>/`, created with
  mode 0700. Each run sets the agent's account variable to its absolute path.
  Supported for Claude Code, Codex, Gemini CLI, opencode and Qwen Code.
- **key**: an API key stored in `<home>/accounts/<agent>/<name>/key`, mode
  0600, and passed in the agent's API-key variable. Supported for Claude
  Code (`--kind oauth-token` or `--kind api-key`), Codex, Gemini CLI and
  Cursor CLI.

Names are 1 to 32 characters from `[a-z0-9-]`; `default` is reserved.

### 2. Commands

- `hashi account add <agent> <name> [--key-stdin | --key-env VAR] [--kind ...] [--yes]`.
  - For a **login** account, Hashi prints the sign-in command and runs it
    with the terminal attached after the owner confirms. Then it checks the
    result: `claude auth status --json`, and the equivalent status command
    where one exists. Otherwise it confirms that the CLI wrote something into
    the directory.
  - For a **key** account, Hashi reads the key without echoing it and never
    takes it as an argument.
- `hashi account login <agent> <name>` signs an existing login account in
  again.
- `hashi account list` shows, for each account, its agent, name and kind,
  whether it is signed in where that can be checked, its state (ready,
  limited until a time, or disabled), when it was last used, and its runs
  today.
- `hashi account disable|enable <agent> <name>`.
- `hashi account remove <agent> <name>` asks first, deletes the directory
  without following symlinks, and removes the account from every token and
  invite that uses it.
- `hashi doctor` lists accounts. `hashi uninstall` treats `accounts/` as
  Hashi's own.

### 3. Choosing accounts per token

- Tokens and invites get an `accounts` map from agent to an ordered list of
  account names, for example `{"claude": ["work", "personal"]}`. An agent
  with no entry uses `default`.
- Where to set it:
  - API: `POST` and `PATCH /v1/tokens`, and `POST /v1/invites`.
  - CLI: `--account claude=work,personal`, repeatable, on `token create`,
    `token update` and `share`; `--clear-accounts`.
  - The dashboard token and share forms.
- Unknown account names are rejected.
- Token holders never see account names. `/v1/me`, MCP and the claim page
  stay as they are. Admins see the names in token listings and requests.

### 4. Failover

- A run uses the first account in the token's list that is enabled and not
  limited.
- When a run fails with that agent's limit message, the account is marked
  limited until a reset time Hashi can parse from the message, or for
  `--account-cooldown` (default 60 minutes). The request then goes back to
  the front of the queue and runs on the next available account in the list.
  This does not count as an interruption, and it happens at most once per
  account in the list.
- With no account left, the request fails with "every account for <agent>
  this token can use is at its limit until <time>".
- A run that succeeds clears that account's limited mark.

### 5. Sessions and records

- Stateful sessions are kept per token, agent and account. Conversation
  history lives inside the account's directory, so a failover starts that
  token's conversation fresh on the new account.
- Each request records the account that ran it, so admins see it in
  `GET /v1/ask/{id}` and the dashboard request detail.
- `GET /v1/accounts` (admin) returns the account list with its state and
  counts.
- The dashboard shows the account in request details and token details, and
  the agent cards show how many accounts each agent has.

### 6. Environment hygiene

- For every run on a non-default account, Hashi removes all of that agent's
  credential variables inherited from the owner before setting the
  account's own:
  - Claude: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
    `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`,
    `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`, `CLAUDE_CONFIG_DIR`.
  - Codex: `OPENAI_API_KEY`, `CODEX_API_KEY`, `CODEX_HOME`.
  - Gemini: `GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GOOGLE_GENAI_USE_GCA`,
    `GOOGLE_GENAI_USE_VERTEXAI`, `GOOGLE_APPLICATION_CREDENTIALS`,
    `GEMINI_CLI_HOME`, `GEMINI_FORCE_ENCRYPTED_FILE_STORAGE`.
  - Cursor: `CURSOR_API_KEY`.
  - Qwen: `QWEN_HOME`, `OPENAI_API_KEY`, `OPENAI_BASE_URL`,
    `BAILIAN_CODING_PLAN_API_KEY`.
  - opencode: `XDG_DATA_HOME`, `OPENCODE_DB`.
- Default-account runs are unchanged.

## Non-goals

- Reading usage windows from provider APIs (undocumented, and it would mean
  reading credentials).
- Changing the owner's default login, or exporting and importing
  credentials.
- Round-robin or quota-aware strategies. The list order is the priority.

## Anti-goals

- Hashi reading, copying or refreshing an agent's credentials.
- An API key on a command line, in logs, in `hashi report`, or in any API
  response.
- Tests that run real agent CLIs or touch real logins. Tests use fake agent
  scripts that print the CLIs' limit messages and record their environment.

## Known limit

Agent runs execute as the owner, so an agent that can read files can read
account directories and keys under `~/.hashi`, just as it can read the
owner's default login today. The security model says so.
