# Coding-agent CLIs Hashi could drive

Research date: 2026-11-05. Scope: CLIs that can run one prompt headlessly, the way Hashi already drives `claude -p --output-format json` and `codex exec --json`.

## What a Hashi backend needs

From `src/agent/mod.rs`, `src/agent/claude.rs`, `src/agent/codex.rs` and `src/agent/runner.rs`:

- `build_args(cfg, Invocation { prompt, resume, model }) -> Vec<OsString>` is pure. The prompt is never parseable as a flag (today: always after `--`). `model` and `resume` pass `is_safe_arg` (alphanumeric first char, max 128 chars, `[A-Za-z0-9-_.:/@[]]`).
- `parse_output(stdout) -> AgentOutput { text, session_id, usage: { input_tokens, output_tokens, cost_usd } }` is pure. `AgentError::Reported` for a failure the agent reported, `AgentError::Parse` otherwise. If parsing fails and the exit was non-zero, `execute` substitutes stderr.
- The runner spawns with no shell, `stdin = /dev/null`, `cwd = per-token workspace` (not necessarily a git repo), `NO_COLOR=1`, `TERM=dumb`, a timeout and an output cap, and `kill_on_drop`.

Two things several candidates need that the runner cannot express today:

1. **Per-backend environment variables** (for example `PWD` for opencode, `GEMINI_CLI_NO_RELAUNCH` for Gemini, `GOOSE_DISABLE_KEYRING` for Goose, `CLINE_NO_AUTO_UPDATE` for Cline). `runner::run` sets a fixed env, so an `env: Vec<(OsString, OsString)>` field on `AgentConfig` or a per-kind hook would be needed.
2. **Prompts in `--flag=<value>` form.** Several CLIs have no positional prompt (Goose, Kimi Code, Aider, Copilot) or do not document `--`. An argv element `--prompt=<text>` is safe in clap, yargs, commander and argparse because everything after the first `=` is the value, even when it starts with `-`.

A cross-cutting hazard: Qwen Code, Aider and (UNVERIFIED) others treat a prompt beginning with `/` as a slash command, and Aider treats a leading `!` as a shell command. Hashi should decide whether to neutralise a leading `/` or `!` (for example by prefixing a space) for those backends.

## Summary table

"Installed here" was checked with `command -v` and `--version` on this machine. Stars are from the GitHub API on 2026-11-05.

| CLI | Binary | Headless flag | JSON output | Resume by id | Usage / cost | Auth | Unattended sandbox default | Open source (stars) | Installed here |
|---|---|---|---|---|---|---|---|---|---|
| opencode (V2) | `opencode` | `run` | `--format json` (JSONL) | `-s ses_...` (can pre-assign) | tokens + USD cost per step | provider env vars or `opencode auth` | asks are rejected in `run` unless `--auto` | yes, MIT (224.7k) | yes, `~/.opencode/bin/opencode` (1.x), but it is SIGKILLed on launch, version not readable |
| Gemini CLI | `gemini` | positional prompt or `-p` | `-o json`, `-o stream-json` | `-r <uuid>`, `--session-id <uuid>` | tokens, no cost | `GEMINI_API_KEY`, Vertex, or Google OAuth | headless `default` mode denies shell and writes | yes, Apache-2.0 (108.7k) | no |
| Qwen Code | `qwen` | positional prompt | `-o stream-json` (Claude Code shape), `-o json` | `-r <uuid>`, `--session-id <uuid>` | tokens, no cost | `OPENAI_API_KEY` + base URL, Anthropic, Gemini, DashScope | headless denies shell and edits unless allowlisted | yes, Apache-2.0 (29.0k) | no |
| Cursor CLI | `agent` (legacy `cursor-agent`) | `-p` | `--output-format json`, `stream-json` | `--resume <id>` | none documented | `CURSOR_API_KEY` or `agent login` | without `--force` it proposes but does not apply | no | no |
| GitHub Copilot CLI | `copilot` | `-p`/`--prompt=` | `--output-format=json` (JSONL, schema undocumented) | `--resume=<id>`, `--session-id=<uuid>` | premium requests only; tokens via side file | `COPILOT_GITHUB_TOKEN`/`GH_TOKEN` + Copilot plan | requests needing approval are denied | no (README only, 11.4k) | no |
| Goose | `goose` | `run --text=` | `--output-format json`, `stream-json` | `-r -n <name>` or `-r --session-id <id>` | tokens + USD (session cumulative) | provider env vars, keyring, config | `GOOSE_MODE=auto` runs tools; `approve` errors headless | yes, Apache-2.0 (56.2k) | yes, 1.44.0 |
| Crush | `crush` | `run` | none (plain text) | `-s <uuid>` | only via `crush session show --json` | provider env vars, config | `run` auto-approves every tool | source-available, FSL-1.1-MIT (29.0k) | no |
| Amp | `amp` | `-x`/`--execute` | `--stream-json` (Claude Code shape) | `amp threads continue <T-id>` | tokens, no cost | `AMP_API_KEY` | no approvals by default: tools just run | no | no |
| Factory Droid | `droid` | `exec` | `-o json`, `-o stream-json` | `-s <id>` | tokens + Factory credits | `FACTORY_API_KEY` | read-only unless `--auto low/medium/high` | no | no |
| Aider | `aider` | `--message=` | none (rich text, scrape `Tokens:` line) | none (`--restore-chat-history` only) | scraped, abbreviated | provider env vars | confirms default to yes on EOF stdin | yes, Apache-2.0 (50.0k) | no |
| Kimi Code | `kimi` | `--prompt=` | `--output-format stream-json` | `-S <id>` | none | `KIMI_MODEL_*` env or `/login` | print mode forces auto-approve | yes, MIT (8.1k; old `kimi-cli` archived, 11.4k) | no |
| Cline CLI v3 | `cline` | positional prompt | `--json` (NDJSON) | not headless (`--id` forces TUI) | tokens + USD | provider env vars, `CLINE_API_KEY` | auto-approves by default; `--auto-approve false` denies | yes, Apache-2.0 (71.3k) | no |

Also installed here: `claude` 2.1.318 and `codex` 0.166.0, the two existing backends.

---

## 1. opencode

Source: `github:sst/opencode` (redirects to `anomalyco/opencode`), cached at `~/.opensrc/repos/github.com/sst/opencode/dev`. **Version warning:** the cached source is **opencode V2** (`@opencode/cli` 2.2.5, CHANGELOG 2026-10-30, effect/cli parser, code in `packages/cli`). The legacy yargs `packages/opencode/src/cli/cmd/run.ts` is gone. The binary installed here is a 1.x build (`~/.opencode/package.json` pins `@opencode-ai/plugin` 1.4.5). Everything below is V2; the 1.x `--format json` schema is UNVERIFIED and may differ, so pin a V2 version before integrating.

