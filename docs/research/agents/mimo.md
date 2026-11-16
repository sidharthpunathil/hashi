# MiMo Code (`mimo`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `MIMO` in `src/agent/specs.rs`.

- **Source:** github:XiaomiMiMo/MiMo-Code (main, packages/cli 0.1.17) and npm:@mimo-ai/cli 0.1.17
- **License:** MIT
- **Version checked:** 0.1.17
- **Install:** curl -fsSL https://mimo.xiaomi.com/install | bash | powershell -ep Bypass -c "irm https://mimo.xiaomi.com/install.ps1 | iex" | npm i -g @mimo-ai/cli
- **Headless call:** `mimo run --format json`, prompt dashdash
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

**What it is.** Xiaomi's terminal coding agent, a fork of **opencode 1.x** (yargs CLI, `packages/cli`, root package.json still named `opencode`), rebranded `mimo` (`packages/cli/src/index.ts:79-80` `.parserConfiguration({"populate--": true}).scriptName("mimo")`). It is NOT a Claude Code or Gemini CLI fork and its JSON output does not match Claude Code's result object: it is opencode 1.x's JSONL event stream. Fixtures derived from source, not run.

**Headless.** `mimo run [message..]` (`packages/cli/src/cli/cmd/run.ts:221-308`). Flags: `-c/--continue`, `-s/--session <id>`, `--fork`, `-m/--model provider/model`, `--agent`, `--format default|json`, `-f/--file`, `--title`, `--attach <url>`, `-p/--password` (note: `-p` is NOT prompt/print here), `--dir`, `--port`, `--variant`, `--thinking`, `--role user|assistant`, `--dangerously-skip-permissions` (alias `--yolo`).

**Prompt.** `--` is honoured: `populate--` is on (index.ts:79) and run.ts:310 joins `[...args.message, ...(args["--"] || [])]`. **Quoting gotcha inherited from opencode 1.x** (run.ts:310-312): each argv element containing a space is wrapped in literal double quotes before joining, so a single element `fix the bug` reaches the model as `"fix the bug"`. Split the prompt on single spaces into separate argv elements after `--` to rebuild it exactly (as for opencode in docs/research/agent-clis.md). Stdin is read when not a TTY and appended after a newline (run.ts:349); /dev/null adds an empty line. cwd is `process.cwd()` (run.ts:698 `bootstrap(process.cwd(), ...)`), no `PWD` gotcha.

**Output `--format json`** (run.ts:417-421): one JSON object per line `{type, timestamp(ms), sessionID, ...data}`. Types: `step_start {part}`, `text {part{type:"text",text,time}}` (only completed text parts, run.ts:494-495), `reasoning` (only with `--thinking`), `tool_use {part{type:"tool",tool,state{status,input,output|error,...}}}` (run.ts:462-463), `step_finish {part{type:"step-finish",reason,cost,tokens{total?,input,output,reasoning,cache{read,write}}}}` (schema `session/message-v2.ts:399-416`), `error {error}` where `error` is the NamedError `{name, data{message,...}}` (run.ts:522-531). No terminal result event; the run ends at process exit. Parser: text = last `text` event after the last `step_start`; session = top-level `sessionID`; usage = sum of `part.tokens.input` (+ cache read/write if Hashi follows the Claude convention) and `part.tokens.output`; cost = sum of `part.cost` (USD). Non-JSON UI lines (permission auto-reject notices, migration notice) go to stderr (`cli/ui.ts:32-39` writes to stderr).

**Resume.** `-s/--session <id>` continues an existing session (run.ts:380-395); ids are opencode-style `ses_...`. Unlike opencode V2 it does NOT create a session with a caller-chosen id (UNVERIFIED what happens on an unknown id: the prompt call likely fails).

**Model.** `-m provider/model` (`Provider.parseModel`, run.ts:668); `--variant` sets reasoning effort.

