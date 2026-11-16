# Command Code (`command-code`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `COMMAND_CODE` in `src/agent/specs.rs`.

- **Source:** npm:command-code
- **License:** UNLICENSED (closed source; npm ships a minified bundle)
- **Version checked:** 1.89.1
- **Install:** npm i -g command-code
- **Headless call:** `command-code --output-format json`, prompt flag eq `--print=`
- **Runs tools without asking when headless:** no

## Notes

Command Code (commandcode.ai, by Langbase; npm `command-code` 1.89.1, `license: UNLICENSED`, no public repo; opensrc could not fetch it: "No repository URL found"). Read the published bundle instead: `npm pack command-code@1.89.1`, `dist/index.mjs` (Node >= 22 guard, then imports `dist/cli.mjs`), and `dist/cli.mjs` (3.4 MB minified, formatted with prettier 3.3.3 into 149,696 lines). Line numbers below are into that formatted file; function names survive minification. Not run here; fixtures derived from source (event payload values illustrative).

**Binaries.** `cmd`, `cmdc`, `command-code`, `commandcode` (package.json `bin`). README: on Windows `cmd` is the shell, use `cmdc`; Hashi should prefer `command-code`.

**Headless.** `-p, --print [query]` (createProgram, line ~110933). The query comes from `--print <q>`, else from the positional `[prompt]` unless it starts with `-` (extractPrintModeQuery, 56776-56782); with no query and non-TTY stdin it reads stdin, and an empty stdin gives `Error: No query provided` exit 1 (resolvePrintInputSource 56926-56947). Prompt style is `--print=<text>`: the root command has subcommands (`login`, `logout`, `mcp`, `mods`, `taste`, `skills`, `update`, ...) and calls `.allowUnknownOption()` (line ~111033), so the positional and `--` routes are unsafe; commander takes everything after `=` as the optional value. Note `-p` with no value would then read stdin, which Hashi sets to /dev/null.

**Output `--output-format json`** (choices text|json, line ~110962). Newline-delimited `{"type":"event","event":<AgentEvent>}` frames (serializeAgentEventLine 56244) followed by exactly one result line (buildPrintResultLine 56245-56256): `{"type":"result","subtype":"success"|"max_turns"|"error","sessionId"?,"stopReason"?,"usage":{"inputTokens","outputTokens","cacheReadTokens","cacheWriteTokens"},"durationMs","finalText","error"?}`. Built in runPrintMode (57018-57140): success line at ~57120; every failure path (no query, not authenticated, run_error, API errors) goes through failResult and still prints a result line with `subtype:"error"`, `finalText:""` and `error` = the stderr message trimmed, which keeps its `Error: ` prefix (57030-57046). Usage is the `run_end` result usage, else summed `model_request_end.usage` (createPrintJsonEventStream 56265-56280). AgentEvent types seen in the agent loop: `run_start{sessionId}`, `text_delta{delta}`, `model_request_end{model,usage,stopReason,effort?}`, `message_end{content}`, `turn_end{turnNumber,hadToolCalls,usage}`, `run_error{error}`, `interrupted`, `run_end{result{finalText,stopReason,turnCount,usage,systemPromptTokens,nextState}}` (30391-30858). **Not Claude Code's shape**: camelCase keys, `finalText` not `result`, `sessionId` not `session_id`, no `is_error`, no cost. Hashi reads the last JSON line (type `result`). Gotcha: the `run_end` frame embeds `nextState` (the whole conversation state), so stdout can be large; mind the output cap. Cost: none in print output (cost only appears in client-driven session stats, contextUsageOf ~57200). Whether `inputTokens` includes cached tokens is UNVERIFIED (cacheReadTokens is reported separately; add it to match the Claude convention).

**Exit codes** (aI, 92296-92309): 0 success, 1 error, 3 auth, 4 permission denied/spend cap, 5 rate limited, 6 connection, 7 server, 8 max turns (`subtype:"max_turns"`, no `error` field), 9 no response (subtype `success` with empty `finalText`), 10 insufficient credits, 130 interrupted. Hashi should treat exit 8 and 9 as failures even though `error` is absent.

**Session.** New print sessions get `crypto.randomUUID()` ids (newPrintSession 56814-56816). `-r, --resume [name]` resumes by id or name; in print mode it requires a value (`--resume requires a session id in print mode`) and fails with `No session "<ref>" found to resume.` (resolveBasePrintSession 56869-56901). Also `-c/--continue` (latest print session), `--session <path|id>`, `--fork-session`, `--no-session` (in-memory only). Sessions are cwd-scoped (createSessionStoreV3 with cwd), so resume must use the same workspace (UNVERIFIED in detail). `--verbose` writes `session: <id>` to stderr.