- **Install:** `curl -fsSL https://opencode.ai/v2/install | bash` (to `$HOME/.opencode/bin`), `npm i -g @opencode/cli@latest`, or `brew install anomalyco/tap/opencode-v2` (README.md:50-55).
- **Binary:** `opencode`.
- **Non-interactive:** `opencode run [message..]` (packages/cli/src/commands/commands.ts:400-451). Flags: `-c/--continue`, `-s/--session <id>`, `--fork`, `-m/--model provider/model#variant`, `--agent`, `--format default|json`, `-f/--file`, `--title`, `--thinking`, `--auto` (hidden aliases `--yolo`, `--dangerously-skip-permissions`, lines 37-47), and the globals `--standalone`, `--server <url>`.
- **Server attachment:** `run` attaches to an already running background opencode server if one exists, otherwise spawns a private `opencode serve --stdio --port 0` (handlers/run.ts:10, services/standalone.ts, server-connection.ts:49-58). **Always pass `--standalone`** so requests from different tokens never share a server.
- **Prompt passing:**
  - `--` is honoured: packages/cli/test/mini.test.ts:146 passes `run ... -- --foo`, and flag.test.ts:44-45 shows option-like text after `--` landing in `message`.
  - **Quoting gotcha (verified):** `run/run.ts:179-182`:
    ```ts
    const value = message.map((part) => (part.includes(" ") ? `"${part.replace(/"/g, '\\"')}"` : part)).join(" ")
    ```
    A single argv element `fix the bug` reaches the model as `"fix the bug"` with literal quotes. Either split the prompt on single spaces into separate argv elements after `--` (the join rebuilds it exactly, though tabs and newlines inside a piece are kept and pieces containing no space are untouched), or accept the quotes.
  - Stdin is read when not a TTY and appended after the message (run.ts:81). `/dev/null` gives `""`, harmless.
  - **cwd gotcha:** run.ts:78 resolves the project root as `process.env.PWD ?? process.cwd()`. Hashi inherits its own `PWD`, so opencode would operate in Hashi's directory. Set `PWD=<workspace>` (or remove `PWD`) for this backend.
- **Model:** `-m provider/model` (optionally `#variant`). packages/schema/src/model.ts:27-41 requires a `/`, else "Invalid model reference". Passes `is_safe_arg` unless the variant uses `#`.
- **Resume:** `-s <id>`. If the id does not exist, a session is created with that id (session-target.ts:62-70, 106-110); ids must start with `ses`. So Hashi can pre-assign `ses_<random>` ids. `-c` resumes the newest session in the directory.
- **Output (`--format json`):** JSONL, envelope `{"type", "timestamp" (ms), "sessionID", ...}` (noninteractive.ts:103-107).

  | type | source | payload |
  |---|---|---|
  | `step_start` | noninteractive.ts:315-337 | `part{id,sessionID,messageID,type:"step-start",snapshot?}` |
  | `text` | 348-362 | `part{...,type:"text",text,time{start,end}}`, one per complete text block |
  | `reasoning` | 373-388 | only with `--thinking` |
  | `tool_use` | 439-541 | `part{...,type:"tool",tool,state{status:"completed"\|"error",input,output?,error?,title?,metadata,time}}` |
  | `step_finish` | 543-557, 729-742 | `part{...,type:"step-finish",reason,cost,tokens{input,output,reasoning,cache{read,write}}}` |
  | `error` | 559-592, 871-876 | `error{type,message,status?,response?,retryAfterMs?}` |

  `reason` is `stop|length|tool-calls|content-filter|error|unknown` (schema/src/llm.ts:5). There is no terminal result event; the run ends at process exit.