**Permissions (important).** Defaults are opencode 1.x: build agent ruleset `"*": "allow"` with only `doom_loop` and `*.env` reads asking (`agent/agent.ts:128-144`), so **bash and edits run without asking in `run`**. Asks are auto-rejected unless `--yolo` (run.ts:534-553). `question` and `plan_exit` are denied in run (run.ts:361-372). A bash delete guard forces an extra `bash_delete` ask for rm/git reset --hard etc. unless `MIMOCODE_AUTO_APPROVE_DELETE=true` (`flag/flag.ts:112-127`); in `run` that ask is rejected. For a read-only run set `MIMOCODE_CONFIG_CONTENT='{"permission":{"bash":"deny","edit":"deny"}}'` (env `MIMOCODE_CONFIG_CONTENT`, flag.ts:95; same config shape as opencode, UNVERIFIED for this fork) or `--agent plan`. `MIMOCODE_PERMISSION` env also exists (flag.ts:110, format UNVERIFIED). No OS sandbox.

**Env.** `MIMOCODE_HOME` (absolute path) relocates config/data/cache/state (`packages/shared/src/global.ts:26-50`); otherwise XDG `~/.config/mimocode`, `~/.local/share/mimocode` etc. `MIMOCODE_DISABLE_AUTOUPDATE`, `MIMOCODE_ENABLE_ANALYSIS=false` (analytics are ON by default, flag.ts:104-106), `MIMOCODE_DISABLE_CLAUDE_CODE` stops inheriting `~/.claude/CLAUDE.md`, Claude MCP and provider env (flag.ts:39-46), `MIMOCODE_DISABLE_PROVIDER_ENV`/`MIMOCODE_MIMO_ONLY` stop env-based provider auto-detection (useful for separate accounts). Credentials live in `auth.json` under the data dir; `MIMOCODE_AUTH_CONTENT` can carry the whole auth.json (`util/credential-env.ts:4-15`). The built-in Xiaomi provider id is `xiaomi`/`mimo` (`provider/error.ts:240`); its API-key env name is not in source (comes from the models.dev catalog; `XIAOMI_API_KEY` is UNVERIFIED), so `api_key_env` is null. First run prints `Performing one time database migration...` to stderr (index.ts:126-130).

**Rate limits.** `session/retry.ts:217-225` treats `rate limit`, `rate_limit`, `rate limited`, `rate increased too quickly` and HTTP 429 as retryable (up to `RATE_LIMIT_MAX_RETRIES = 5`, retry.ts:28); terminal: `quota exhausted` (retry.ts:405-408), `FreeUsageLimitError` -> `Usage limit reached`, `SubscriptionUsageLimitError` (retry.ts:410-413); `insufficient_quota` -> `Quota exceeded. Check your plan and billing details.` (`provider/error.ts:361-364`). Exit code 1 on error.

**Git repo.** Not required (opencode behaviour, UNVERIFIED for the fork). **Windows:** native via install.ps1 or npm.

## Success output (derived from source or docs, not recorded)

```
{"type":"step_start","timestamp":1763287200123,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_01","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_01","type":"step-start"}}
{"type":"tool_use","timestamp":1763287200900,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_02","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_01","type":"tool","callID":"call_01","tool":"read","state":{"status":"completed","input":{"file_path":"src/lib.rs"},"output":"pub fn add(a: i32, b: i32) -> i32 { a + b }","title":"src/lib.rs","metadata":{},"time":{"start":1763287200500,"end":1763287200890}}}}
{"type":"step_finish","timestamp":1763287200950,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_03","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_01","type":"step-finish","reason":"tool-calls","cost":0.0011,"tokens":{"input":5123,"output":40,"reasoning":0,"cache":{"read":0,"write":0}}}}
{"type":"step_start","timestamp":1763287201000,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_04","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_02","type":"step-start"}}
{"type":"text","timestamp":1763287201880,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_05","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_02","type":"text","text":"`add` returns the sum of its two arguments.","time":{"start":1763287201200,"end":1763287201880}}}
{"type":"step_finish","timestamp":1763287201890,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","part":{"id":"prt_06","sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","messageID":"msg_02","type":"step-finish","reason":"stop","cost":0.0013,"tokens":{"input":5210,"output":12,"reasoning":0,"cache":{"read":4096,"write":0}}}}
```

## Error output

```
{"type":"error","timestamp":1763287200500,"sessionID":"ses_1a2b3c4d5e6fMiMoHashi01","error":{"name":"APIError","data":{"message":"Too Many Requests","statusCode":429,"isRetryable":true}}}
```