**Permissions (safe by default).** Print mode installs a `print-permission-gate` mod that blocks `edit_file`, `write_file`, `shell_command`, `monitor_command`, `kill_shell` and tells the model `Tool "<name>" requires permissions. Use --yolo (or --dangerously-skip-permissions) to enable file writes and shell commands in print mode.` (TI set 92402-92408; createPrintPermissionGateMod / resolvePrintHarnessMods 56957-56973). Only `--dangerously-skip-permissions` / `--yolo` removes the gate (and also loads project mods, printMode 146347-146351). Interactive-only tools (ask_user_question, exit_plan_mode, ...) are withheld headless unless `--tools-all` / `--tools-enable`. `--permission-mode bypass` maps to bypass in the harness (resolveLaunchPermissionMode 146281-146289; choices include yolo, bypass, dont-ask, bypassPermissions at 110803-110815); whether it defeats the gate is UNVERIFIED, so it is listed unsafe. A settings flag `permissions.disableBypass` can force-disable bypass (~146990). Reads are not gated. No git repo or trust step is needed in print mode (trust prompt is interactive only; resolveStartupTrust 56783).

**Model.** `-m, --model <model>`; unknown ids are rejected (resolveModelFlagOrExit, ~146963). Special case: a TypeSafe System One decision model id switches to a different JSON protocol (runSystemOnePrint 146594-146660), avoid those ids.

**Auth.** `COMMAND_CODE_API_KEY` env (uo, 77889) wins over the stored key in `$HOME/.commandcode/auth.json` (getCommandAuthKey 5327-5332; file names auth.json / auth.staging.json / auth.local.json, 77890-77894). Not authenticated: `Error: Not authenticated. Please run "<bin> login" first.` exit 3 (bI 92393). BYOK provider keys live in ~/.commandcode/config.json; `--local-only` / `CMD_LOCAL_ONLY=1` sends nothing to Command Code (5528, 5547). **config_dir_env is null**: the auth/config dir is `${HOME or USERPROFILE}/.commandcode` (homeDir / resolveCommandAuthDir 5260-5263), with no dedicated override; relocating needs a per-account `HOME`, which also moves everything else.

**Limits** (classifyPrintModeError 56126-56200): rate limit `Error: Rate limit exceeded. Please wait a moment and try again.` or the window message `You've reached today's limit on <model>` / `You've reached your <window> usage limit.` (formatWindowLimitMessage); `Insufficient credits for Command Code. Buy extra usage ... or upgrade` exit 10; spend cap errors exit 4.

**Env.** `COMMANDCODE_SKIP_UPDATES` (or any `CI`) skips the update system (shouldSkipUpdateSystem 56344-56350); `--no-auto-update` also pinned. `CMD_DISABLE_REMOTE_MODEL_CATALOG` exists (78024). Telemetry opt-out env not found (UNVERIFIED). Slash commands: print mode passes the query straight to `harness.runTurn` (runHarnessQuery 9399-9424), so a leading `/` is probably sent to the model as text (UNVERIFIED).

**Docs** at https://commandcode.ai/docs were not fetched; everything above is from the bundle.

## Success output (derived from source or docs, not recorded)

```
{"type":"event","event":{"type":"run_start","sessionId":"6f1d2c3b-9a8e-4f7d-8c6b-5a4e3d2c1b0a"}}
{"type":"event","event":{"type":"text_delta","delta":"`add` returns the sum of its two arguments."}}
{"type":"event","event":{"type":"model_request_end","model":"claude-sonnet-4-6","usage":{"inputTokens":5210,"outputTokens":12,"cacheReadTokens":4096,"cacheWriteTokens":0},"stopReason":"end_turn"}}
{"type":"event","event":{"type":"message_end","content":[{"type":"text","text":"`add` returns the sum of its two arguments."}]}}
{"type":"event","event":{"type":"turn_end","turnNumber":1,"hadToolCalls":false,"usage":{"inputTokens":5210,"outputTokens":12,"cacheReadTokens":4096,"cacheWriteTokens":0}}}
{"type":"result","subtype":"success","sessionId":"6f1d2c3b-9a8e-4f7d-8c6b-5a4e3d2c1b0a","stopReason":"end_turn","usage":{"inputTokens":5210,"outputTokens":12,"cacheReadTokens":4096,"cacheWriteTokens":0},"durationMs":3120,"finalText":"`add` returns the sum of its two arguments."}
```

## Error output

```
{"type":"event","event":{"type":"run_start","sessionId":"6f1d2c3b-9a8e-4f7d-8c6b-5a4e3d2c1b0a"}}
{"type":"event","event":{"type":"run_error","error":{"name":"RateLimitError","message":"429 Too Many Requests"}}}
{"type":"result","subtype":"error","sessionId":"6f1d2c3b-9a8e-4f7d-8c6b-5a4e3d2c1b0a","usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0},"durationMs":840,"finalText":"","error":"Error: Rate limit exceeded. Please wait a moment and try again."}
```
