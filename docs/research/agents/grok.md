# Grok Build (xAI) (`grok`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `GROK` in `src/agent/specs.rs`.

- **Source:** closed
- **License:** proprietary (xAI)
- **Version checked:** 0.2.20 (77224a6aa69) [stable], ran here
- **Install:** curl -fsSL https://x.ai/cli/install.sh | bash
- **Headless call:** `grok --output-format json`, prompt flag eq `--single=`
- **Runs tools without asking when headless:** no

## Notes

**Which project.** The installed `~/.grok/bin/grok` (symlink to `~/.grok/downloads/grok-0.2.20-macos-aarch64`, Mach-O arm64) is **xAI's official "Grok Build" CLI** (`grok --version` prints `grok 0.2.20 (77224a6aa69) [stable]`, `--help` header "Grok Build TUI", installer `curl -fsSL https://x.ai/cli/install.sh | bash`). It is a closed-source Rust binary, NOT `superagent-ai/grok-cli` (that is a different TypeScript project, npm `@vibe-kit/grok-cli`, binary also `grok`; Hashi should not confuse them: superagent's uses `-p/--prompt` with a different output). Everything below comes from the docs that ship with the install (`~/.grok/README.md`, `~/.grok/docs/user-guide/14-headless-mode.md`, `22-permissions-and-safety.md`), from `grok --help`, from probing which flags clap accepts (`grok <flag> --version`), and from strings in the binary.

**Headless.** `grok -p/--single <PROMPT>` runs one prompt and exits (README.md:500-517, 14-headless-mode.md:13-20). There is no positional prompt (`Usage: grok [OPTIONS] [COMMAND]`), so `--` is irrelevant; the binary is clap, and `--single=<prompt>` is the safe form (clap takes everything after the first `=` literally, so a prompt starting with `-` is fine). Verified: `--single='Reply with exactly: OK'` was accepted. Alternatives: `--prompt-file <PATH>`, `--prompt-json <JSON>`.

**Hidden flags.** `-s/--session-id`, `--no-auto-update` and `--yolo` are documented (14-headless-mode.md:27-49) but missing from `--help`; probing showed clap accepts them. `--no-project-root` / `--cwd-only` (documented at 14-headless-mode.md:428-440) are **rejected** by 0.2.20 (`error: unexpected argument '--no-project-root' found`, exit 2), so do not pin them.

**Output.** `--output-format plain|json|streaming-json` (README.md:668-694). `json`: one object `{"text","stopReason","sessionId","requestId"}`; `streaming-json`: `{"type":"text","data":...}`, `{"type":"thought","data":...}`, terminal `{"type":"end","stopReason","sessionId","requestId"}`. **No token usage or cost is documented in either format** (usage lives only in `~/.grok/sessions/<cwd>/<id>/signals.json`, README.md:2283-2297). Whether the json object is printed on one line or pretty-printed is UNVERIFIED (docs show it pretty-printed); Hashi's "one object or last JSON line" parsing must handle a multi-line object. The error shape in json mode is UNVERIFIED: exit codes are 0 ok, 1 auth/network/runtime error, 130 SIGINT, 143 SIGTERM (14-headless-mode.md:398-405); errors are expected on stderr, so `error_ptr` is null and Hashi's stderr fallback on non-zero exit applies.

**Real run (no fixture obtained).** Ran once in a fresh `mktemp -d` with stdin `/dev/null`, `NO_COLOR=1 TERM=dumb`, `timeout 120`: `grok --single='Reply with exactly: OK' --output-format json --permission-mode dontAsk --no-auto-update`. The cached login had expired, so **headless mode started an interactive browser OAuth flow** ("Signing in with Grok... Open this URL to sign in: https://auth.x.ai/oauth2/authorize?...redirect_uri=http://127.0.0.1:<port>/callback..." on stderr), empty stdout, and **hung until `timeout` killed it (exit 124)**. So an unauthenticated or expired account does not fail fast: it blocks for the whole Hashi timeout and may pop a browser on the host. Docs say browser tokens expire after 7 days (README.md:92). Hashi should prefer `XAI_API_KEY` for unattended accounts, and treat a timeout whose stderr contains "Signing in with Grok" or "auth.x.ai/oauth2" as "needs sign in". `fixture_success` below is therefore derived from the docs, not recorded.

**Session / resume.** Headless starts a fresh session each call. `-r/--resume <ID>` resumes and errors if not found; `-s/--session-id <ID>` creates or resumes a caller-chosen id (14-headless-mode.md:163-200), so Hashi could pre-assign ids with `-s hashi-<uuid>`. Sessions are stored per cwd under `~/.grok/sessions/<url-encoded-cwd>/<id>/` (README.md:2283), so resume must use the same workspace (UNVERIFIED whether `-r` looks across cwds). Note `-r` takes an OPTIONAL value in `--help` (`-r, --resume [<SESSION_ID>]`), so always pass the id right after it.

