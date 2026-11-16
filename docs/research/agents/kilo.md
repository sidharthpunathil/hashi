# Kilo Code CLI (`kilo`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `KILO` in `src/agent/specs.rs`.

- **Source:** github:Kilo-Org/kilocode
- **License:** MIT
- **Version checked:** 7.8.28 (source, main); installed 1.0.16 npm wrapper whose native binary is SIGKILLed on launch
- **Install:** npm install -g @kilocode/cli | curl -fsSL https://kilo.ai/cli/install | bash | brew install Kilo-Org/tap/kilo
- **Headless call:** `kilo run --format json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

## Kilo Code CLI (`kilo`)

Source: `github:Kilo-Org/kilocode` main, cached at `~/.opensrc/repos/github.com/Kilo-Org/kilocode/main`, package `packages/opencode` = `@kilocode/cli` 7.8.28 (packages/opencode/package.json). It is a fork of **opencode 1.x** (yargs, `packages/opencode/src/cli/cmd/run.ts`), NOT the opencode V2 effect/cli rewrite that Hashi's `opencode.rs` targets. Kilo changes are marked `kilocode_change`. License MIT (LICENSE:1). The npm package's `repository` field points at `Kilo-Org/kilo`; the code lives in `Kilo-Org/kilocode`.

### Installed binary here: could not run (fixture is derived)
- `/opt/homebrew/bin/kilo` is the npm wrapper of `@kilocode/cli` **1.0.16** (Feb 2026), far behind source 7.8.28. The native binary `node_modules/@kilocode/cli-darwin-arm64/bin/kilo` (ad-hoc signed, `Identifier=a.out`) is **SIGKILLed on launch (exit 137)**, same symptom as the opencode 1.x build on this machine. So `--version`, `--help` and the real run printed nothing.
- The one real run (`kilo run --format json -- 'Reply with exactly: OK'`, fresh `mktemp -d`, stdin /dev/null, env below) printed **empty stdout, empty stderr, exit 0**.
- **Wrapper gotcha (verified by running and reading `bin/kilo`):** the Node wrapper does `spawnSync(native, argv, {stdio:"inherit"})` and exits `typeof result.status === "number" ? result.status : 0`, so a native child killed by a signal is reported as **exit 0**. Also, Hashi's `kill_on_drop` would kill only the Node wrapper and orphan the native child. Hashi should either resolve the native binary (`$(npm root -g)/@kilocode/cli/node_modules/@kilocode/cli-<os>-<arch>/bin/kilo`, or the brew tap build, UNVERIFIED whether that is native) or at minimum treat empty stdout as failure. `KILO_BIN_PATH` makes the wrapper exec a given binary (bin/kilo), still through spawnSync.
- fixture_success and fixture_error below are **derived from source**, not recorded. Field layout follows `emit()` in run.ts:791-804 and the v2 SDK types; ids, timestamps and numbers are illustrative.

### Headless command
- `kilo run [message..]` (run.ts:134-276). Non-interactive is the default mode; it streams events and exits when the session drains (run.ts:1065-1130). `--interactive/-i` and hidden `--mini` need a TTY (run.ts:341-343).
- **`--` is honoured** and is the right form: yargs `populate--` is on (src/index.ts:57), and `buildRunMessage` (src/kilocode/cli/cmd/run-message.ts:8-11) passes atoms after `--` through **raw**, while positionals before `--` that contain a space are wrapped in literal double quotes (the opencode 1.x quoting quirk, kept). So `["run", ..., "--", "<whole prompt>"]` reaches the model exactly; no word splitting needed (unlike Hashi's opencode V2 backend). A leading `-` after `--` is fine (comment cites issue #9622).
- Stdin: when not a TTY it is read and appended after the message with a newline (run.ts:441-455, `readPipedStdin` bounded when argv has a message). `/dev/null` gives EOF, harmless.
- **cwd gotcha (same as opencode):** project root is `process.env.PWD ?? process.cwd()` (run.ts:355). Hashi inherits its own `PWD`, so set `PWD=<workspace>` (or unset it) for this backend; alternatively pass `--dir <workspace>` (run.ts:218-221, 356-367, it chdirs). The env map here cannot express a per-run path, so Hashi must inject `PWD` itself.
- **Daemon gotcha:** unless `--attach` is given, `run` first tries a running Kilo daemon (`KiloRunDaemon.attach`, src/kilocode/cli/cmd/run.ts:90-97, `DaemonClient.maybe`, src/kilocode/daemon/client.ts:13-37) and only falls back to an in-process server. `KILO_NO_DAEMON=1` disables that (client.ts:14), so runs never share the owner's daemon (this is Kilo's equivalent of opencode V2 `--standalone`, which does not exist here).
- `--command <name>` runs a slash/custom command with the message as arguments (run.ts:151-154, 1087-1095). A prompt that merely starts with `/` is sent as plain text through `session.prompt` (run.ts:1096-1102), so slash_commands_in_prompt=false (source, not run).
- Other flags: `-c/--continue`, `-s/--session <id>`, `--fork`, `--cloud-fork`, `--share`, `-m/--model provider/model`, `--agent`, `--variant`, `--format default|json`, `-f/--file`, `--title`, `--attach <url>`, `-p/--password`, `-u/--username`, `--dir`, `--port`, `--thinking`, `--auto` (run.ts:155-276). Note `-p` is **password**, not prompt.

### Model, resume
- `-m provider/model`, split on the first `/` (run.ts:38-45). Default provider is the Kilo gateway, which needs `KILO_API_KEY` or `kilo auth login` (src/kilocode/provider/provider.ts:303-314, src/kilocode/provider/catalog.ts:7).
- `-s <id>` resumes; unlike opencode V2 it **cannot pre-assign** ids: an unknown id prints `Session not found` and exits 1 (run.ts:543-553). Ids are opencode 1.x style `ses_...`. The id is on every JSON line as top-level `sessionID`, so read it from the first run's output.

### Output (`--format json`): JSONL
Envelope `{"type","timestamp"(ms),"sessionID",...}` (run.ts:791-804). Events:
- `step_start` `{part: StepStartPart}` (run.ts:875-877)
- `text` `{part: TextPart{id,sessionID,messageID,type:"text",text,time{start,end}}}`, only once the part has `time.end` (run.ts:883-884)
- `reasoning` only with `--thinking` (run.ts:896-897)
- `tool_use` `{part: ToolPart}` when status is completed or error (run.ts:854-855)
- `step_finish` `{part: StepFinishPart{reason, cost (USD), tokens{total?,input,output,reasoning,cache{read,write}}, model?, time?}}` (run.ts:879-881; packages/sdk/js/src/v2/gen/types.gen.ts:886-920)
- `error`: three shapes. Session errors: `{"error": {name, data:{message,...}}}` (run.ts:911-923, union at types.gen.ts:6690-6705, `APIError` at 647-661); prompt request failure `{"error": <object>}` (run.ts:1104-1107); and **plain strings** `{"error":"run ended with an auto-rejected permission; pass --auto for autonomous use"}` (run.ts:1033-1038), `{"error":"run ended without an assistant message; the model returned no output"}` (run.ts:1113-1118), or a thrown error message (run.ts:1120-1123). error_ptr `/error/data/message` covers the object form; Hashi should fall back to `/error` when it is a string, and treat any `error` line as failure.
- Session errors are also written to stderr first (run.ts:919-920).
- There is no terminal result event. Parser: same as Hashi's opencode backend (text of the last step that produced text, sum step_finish tokens and cost). Add `cache.read`/`cache.write` to input to match the Claude convention.

### Exit codes
Exit 1 on any session error, failed prompt request, auto-rejected permission, or **empty assistant output** (Kilo change, run.ts:1104-1118). Exit 0 otherwise. Through the npm wrapper, a signal kill becomes exit 0 (see above).

### Permissions when headless
- Defaults (src/agent/agent.ts:136-158, src/kilocode/agent/index.ts:55-68, 378-392): `"*": "allow"` (so **edit/write and most tools run without asking**), `bash` is `"*": "ask"` plus an allowlist of read-only commands and `touch`, `mkdir`, `cp`, `mv`, `tar`, `unzip`, `gzip`, etc. that run unasked, `external_directory` ask, `.env` reads ask, `doom_loop` ask, `recall`/`kilo_memory_*` ask.
- In `run` without `--auto`/`--yolo`/`--dangerously-skip-permissions`, every ask is answered `reject` and the run then **fails** with exit 1 (run.ts:945-1011, 1032-1038); skill shell batches and sandbox escalations are always rejected (run.ts:950-953); question/suggest/plan_enter/plan_exit are denied for the session (run.ts:481-506); subagent asks of headless roots are denied (src/kilocode/permission/headless.ts). `--auto` replies `once` to every ask (run.ts:956-963); `--yolo` and `--dangerously-skip-permissions` likewise (run.ts:298, 994-998). These three are unsafe_flags.
- So with no extra config, auto_approves_tools_headless is **true** (file edits and allowlisted shell commands run). Safe config: `KILO_PERMISSION` is a runtime JSON overlay deep-merged into `permission` after all config files including managed ones (src/config/config.ts:1016-1022; core/src/flag/flag.ts:146-148). Rules are last-match-wins (src/permission/index.ts:102-106). The env value above starts with `"*":"deny"`, allows only read-only tools, denies `.env` reads, and denies `bash`, `edit`, `webfetch`, `websearch`, `external_directory`, `task` by name. Caveat: mergeDeep keeps the key position of keys the owner already set, so an owner rule placed after their own `"*"` (for example `"myserver_*":"allow"`) can still win; per-agent `agent.<name>.permission` config may also merge after it (UNVERIFIED). With `KILO_PERMISSION` set, a denied tool call returns an error to the model rather than a reject, so the run can still succeed (UNVERIFIED end to end).
- `KILO_DISABLE_PROJECT_CONFIG=1` skips workspace `kilo.json`/`.kilo` config sources (src/kilocode/config/sources.ts:63-64, 196-201), so a token's workspace cannot loosen permissions or add MCP servers.
- No OS sandbox by default (a `kilo-sandbox` package exists; not investigated).

### Env, auth, config
- Config dir: `KILO_CONFIG_DIR` relocates **only** the config dir (core/src/global.ts:80; flag.ts:140-142). Credentials live in `<XDG_DATA_HOME>/kilo/auth.json` (core/src/credential.ts:165, 293) and state under XDG_STATE_HOME (global.ts:22-26). No single env var relocates everything, so config_dir_env is null; set `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` together (or `HOME`) for a separate login.
- Credentials: `KILO_API_KEY` (Kilo gateway), `KILO_ORG_ID`, `KILO_API_URL`, `KILO_AUTH_CONTENT` (inline auth JSON), plus models.dev provider vars (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `GEMINI_API_KEY`, ...). Also `KILO_CONFIG`, `KILO_CONFIG_CONTENT`, `KILO_CONFIG_DIR` can carry provider config. Login: `kilo auth login` (src/cli/cmd/providers.ts:242, 303); Kilo account login also via `kilo account login` (src/cli/cmd/account.ts:178), UNVERIFIED which one the default gateway expects.
- `KILO_DISABLE_AUTOUPDATE=1` (flag.ts:47; src/cli/upgrade.ts:10; upgrade is only invoked from the TUI worker, src/cli/tui/worker.ts:108, so it is belt and braces for `run`). `KILO_TELEMETRY_LEVEL` other than `all` disables PostHog telemetry (packages/kilo-telemetry/src/telemetry.ts:137-139). `KILO_DISABLE_MODELS_FETCH`, `KILO_DISABLE_CLAUDE_CODE` (skip importing Claude Code prompts/skills, flag.ts:28-29, 61-63) and `KILO_DISABLE_EXTERNAL_SKILLS` are further hardening options, not pinned here.
- `KILO_AUTO_SHARE` or config `share: "auto"` publishes the session (run.ts:626-639); Hashi should never pass `--share`.

### Rate limits
Retryable patterns (src/session/retry.ts:33-41): `429`, `rate limit`, `rate_limit`, `too many requests`, `resource exhausted`, `overloaded`, `try again later`. Normalized messages: `Provider rate limit exceeded. Please try again shortly.` and `Quota exceeded. Check your plan and billing details.` (src/provider/error.ts:127-145). Retries up to 5 with backoff (retry.ts:25-30), so a rate limit can stall the run up to the Hashi timeout before an `error` line appears. Kilo gateway credit/balance messages: UNVERIFIED.

### argv
New: `["run", "--format", "json", "-m", "anthropic/claude-sonnet-4-5", "--", "<prompt>"]`; resume: `["run", "--format", "json", "-s", "ses_...", "--", "<prompt>"]`; env as below plus `PWD=<workspace>`.

### Install
`npm install -g @kilocode/cli`, `curl -fsSL https://kilo.ai/cli/install | bash`, or `brew install Kilo-Org/tap/kilo` (README.md:50-62).

## Success output (derived from source or docs, not recorded)

```
{"type":"step_start","timestamp":1794812400123,"sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","part":{"id":"prt_a1","sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","messageID":"msg_a1","type":"step-start"}}
{"type":"text","timestamp":1794812401880,"sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","part":{"id":"prt_a2","sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","messageID":"msg_a1","type":"text","text":"OK","time":{"start":1794812401200,"end":1794812401880}}}
{"type":"step_finish","timestamp":1794812401890,"sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","part":{"id":"prt_a3","sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","messageID":"msg_a1","type":"step-finish","reason":"stop","cost":0.00042,"tokens":{"total":9133,"input":9120,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}
```

## Error output

```
{"type":"error","timestamp":1794812400500,"sessionID":"ses_1f2e3d4c5b6aFfAbCdEfGhIjKl","error":{"name":"APIError","data":{"message":"Provider rate limit exceeded. Please try again shortly.","statusCode":429,"isRetryable":true}}}
```
