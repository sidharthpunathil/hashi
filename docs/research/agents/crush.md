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
