# Crush (`crush`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **ran**. The spec Hashi runs is `CRUSH` in `src/agent/specs.rs`.

- **Source:** github:charmbracelet/crush
- **License:** FSL-1.1-MIT
- **Version checked:** v0.100.0 (installed, run here); source read at cached main
- **Install:** brew install charmbracelet/tap/crush | npm i -g @charmland/crush | go install github.com/charmbracelet/crush@latest
- **Headless call:** `crush run -q`, prompt dashdash
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

Source: `~/.opensrc/repos/github.com/charmbracelet/crush/main`, paths under `internal/`. Installed: `crush version v0.100.0`. Hashi itself shipped a Crush backend in 0.4.0 and removed it in 0.4.1 (commit aa2a136; the deleted `src/agent/crush.rs` documents the hardening it needed). Nothing material changed for headless use since.

**Real run (v0.100.0)**: `cd $(mktemp -d) && env NO_COLOR=1 TERM=dumb CRUSH_DISABLE_METRICS=1 DO_NOT_TRACK=1 CRUSH_DISABLE_PROVIDER_AUTO_UPDATE=1 timeout 120 crush run -q -D $D/.crushdata -- 'Reply with exactly: OK' </dev/null` exited 1. Stdout was exactly one newline (recorded as `fixture_error`; the deferred `Fprintln` in app/app.go:484 always prints it). Stderr carried a boxed `ERROR` banner: `Agent processing failed: failed to start agent processing stream: authentication error: failed to refresh cached credentials, create oauth2 token: ... ValidationException: The provided authorization grant is invalid, expired, revoked, or malformed.` Crush had no provider configured in `~/.config/crush`, yet it **auto-detected AWS credentials from the environment/`~/.aws` and tried Bedrock**. So `fixture_success` is DERIVED from source: the streamed assistant text then a newline.

**Headless**: `crush run [prompt...]` (alias `r`), cmd/run.go:31-166. Positional args are joined with a space (run.go:83); `--` is honoured by cobra/pflag, which is what the removed Hashi backend relied on. Stdin is prepended only when it is a pipe or regular file (`MaybePrependStdin`, run.go:85); `/dev/null` is neither. Empty prompt: `no prompt provided`. No slash-command parsing in `run` (prompt goes straight to `AgentCoordinator.Run`, app/app.go:459).

**Output**: plain text only, no JSON mode. Every assistant message of the session is streamed to stdout as deltas, with no separator between messages of different steps (app/app.go:510-531), then a final newline. Leading spaces/tabs of each message are trimmed. Errors return `agent processing failed: <err>` (app.go:502) which fang prints to stderr; exit 1. A cancel (SIGINT/SIGTERM, `context.Canceled`) returns nil, so exit 0 with partial text (app.go:499-501).

**Session id and usage**: not printed by `run`. `crush session last --json` (cmd/session.go:65-67, 366-430) prints `{meta:{id (hash), uuid, title, created, modified, cost, prompt_tokens, completion_tokens, total_tokens}, messages:[...]}`; run it right after with the same `-D`. `prompt_tokens` is the last step's context size, not a sum; `cost` is cumulative. Resume: `-s/--session <uuid|hash|prefix>` (run.go:173, resolved by `resolveSessionID`, session.go:221).

**Permissions (unsafe)**: a non-interactive run auto-approves every permission request of its session (`app.Permissions.AutoApproveSession(sess.ID)`, app/app.go:447-449), regardless of `-y`. The only control is which tools exist: `options.disabled_tools` in a config file (config/config.go:469, resolved at 1156). There is no CLI flag to disable tools, so `pinned_args` cannot make it safe; Hashi would have to write a `crush.json` into the data dir (`-D`) with `disabled_tools` listing bash, edit, multiedit, write, download, fetch, agentic_fetch, sourcegraph, agent*, job_*, lsp_rename, lsp_replace_symbol, lsp_restart and the MCP resource tools (see the removed crush.rs for the full list of 33 tools). `-y` (count flag, `-yy` also drops the exec block list, cmd/root.go:62) only matters interactively.

**Config hazards**: `main.go:12` imports `godotenv/autoload`, so a `.env` in the workspace is loaded. Project configs `.crush.json`, `crush.json`, `.crushrc`/`crushrc` are searched upward from cwd (config/load.go). Default data dir is `.crush` in the cwd (config/config.go:26, load.go:738-741), so without `-D` each workspace gets its own session DB. `CRUSH_CLIENT_SERVER=1` makes `run` talk to a shared server over a socket (cmd/root.go:259-262); keep it unset.

**Config dir**: no single variable. `CRUSH_GLOBAL_CONFIG` (load.go:1327), `CRUSH_GLOBAL_DATA` (load.go:1374), `CRUSH_CACHE_DIR` (1350) and `CRUSH_SKILLS_DIR` (1480) together move user-level state; the removed Hashi backend also set `HOME` because the Copilot token import reads `$HOME/.config/github-copilot`. While providers are configured, any `CRUSH_<VAR>` overrides `<VAR>` (`PushPopCrushEnv`), so strip inherited `CRUSH_*`. Telemetry off: `CRUSH_DISABLE_METRICS` or `DO_NOT_TRACK` (cmd/root.go:948-951).

**Model**: `-m/--model <model|provider/model>`, plus `--small-model`, `--reasoning-effort` (run.go:170-172).

