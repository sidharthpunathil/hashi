# oh-my-pi (omp) (`omp`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `OMP` in `src/agent/specs.rs`.

- **Source:** github:can1357/oh-my-pi
- **License:** MIT
- **Version checked:** 18.20.3 (@oh-my-pi/pi-coding-agent, main fetched 2026-11-16; not installed, not run)
- **Install:** curl -fsSL https://omp.sh/install | sh | brew install can1357/tap/omp | bun install -g @oh-my-pi/pi-coding-agent
- **Headless call:** `omp -p --mode json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

**Confirmed:** `omp` is oh-my-pi by Stencil Labs (can1357), an MIT fork of badlogic/pi-mono (README.md:26, 571, 587), npm `@oh-my-pi/pi-coding-agent` 18.20.3 with bin `omp` (packages/coding-agent/src/../package.json:2, 12-27). Runs on Bun (cli.ts:1, refuses older Bun, cli.ts:48-53); release installs ship a compiled binary. Read from source only; not installed, nothing executed. Fixtures are DERIVED from source.

**Routing:** `omp <first token>` routes to a subcommand if it is one, everything else goes to `launch` (cli.ts:600-608). base_args start with `-p`, so a prompt can never be mistaken for a subcommand.
**Arg parser** (packages/coding-agent/src/cli/args.ts:250-445): `--` is honoured; after it every element is a literal message, including ones starting with `@` or `-` (args.ts:266-275, 424-428), unlike Pi. `-p` is a plain boolean (args.ts:394-395). Unknown flags are a hard error after the extension-aware reparse (args.ts:429-437, 494-505). `--flag=value` works for every flag (args.ts:281-292). Every CLI message after the first is a separate prompt; CLI messages `/compact`, `/handoff`, `/retry`, `/mcp reauth` are executed locally (modes/print-mode.ts:327-372), and session.prompt also expands extension commands, file slash commands and prompt templates for any `/`-prefixed text (session/agent-session.ts:8965-8995).

**Output `--mode json`** (modes/print-mode.ts:93-123, 198-202, 389-392): header line `{type:"session",version:3,id,title?,timestamp,cwd,...}` (session/session-entries.ts:42-55; id is UUIDv7, session/session-manager.ts:151-152), then session events. Differences from Pi: `message_update` drops the partial AND has no `usage` field; `done`/`error` stream events are reduced to `{type, reason}`; `agent_end` carries `isTerminal`/`yielded` and loop pauses also appear as `agent_end` with `isTerminal:false` (session/loop-end-wire.ts:17-42); `tool_stream_update` is reduced to ids. Usage on assistant `message_end`: `{input, output, cacheRead, cacheWrite, totalTokens, ..., cost{input,output,cacheRead,cacheWrite,total}}` (packages/catalog/src/types.ts:218-275). Same parser as Pi: last `text_end` content, sum `message_end` usage where present, session id from header `/id`.
**Exit codes are better than Pi:** json mode exits 1 when the last assistant message ends in `error`/`aborted` (issue #11498, print-mode.ts:584-591, 666-668), 3 when the `--max-cost` gate stopped the run (print-mode.ts:65, 668). Retries print `Retrying (n/m): ...` to stderr (print-mode.ts:410). `errorMessage` can be present on a non-error stop (text mode prints it as a warning, print-mode.ts:603-609), and failed retry attempts also emit it, so `error_kind: message_end` can over-report; prefer exit code 1 as the failure signal and use errorMessage for the message. UNVERIFIED how Hashi combines exit code and parsed errors.

**Permissions:** omp HAS approvals. Modes `always-ask | always-ask-cached | write | yolo` (tools/approval.ts:44); the schema default is `yolo` (approval.ts:84-111), so by default every tool including bash runs headlessly. `--approval-mode always-ask` (cli/flag-tables.ts:283-293, applied as a runtime override in main.ts:1993-1996) auto-allows only `read`-tier tools (approval.ts:132-158, 320-322); anything that would prompt fails closed in print mode with `Tool "X" requires approval but no interactive UI available.` (extensibility/extensions/wrapper.ts:448-470), and the error goes back to the model. `--auto-approve`/`--yolo` force yolo (args.ts:410-411); `--plan-yolo` is also listed unsafe (UNVERIFIED exact semantics). An operator `--approval-mode yolo|write` or a user config `tools.approval.<tool>: allow` still grants tools: Hashi should strip `--approval-mode` from extra args. `--no-tools` exists for a fully tool-less run. Restricted folders always ask (wrapper.ts:455-460).
**Foreign config discovery (hazard):** omp loads rules, skills, MCP servers and context files from Claude Code, Cursor, Codex, Gemini, Cline, opencode, VS Code and `.mcp.json` in the project and home (packages/coding-agent/src/discovery/*.ts). It honours `CLAUDE_CONFIG_DIR` (docs/environment-variables.md:606). `--no-extensions --no-skills --no-rules` trim most of it; there is no `--no-mcp` flag in the parser (MCP tools then sit behind always-ask, UNVERIFIED tier). It also reads `.env` from the launch cwd (the per-token workspace), `~/.omp/agent/.env`, `~/.omp/.env` and `~/.env` for unset keys (docs/environment-variables.md:15-25), so a workspace `.env` can supply credentials or `PI_*` settings. Bun's own cwd `.env` autoload may apply too (UNVERIFIED for the compiled binary).

**Model:** `--model <id>` and `--provider` (flag-tables.ts:155-160); also role models `--smol/--slow/--plan`. **Resume:** `--resume <id>` (alias `--session`; optional-value flag, flag-tables.ts:304-307, 87-89) resolves by id/prefix for this cwd (main.ts:1324-1366), errors `Session "<id>" not found.` otherwise. No `--session-id` pre-assign flag; `-c <uuid>` is rewritten to `--resume` (main.ts:1232-1251). `--no-session` forbids `--resume` (main.ts:1320-1323).
**Env:** `PI_CODING_AGENT_DIR` overrides the agent dir (default `~/.omp/agent`, holds `agent.db` with settings and auth, utils/src/dirs.ts:980-982) but only for the default profile; `PI_CONFIG_DIR` renames the `~/.omp` root (a dirname under home, not a path); `OMP_PROFILE`/`--profile` selects isolated named profiles (docs/environment-variables.md:25, 601-602). Whether `PI_CODING_AGENT_DIR` alone relocates ALL login state (the config root `.env` and auth-broker token live under `~/.omp`) is UNVERIFIED. `PI_NO_TITLE=1` / `--no-title` stop the extra title-generation model call (docs/environment-variables.md:496). `--offline` disables web search, title, judge and similar network side calls and restricts model calls to the session provider (sdk.ts:4896-4898). Auto-update only runs from interactive mode (main.ts:764-770).
**Limits:** account-limit classification in packages/ai/src/error/rate-limit.ts:25-106 (insufficient balance, exceeded credits, spend limit, plan quota, `will reset`, Chinese quota text).

## Success output (derived from source or docs, not recorded)

```
{"type":"session","version":3,"id":"019a8f3c-5b2e-7d41-9c3a-2f6e8b1d4a80","timestamp":"2026-11-16T10:00:00.000Z","cwd":"/tmp/hashi-ws-1"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_start","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1763287200200}}
{"type":"message_update","assistantMessageEvent":{"type":"text_start","contentIndex":0}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"OK"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_end","contentIndex":0,"content":"OK"}}
{"type":"message_update","assistantMessageEvent":{"type":"done","reason":"stop"}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":2410,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":2414,"cost":{"input":0.00723,"output":0.00006,"cacheRead":0,"cacheWrite":0,"total":0.00729}},"stopReason":"stop","timestamp":1763287200200}}
{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":2410,"output":4,"cacheRead":0,"cacheWrite":0,"totalTokens":2414,"cost":{"input":0.00723,"output":0.00006,"cacheRead":0,"cacheWrite":0,"total":0.00729}},"stopReason":"stop","timestamp":1763287200200},"toolResults":[]}
{"type":"agent_end","messages":[],"isTerminal":true,"yielded":true}
```

## Error output

```
{"type":"session","version":3,"id":"019a8f3c-5b2e-7d41-9c3a-2f6e8b1d4a81","timestamp":"2026-11-16T10:00:00.000Z","cwd":"/tmp/hashi-ws-1"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}],"timestamp":1763287200100}}
{"type":"message_update","assistantMessageEvent":{"type":"error","reason":"error"}}
{"type":"message_end","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"error","errorMessage":"401 invalid x-api-key","timestamp":1763287200200}}
{"type":"agent_end","messages":[],"isTerminal":true,"yielded":true}
```
