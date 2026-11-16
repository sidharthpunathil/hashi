# Qoder CLI (`qoder`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `QODER` in `src/agent/specs.rs`.

- **Source:** npm:@qoder-ai/qodercli 1.1.89 (closed, minified bundle bundle/qodercli.js read; opensrc found no repository) + docs https://download.qoder.com/qodercli/docs/en/llms.txt
- **License:** Proprietary (Alibaba Qoder); package LICENSE file is Apache-2.0 text but no source is published
- **Version checked:** 1.1.89
- **Install:** curl -fsSL https://qoder.com/install | bash | npm install -g @qoder-ai/qodercli
- **Headless call:** `qodercli -p --output-format json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

**What it is.** Alibaba's **Qoder CLI** (`@qoder-ai/qodercli`, closed source, minified and string-obfuscated bundle `bundle/qodercli.js` 35 MB). It is a **Claude Code clone**: same flag names (`-p/--print`, `--output-format text|json|stream-json`, `--input-format stream-json`, `--include-partial-messages`, `--permission-mode`, `--allowed-tools`, `--disallowed-tools`, `--max-turns`, `--settings`, `--setting-sources`, `--mcp-config`, `--agents`, `--system-prompt`, `--append-system-prompt`, `-c/--continue`, `-r/--resume`, `--session-id`, `--fork-session`) and the same stream-json/result schema. Docs: https://docs.qoder.com/cli/run-in-scripts.md, https://docs.qoder.com/cli/cli-reference.md, https://docs.qoder.com/cli/settings-reference.md, https://docs.qoder.com/cli/authentication.md. Fixtures derived from the bundle's zod schema, not run.

**Binaries.** npm installs `qodercli` (the CLI) and `qoder` (`bundle/qoder-npm-dispatcher.cjs`, which routes to the Qoder IDE or the CLI); the native installer (`curl -fsSL https://qoder.com/install | bash`, recommended) installs `qoder`. Prefer `qodercli` so the dispatcher never launches the IDE. Same bundle also builds the China edition `qoderclicn` (see qoder-cn.json).

**Headless.** `qoder -p "<prompt>"` (`--print`); headless is also forced when stdin or stdout is not a TTY, CI is set, or `-p/--prompt`/`--print` appear in argv (bundle offset ~2668556). `--` separates the prompt from variadic options (cli-reference: "use `-p` or `--` to separate from the query"). Stdin is read when piped (`echo ... | qoder -p`); /dev/null is harmless (UNVERIFIED).

**Output `--output-format json`.** One result object matching **Claude Code's** `--output-format json` result (zod schema at bundle offset ~22703663; builder ~30785170): success `{type:"result", subtype:"success", duration_ms, duration_api_ms, is_error, num_turns, result, stop_reason, terminal_reason?, total_cost_usd, total_credits, usage, modelUsage, permission_denials, fast_mode_state, uuid, session_id}`; errors `{type:"result", subtype:"error_during_execution"|"error_max_turns"|"error_max_budget_usd"|..., is_error:true, errors[], error_message?, error_code?, ..., session_id}` with **no `result` field** (Hashi should read `/errors/0`). **`total_cost_usd` is hard-coded 0**; billing is in Qoder credits (`total_credits`), so `cost_ptr` is null. `usage` uses Claude's snake_case keys (`input_tokens`, `output_tokens`, `cache_*`). Whether the object is printed on one line like Claude Code is UNVERIFIED; `claude_result` takes the last JSON line.

**Resume.** `-r/--resume [id]` restores a session (no id opens a picker, so always pass one), `--session-id <id>` "Resume the session with the specified ID" per cli-reference (README calls it "use the specified session ID"; env `QODER_SESSION_ID`), `-c/--continue` most recent; these conflict with each other.

**Model.** `-m/--model` (`auto`, `lite`, `performance`, or a BYOK key); env `QODER_MODEL`.

**Permissions.** Modes `default|plan|auto|bypass_permissions|accept_edits|dont_ask`; `--yolo` and `--dangerously-skip-permissions` == `bypass_permissions`. In plain `-p` (text/json output) every operation that would ask is **denied** (run-in-scripts.md "Permission Control"), so the default is safe: no pinned flags. `--permission-mode accept_edits` auto-approves edits inside the cwd.

**Auth/config.** Browser login (`qodercli login`, tokens refreshed every 30 min; `NO_BROWSER=1` prints the URL instead of opening a browser) or `QODER_PERSONAL_ACCESS_TOKEN` (PAT wins over a stored login). Also `QODER_SERVICE_ACCOUNT_KEY`, `QODER_JOB_TOKEN` (env helpers in bundle, purpose UNVERIFIED). User config root: `--config-dir` > `QODER_CONFIG_DIR` > `QODER_CLI_HOME` + dir name > `~/.qoder`; `QODER_CONFIG_DIR_NAME` renames the dir (package README). Settings key `general.enableAutoUpdate` (default true; native installs self-update) and `aiCodeStatistics.enabled` (default true, reports AI-code statistics) are not settable by env; pass `--settings '{"general":{"enableAutoUpdate":false}}'` if needed (UNVERIFIED that it stops a background update in -p). Env mirrors of flags: `QODER_MODEL`, `QODER_WORKING_DIR`, `QODER_SESSION_ID`, `QODER_SESSION_NAME`, `QODER_PERMISSION_MODE`, `QODER_MCP_CONFIG` (Hashi should not pass `QODER_PERMISSION_MODE` through).

**Slash commands.** Leading `/` in a `-p` prompt runs a slash command (Claude Code behaviour; `/login` etc. documented as session commands).

**Rate limits.** Bundle strings `quota_exhausted`, `quota_exceeded`, `QuotaExceeded`, `Rate limited`, `rate limit`; exact user-facing text UNVERIFIED (fixture message illustrative). `--list-models` exits non-zero when not logged in.

## Success output (derived from source or docs, not recorded)

```
{"type":"result","subtype":"success","duration_ms":4210,"duration_api_ms":3900,"is_error":false,"num_turns":1,"result":"Paris.","stop_reason":"end_turn","total_cost_usd":0,"total_credits":0.4,"usage":{"input_tokens":8120,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"modelUsage":{"auto":{"inputTokens":8120,"outputTokens":5,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"webSearchRequests":0,"costUSD":0,"contextWindow":0,"maxOutputTokens":0}},"permission_denials":[],"fast_mode_state":"off","uuid":"9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b","session_id":"4b7e2f9a-1c3d-4e5f-8a9b-0c1d2e3f4a5b"}
```

## Error output

```
{"type":"result","subtype":"error_during_execution","duration_ms":320,"duration_api_ms":0,"is_error":true,"num_turns":0,"stop_reason":null,"total_cost_usd":0,"total_credits":0,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"modelUsage":{},"permission_denials":[],"errors":["Quota exhausted for the current plan"],"error_message":"Quota exhausted for the current plan","error_code":"quota_exhausted","fast_mode_state":"off","uuid":"9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6c","session_id":"4b7e2f9a-1c3d-4e5f-8a9b-0c1d2e3f4a5b"}
```