- **Parser:** text = `part.text` of the `text` events after the last `step_start` (or all joined); session id = top-level `sessionID`; usage = sum of `part.tokens.input` (plus `cache.read`/`cache.write` to match the Claude convention) and `part.tokens.output`; cost = sum of `part.cost` (USD). Any `error` event means `Reported`.
- **Usage/cost:** yes, both.
- **Auth:** provider env vars such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` (models.dev env names, core/src/integration.ts:398, 701), or `opencode auth login` (stored in the SQLite data dir, core/src/credential/sql.ts). Subscription OAuth logins exist for some providers (UNVERIFIED for V2).
- **Permissions:** most tools default to allow, `external_directory`, `doom_loop` and `.env` reads default to ask (packages/web/src/content/docs/permissions.mdx:172-178). In `run` without `--auto`, every ask is answered reject and the model is told to continue (noninteractive.ts:189-217); forms and questions are cancelled (219-238). **Safe default:** omit `--auto`, and deny `bash`/`edit` through `OPENCODE_CONFIG_CONTENT` (core/src/config.ts:297) when a read-only agent is wanted. No OS sandbox.
- **Exit codes:** 0 success, 1 on any error event or bad flag, 130 on SIGINT (noninteractive.ts:584, 758-761). Errors before a session exists print `{"type":"error","sessionID":"",...}` and exit 1 (run.ts:265-279).
- **Git repo:** not required (file-access.ts:115 falls back to the directory; UNVERIFIED end to end).
- **Windows:** supported (desktop builds, `OPENCODE_GIT_BASH_PATH`); CLI Windows binaries UNVERIFIED.
- **Env:** `OPENCODE_DISABLE_AUTOUPDATE=1` (updater.ts:535), `OPENCODE_DISABLE_MODELS_FETCH`, `OPENCODE_CONFIG_DIR`. stderr carries ANSI codes regardless of `NO_COLOR`; stdout is clean in json mode.

**argv, new run** (prompt `fix the failing test`):
```
["run", "--standalone", "--format", "json", "-m", "anthropic/claude-sonnet-4-5", "-s", "ses_hashi7f3a9c21", "--", "fix", "the", "failing", "test"]
```
**argv, resume:**
```
["run", "--standalone", "--format", "json", "-s", "ses_hashi7f3a9c21", "--", "now", "add", "a", "comment"]
```

Fixture `opencode_success.jsonl`:
```jsonl
{"type":"step_start","timestamp":1762336800123,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9A","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9A","type":"step-start"}}
{"type":"tool_use","timestamp":1762336800900,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9B","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9A","type":"tool","tool":"read","state":{"status":"completed","input":{"filePath":"src/lib.rs"},"output":"pub fn add(a: i32, b: i32) -> i32 { a + b }","title":"src/lib.rs","metadata":{},"time":{"start":1762336800500,"end":1762336800890}}}}
{"type":"step_finish","timestamp":1762336800950,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9C","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9A","type":"step-finish","reason":"tool-calls","cost":0.0019,"tokens":{"input":5123,"output":40,"reasoning":0,"cache":{"read":0,"write":0}}}}
{"type":"step_start","timestamp":1762336801000,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9D","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9E","type":"step-start"}}
{"type":"text","timestamp":1762336801880,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9F","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9E","type":"text","text":"`add` returns the sum of its two arguments.","time":{"start":1762336801200,"end":1762336801880}}}
{"type":"step_finish","timestamp":1762336801890,"sessionID":"ses_hashi7f3a9c21","part":{"id":"prt_01JC9G","sessionID":"ses_hashi7f3a9c21","messageID":"msg_01JC9E","type":"step-finish","reason":"stop","cost":0.00231,"tokens":{"input":5210,"output":12,"reasoning":0,"cache":{"read":4096,"write":0}}}}
```
Fixture `opencode_failure.jsonl` (the `error.type` value is illustrative; the schema only says string):
```jsonl
{"type":"error","timestamp":1762336800500,"sessionID":"ses_hashi7f3a9c21","error":{"type":"api","message":"Invalid API key","status":401}}
```

---

## 2. Gemini CLI

Source: `github:google-gemini/gemini-cli`, cached at `~/.opensrc/repos/github.com/google-gemini/gemini-cli/main` (0.69.0-nightly).

- **Install:** `npm install -g @google/gemini-cli` (Node 20+) or `brew install gemini-cli` (README:48, 54).
- **Binary:** `gemini`.
- **Non-interactive:** `-p/--prompt <string>` or the positional `[query..]` (packages/cli/src/config/config.ts:289-511). The positional runs headless whenever stdin or stdout is not a TTY or `CI` is set (config.ts:553-564, core/src/utils/headless.ts:30-49), which is always true under Hashi. `-p` together with a positional is an error (config.ts:259).
- **Prompt passing:** use `--prompt=<text>`: the `=` form is unambiguous in yargs. `-p -x` (separate value starting with `-`) is likely rejected, and whether text after `--` populates `[query..]` is standard yargs 17 behaviour but UNVERIFIED (config.ts has no `populate--` setting). Stdin, if not a TTY, is read with a 500 ms timeout and 8 MB cap (utils/readStdin.ts) and placed before the prompt (gemini.tsx:944-951); `/dev/null` is harmless.
- **Model:** `-m <name>` (bare names such as `gemini-2.5-pro`), or `GEMINI_MODEL`.
- **Resume:** `-r/--resume <uuid|index|latest>` (config.ts:412-431, utils/sessionUtils.ts:468-498). `--session-id <id>` starts a new session with a caller-chosen id (`[A-Za-z0-9-_]`) and exits 42 if it already exists (gemini.tsx:413-418); it cannot be combined with `-r`. Sessions live under `~/.gemini/tmp/<project-id>/chats`, so **resume must use the same cwd** (Hashi's per-token workspace satisfies this). An unknown id exits 42.
- **Output:** `-o text|json|stream-json` (config.ts:478-484).
  - `json`: one pretty-printed object `{session_id, response, stats, error?, warnings?}` (core/src/output/types.ts:21-27). `response` is only the last turn's text (nonInteractiveCli.ts:315). Fatal errors go to stderr in this mode (cli/src/utils/errors.ts:94-106).
  - `stream-json` (recommended, types.ts:30-117): `init {timestamp, session_id, model}`, `message {role, content, delta?}` (assistant content arrives as deltas), `tool_use {tool_name, tool_id, parameters}`, `tool_result {tool_id, status, output?, error?}`, `error {severity, message}` (non-fatal), and `result {status:"success"|"error", error?, stats?{total_tokens, input_tokens, output_tokens, cached, input, duration_ms, tool_calls, models{}}}`. Fatal errors also emit `result` with `status:"error"` (errors.ts:76-93).
- **Parser:** session id from `init.session_id` (not repeated on `result`); text = assistant `content` deltas concatenated, reset after each `tool_result` so the final answer is what remains; usage from `result.stats.input_tokens`/`output_tokens`; `cost_usd: None`; `result.status == "error"` means `Reported(result.error.message)`.
- **Usage/cost:** tokens yes, cost no.
- **Auth:** `GEMINI_API_KEY`; Vertex (`GOOGLE_GENAI_USE_VERTEXAI=true` plus GCP vars); `GOOGLE_GENAI_USE_GCA=true`; or a Google OAuth login done once interactively, cached in `~/.gemini/oauth_creds.json` (core/src/config/storage.ts:22, 256). No auth exits 41. `GEMINI_CLI_HOME` relocates `~/.gemini`.
- **Folder trust (blocks Hashi unless handled):** trust checking defaults on (trustedFolders.ts:32-35). Headless in an untrusted cwd throws `FatalUntrustedWorkspaceError`, exit 55 (utils/userStartupWarnings.ts:84-104). Pass `--skip-trust` (sets `GEMINI_CLI_TRUST_WORKSPACE=true`, config.ts:527).
- **Approval/sandbox:** `--approval-mode default|auto_edit|yolo|plan` or `-y`. In headless `default`, anything that would ask becomes DENY (core/src/policy/policy-engine.ts:301, 1010): shell and file writes are refused and the model sees an error result. `-s/--sandbox` (docker, podman or sandbox-exec) is off by default. **Safe default:** `--approval-mode default` (effectively read-only); `auto_edit` if edits are wanted; `yolo` only together with `--sandbox`.
- **Exit codes** (core/src/utils/exitCodes.ts): 0 ok, 1 bad argv or unknown, 41 auth, 42 input, 44 sandbox, 52 config, 53 turn limit, 54 fatal tool error, 55 untrusted folder, 130 cancel. `handleError` may exit with an HTTP status mod 256 (errors.ts:23-44). An empty or blocked model response yields `status:"error"` with exit 0 (nonInteractiveCli.ts:620-629), so the parser must check `status`.
- **Process model:** index.ts:56-125 relaunches itself as a child node process. Hashi's `kill_on_drop` SIGKILLs only the parent and would orphan the child. Set `GEMINI_CLI_NO_RELAUNCH=1`.
- **Git repo:** not required. **Windows:** Node CLI, supported (details UNVERIFIED).
- **Env:** `GEMINI_TELEMETRY_ENABLED=false`; `NO_COLOR` honoured; update check only in interactive mode.

**argv, new run:**
```
["-o", "stream-json", "--skip-trust", "--approval-mode", "default", "-m", "gemini-2.5-pro", "--session-id", "3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41", "--prompt=<prompt>"]
```
**argv, resume:**
```
["-o", "stream-json", "--skip-trust", "--approval-mode", "default", "-r", "3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41", "--prompt=<prompt>"]
```

Fixture `gemini_success.jsonl`:
```jsonl
{"type":"init","timestamp":"2026-11-05T10:00:00.000Z","session_id":"3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41","model":"gemini-2.5-pro"}
{"type":"message","timestamp":"2026-11-05T10:00:00.010Z","role":"user","content":"what does src/lib.rs do?"}
{"type":"tool_use","timestamp":"2026-11-05T10:00:00.900Z","tool_name":"read_file","tool_id":"read_file-1762336800-0","parameters":{"file_path":"src/lib.rs"}}
{"type":"tool_result","timestamp":"2026-11-05T10:00:00.950Z","tool_id":"read_file-1762336800-0","status":"success","output":""}
{"type":"message","timestamp":"2026-11-05T10:00:01.200Z","role":"assistant","content":"It defines `add`, which","delta":true}
{"type":"message","timestamp":"2026-11-05T10:00:01.300Z","role":"assistant","content":" sums two integers.","delta":true}
{"type":"result","timestamp":"2026-11-05T10:00:01.310Z","status":"success","stats":{"total_tokens":8150,"input_tokens":8120,"output_tokens":30,"cached":0,"input":8120,"duration_ms":1310,"tool_calls":1,"models":{"gemini-2.5-pro":{"total_tokens":8150,"input_tokens":8120,"output_tokens":30,"cached":0,"input":8120}}}}
```
Fixture `gemini_failure.jsonl` (message text illustrative):
```jsonl
{"type":"init","timestamp":"2026-11-05T10:00:00.000Z","session_id":"9b2d4c11-0e6f-4a7b-8c3d-2f1e0a9b8c7d","model":"gemini-2.5-pro"}
{"type":"result","timestamp":"2026-11-05T10:00:00.420Z","status":"error","error":{"type":"Error","message":"[API Error: 429 quota exceeded]"},"stats":{"total_tokens":0,"input_tokens":0,"output_tokens":0,"cached":0,"input":0,"duration_ms":420,"tool_calls":0,"models":{}}}
```

---

## 3. Qwen Code

Source: `github:QwenLM/qwen-code` (v0.26.7), a Gemini CLI fork whose headless output copies Claude Code's `stream-json`.

- **Install:** `npm i -g @qwen-code/qwen-code@latest` (Node 22+), `brew install qwen-code`, or standalone sh/PowerShell scripts (README.md:34-62).
- **Binary:** `qwen` (packages/cli/package.json:13-14).
- **Non-interactive:** positional `[query..]` (preferred; `-p/--prompt` is deprecated, config/top-level-options.ts:173-174). A prompt means one-shot unless `-i` (config/config.ts:999-1017). Non-TTY stdin is prepended (llm.tsx:1661-1667); `/dev/null` is harmless.
- **Prompt passing:** `-- <prompt>` is expected to land in `query` (yargs, UNVERIFIED); `--prompt=<prompt>` is the fallback. A prompt equal to a subcommand name (`mcp`, `review`) may misroute (config.ts:944-986), and a leading `/` runs a slash command (nonInteractiveCliCommands.ts). `--prompt=` avoids the subcommand issue.
- **Model:** `-m/--model`.
- **Resume:** `-r/--resume <id|title>`, `-c/--continue`. `--session-id <uuid>` starts a new session with a chosen UUID and cannot be combined with resume (config.ts:826-846). `session_id` is on every stream message.
- **Output `-o stream-json`** (nonInteractive/types.ts): `system/init {uuid, session_id, cwd, tools, mcp_servers, model, permission_mode, slash_commands, qwen_code_version, agents}` (nonInteractiveHelpers.ts:236-249); `assistant {message{id, role, model, content[text|thinking|tool_use], usage}}`; `user` (tool results); `stream_event` only with `--include-partial-messages`; terminal `result {subtype: success|error_max_turns|error_during_execution, is_error, duration_ms, duration_api_ms, num_turns, result, usage{input_tokens, output_tokens, cache_creation_input_tokens?, cache_read_input_tokens?, total_tokens?}, permission_denials[], error?{message}}` (BaseJsonOutputAdapter.ts:1256-1330). `-o json` prints one array of all messages at the end (JsonOutputAdapter.ts:116-135).
- **Parser:** nearly identical to `claude::parse_output`: last JSON line with `type == "result"`, `result`, `session_id`, `usage`, `is_error`. Differences: no `total_cost_usd`; the error reason is `error.message`, not `result`. Filter on `type == "result"` rather than "last line starting with `{`".
- **Usage/cost:** tokens yes, cost no (types.ts:40-47).
- **Auth:** Qwen OAuth free tier ended 2026-04-15 (docs/users/configuration/auth.md:12, 99). Env-only: `OPENAI_API_KEY` + `OPENAI_BASE_URL` + `OPENAI_MODEL`/`QWEN_MODEL`; or `ANTHROPIC_API_KEY`/`ANTHROPIC_MODEL`/`ANTHROPIC_BASE_URL`; or `GEMINI_API_KEY` + `GEMINI_MODEL`; or Alibaba `BAILIAN_CODING_PLAN_API_KEY` (utils/modelConfigUtils.ts:220-250). `--auth-type openai|openai-responses|anthropic|qwen-oauth|gemini|vertex-ai`.
- **Approval:** `--approval-mode plan|default|auto-edit|auto|yolo`, default `auto` (config.ts:1995-2018). Headless with `plan`, `default` or `auto` denies shell, monitor, edit and write_file unless allowlisted; `auto-edit` denies only shell and monitor (config.ts:2195-2240). Denials go to `permission_denials` (coreToolScheduler.ts:4448-4467). **Safe default:** no flag (read-only effect). Extras: `--max-wall-time`, `--max-tool-calls` (exit 55), `--safe-mode`, `--bare`. Folder trust is off by default.
- **Exit codes:** 1 generic, 41 auth, 42 input, 44 sandbox, 52 config, 53 turn limit, 54 tool, 55 budget, 130 SIGINT (core/src/utils/errors.ts:311-364). Early fatal errors go to stderr as `{"error":{"type","message","code"}}` in json mode (core/src/output/json-formatter.ts:30-37).
- **Git:** not required. **Windows:** yes. **Relaunch:** inherited from Gemini CLI; whether `GEMINI_CLI_NO_RELAUNCH` or a Qwen-named equivalent applies is UNVERIFIED.

**argv, new run:**
```
["-o", "stream-json", "-m", "qwen3-coder-plus", "--session-id", "9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f", "--prompt=<prompt>"]
```
**argv, resume:**
```
["-o", "stream-json", "-r", "9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f", "--prompt=<prompt>"]
```

Fixture `qwen_success.jsonl`:
```jsonl
{"type":"system","subtype":"init","uuid":"9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f","session_id":"9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f","cwd":"/srv/hashi/ws/u1","tools":["read_file","glob","grep_search"],"mcp_servers":[],"model":"qwen3-coder-plus","permission_mode":"default","slash_commands":[],"qwen_code_version":"0.26.7","agents":[]}
{"type":"assistant","uuid":"c2a1e0d4-1111-4a2b-9c3d-000000000001","session_id":"9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f","message":{"id":"msg_1","type":"message","role":"assistant","model":"qwen3-coder-plus","content":[{"type":"text","text":"Paris."}],"usage":{"input_tokens":812,"output_tokens":5}},"parent_tool_use_id":null}
{"type":"result","subtype":"success","uuid":"c2a1e0d4-1111-4a2b-9c3d-000000000002","session_id":"9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f","is_error":false,"duration_ms":4210,"duration_api_ms":3900,"num_turns":1,"result":"Paris.","usage":{"input_tokens":812,"output_tokens":5,"cache_read_input_tokens":0},"permission_denials":[]}
```
Fixture `qwen_failure.jsonl`:
```jsonl
{"type":"result","subtype":"error_during_execution","uuid":"c2a1e0d4-1111-4a2b-9c3d-000000000003","session_id":"9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f","is_error":true,"duration_ms":120,"duration_api_ms":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"permission_denials":[],"error":{"message":"401 Incorrect API key provided"}}
```

---

## 4. Cursor CLI

Closed source. Everything here is from official docs (cursor.com/docs/cli/headless, /reference/parameters, /reference/output-format, /reference/permissions); nothing verified from source.

- **Install:** `curl https://cursor.com/install -fsS | bash` (macOS, Linux, WSL); Windows: `irm 'https://cursor.com/install?win32=true' | iex`.
- **Binary:** `agent` (legacy name `cursor-agent`). `AgentKind::binary_name` would need to try both.
- **Non-interactive:** `-p/--print`, with `--output-format text|json|stream-json`; `--stream-partial-output` for deltas.
- **Prompt passing:** positional `[prompt...]`. `--` handling is UNVERIFIED; test before relying on it.
- **Model:** `--model <id>`; `agent models` lists them.
- **Resume:** `--resume <chatId>` using the `session_id` from output; `--continue` for the latest; `agent create-chat` returns a fresh id.
- **Output `json`:** one object on success: `{type:"result", subtype:"success", is_error:false, duration_ms, duration_api_ms, result, session_id, request_id?}`. On failure **no JSON is written**: non-zero exit and stderr. `stream-json`: `system/init {apiKeySource, cwd, session_id, model, permissionMode}`, `user`, `assistant`, `tool_call` (started/completed), terminal `result`.
- **Parser:** essentially `claude::parse_output` minus cost and usage. When stdout has no JSON, the existing `execute` fallback to stderr already does the right thing.
- **Usage/cost:** none documented. Third-party adapters read an undocumented camelCase `usage{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens}` (UNVERIFIED); parse as optional.
- **Auth:** `CURSOR_API_KEY` (or `--api-key`), or `agent login`. Billed to the Cursor plan.
- **Permissions:** print mode without `-f/--force` (alias `--yolo`) only proposes changes, it does not write files. Allow/deny rules (`Shell(git)`, `Read(glob)`, `Write(glob)`, `WebFetch(domain)`, `Mcp(server:tool)`) in `~/.cursor/cli-config.json` or `<proj>/.cursor/cli.json`, deny wins. `--sandbox enabled|disabled`, `--trust` (trust the workspace headlessly), `--approve-mcps`. What happens to a shell command that needs approval without `--force` is UNVERIFIED. **Safe default:** `--trust --sandbox enabled`, no `--force`, plus an allowlist.
- **Exit codes:** 0 success, non-zero otherwise (no codes documented).
- **Git:** not required (`-w/--worktree` needs it). **Windows:** native installer.

**argv, new run:** `["-p", "--output-format", "json", "--trust", "--sandbox", "enabled", "--model", "<model>", "--", "<prompt>"]` (`--` UNVERIFIED)
**argv, resume:** `["-p", "--output-format", "json", "--trust", "--sandbox", "enabled", "--resume", "<session_id>", "--", "<prompt>"]`

Fixture `cursor_success.json` (from the docs' documented shape):
```json
{"type":"result","subtype":"success","is_error":false,"duration_ms":5234,"duration_api_ms":5234,"result":"I added a unit test for `add` in tests/add.rs.","session_id":"c6b62c6f-7ead-4fd6-9922-e952131177ff","request_id":"10e11780-df2f-45dc-a1ff-4540af32e9c0"}
```
Failure: empty stdout, non-zero exit, message on stderr.

---

## 5. GitHub Copilot CLI

Closed source; `github/copilot-cli` holds only README, changelog and installer. The JSON event schema is **not officially documented**; field names below come from third-party parsers and are UNVERIFIED.

- **Install:** `npm install -g @github/copilot`, `brew install copilot-cli`, `winget install GitHub.Copilot`, or `curl -fsSL https://gh.io/copilot-install | bash`. Needs a Copilot subscription.
- **Binary:** `copilot`.
- **Non-interactive:** `-p/--prompt`, `-s/--silent`, `--output-format=json` (JSONL), `--no-ask-user`.
- **Prompt passing:** `--prompt=<text>`.
- **Model:** `--model=<m>` or `COPILOT_MODEL`.
- **Resume:** `--resume=<id>`; `--session-id=<uuid>` resumes or creates with that UUID, so Hashi can pre-assign ids; `--continue` is per repo.
- **Output:** `{type, data}` events: `assistant.message` (`data.content`), `assistant.message_delta`, `tool.execution_start`/`tool.execution_complete`, `session.idle`, and a terminal `result {sessionId, exitCode, usage{premiumRequests, totalApiDurationMs, sessionDurationMs, codeChanges}}`. Final text = last `assistant.message` `data.content`. Tokens only via `--usage-output-file` (`modelMetrics.<model>.usage.{inputTokens, outputTokens, ...}`). One report says Anthropic models produce a flatter stream; parse defensively.
- **Usage/cost:** premium requests, no USD.
- **Auth:** `COPILOT_GITHUB_TOKEN`, then `GH_TOKEN`, then `GITHUB_TOKEN` (fine-grained PAT with "Copilot Requests" permission or OAuth token; classic `ghp_` PATs not supported).
- **Permissions:** documented: without permission options, requests needing approval are **denied**; `-p` never prompts. `--allow-tool`/`--deny-tool` with `shell(cmd:*)`, `write`, `read`, `url(domain)`, `<mcp>(tool)`; `--allow-all-tools`, `--allow-all`/`--yolo`; `--add-dir`; `--trust-folder`. **Safe default:** explicit `--allow-tool=read` (and `write` if needed), `--add-dir=<ws>`, no `--trust-folder`.
- **Exit codes:** non-zero on failure (undocumented values). **Git:** not required. **Windows:** native.

**argv, new run:** `["--prompt=<prompt>", "--output-format=json", "--no-ask-user", "--model=<m>", "--session-id=<uuid>", "--allow-tool=read", "--add-dir=<ws>"]`
**argv, resume:** `["--resume=<uuid>", "--prompt=<prompt>", "--output-format=json", "--no-ask-user", "--allow-tool=read", "--add-dir=<ws>"]`

Fixture `copilot_success.jsonl` (UNVERIFIED shape):
```jsonl
{"type":"assistant.message","data":{"content":"The build passes now.","toolRequests":[]}}
{"type":"session.idle","data":{}}
{"type":"result","timestamp":"2026-11-05T10:00:11.278Z","sessionId":"7809553b-036e-41e9-a66f-2e32bf3f5a3e","exitCode":0,"usage":{"premiumRequests":1,"totalApiDurationMs":966,"sessionDurationMs":11278,"codeChanges":{"linesAdded":0,"linesRemoved":0,"filesModified":[]}}}
```

---

## 6. Goose

Source: `github:block/goose` (now `aaif-goose/goose`). Line numbers are for tag v1.44.0, which is what is installed here (`goose --version` prints `1.44.0`); main (1.58.0) keeps the same JSON structs.

- **Install:** `curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | CONFIGURE=false bash`, `brew install block-goose-cli`, or `download_cli.ps1` on Windows.
- **Binary:** `goose`.
- **Non-interactive:** `goose run -t/--text <TEXT>`, `-i <FILE>` (`-` for stdin), or `--recipe` (crates/goose-cli/src/cli.rs:213-237, 1749-1845). No input exits 1.
- **Prompt passing:** `--text` has no `allow_hyphen_values`, so `-t --help` is parsed as a flag. Use the attached form `--text=<prompt>` (clap 4 takes the rest literally; not executed here). Alternative: a temp file and `-i <path>`.
- **Model:** `--provider <p> --model <m>` (or `GOOSE_PROVIDER`/`GOOSE_MODEL`). On resume, the session's saved provider and model are reused unless overridden (session_builder.rs:236-298).
- **Resume:** session ids look like `20261105_3` (session_manager.rs:1508-1515) and are **not in the JSON output** (only in the banner). Mint a name: start with `-n hashi-<uuid>`, resume with `-r -n hashi-<uuid>` (cli.rs:397-480). `-r --session-id <id>` also works. Resuming from a different cwd warns and stays in the current cwd.
- **Output `--output-format json`** (session/mod.rs:69-89, 1418-1449): one pretty-printed document `{messages: [Message], metadata: {total_tokens, input_tokens?, output_tokens?, cost_usd?, status}}`. Message: `{id, role, created, content: [{type: text|toolRequest|toolResponse|thinking|...}], metadata{userVisible, agentVisible}}` (goose-provider-types/src/conversation/message.rs:258-272, 711-728, 813-823). On resume `messages` is the full history and usage is session-cumulative (diff against the previous run).
- **`stream-json`** (mod.rs:91-118): `{"type":"message","message":{...}}`, `{"type":"notification",...}`, `{"type":"error","error":"..."}`, `{"type":"complete","total_tokens","input_tokens","output_tokens","cost_usd"}`. An `error` is still followed by `complete`.
- **Stdout hazards:** without `-q`, a banner goes to stdout even in JSON modes (session_builder.rs:680-687). `render_error` writes setup errors to **stdout** then exits 1 (output.rs:602-604). A provider failure mid-run rolls back and still prints JSON with `status:"completed"` and **exit 0** (mod.rs:1385-1405). The parser must treat "no assistant message after the last user message" as failure. Prefer `stream-json`, where an `error` event is explicit.
- **Parser (stream-json):** concatenate `text` parts of assistant `message` events; first `error` event means `Reported`; usage from `complete` (cumulative, so Hashi needs the previous totals or must accept cumulative numbers).
- **Usage/cost:** tokens and USD (when pricing is known).
- **Auth:** env first (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, ...), then `~/.config/goose/config.yaml`, then keyring (config/base.rs:94-102, 186-193). Set `GOOSE_DISABLE_KEYRING=1` on servers. `GOOSE_PATH_ROOT` relocates config/data/state.
- **Permissions:** `GOOSE_MODE=auto` (default) auto-allows tools headlessly; `approve`/`smart_approve` error with "Tool approval required in non-interactive mode" (exit 1); `chat` disables tools (mod.rs:1240-1303). No sandbox (`--container` runs extensions in a container). **Safe default:** `GOOSE_MODE=chat` for no tools, or `auto` with `--no-profile --with-builtin developer` and OS-level isolation. `--max-turns` defaults to 1000.
- **Exit codes:** 0 (including mid-run provider failures), 1 on setup errors and approval errors.
- **Git:** not required (UNVERIFIED). **Windows:** supported. **Telemetry:** opt-in; `GOOSE_TELEMETRY_OFF=1`.

**argv, new run:** `["run", "-q", "--output-format", "stream-json", "--provider", "anthropic", "--model", "claude-sonnet-4-5", "--no-profile", "--with-builtin", "developer", "-n", "hashi-3f2a9c1e", "--text=<prompt>"]`
**argv, resume:** `["run", "-q", "--output-format", "stream-json", "-r", "-n", "hashi-3f2a9c1e", "--text=<prompt>"]`

Fixture `goose_success.jsonl`:
```jsonl
{"type":"message","message":{"id":"msg_01","role":"assistant","created":1762300004,"content":[{"type":"toolRequest","id":"toolu_1","toolCall":{"status":"success","value":{"name":"developer__text_editor","arguments":{"command":"write","path":"README.md","file_text":"# Demo\n"}}}}],"metadata":{"userVisible":true,"agentVisible":true}}}
{"type":"message","message":{"id":null,"role":"user","created":1762300005,"content":[{"type":"toolResponse","id":"toolu_1","toolResult":{"status":"success","value":[{"type":"text","text":"Wrote README.md"}]}}],"metadata":{"userVisible":true,"agentVisible":true}}}
{"type":"message","message":{"id":"msg_02","role":"assistant","created":1762300007,"content":[{"type":"text","text":"Created README.md."}],"metadata":{"userVisible":true,"agentVisible":true}}}
{"type":"complete","total_tokens":2046,"input_tokens":1834,"output_tokens":212,"cost_usd":0.0081}
```
Fixture `goose_failure.jsonl`:
```jsonl
{"type":"error","error":"Request failed: 401 Unauthorized"}
{"type":"complete","total_tokens":null}
```

---

## 7. Crush

Source: `github:charmbracelet/crush` (Go, cobra + pflag + fang).

- **Install:** `brew install charmbracelet/tap/crush`, `npm install -g @charmland/crush`, `winget install charmbracelet.crush`, scoop, nix (README.md:25-55).
- **Binary:** `crush`.
- **Non-interactive:** `crush run [prompt...]` (internal/cmd/run.go:31-36). Flags: `-q`, `-v`, `-m model|provider/model`, `--small-model`, `-s/--session <id>`, `-C/--continue` (run.go:167-176); globals `-c/--cwd`, `-D/--data-dir`, `-y/--yolo` (root.go:57-62).
- **Prompt passing:** pflag stops at `--` (pflag v1.0.9 flag.go:1144-1147), so `crush run -q -- "--help"` is safe. Args are joined with spaces (run.go:83). Stdin is read only if it is a pipe or regular file (root.go:960-977), so `/dev/null` is ignored.
- **Output:** **plain text only.** All assistant text of every step goes to stdout (internal/app/app.go:335-538). Session id, tokens and cost are never printed by `run`. Afterwards, `crush session last --json` or `crush session show <uuid> --json` returns `{meta{id, uuid, title, cost, prompt_tokens, completion_tokens, total_tokens}, messages[]}` (internal/cmd/session.go:436-470, 571-625), session-cumulative. That is a second process per request and races under concurrency.
- **Resume:** `-s <uuid|hash prefix>` (run.go:357-383).
- **Permissions:** `crush run` **auto-approves every permission request** for its session (app.go:447-449, permission.go:319-330). The only restriction is `options.disabled_tools` in config. No sandbox.
- **Auth:** provider env vars; config files including `./crush.json` in the workspace; **`godotenv/autoload` loads the workspace `.env`** into the process.
- **Exit codes:** 1 on error, 130 on signals (root.go:233-241). **Windows:** yes. **Telemetry:** `CRUSH_DISABLE_METRICS=1`; an unconditional GitHub release check runs at startup.

**argv, new run:** `["run", "-q", "-m", "anthropic/claude-sonnet-4-5", "--", "<prompt>"]`, then `["session", "last", "--json"]`
**argv, resume:** `["run", "-q", "-s", "<uuid>", "--", "<prompt>"]`

Fixture `crush_success.txt` (stdout): `I created hello.py with a main() that prints "hi".\n`
Fixture `crush_session.json`:
```json
{"meta":{"id":"a41f09c2d7e3b5f1","uuid":"3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6b","title":"Create hello.py","created":"2026-11-05T10:00:00Z","modified":"2026-11-05T10:00:07Z","cost":0.0123,"prompt_tokens":1834,"completion_tokens":212,"total_tokens":2046},"messages":[]}
```

---

## 8. Amp

Closed source; from ampcode.com docs (cli, execute-mode, streaming-json).

- **Install:** `curl -fsSL https://ampcode.com/install.sh | bash` (Homebrew and Windows options exist; exact commands UNVERIFIED).
- **Binary:** `amp`. **Non-interactive:** `-x/--execute "<prompt>"` (also automatic when stdout is redirected).
- **Output:** `--stream-json`, documented as Claude Code compatible: `system/init {session_id: "T-..."}`, `assistant`, `user`, terminal `result {subtype: success|error_during_execution|error_max_turns, is_error, result, session_id, usage?{input_tokens, output_tokens, cache_*}, permission_denials[], error?}`. No cost field. The Claude parser would almost work as is.
- **Model:** `--mode` (values UNVERIFIED), not a raw model id. **Resume:** `amp threads continue <T-id> -x "<prompt>" --stream-json`.
- **Prompt passing:** neither `--execute=<prompt>` nor `--` is documented (UNVERIFIED).
- **Auth:** `AMP_API_KEY` (paid credits).
- **Permissions:** "By default, Amp does not ask for approval before running tools". Restrict with `amp.tools.disable` in `.amp/settings.json` or `--settings-file`. This is the weakest unattended default among the Claude-shaped CLIs.

Fixture `amp_success.jsonl`:
```jsonl
{"type":"system","subtype":"init","cwd":"/srv/hashi/ws/u1","session_id":"T-5e2b8c41-0d9f-4a7e-b3c6-91f0e2d4a8b7","tools":["Read","Bash","edit_file"],"mcp_servers":[]}
{"type":"result","subtype":"success","duration_ms":5400,"is_error":false,"num_turns":1,"result":"8","session_id":"T-5e2b8c41-0d9f-4a7e-b3c6-91f0e2d4a8b7","usage":{"input_tokens":1520,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}
```

## 9. Factory Droid

Closed source; from docs.factory.com.

- **Install:** `curl -fsSL https://app.factory.ai/cli | sh` or `npm install -g droid`. **Binary:** `droid`. **Non-interactive:** `droid exec` with positional prompt, `-f prompt.md`, or stdin; `--cwd`.
- **Output:** `-o json`: `{type:"result", subtype, is_error, duration_ms, num_turns, result, session_id, usage{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, factory_credits}}`. Cost is credits, not USD.
- **Model:** `-m <id>`. **Resume:** `-s <session_id>`.
- **Auth:** `FACTORY_API_KEY`.
- **Permissions:** read-only by default; `--auto low` allows file edits; actions above the level fail fast with non-zero exit (no hang). Exit codes: 0 completed, 1 runtime error, 2 bad args. Good safety model; popularity is lower.
- **argv:** `["exec", "-o", "json", "--auto", "low", "--cwd", "<ws>", "-m", "<m>", "-s", "<id>", "--", "<prompt>"]` (`--` UNVERIFIED; `-f <file>` is the documented safe alternative).

Fixture `droid_success.json`:
```json
{"type":"result","subtype":"success","is_error":false,"duration_ms":8123,"num_turns":3,"result":"A TypeScript monorepo with an API and a UI package.","session_id":"8af22e0a-d222-42c6-8c7e-7a059e391b0b","usage":{"input_tokens":11234,"output_tokens":187,"cache_read_input_tokens":8192,"cache_creation_input_tokens":0,"factory_credits":2}}
```

## 10. Aider

Source: `github:Aider-AI/aider` (0.86.3.dev, Python).

- **Install:** `python -m pip install aider-install && aider-install`. **Binary:** `aider`.
- **Non-interactive:** `--message=<text>` (also `--msg`, `-m`) or `--message-file` (args.py:639-656). Positionals (including after `--`) are files to edit, not the prompt.
- **Output:** **no machine-readable mode.** Rich text on stdout: announcement lines, then the reply, then `Tokens: 2.4k sent, 312 received. Cost: $0.0089 message, $0.0089 session.` (base_coder.py:2023-2068). Counts are abbreviated (`2.4k`), so usage is lossy.
- **Resume:** none by id; `--restore-chat-history` replays `.aider.chat.history.md` in the cwd.
- **Hazards:** confirms read EOF from `/dev/null` and take the default "y" (io.py:806-885), which can `git init`, opt in to analytics, start OAuth, or run suggested shell commands unless `--yes-always` and friends are set. A prompt starting with `/` or `!` is a command (`!` runs a shell command, commands.py:255-256). The workspace `.aider.conf.yml` and `.env` are loaded with override (main.py:361-381, 464-477). API errors still exit 0 (main.py:1126-1133).
- **argv:** `["--model", "sonnet", "--yes-always", "--no-git", "--no-auto-commits", "--no-analytics", "--no-check-update", "--no-show-release-notes", "--no-suggest-shell-commands", "--no-detect-urls", "--no-pretty", "--no-stream", "--message= <prompt>"]`

Fixture `aider_success.txt`:
```
Aider v0.86.3
Model: anthropic/claude-sonnet-4-5 with diff edit format
Git repo: none
Repo-map: disabled

`add` returns the sum of its two arguments.

Tokens: 2.4k sent, 312 received. Cost: $0.0089 message, $0.0089 session.
```

## 11. Kimi Code

`MoonshotAI/kimi-cli` (Python) is **archived**; its successor is `MoonshotAI/kimi-code` (`@moonshot-ai/kimi-code` 2.5.0, TypeScript).

- **Install:** `curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash`, `brew install kimi-code`, or `npm i -g @moonshot-ai/kimi-code`. Windows needs Git Bash. **Binary:** `kimi`.
- **Non-interactive:** `-p/--prompt` only; any positional fails with "unknown command" (apps/kimi-code/src/cli/commands.ts:52-143, 209-212). Use `--prompt=<text>`.
- **Output:** `--output-format stream-json`: `{"role":"assistant","content":...,"tool_calls":[...]}`, `{"role":"tool",...}`, and a final `{"role":"meta","type":"session.resume_hint","session_id":...}` (prompt-render.ts:148-235, 390-410). **No usage or cost.**
- **Resume:** `-S <id>`; fails if the session's cwd differs.
- **Auth:** `KIMI_MODEL_NAME` + `KIMI_MODEL_API_KEY` + `KIMI_MODEL_BASE_URL`, or `/login`.
- **Permissions:** print mode **forces auto-approve** and rejects `--plan` (options.ts:87-94, run-v2-print.ts:433-438). No deny-by-default headless mode.
- **Exit codes:** 1 error (plain text on stderr), 4 session busy, 130/143/129 signals.

Fixture `kimi_success.jsonl`:
```jsonl
{"role":"assistant","content":"Paris."}
{"role":"meta","type":"session.resume_hint","session_id":"0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3","command":"kimi -r 0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3","content":"To resume this session: kimi -r 0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3"}
```

## 12. Cline CLI (v3)

Source: `github:cline/cline`, `apps/cli` (`@cline/cli` 3.0.74, TypeScript). The older `cline task new`, `--oneshot` and `-o` flags no longer exist.

- **Install:** `npm i -g cline`. **Binary:** `cline`.
- **Non-interactive:** positional `[prompt]` with `--json` (src/commands/program.ts:24-126). The prompt must be one argv element containing whitespace, otherwise "Unknown command or unquoted prompt" (main.ts:785-791); a one-word prompt needs a trailing space. `normalizeCliArgs` rewrites flag-like tokens anywhere in argv (utils/helpers.ts:509-552); `--` is UNVERIFIED.
- **Output:** NDJSON with `ts`; `agent_event` lines and a final `run_result {finishReason, iterations, usage{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, totalCost}, durationMs, text, model}` (runtime/run-agent.ts:369-380). Session id is not in `run_result`.
- **Resume:** **not available headlessly**: `--id` forces the TUI and `--json --id` errors (main.ts:727-745, 795-801).
- **Permissions:** auto-approve all by default (main.ts:885); `--auto-approve false` denies with `Tool "X" requires approval in a TTY session` (utils/approval.ts:69-77).
- **Other:** self-updates on startup unless `CLINE_NO_AUTO_UPDATE=1`; telemetry on.

Fixture `cline_success.jsonl`:
```jsonl
{"ts":"2026-11-05T10:00:01.000Z","type":"agent_event","event":{"type":"content_end","contentType":"text","text":"Paris.","agentId":"a1","conversationId":"1762336800000_ab12c"}}
{"ts":"2026-11-05T10:00:01.100Z","type":"run_result","finishReason":"completed","iterations":1,"usage":{"inputTokens":900,"outputTokens":6,"cacheReadTokens":0,"cacheWriteTokens":0,"totalCost":0.0028},"durationMs":2100,"text":"Paris.","model":{"id":"anthropic/claude-sonnet-4.5","provider":"cline"}}
```

---

## Recommendation: integrate these first

Criteria: headless maturity, JSON output that is stable and carries text, session id and usage, popularity, and a safe unattended default that does not hang or silently grant shell access.

1. **Gemini CLI.** The most complete headless contract after Claude Code: documented `stream-json` with `init.session_id`, token stats on `result`, explicit `result.status`, a typed exit code table, caller-chosen session ids (`--session-id`), and headless approval that **denies** shell and writes by default. 108.7k stars, Apache-2.0, Google-maintained. Integration costs: `--skip-trust` (otherwise exit 55 in every new workspace), `GEMINI_CLI_NO_RELAUNCH=1` so `kill_on_drop` kills the real process, prompt via `--prompt=`, and checking `status` because a blocked response can exit 0. No cost reporting.
2. **opencode.** The most popular open-source agent (224.7k stars), provider-agnostic, and its JSONL is the only one here that gives tokens **and** USD cost per step plus `sessionID` on every line. `run` rejects permission asks unless `--auto`, so it never hangs, and `-s ses_...` lets Hashi pre-assign session ids. Costs: V2 is weeks old (pin a version; the 1.x build installed here differs and currently fails to launch), `--standalone` is mandatory, the space-quoting quirk means splitting the prompt into words after `--`, and `PWD` must be set to the workspace, which needs per-backend env support in the runner.
3. **Qwen Code.** Cheapest to build: its `stream-json` `result` line is Claude Code's shape, so `claude::parse_output` can be reused almost verbatim (filter on `type == "result"`, read `error.message`, no cost). Headless denies shell and edits unless allowlisted, `--session-id` pre-assigns ids, and it works with any OpenAI-compatible or Anthropic key through env vars. Same Gemini-fork caveats (prompt via `--prompt=`, leading `/` is a slash command). 29.0k stars.
4. **Cursor CLI** (closed source). Large installed base, and `--output-format json` is again Claude-shaped (`result`, `session_id`, `is_error`), with print mode not writing files unless `--force`. It ranks fourth because usage is undocumented, failures write no JSON (the existing stderr fallback covers this), `--` handling is unverified, and the binary name changed (`agent` versus `cursor-agent`).

Close runners-up: **Goose** (installed here, open source, tokens and USD in JSON) loses on safety and reliability: session ids are absent from output, setup errors go to stdout, mid-run provider failures exit 0 with `status:"completed"`, and the headless choice is "all tools" (`auto`) or "no tools" (`chat`). **Factory Droid** has the best permission model (read-only default, fail-fast) and a Claude-shaped `json` result but is closed source, lower profile and reports credits rather than USD. **Copilot CLI** has a good deny-by-default story but an undocumented event schema.

Not recommended for now: **Crush** (no JSON, auto-approves everything, loads workspace `.env`), **Aider** (no JSON, EOF confirms default to yes, `!` prompts run shell, errors exit 0), **Kimi Code** (print mode forces auto-approve, no usage), **Cline CLI** (no headless resume, auto-approves by default, self-updates), **Amp** (no approvals by default).

## Verification notes

- Read from source via `opensrc`: opencode, Gemini CLI, Qwen Code, Crush, Goose, Aider, Kimi CLI and Kimi Code, Cline. One opencode claim (the `formatMessage` quoting) was re-checked directly at `packages/cli/src/run/run.ts:179-182`.
- From official docs only, not source: Cursor CLI, Copilot CLI, Amp, Factory Droid.
- Nothing was executed except `--version`/`--help` on installed binaries: `goose` 1.44.0 (`goose run --help` lists `--output-format text|json|stream-json`, `--session-id`, `-n`, `--no-session`, `--max-turns`), `claude` 2.1.318, `codex` 0.166.0. `opencode --version` was killed with exit 137 before printing (ad-hoc signed 1.x binary), so its version is unknown.
- Every `--` positional claim for yargs-based CLIs (Gemini, Qwen) is UNVERIFIED; the `--prompt=<text>` form is recommended instead. Before shipping any backend, record real fixtures from a pinned version with a harmless prompt and replace the derived fixtures above.
