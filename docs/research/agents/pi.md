# Pi (`pi`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `PI` in `src/agent/specs.rs`.

- **Source:** github:badlogic/pi-mono
- **License:** MIT
- **Version checked:** 1.5.2 (@earendil-works/pi-coding-agent, main fetched 2026-11-16; not installed, not run)
- **Install:** npm install -g --ignore-scripts @earendil-works/pi-coding-agent | curl -fsSL https://pi.dev/install.sh | sh
- **Headless call:** `pi -p --mode json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

Pi coding agent (Mario Zechner). The repo `badlogic/pi-mono` now publishes as `@earendil-works/pi-coding-agent` 1.5.2 (packages/coding-agent/src/../package.json:2-3, bin `pi`); the old npm name `@mariozechner/pi-coding-agent` stops at 0.73.1. Read from source only; not installed here, nothing executed. Both fixtures are DERIVED from source, not recorded.

**Headless:** `pi -p` (print mode) or `--mode json`. `resolveAppMode` (packages/coding-agent/src/main.ts:115-126) picks print when `-p` is given OR stdin/stdout is not a TTY, so under Hashi (stdin=/dev/null) it never opens the TUI. `readPipedStdin` (main.ts:83-100) reads /dev/null as empty and returns undefined, harmless.

**Arg parser** (packages/coding-agent/src/cli/args.ts:72-271, hand-rolled): `--` is honoured (args.ts:83-91): everything after it is a message, EXCEPT an element starting with `@`, which becomes a file attachment (args.ts:85-86). Hashi must refuse or space-prefix prompts starting with `@`. Each element after `--` is a SEPARATE prompt sent in sequence (print-mode.ts:138-144, cli/initial-message.ts:34-37), so pass the prompt as one element. `-p` only swallows the next token when it does not start with `-` or `@` (args.ts:177-183), so `-p --mode json -- <prompt>` is safe. Unknown `--flags` are collected as extension flags, not errors (args.ts:249-262); unknown short flags are errors (263-264).

**Output `--mode json`** (packages/coding-agent/src/modes/print-mode.ts:115-134, modes/json-event.ts:48-61): JSONL. First line is the session header `{type:"session",version:3,id,timestamp,cwd}` (core/session-manager.ts:43-50, id is a UUIDv7, session-manager.ts:265). Then every AgentSessionEvent (core/agent-session.ts:209-251; packages/agent/src/types.ts:528-555): `agent_start`, `turn_start`, `message_start`, `message_update` (partial snapshot stripped, adds cumulative `usage`; `assistantMessageEvent` is text_start/text_delta/text_end{content}/thinking_*/toolcall_*), `message_end` {message} for user, assistant and toolResult messages, `tool_execution_start|update|end`, `turn_end`, `agent_end {messages, willRetry}`, `agent_settled`, `auto_retry_start {attempt,maxAttempts,delayMs,errorMessage}`, `auto_retry_end {success,attempt,finalError?}`, `compaction_*`, `queue_update`, `entry_appended`. No terminal result line.
Assistant message (packages/ai/src/types.ts:593-615): `content[]` of text/thinking/toolCall, `usage {input, output, cacheRead, cacheWrite, totalTokens, reasoning?, cost {input,output,cacheRead,cacheWrite,total(USD)}}` (types.ts:467-495), `stopReason` pending|stop|length|toolUse|error|aborted|deferred (types.ts:497), `errorMessage?`.

**Parser choice:** text = `/assistantMessageEvent/content` of the LAST `message_update` that has it (only `text_end` and `thinking_end` carry a string `content`; a reply that ends with a thinking block would be misread, rare). Alternative is the final assistant `message_end` `/message/content/*` filtered to `type=="text"`, which a single RFC 6901 pointer cannot express (thinking usually sits at index 0). Usage: sum `message_end` lines that HAVE `/message/usage` (user and toolResult message_end lines have none, so the parser must skip missing pointers). `input` excludes `cacheRead`/`cacheWrite`; add them if Hashi wants the Claude convention. Session id = header `/id` (no other top-level `id` key in the event union).
**Errors:** provider failures do NOT emit an `error` event; they end the assistant message with `stopReason:"error"` and `errorMessage` (packages/agent/src/agent-loop.ts:453-464). In json mode the exit code stays 0 even then: only text mode sets exit 1 (print-mode.ts:146-167). Retryable errors (rate limit, 429, 5xx, overloaded) are retried 3 times by default (core/settings-defaults.ts:18-22; patterns packages/ai/src/utils/retry.ts:7-50); each failed attempt still emits a `message_end` with `errorMessage`, followed by `auto_retry_start`. So `error_kind: message_end` flags a run as failed even if a later retry succeeded: Hashi should treat only the LAST assistant `message_end` as decisive (or check that no successful assistant message follows). UNVERIFIED how Hashi's generic jsonl parser orders this. Setup errors (bad flag, no model: `formatNoModelsAvailableMessage`, main.ts:992-995) print to stderr and exit 1 with no JSON. Non-retryable account limits (`insufficient_quota`, `quota exceeded`, `Monthly usage limit reached`, OpenCode `GoUsageLimitError`) are in retry.ts:7-35.

**Permissions:** pi has NO approval prompts at all: whatever tools are active just run. Default active tools are `read,bash,edit,write` (core/settings-manager.ts:84-85). Safe headless config is a tool allowlist: `--tools read,grep,find,ls` (help text args.ts:403 calls this read-only mode; args.ts:151-161). `--tools` keeps MCP tools unless an entry starts with `mcp__` (args.ts:320-322), hence `--no-mcp` (args.ts:191-192). `--no-extensions` also disables the built-in extensions codemode, tool-search, mcp, llama.cpp (extensions/index.ts:7-14, args.ts:327). `--no-tools` (args.ts:147-148) is the strictest alternative. `--no-approve`/`-na` ignores project-local files (.pi/settings, extensions) in the workspace (args.ts:241-244); `--approve`/`-a` trusts them, which can load workspace extensions (code execution), so it is listed as unsafe. An operator `--tools` that re-adds `bash`/`write`/`edit` would grant shell/write without asking: Hashi should strip `--tools`/`-t`/`--no-tools` overrides and `-e/--extension` from extra args (value-taking, so not listed in unsafe_flags). No OS sandbox.
**Slash commands:** a prompt starting with `/` runs an extension command or expands a prompt template or `/skill:name` (core/agent-session.ts:2184-2190, 2232-2236). `--no-extensions --no-prompt-templates --no-skills` removes most of them; built-in interactive-only commands are not reachable in print mode (UNVERIFIED for every command).

**Model:** `--model <pattern>` accepts `provider/id` and an optional `:<thinking>` suffix (args.ts:117-118, 302); `--provider` narrows a bare pattern. **Resume:** `--session-id <id>` uses this cwd's session with that id, creating it otherwise (args.ts:136-137, 311); ids must match `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$` (session-manager.ts:268-274), so Hashi can even pre-assign ids. It cannot be combined with `--session`, `--continue` or `--resume` (main.ts:362-383). `--session <path|id>` opens by full/partial id from any project; if the session's cwd no longer exists, non-interactive runs exit 1 (main.ts:744-770).

**Env:** `PI_CODING_AGENT_DIR` relocates the whole agent dir (settings.json, auth.json, models.json, sessions; config.ts:606-635, getAuthPath config.ts:648-650); `PI_CODING_AGENT_SESSION_DIR` moves sessions only. `PI_OFFLINE=1` (or `--offline`) disables startup network operations and sets `PI_SKIP_VERSION_CHECK=1` (main.ts:616-620). `PI_TELEMETRY=0` turns off install telemetry (enabled by default, core/settings-defaults.ts:32, core/telemetry.ts:8-10). Credentials: provider env vars listed in args.ts:419-463 (first match per provider), `--api-key`, or `pi auth login <provider>` (cli/auth-command.ts:41-61). Requires Node >= 22.19.
**Other:** text mode prints only the final assistant text blocks and exits 1 on error/aborted/length (print-mode.ts:146-167), a usable fallback if usage and session id are not needed. SIGTERM exits 143 after disposing (print-mode.ts:54-73).

## Success output (derived from source or docs, not recorded)

```
{"type":"session","version":3,"id":"019a8f3c-5b2e-7d41-9c3a-2f6e8b1d4a70","timestamp":"2026-11-16T10:00:00.000Z","cwd":"/tmp/hashi-ws-1"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_start","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"pending","timestamp":1763287200200}}
{"type":"message_update","usage":{"input":1830,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":1831,"cost":{"input":0.00549,"output":0.000015,"cacheRead":0,"cacheWrite":0,"total":0.005505}},"assistantMessageEvent":{"type":"text_start","contentIndex":0}}
{"type":"message_update","usage":{"input":1830,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":1832,"cost":{"input":0.00549,"output":0.00003,"cacheRead":0,"cacheWrite":0,"total":0.00552}},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"OK"}}
{"type":"message_update","usage":{"input":1830,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":1834,"cost":{"input":0.00549,"output":0.00006,"cacheRead":0,"cacheWrite":0,"total":0.00555}},"assistantMessageEvent":{"type":"text_end","contentIndex":0,"content":"OK"}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1830,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":1834,"cost":{"input":0.00549,"output":0.00006,"cacheRead":0,"cacheWrite":0,"total":0.00555}},"stopReason":"stop","timestamp":1763287200200}}
{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1830,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":1834,"cost":{"input":0.00549,"output":0.00006,"cacheRead":0,"cacheWrite":0,"total":0.00555}},"stopReason":"stop","timestamp":1763287200200},"toolResults":[]}
{"type":"agent_end","messages":[],"willRetry":false}
{"type":"agent_settled","aborted":false}
```

## Error output

```
{"type":"session","version":3,"id":"019a8f3c-5b2e-7d41-9c3a-2f6e8b1d4a71","timestamp":"2026-11-16T10:00:00.000Z","cwd":"/tmp/hashi-ws-1"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_start","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"error","errorMessage":"401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}","timestamp":1763287200200}}
{"type":"message_end","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"error","errorMessage":"401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}","timestamp":1763287200200}}
{"type":"agent_end","messages":[],"willRetry":false}
{"type":"agent_settled","aborted":false}
```
