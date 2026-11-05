# Configuration

Pass flags to `hashi serve` (for example `hashi serve --port 9000 --tunnel`). Running bare `hashi` starts the server with defaults and environment variables. Every flag also reads an environment variable.

| Flag | Env | Default |
|---|---|---|
| `--home` | `HASHI_HOME` | `~/.hashi` |
| `--bind` | `HASHI_BIND` | `127.0.0.1`, or the one saved from the dashboard in `~/.hashi/settings.json` (see [settings.json](#settingsjson)) |
| `--port` | `HASHI_PORT` | `7878`, or the one saved from the dashboard in `~/.hashi/settings.json` |
| `--tunnel` | `HASHI_TUNNEL` | off |
| `--tailscale serve\|funnel` | `HASHI_TAILSCALE` | off. Go public through Tailscale Serve (your tailnet) or Funnel (the internet); see [remote access](remote-access.md#tailscale-serve-and-funnel). Not with `--tunnel` |
| `--tailscale-port` | `HASHI_TAILSCALE_PORT` | `443`. HTTPS port for `--tailscale` (Funnel: 443, 8443 or 10000) |
| `--tailscale-bin` | `HASHI_TAILSCALE_BIN` | found on `PATH`. Also read by `hashi doctor` |
| | `HASHI_TAILSCALE_CONTROL_HOST` | none. A self-hosted control server's host (such as `headscale.example.com`): its approval links are shown as well as `https://login.tailscale.com/...` ones. Any other link the CLI prints is not shown |
| `--cloudflared-bin` | `HASHI_CLOUDFLARED_BIN` | found on `PATH`. Also read by `hashi doctor` |
| `--tunnel-token` | `HASHI_TUNNEL_TOKEN` | none. Runs a named tunnel; needs `--public-url`. Prefer the variable: the flag shows in the process list, and Hashi warns |
| `--trust-proxy` | `HASHI_TRUST_PROXY` | off. Read client addresses from `X-Forwarded-For` on loopback connections |
| `--cors-origin` | `HASHI_CORS_ORIGINS` (comma separated) | none. Origins allowed to call Hashi from a browser |
| `--allowed-host` | `HASHI_ALLOWED_HOSTS` (comma separated) | none. Extra host names accepted over plain HTTP |
| `--public-url` | `HASHI_PUBLIC_URL` | none. Base for invite links, for example `https://hashi.example.com`. Without it, `hashi share` uses the running server's public URL from `~/.hashi/public-url` when there is one |
| `--tls-cert`, `--tls-key` | `HASHI_TLS_CERT`, `HASHI_TLS_KEY` | none. Serve HTTPS with these PEM files |
| `--name` | `HASHI_NAME` | `Hashi`. Shown on invite and My access pages |
| `--headless` | | off (on when stdout is not a terminal) |
| `--no-update-check` | `HASHI_NO_UPDATE_CHECK` | off. Set to skip the daily check for a newer release (also skipped by `hashi doctor`) |
| | `HASHI_REDUCED_MOTION` | off. Set to `1` to freeze dashboard animation |
| `--default-agent` | `HASHI_DEFAULT_AGENT` | `claude` |
| `--concurrency` | `HASHI_CONCURRENCY` | `1` run per agent |
| `--max-running` | `HASHI_MAX_RUNNING` | total memory in GiB / 8, between 1 and the CPU count. Runs at once across all agents |
| `--memory-reserve` | `HASHI_MEMORY_RESERVE` | the larger of `2G` and `20%`. Kept free for you; new runs wait while less would be left (`0` disables) |
| `--run-memory-limit` | `HASHI_RUN_MEMORY_LIMIT` | the larger of `2G` and `40%`. One run's whole process tree is stopped above this |
| `--disk-floor` | `HASHI_DISK_FLOOR` | `1G`. New runs wait while the volume holding the Hashi home has less free (`0` disables) |
| `--nice` | `HASHI_NICE` | `10`. Lower priority for agent runs, `0` to `19` (`0` disables; on Windows any value above `0` means below-normal priority) |
| `--timeout` | `HASHI_TIMEOUT` | `300` seconds |
| `--rate-limit` | `HASHI_RATE_LIMIT` | `60` per minute per token (`0` disables) |
| `--max-connections` | `HASHI_MAX_CONNECTIONS` | `512` open connections; more wait until one closes |
| `--queue-limit` | `HASHI_QUEUE_LIMIT` | `100` pending requests |
| `--retention-days` | `HASHI_RETENTION_DAYS` | `14` |
| `--account-cooldown` | `HASHI_ACCOUNT_COOLDOWN` | `60` minutes. How long an account that hit its usage limit is skipped when the agent's message gives no reset time (1 to 10080) |
| `--account-strategy` | `HASHI_ACCOUNT_STRATEGY` | `in-order`. How runs choose among a token's accounts when its project sets nothing: `in-order`, `most-left` or `soonest-reset`. See [usage and strategies](#usage-and-strategies) |
| `--rotate-at` | `HASHI_ROTATE_AT` | `90` percent used, `50` to `99`. Runs move off an account at this point, when its project sets nothing |
| `--no-usage-poll` | `HASHI_USAGE_POLL=0` | off. Stop reading Claude Code and Codex quota in the background. Runs and their limit errors are still recorded |
| `--claude-bin` | `HASHI_CLAUDE_BIN` | found on `PATH` |
| `--codex-bin` | `HASHI_CODEX_BIN` | found on `PATH` |
| `--gemini-bin`, `--opencode-bin`, `--qwen-bin`, `--cursor-bin` | `HASHI_GEMINI_BIN`, `HASHI_OPENCODE_BIN`, `HASHI_QWEN_BIN`, `HASHI_CURSOR_BIN` | found on `PATH` |
| `--enable-crush` | `HASHI_ENABLE_CRUSH` | off. Offer Crush, which is opt-in: while off it is not probed, not listed and a request for it answers "Crush is not installed or not enabled". See [security model](security-model.md#crush) |
| `--crush-bin` | `HASHI_CRUSH_BIN` | found on `PATH` |
| `--crush-allow-newer` | `HASHI_CRUSH_ALLOW_NEWER` | off. Run a Crush release newer than 0.100.x, the newest whose tool list Hashi has checked. Without it such a release is refused |
| `--max-turns` | `HASHI_MAX_TURNS` | `10` (Claude) |
| `--claude-permission-mode` | `HASHI_CLAUDE_PERMISSION_MODE` | Claude's default |
| `--codex-sandbox` | `HASHI_CODEX_SANDBOX` | `read-only` |

Sizes take binary units or a share of the total: `512M`, `4G`, `1.5G`, `25%`.

Logs always go to `~/.hashi/logs/hashi.log` with secrets redacted (rotated past 5 MB, three old files kept), and with `--headless` also to stderr. Set `HASHI_LOG=hashi=debug` for more detail.

## Commands

```bash
hashi                                      # start the bridge with the dashboard
hashi serve --headless                     # start it with plain log lines instead
hashi doctor                               # agents, cloudflared, tailscale and its sign-in
hashi install cloudflared                  # show the official install command, run it on yes
hashi install tailscale --yes              # run it without asking
hashi install crush                        # Crush, from its README; then start with --enable-crush
hashi token create ci --agents codex --expires-in-days 30
hashi token list                           # status and today's use per token
hashi token update hashi_1a2b3c4d --pause  # or --resume, --daily-requests 100, --clear-limits
hashi token revoke hashi_1a2b3c4d
hashi share alice --agents claude          # one-time invite link (see Invites and limits)
hashi account add claude work              # another login for an agent (see Accounts)
hashi project add api ~/code/api           # a folder to share (see Projects)
hashi share bob --project api              # an invite that works in that folder
hashi invite list                          # pending, claimed and expired invites
hashi requests open --hours 48             # let people ask for access (see Access requests)
hashi requests list                        # pending requests; --all for decided ones too
hashi requests accept <id> --daily-requests 50   # or: hashi requests decline <id> --reason "..."
hashi request '<ask-access link>' --name Sam      # on a friend's machine: ask someone's Hashi
hashi audit                                # who changed what
hashi ask "What does this repo do?" -a claude   # one prompt, no server
hashi report                               # redacted diagnostics for a bug report
hashi update                               # install the latest release (--check, --version v0.3.0, --allow-downgrade, --yes)
hashi uninstall                            # remove the binary and ~/.hashi (--dry-run, --keep-data, --yes)
```

`hashi install` prints the vendor's documented command for your platform (Homebrew on macOS, Cloudflare's package repository or Tailscale's install script on Linux) and runs it with your terminal attached, so `sudo` can prompt. Where the vendor documents no command for your system or its package manager is not on `PATH` (Windows, for both helpers), it prints the download page instead. Without a terminal it refuses unless you pass `--yes`. The commands and their sources are listed in [remote access](remote-access.md#installing-the-helpers).

The update check sends one request to api.github.com a day, carrying only the Hashi version in its User-Agent (such as `hashi/0.3.0`), and caches the answer in `~/.hashi/update-check.json`. `hashi update` downloads the archive for your platform from the GitHub release, verifies its SHA-256 checksum, checks that the new binary reports the expected version and swaps it in; restart Hashi afterwards. It refuses a binary installed by Cargo (update that with `cargo install --locked --git https://github.com/sidharthpunathil/hashi --force`) or one in a directory you cannot write to, and asks before replacing unless you pass `--yes`. Without a terminal to ask on, it refuses unless you pass `--yes`.

- `--version v0.3.0` installs that release. One older than the running Hashi is refused unless you also pass `--allow-downgrade`, which prints a warning first: this build keeps the database at its own schema version, and an older release may refuse to start on it or misread it. Back up `~/.hashi` before a downgrade.
- Downloads are HTTPS only, and only a regular file in the archive is installed (see [supply chain](security-model.md#supply-chain)).

### Invites and limits

```bash
hashi share alice --daily-requests 50 --daily-budget 5 --expires-in-days 30
hashi share bob --agents claude,codex --stateful --link-hours 168
hashi invite list                           # pending, claimed and expired invites
hashi invite cancel <id>
hashi token create ci --agents codex --max-pending 2 --monthly-requests 500
hashi token update hashi_1a2b3c4d --pause   # --resume, --expires-in-days 0, --clear-limits, --note ..
hashi token revoke hashi_1a2b3c4d
```

`hashi share <label>` prints a one-time link, `<base>/claim/hashi_inv_<code>`. The base is `--public-url` (or `HASHI_PUBLIC_URL`), else the running server's public URL from `~/.hashi/public-url`, else this machine on `--port` (from `settings.json`, else `7878`), which only works locally. The link works once and expires after `--link-hours` (`1`, `24` or `168`; default `24`). `--expires-in-days` counts from the claim. Every flag of `hashi token create` that shapes access works on `share` too: `--agents`, `--stateful`, `--account`, `--project`, `--share-usage` and the limit flags. At most 100 invites can be pending.

The claim page shows who is sharing, which agents the person gets, their limits, the project's name if any, and a notice that you can see their prompts and answers. Claiming gives them a personal `user` token once, with ready-made curl, Python, TypeScript and MCP snippets, Add to Cursor and Add to VS Code buttons and a `codex mcp add` command. It also offers "Set up from a terminal": one `sh` or PowerShell command that installs Hashi and runs `hashi connect` with the link (see [installer options](#installer-options)).

**Limits** apply to any `user` token, from `share`, `token create`, `token update` or the API:

| Flag | Limit |
|---|---|
| `--daily-requests`, `--monthly-requests` | Prompts per UTC day or month |
| `--daily-budget`, `--monthly-budget` | Reported spend in USD per UTC day or month. Only Claude Code, opencode and Crush report cost, so budgets bound only those |
| `--max-pending` | Prompts queued or running at once, 1 to 100. A `user` token created without limits gets 3 |

Over a cap, a prompt answers `429 limit_reached` with the limit and the time it resets. `hashi token update --clear-limits` removes every limit before applying the limit flags given with it.

**Control.** Pause, resume, extend, edit or revoke from the dashboard (Tokens view: `p`, `e`, `d`), `hashi token update` and `hashi token revoke`, or `PATCH` and `DELETE /v1/tokens/{id}`. Pausing holds queued prompts, expiry fails them, and revoking stops running prompts at once and deletes the token's workspace. Every change is recorded in `hashi audit`.

**Self-service.** `/me` is a page where a token holder pastes their token to see its status, limits and today's use, and ask for more (see [access requests](#access-requests)). `GET /v1/me` returns the same. Hashi is also an MCP server at `/mcp`, so a token holder can add your agents as tools in their own Claude Code, Cursor or Codex.

### Accounts

An account is another login, or an API key, for one agent. Tokens and invites choose which accounts they use per agent, in order; an agent they do not list uses `default`, your own login, exactly as before.

```bash
hashi account add <agent> <name>                     # a login account: prints the sign-in command, runs it after you confirm
hashi account add <agent> <name> --key-stdin         # an API key, typed or pasted without echo (or piped in)
hashi account add <agent> <name> --key-env VAR       # an API key from an environment variable
hashi account add claude bot --kind oauth-token --key-stdin   # a token from `claude setup-token`
hashi account login <agent> <name>                   # sign a login account in again
hashi account list                                   # kind, signed in, state, last use, runs today, tokens
hashi account disable <agent> <name>                 # skip it until `hashi account enable`
hashi account clear-limit <agent> <name>             # take runs again now, before the reset it was marked limited until (dashboard: c)
hashi account remove <agent> <name>                  # asks first (--yes), deletes it and takes it off every token and invite

hashi token create ci --account claude=work,personal --account codex=ci
hashi token update hashi_1a2b3c4d --account claude=personal   # replaces that agent's list
hashi token update hashi_1a2b3c4d --clear-accounts            # every agent back on default
hashi share alice --account claude=work
```

Names are 1 to 32 characters from `a-z`, `0-9` and `-`; `default` is reserved. `--account` is repeatable, takes `<agent>=<name>[,<name>...]` and refuses names that do not exist. In the dashboard, the new token and share forms have an accounts field that takes the same form (`claude=work,personal codex=ci`).

| Agent | Login account (directory variable, sign-in command) | Key account (variable) |
|---|---|---|
| Claude Code | `CLAUDE_CONFIG_DIR`, `claude auth login` | `--kind api-key`: `ANTHROPIC_API_KEY`; `--kind oauth-token`: `CLAUDE_CODE_OAUTH_TOKEN` |
| Codex | `CODEX_HOME`, `codex login` (or `codex login --device-auth`) | `CODEX_API_KEY` |
| Gemini CLI | `GEMINI_CLI_HOME`, `NO_BROWSER=true gemini`, then Sign in with Google | `GEMINI_API_KEY` |
| opencode | `XDG_DATA_HOME`, `opencode auth login --standalone` | not supported |
| Qwen Code | `QWEN_HOME`, `qwen`, then `/auth` | not supported |
| Cursor CLI | not supported | `CURSOR_API_KEY` |
| Crush | `HOME` with `CRUSH_GLOBAL_CONFIG`, `CRUSH_GLOBAL_DATA`, `CRUSH_CACHE_DIR` and `CRUSH_SKILLS_DIR` under it, `crush login` (Charm Hyper; `crush login copilot`, `openai` or `grok` for the others) | not supported |

Crush's `default` account is not your own Crush login: it is `~/.hashi/accounts/crush/default/`, so your `~/.config/crush` (with its MCP servers and hooks) never loads in a Hashi run. Sign it in with `hashi account login crush default`, or leave your provider variables (such as `ANTHROPIC_API_KEY`) in Hashi's environment, which the default account still sees. Other Crush accounts see none of the provider variables in Crush's README, and no Crush run sees an inherited `CRUSH_*` or `XDG_*` directory variable.

A login account is the directory `~/.hashi/accounts/<agent>/<name>/` (mode `0700`). `hashi account add` prints the sign-in command with that variable set and, on a terminal, runs it with the terminal attached once you confirm (`--yes` skips the question); without a terminal it only prints it. It then checks the result with `claude auth status --json` for Claude Code, and for the other CLIs that the CLI wrote something into the directory. Every run on the account sets the variable to the same absolute path, which matters to Claude Code on macOS, whose Keychain entry is tied to that exact string. Hashi never reads, copies or refreshes what the CLI keeps there.

A key account keeps its key in `~/.hashi/accounts/<agent>/<name>/key` (mode `0600`) and passes it in the variable above. Its runs also point the login directory variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`) at that same directory, so your own `~/.claude`, `~/.codex` or `~/.gemini` settings never decide which credentials the run uses. The key is never accepted as an argument and never printed, logged or returned by the API.

**Failover.** A run uses the first account in the token's list that is enabled and not limited. When it fails with that agent's usage limit message, the account is marked limited until the reset time in the message, or for `--account-cooldown`, and the prompt goes back to the front of the queue to run on the next account. This is not counted as an interruption, and each account is tried at most once per prompt. With none left, the prompt fails with "every account for <agent> this token can use is at its limit until <time>". A successful run clears the account's limited mark. Messages recognised:

- Claude Code: the session, weekly and Opus limits, "Credit balance is too low" and "Request rejected (429)".
- Codex: "hit your usage limit", "out of credits" and "spend cap". The reset comes from "Try again at 3:45 PM." or "Try again at Nov 7th, 2026 3:45 PM.", read in this machine's time zone; "Try again later." uses the cooldown.
- Gemini CLI: "exhausted your daily quota", "exhausted your capacity", "quota exceeded", `QUOTA_EXHAUSTED`, `RATE_LIMIT_EXCEEDED` and "Please retry in Ns". N is the reset when it is over 300 seconds; a shorter wait uses the cooldown.
- Qwen Code: an exhausted token plan ("The quota will reset at MM-DD HH:MM:SS UTC.", read in whichever year puts it nearest now; one up to a day ago has already reset), "free allocated quota exceeded", `insufficient_quota` and "Qwen OAuth free tier has been discontinued".
- opencode: its typed limit errors (`provider.rate-limit`, `provider.quota`, `provider.go-limit`, `provider.budget-limit`, `provider.model-budget-limit`, `provider.free-limit`, `provider.chatgpt-limit`), with `retryAfterMs` as the reset, and "usage limit reached".
- Crush: Crush retries provider errors itself and then reports the provider's own message, so only generic wording is matched: "rate limit" (also `rate-limit`, `rate_limit`), "too many requests", "quota", "usage limit", `resource_exhausted` and a bare 429. Crush reports no quota of its own.
- Cursor CLI: "usage limit", "rate limit", `resource_exhausted`, "quota", its limit codes (`FREE_USER_USAGE_LIMIT`, `PRO_USER_USAGE_LIMIT`, `FREE_USER_RATE_LIMIT_EXCEEDED`, `PRO_USER_RATE_LIMIT_EXCEEDED`, `RATE_LIMITED`, `GENERIC_RATE_LIMIT_EXCEEDED`) and a bare 429. "monthly cycle ends on M/D/YYYY" gives the reset date.

Limits are tracked only for tokens that list accounts for the agent.

**Sessions.** A stateful token keeps one conversation per agent and account, because the conversation lives in the account's directory. Failing over starts it fresh on the new account; `DELETE /v1/session/{agent}` clears it on every account.

**Seeing it.** Admins see the account that ran each request in `GET /v1/ask/{id}` and the dashboard's request detail, each token's accounts at the foot of the tokens view, and the number of accounts on each agent card. `GET /v1/accounts` returns the list with its state and counts.

### Usage and strategies

```bash
hashi account usage                    # a bar per window with its percent and reset, the plan, source and age, the state
hashi account usage --agent codex      # one agent
hashi account usage --refresh          # read Claude Code and Codex quota now first
hashi account usage --json             # each account as GET /v1/accounts shows it
hashi account list                     # the USAGE column: "62% left", "8% left (warning)", "limited until ..." or "unknown"
```

**Where the figures come from.** Each window an account has (Claude Code's 5 hours, week, week for Opus and Sonnet, and the per-model weekly ones; Codex's primary and secondary windows) is kept with its used percent, reset time and source:

- `poll`: a read of the plan's live quota. While `hashi serve` runs, each Claude Code and Codex account that can take runs (the default account, and every enabled login account bound to a token or project) is read every five minutes plus up to a minute of jitter, and again about a minute after each run on it, never more than once a minute. Claude Code is read from its OAuth usage endpoint with the account's own token (see the [security model](security-model.md#usage-limits)); Codex through a short-lived `codex app-server` started with the account's `CODEX_HOME`, which must report that same directory back. A Codex read is skipped while a run on the account is active, so two token refreshes never race. A 429 or 403 waits for `retry-after` (five minutes without one, an hour at most), and a rejected sign-in waits an hour.
- `run`: Claude Code runs use `--output-format stream-json --verbose`, and each `rate_limit_event` updates the account. A `rejected` limit, or an assistant message with `error: "rate_limit"`, marks the account limited until the event's reset time, which is preferred over reading the message text.
- `error`: a run that failed with the agent's limit message counts as fully used until its reset.

Accounts without live figures (API keys, Gemini CLI, opencode, Qwen Code, Cursor CLI and Crush) show `unknown`, with their runs, tokens and reported cost today. A Claude Code sign-in whose token has expired shows "unknown, refreshes on its next run": Hashi never refreshes it, and Claude Code renews it itself when it next runs.

**Summary.** The *headroom* is 100 minus the highest used percent across windows, the *binding window* is that window, and the *state* is `ok`, `warning` (at or above the rotate threshold), `limited` (until a time) or `unknown`.

**Strategies.** Set per project with `--account-strategy`, or for the server with `hashi serve --account-strategy`:

- `in-order` (the default): the first available account in the list, failing over on limits.
- `most-left`: the available account with the most headroom. Unknown headroom ranks after every known account under the threshold and ahead of the ones over it. List order breaks ties.
- `soonest-reset`: among accounts under the threshold, the one whose weekly window resets soonest, so quota that would expire unused is spent first. Without any known weekly reset it works as `most-left`.

For all three, an account at or above the rotate threshold (`--rotate-at`, 90% by default) is skipped while any account is under it; when every account is over, the run goes to the one with the most headroom rather than waiting; limited and disabled accounts are never picked.

```bash
hashi project add api ~/code/api --account claude=work,personal --account-strategy most-left --rotate-at 80
hashi project update api --account-strategy soonest-reset
hashi project update api --clear-account-strategy --clear-rotate-at   # back to the server's defaults
```

**Sessions.** A stateful conversation stays on its account until that account reaches the threshold or its limit; only then does the next message start a fresh conversation on the newly chosen account. A new conversation always starts on the strategy's choice. `GET /v1/tokens` lists, for each token, which account each of its conversations is on.

**Dashboard.** Agent cards show the best account's headroom, and the log and a toast say when an account crosses the threshold, is limited or recovers. `hashi serve --headless` prints the same lines.

### Projects

A project is a folder on this machine with its own agents, accounts and defaults. Bind tokens and invites to it instead of choosing those on each one.

| Command | What it does |
|---|---|
| `hashi project add <name> <path> [--agents claude,codex] [--account claude=work,personal]... [--default-agent claude] [--model claude=sonnet]... [--note ..]` | Add a project. `<path>` must be an existing folder, outside the Hashi home; symlinks are resolved. Without `--agents`, the installed agents |
| `hashi project update <name> [--path ..] [--agents ..] [--account ..]... [--clear-accounts] [--default-agent ..] [--clear-default-agent] [--model ..]... [--clear-models] [--note ..] [--account-strategy ..] [--clear-account-strategy] [--rotate-at ..] [--clear-rotate-at]` | Change a project. Refused if a bound token or pending invite would be left with accounts outside it |
| `--account-strategy in-order\|most-left\|soonest-reset`, `--rotate-at <50-99>` | On `project add` and `project update`: how the project's runs choose accounts and when they move off one. Default: the server's. See [usage and strategies](#usage-and-strategies) |
| `hashi project list` | Projects with their bound tokens, prompts today and this month, spend this month, agents, accounts, models, strategy and folder |
| `hashi project remove <name> [--unbind]` | Remove a project. Refused while tokens or pending invites are bound, unless `--unbind` puts them back on their own workspace; prompts still queued under the project then fail with `project removed`. The folder is never touched |
| `hashi token create <name> --project <name>` | Bind a new token. Also on `hashi share` |
| `hashi token update <id> --project <name>` or `--clear-project` | Bind a token, or put it back on its own workspace |

A bound token's run starts in the project folder. It may use only the agents both the token and the project allow. Its account list for an agent, when it has one, must be a subset of the project's list for that agent (`default` when the project has none), or the command is refused; without a list it uses the project's. When a prompt names no model, the project's model for that agent is used. Limits work as before. Stateful conversations are kept per token, agent, account and project. The name `default` is reserved for no project. The dashboard's token and share forms have a project selector, and the request and token details show the project.

### Connecting to someone else's Hashi

A person you invite does not run a server. With Hashi installed, these commands redeem the invite link and talk to your bridge:

```bash
hashi connect 'https://host/claim/hashi_inv_...'   # redeem the link and save the token
hashi connect '<link>' --name work                 # save it under your own name
hashi connect '<link>' --setup claude              # also run `claude mcp add` for it
hashi remote list                                  # saved bridges: names and URLs, never tokens
hashi remote remove work                           # forget one (the owner can still revoke the token)
hashi ask --remote "What does this repo do?"       # one saved bridge: the name can be left out
hashi ask --remote work "Explain main.rs" -a codex
```

- `hashi connect` accepts only `https://` links, or `http://` for `localhost` and loopback addresses, with the path `/claim/hashi_inv_<code>`. It saves `{name, baseUrl, token}` in `~/.hashi/remotes.json`, readable only by you, and prints the `claude mcp add` and `codex mcp add` commands, a first `hashi ask --remote` and your `/me` link. The name defaults to the server's name in lowercase with dashes.
- It never runs another program unless you pass `--setup claude`, which runs `claude mcp add --transport http <name> <base>/mcp --header "Authorization: Bearer <token>"` (set `HASHI_CLAUDE_BIN` to use a different executable).
- `hashi ask --remote` sends the prompt, waits up to 60 seconds at a time until it completes or fails, and prints the answer with control characters removed. A failure prints the server's error and exits non-zero. Without `-a`, the server's default agent answers.

### Access requests

People can ask you for access instead of waiting for an invite. Nobody can until you open the link, and nothing is granted until you accept.

```bash
hashi requests open                                  # a link for a week (--hours 1 to 720, default 168)
hashi requests open --project api --note "For the API team"
hashi requests close                                 # opening a new link also closes the old one
hashi requests list                                  # pending requests, with what each asks for
hashi requests list --all                            # decided and expired ones too
hashi requests accept 1a2b3c4d                       # with what they asked for
hashi requests accept 1a2b3c4d --agents claude --daily-requests 20 --expires-in-days 30
hashi requests decline 1a2b3c4d --reason "Only the team for now"
```

- **The link.** `hashi requests open` prints `<base>/ask-access/hashi_ask_<code>` once, using the same base URL as `hashi share` (`--public-url`, `HASHI_PUBLIC_URL`, or the running server's public URL). The code has 128 random bits and only its hash is stored. With `--project`, requests are for that project and people choose among its agents. The page shows your bridge's name and your note, and asks for a name (1 to 60 characters), a message (up to 500) and the agents wanted. It never shows other people's requests or any account names.
- **New people.** Accepting creates a single-use invite exactly as `hashi share` would, filled in with their name, the agents they asked for (within the link's project) and the project. Every `hashi share` flag works on `accept` and overrides what was filled in: `--label`, `--agents`, `--stateful`, `--expires-in-days`, `--link-hours` (default 168), `--account`, `--project` and the limit flags. The invite's code is derived from the requester's pickup secret and never shown to you, so their page, or `hashi request status`, collects it. If they never do, it expires like any invite.
- **More.** A token holder asks from the "Ask for more" form on `/me`, `POST /v1/me/requests` or the MCP tool `request_more`: higher daily or monthly prompts or budget, more agents, a later expiry, or a message. `hashi requests list` shows their current limits next to what accepting would give. `accept` applies the asked values, with your flags replacing any of them (`--expires-in-days 0` removes the expiry); the result is saved as `hashi token update` would, and the token's project still bounds its agents.
- **Limits.** Submissions count against the per-minute rate limit (`--rate-limit`) per client address, and failed lookups of codes and pickup secrets are throttled like invite claims. A link holds at most 50 pending requests and the server 200. A token holds at most 3 open requests and opens at most one an hour. A request nobody decides expires after 7 days, and decided or expired requests are deleted 30 days later by the hourly maintenance.
- **The dashboard** shows a toast when a request arrives and the number waiting as a badge on the Commands tab (`5 Commands ·2`). The Commands row "Access requests" lists them and accepts, declines, opens and closes the link (see [What each popup does](#what-each-popup-does)). Every link change, request, acceptance and decline is in `hashi audit`, the dashboard's as `dashboard`.

On a friend's machine, `hashi request` asks from a terminal:

```bash
hashi request 'https://host/ask-access/hashi_ask_...' --name Sam --message "For the docs" --agents claude
hashi request status            # checks each pending request; accepted ones are connected
```

It accepts the same links as `hashi connect` (`https://`, or `http://` for loopback), keeps each pickup secret in `~/.hashi/pending-requests.json` (readable only by you) and, once a request is accepted, redeems the invite and saves the remote exactly as `hashi connect` does (`--remote-name` picks its name). Declined, expired and collected requests are dropped from the file. `--name` defaults to your login name.

### Remote accounts

A bridge you connected to can also be one of your own accounts, so the access a friend shares joins your account lists, projects, failover and strategies:

```bash
hashi account add claude bob-claude --remote bob                     # Claude Code on the remote saved as "bob"
hashi account add codex bob-codex --remote bob --remote-agent codex  # --remote-agent: the agent on their bridge (default: the same)
hashi token update hashi_1a2b3c4d --account claude=work,bob-claude
hashi account list                                                   # KIND shows "remote via bob"
hashi account usage                                                  # "via bob" or "via bob, shared", with source remote
```

- `--remote` names a remote from `hashi remote list`. Adding the account checks the bridge's `/v1/status`: one that reports this Hashi's own instance id is refused, and an unreachable one is added with a warning (runs check again, see below). It prints a reminder that the friend running that bridge sees every prompt sent through the account and every answer, as their claim page told you. The remote's token stays in `~/.hashi/remotes.json`; the database keeps only the remote's name and the instance id the bridge reported. `hashi remote remove` (and removing a remote in the dashboard) switches off every account that used it, with the reason in `hashi account list`, so a new remote saved later under the same name never receives their prompts. Each forward first checks that the bridge still reports the instance id recorded when the account was added; a different one switches the account off, and the prompt runs on the next account. To point an account at a new bridge, remove it and add it again.
- A run sends the prompt to the remote's `POST /v1/ask` with the remote's token and long-polls it, at most 30 seconds at a time, until it finishes. The run timeout (`--timeout`) applies, and a timeout or a cancel (`DELETE /v1/ask/{id}`, revoking the token) cancels the request on the remote bridge too, best effort. The answer, its tokens and its cost come from the remote, and the cost counts against your token's budget like a local run's.
- What the remote says decides what happens to the account:

| The remote answers | The account | The prompt |
|---|---|---|
| `limit_reached`, `too_many_pending`, `rate_limited`, or every account behind the token is at its limit | limited until `resetAt` (or `--account-cooldown`) | runs on the next account |
| `token_paused`, `unauthorized` (revoked or expired), `forbidden`, or the bridge is this Hashi | disabled, with the reason in `hashi account list` and `disabledReason` in `GET /v1/accounts` | runs on the next account |
| no answer (a network error, a `5xx`) | unchanged | runs on the next account |
| the run failed there | unchanged | fails with the remote's error |

  `hashi account enable` switches a disabled remote account back on and clears its reason.
- **Sessions.** A stateful conversation on a remote account is the remote's: the remote token's own session mode decides whether it continues there. Hashi does not carry a conversation across accounts.
- **Usage.** While the server runs, each enabled remote account bound to a token or project is read on the usage cadence (every five minutes with jitter, and about a minute after each run) with `GET /v1/me` on the remote. When the friend shares usage (`--share-usage` on their side), the account gets one `via <remote>` window with the headroom left on their accounts and its reset, so `most-left`, `soonest-reset` and `--rotate-at` treat it like a local account. The remote token's own caps (`dailyRequests`, `monthlyRequests`, `dailyBudgetUsd`, `monthlyBudgetUsd`) are kept as windows too, and when one runs out the account counts as limited until it resets. `POST /v1/accounts/{agent}/{name}/refresh` reads one now.
- **Loops.** A forwarded prompt carries `X-Hashi-Hops` (one more per bridge) and `X-Hashi-From` (this Hashi's instance id). A bridge refuses a prompt that has already been forwarded through more than 2 bridges, or one that it sent itself, and never forwards a prompt that came through 2 already.
- People you share with never see that an account is remote: their answers and errors name no bridge.

**Sharing usage with the people you share with.** `--share-usage` on `hashi token create` and `hashi share` (or `hashi token update <id> --share-usage true|false`, `shareUsage` in the API) lets that token see, in `/v1/me` and on its `/me` page, how much quota is left for each of its agents: the headroom of the best account serving it, the next reset and the state. Account names, the number of accounts and the windows are never included. The claim page says "The bridge owner lets you see how much quota is left". Off by default.

## Dashboard keys

Run `hashi` in a terminal and the dashboard takes over the screen. When stdout is not a terminal, or with `--headless`, Hashi prints plain log lines instead.

Nearly everything the CLI does can be done from the dashboard, through the same library code. The two exceptions are `hashi serve` (the dashboard is the running server, and its flags apply only at start) and `hashi uninstall`. A test walks the command tree and fails when a new subcommand has no place in the dashboard.

| Key | Action |
|---|---|
| `1` `2` `3` `4` `5` | Overview, Requests, Tokens, Logs, Commands |
| `Tab` `Shift-Tab`, `→` `←`, `l` `h` | Next / previous view |
| `↑` `↓` / `j` `k` | Move the selection |
| `PgUp` `PgDn` / `J` `K` | Scroll the request detail |
| `Enter` | Run the selected row (Commands view) |
| `a` | Ask an agent: the prompt popup (any view) |
| `n` / `d` | Create a token (Tokens and Commands views) / revoke one (Tokens view) |
| `e` | Edit the selected token: name, note, expiry, limits, accounts and project (Tokens view) |
| `s` | Share: create an invite link (Tokens and Commands views) |
| `p` | Pause or resume the selected token |
| `i` / `x` | Switch between tokens and pending invites / cancel the selected invite (Tokens view) |
| `x` | Cancel the selected queued or running request, after confirmation (Requests view). Audited as `dashboard` |
| `c` | Copy the API URL, or the token or invite link being shown |
| `t` | Go public: start a Cloudflare quick tunnel on the running server. Without `cloudflared`, opens its install popup; with Tailscale public, asks to switch |
| `r` | Refresh |
| `?` | All key bindings |
| `Esc` | Close a popup |
| `q` | Quit and stop the bridge. Asks first while requests are in flight |
| `Ctrl-C` | Quit immediately |

No two actions in one view share a key. Forms move between fields with `↑` `↓` or `Tab`, change a choice with `←` `→` or space, save with `Enter` and cancel with `Esc`; a field that fails shows why next to it. Keys and other secrets typed into a form are masked and never reach the log.

### Commands view

Tab `5` lists everything the owner can do, grouped, one row each with its key (or `enter`), what it does, its state right now and the command that does the same from a terminal:

| Group | Rows | Same from a terminal |
|---|---|---|
| Go public | Cloudflare quick tunnel (`t`), Tailscale serve, Tailscale funnel, stop going public | `hashi serve --tunnel`, `--tailscale serve`, `--tailscale funnel` |
| Share | new invite (`s`), new token (`n`), access requests | `hashi share`, `hashi token create`, `hashi requests list` |
| Ask | ask an agent (`a`) | `hashi ask`, `hashi ask --remote` |
| Manage | accounts, projects, bridges I use, connect to a bridge | `hashi account`, `hashi project`, `hashi remote`, `hashi connect` |
| Server | port and bind address, copy the API URL (`c`) | `hashi serve --port --bind` |
| Install | cloudflared, tailscale, Crush | `hashi install cloudflared\|tailscale\|crush` |
| Maintenance | check for updates, install the update, doctor, diagnostic report, audit log | `hashi update [--check]`, `hashi doctor`, `hashi report`, `hashi audit` |
| Help | all key bindings (`?`) | `hashi --help` |

The tab label shows how many requests for access are waiting, as `Commands ·2`. It updates when a request arrives and with the regular refresh.

States include "live at <url>", "starting", "approve at <url>", "not installed" and "signed out". In an install popup, `enter` runs the command with the terminal attached and brings the dashboard back afterwards, `c` copies it and `esc` cancels. Ctrl-C while it runs goes to the install, not to Hashi. Signing an account in and installing an update step aside the same way.

### What each popup does

| Popup | Keys | Notes |
|---|---|---|
| Ask | `Tab` field, `←` `→` change, `Enter` new line in the prompt, `Ctrl-S` send | Choose this Hashi or a saved bridge, the agent, and (here) the account. A local prompt goes through the normal queue as the oldest active admin token, so limits and the queue apply and it shows in Requests, where its answer appears in the detail. Picking an account runs that prompt on it only. A bridge's answer opens in its own popup |
| Edit token | `↑` `↓` field, `←` `→` project, `Enter` save | Expiry takes days from now or `never`; empty keeps it. Empty limits mean none. Uses the same update as `hashi token update` and `PATCH /v1/tokens/{id}` |
| Accounts | `a` add, `l` sign in again, `d` disable, `e` enable, `x` remove | Adding a login account steps aside for the agent's own sign-in, as `hashi account add` does. A key account takes the key in a masked field and stores it owner-only |
| Access requests | `a` accept, `d` decline, `o` open link, `x` close link, `s` show all | Pending requests, newest first, with what each asks for; `s` adds decided and expired ones, as `hashi requests list --all`. Accept opens a form prefilled as `hashi requests accept` would fill it (for a "more" request, next to the token's access now): `↑` `↓` field, type to edit, `←` `→` the invite's validity, `Enter` accept. Decline takes an optional reason the requester sees. Open makes a link with the defaults of `hashi requests open`, closing any open one, and shows it once with a QR code when there is room: `c` copies it, `t` goes public |
| Projects | `a` add, `e` edit, `x` remove | The same checks as `hashi project add` and `update`, with the account strategy (`←` `→`: server default, in-order, most-left, soonest-reset) and rotate at % (50 to 99, empty for the server default). Removing a project with bound tokens puts them back on their own workspace; its folder is never touched |
| Bridges I use | `c` connect, `a` ask it, `x` forget, `r` request status | Names and URLs only, never tokens. Connect takes a pasted invite link and saves it as `hashi connect` does. Below, the requests for access this machine sent: `r` checks them as `hashi request status` does, and an accepted one is saved as a bridge |
| Audit log | `↑` `↓`, `PgUp` `PgDn` | The last 500 entries, newest first |
| Doctor | `u` check for updates, `r` recheck | Agents with versions, Crush, cloudflared, Tailscale and its sign-in state, accounts, the update state, the home and the resource guard |
| Diagnostic report | `←` `→` change, `Enter` write | `include prompts` is off by default. Saves in the current folder (like `hashi report`) or in `~/.hashi/reports/`, owner-only, and shows the path with `c` to copy it |
| Install the update | `y` / `n` | Steps aside for `hashi update`, then says to quit and start Hashi again |
| Port and bind address | `↑` `↓` field, `←` `→` change, `Enter` move | See below |

The invite popup also shows the link as a QR code when the terminal has room, and asks for a larger one otherwise.

### Moving the server

The port and bind address row moves a running server without stopping it. Choose a port (1 to 65535) and a bind address: `127.0.0.1` (this machine), `0.0.0.0` or `::` (all interfaces), or a typed IP such as a Tailscale address.

1. The new address is bound first. If that fails (the port is in use, the address is not on this machine, permission denied), the form says so next to the field and the server keeps serving where it was.
2. The server serves on the new address with the same TLS settings, connection cap and limits, and the URL in the header changes.
3. The old address stops taking connections, finishes its open requests within the usual 10 second grace period, and closes.

Queued and running prompts carry on: they live in the queue, not the listener. A Cloudflare quick tunnel or a Tailscale route that is up is restarted on the new port. A named tunnel cannot be retargeted by Hashi: change the port of its hostname in the Cloudflare dashboard too. A bind outside loopback without TLS asks first, with the warning `hashi serve` prints.

### settings.json

With "keep for next start" on (the default), the move is saved in `~/.hashi/settings.json`, readable only by you:

```json
{
  "port": 9000,
  "bind": "127.0.0.1"
}
```

At startup each value comes from, in order: its flag (`--port`, `--bind`), its environment variable (`HASHI_PORT`, `HASHI_BIND`), `settings.json`, then the default (`7878`, `127.0.0.1`). When a flag or variable is set, the form says it wins over the saved value at the next start. A file that cannot be read, or that saves port `0`, is ignored with a warning in the log. While the server listens outside loopback, however that was chosen, the dashboard header shows `! exposed, no TLS` (or `! exposed, HTTPS`) next to the local URL. `hashi uninstall` counts `settings.json` and `reports/` as Hashi's own.

Running requests count up their elapsed time, and every status pairs a symbol with its colour (`✓` done, `✗` failed, `◌` queued, a spinner for running). Body text uses your terminal's own foreground colour, so light and dark themes both stay readable, and `HASHI_REDUCED_MOTION=1` freezes the animation.

## Installer options

| Variable | Effect |
|---|---|
| `HASHI_VERSION=v0.3.0` | Install a specific release instead of the latest |
| `HASHI_INSTALL_DIR=/some/dir` | Install somewhere else |
| `HASHI_NO_DOCTOR=1` | Skip the `hashi doctor` check at the end |
| `HASHI_CONNECT='<invite link>'` | After installing, run `hashi connect -- <link>` instead of `hashi doctor`. Only a link of the shape the claim page builds (`https://<host>[:port]/claim/hashi_inv_<hex>`, or `http://` on loopback) is accepted; anything else stops the installer before it downloads. It fails with a clear message if the link is used up or expired |

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_VERSION=v0.3.0 sh
```

```powershell
$env:HASHI_VERSION = "v0.3.0"; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

The claim page builds the one-command setup for an invited person from its own link:

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_CONNECT='https://host/claim/hashi_inv_...' sh
```

```powershell
$env:HASHI_CONNECT='https://host/claim/hashi_inv_...'; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

## Reporting a bug

```bash
hashi report                     # writes hashi-report-<time>.md in this directory
hashi report --stdout            # print it instead
hashi report --include-prompts   # add the first 500 characters of failed prompts
```

The report holds the version, OS, agent and cloudflared detection, `HASHI_*` settings, database counts, the last 20 failed requests, the last 200 log lines and any crash file. Tokens, invite codes, provider keys and your home directory are redacted, and prompts are left out unless you ask for them. Nothing is uploaded: the command prints a link to a prefilled GitHub issue, and you attach the file yourself after reading it.

If Hashi crashes, it saves `~/.hashi/logs/crash-<time>.txt` and the next report includes it. API errors with status 500 carry an `errorId` that matches a line in the log; quote it in the issue.