**Limit patterns**: UNVERIFIED. Crush surfaces provider errors from its fantasy/catwalk clients verbatim; it has no Crush-specific rate-limit string, so the list is generic provider wording.

## Success output (recorded)

```
OK
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. Logins and keys entered in the TUI are written to ScopeGlobal, the data config GlobalConfigData() = $CRUSH_GLOBAL_DATA/crush.json (internal/config/scope.go:9-10; internal/config/load.go:1373-1376; internal/cmd/login.go:83-207 via SetProviderAPIKey, internal/config/store.go:579-640). No keyring: the only 'keyring' hits in the repo are apt install lines in README.md:133-135. Caveats: (1) the owner's ~/.config/crush/crush.json (GlobalConfig, load.go:1326-1330) is still merged before the data config (lookupConfigs, load.go:1060-1064), so a key the owner put there by hand reaches every account; setting CRUSH_GLOBAL_CONFIG to the account dir too closes that (here ~/.config/crush holds only skills/). (2) `crush login copilot` first imports a Copilot token from $HOME/.config/github-copilot/apps.json (internal/oauth/copilot/disk.go:10-35, login.go:121-135), so a second Copilot account needs `-f` and a fresh device login, and that import is owner-wide. (3) Crush also auto-detects AWS credentials and uses Bedrock when no key is set (load.go:568, 1202).
- **API-key accounts:** no. Catwalk providers default their api_key to a template such as $ANTHROPIC_API_KEY, but a provider api_key in any loaded config file replaces it (internal/config/load.go:455-456), and the stored login lives in those files. So the owner's stored key or OAuth token outranks the env key. A key account works only when CRUSH_GLOBAL_DATA and CRUSH_GLOBAL_CONFIG point at an empty folder as well. Which env var applies depends on the chosen model's provider; there is no Crush-specific key variable. While providers are resolved, any inherited CRUSH_<VAR> overrides <VAR> (PushPopCrushEnv, load.go:406-436), so CRUSH_ANTHROPIC_API_KEY must be stripped too.
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": "crush.json", "pointer": null, "jwt_claim": {"pointer": "/providers/openai/oauth/id_token", "claim": "email"}}
- **Switch:** yes. The login is plain JSON in GlobalConfigData: $XDG_DATA_HOME/crush/crush.json, else ~/.local/share/crush/crush.json (internal/config/load.go:1373-1392), the same file CRUSH_GLOBAL_DATA moves, so sub is ''. OAuth tokens carry expires_at as unix seconds (internal/oauth/token.go:29-40) and Crush refreshes the Hyper token in place at startup (load.go:307-309). Caveats: crush.json also holds model choices and other TUI settings written to ScopeGlobal, so those switch with the login; the expiry pointer only exists for a Hyper login (Copilot, ChatGPT and Grok tokens are under /providers/copilot|openai|xai/oauth/expires_at), and a missing pointer must be treated as no expiry. On this machine ~/.local/share/crush holds only projects.json, so the plain command has no login now.
- **Live quota:** yes. Only for Charm Hyper logins; Crush fetches it after each Hyper request for the TUI header (internal/agent/hyper_credits.go:14, 60-95). Copilot, ChatGPT, Grok and API-key providers report nothing through Crush.
- **Usage in output:** none
- **Limit messages:** Crush has no rate-limit wording of its own: provider errors from the fantasy/catwalk clients are passed through as 'agent processing failed: <err>' on stderr with exit 1 (internal/app/app.go:502); no 429, 'rate limit' or 'insufficient' strings exist in internal/ outside a DuckDuckGo tool message (internal/agent/tools/search.go:52). The generic 'rate limit' and 'too many requests' are reasonable. Worth adding for the OAuth providers it now supports: 'usage limit' (ChatGPT plans), 'quota' / 'insufficient_quota' (OpenAI), and a Hyper out-of-credits message (wording UNVERIFIED). Since output is Text, Hashi must look for these on stderr, not stdout.

Source: github:charmbracelet/crush main; installed `crush version v0.100.0`, whose `--help` and `login --help` match source. Ran only `crush --help`, `crush login --help`, `crush dirs` and listed names: ~/.config/crush has skills/, ~/.local/share/crush has projects.json, no crush.json, so no stored login.

- **Storage**: SetProviderAPIKey writes providers.<id>.api_key for keys, and for OAuth both providers.<id>.oauth (access_token, refresh_token, id_token, account_id, expires_in, expires_at) and providers.<id>.api_key = access token, except openai (internal/config/store.go:579-640; internal/oauth/token.go:29-40).
- **Status**: no command tests a login. `crush models` lists configured providers but only reads config (internal/cmd/models.go:26-150), and `crush login copilot` without -f prints 'already logged in' only for Copilot (login.go:111-119), so status_check is null.
- **Identity**: only ChatGPT and Grok logins hold an id_token (openai scope includes email, internal/oauth/openai/oauth.go:40, 188; grok/oauth.go:41, 196); Crush itself reads only the account id (openai/oauth.go:197-201). Hyper and Copilot keep no email.
- **Usage**: `run` prints plain text only (internal/app/app.go:510-531); tokens and cost are only in `crush session last --json` (internal/cmd/session.go:65-67, 366-430), so usage_in_output is none, matching Output::Text.