**Model.** `-m/--model <MODEL>` (e.g. `grok-build`); `grok models` lists them.

**Permissions headless.** Modes: `default`, `dontAsk`, `bypassPermissions`, `acceptEdits`, `plan`, `auto` (22-permissions-and-safety.md, "Permission Modes"). `dontAsk` "silently deny anything without an explicit allow rule or fast-path" and is documented as "ideal for headless/CI". Fast paths that run even under `dontAsk`: `read_file`, `list_dir`, `web_search`, `todo_write`, skills, `grep`, and a curated list of read-only shell commands (`ls`, `cat`, `head`, `git status`, `git log`, `cargo check`, `kubectl get` ...). What plain `default` mode does headless (deny or hang) is UNVERIFIED; the docs say only "User Prompt (in interactive TUI) or denial (in dontAsk / headless)". The docs nevertheless describe headless as running "with full tool access" and all CI examples add `--yolo`/`--always-approve`. Pinned safe set: `--permission-mode dontAsk`, plus `--tools read_file,grep,list_dir` which removes `run_terminal_cmd`, `search_replace`, web tools and `task` entirely (README.md:528-560; `--tools` disables default tool injection), plus `--disable-web-search`, `--no-memory`, `--no-subagents`, `--no-auto-update`, `--verbatim`. `--sandbox <PROFILE>` (env `GROK_SANDBOX`) adds OS isolation (profiles UNVERIFIED). `read_file`/`cat` are not confined to the workspace, so the owner's files remain readable.

**Workspace config hazard.** Grok loads project-scoped `.grok/config.toml` (MCP servers, permission rules; "Project-level .grok/config.toml takes precedence over the global one"), `.grok/hooks/`, `.grok/agents`, `.mcp.json`, and Claude/Cursor compat files `.claude/settings.json` from the cwd (README.md:2360-2382, 22-permissions-and-safety.md "Configuring Permissions"). A token holder could plant these in the workspace. The binary contains env switches `GROK_CLAUDE_MCPS_ENABLED`, `GROK_CURSOR_MCPS_ENABLED`, `GROK_CLAUDE_HOOKS_ENABLED`, `GROK_CURSOR_HOOKS_ENABLED`, `GROK_MANAGED_MCPS_ENABLED` (strings), which are pinned to `0` in `env`; their accepted values and whether native `.grok/config.toml` MCP/permission entries can be disabled are UNVERIFIED. Since `--deny` beats `--allow` and project rules are merged, a planted project `allow` rule cannot override `--tools` (tools are removed, not gated), which is why `--tools` is the primary guard.

**Slash commands.** The TUI has slash commands; `--verbatim` ("Send the prompt exactly as given") is pinned to stop prompt expansion. Whether a headless prompt starting with `/` runs a command without `--verbatim` is UNVERIFIED, so `slash_commands_in_prompt` is set true conservatively.

**Env.** `GROK_HOME` relocates `~/.grok` including `auth.json` (README.md:2401, 14-headless-mode.md:449), so it is the per-account config dir. `XAI_API_KEY` takes precedence over browser credentials (README.md:102-111). Update checks: `--no-auto-update`, `GROK_DISABLE_UPDATE_CHECK=1`, and non-TTY stderr disables them automatically (14-headless-mode.md:476-485). Telemetry: `GROK_TELEMETRY_ENABLED`, `GROK_TELEMETRY_TRACE_UPLOAD` (README.md:1294-1331). Project root discovery walks up from cwd looking for `.git` (14-headless-mode.md:425-440); Hashi's workspace is not a repo, fine unless it sits inside one.

**Rate limits.** No documented message; `limit_patterns` are generic guesses (UNVERIFIED). xAI API errors are HTTP 429 style.

**Process.** `grok agent leader`/`--leader` can share a leader process; headless `-p` does not use one unless `[cli] use_leader` is set in config (README agent options; UNVERIFIED for `-p`).

`fixture_success` is DERIVED from the documented json shape (README.md:675-684), ids invented; the real run produced no stdout because the login had expired (see above). `fixture_error` is the exact stdout of that real run: empty, with the sign-in prompt on stderr and exit 124 from `timeout`.

## Success output (derived from source or docs, not recorded)

```
{"text":"OK","stopReason":"EndTurn","sessionId":"0199f3a2-7c41-7d10-9b2e-5a1c3e7d9f20","requestId":"0199f3a2-7c55-7a31-8e02-c41b6d2e8a77"}
```
