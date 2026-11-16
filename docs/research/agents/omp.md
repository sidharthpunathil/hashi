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

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. With PI_CODING_AGENT_DIR set (and no profile), agentDir is that path and XDG redirection is skipped (packages/utils/src/dirs.ts:361-367, 386); agent.db with the auth_credentials table (packages/utils/src/dirs.ts:980-982, packages/ai/src/auth/sqlite-credential-store.ts:644-700) and config.yml with any auth.broker settings (packages/coding-agent/src/session/auth-broker-config.ts:10-13) are under it. A profile wins over the override: OMP_PROFILE or PI_PROFILE (dirs.ts:44, 96) or `--profile` drops it (dirs.ts:366). No OS keyring (keychain appears only in browser, bash and Perplexity import code). Caveat: unset vars are refilled from ~/.env, ~/.omp/.env (config root, NOT moved) and the workspace .env (packages/utils/src/env.ts:290-293), so an owner key or OMP_AUTH_BROKER_URL kept in those files reaches a login account.
- **API-key accounts:** yes. On its own the env key does NOT outrank stored login: the key cascade tries stored OAuth, then a login-stored api_key, then a models.yml fallback key, and only then the env var (packages/ai/src/auth/cascade.ts:280-320, 440-470, 575-590). It works under Hashi only because a key account gets its own PI_CODING_AGENT_DIR (empty agent.db) and OMP_PROFILE plus OMP_AUTH_BROKER_* are removed; PI_PROFILE is not removed (see spec_corrections).
- **Status check:** {"args": ["usage", "--provider", "anthropic"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. Login is rows in a SQLite database in WAL mode (packages/ai/src/auth/sqlite-credential-store.ts:644-646), not a JSON file: there is no pointer for expiry or identity, and copying agent.db while any omp holds it open can lose or corrupt the WAL. agent.db also holds the usage cache, usage history and client stats (sqlite-credential-store.ts:651-692) and model/command usage tables (packages/coding-agent/src/session/agent-storage.ts:282-319), so a swap moves more than the login. omp has native alternatives: named profiles (`omp --profile <name>`, OMP_PROFILE) and a multi-credential pool per provider, plus `omp auth export|import` (packages/coding-agent/src/commands/auth.ts:167-200; import merges, it does not replace).
- **Live quota:** yes. Run with PI_CODING_AGENT_DIR pointing at the account. Only providers with a usage module report; API-key accounts show up under accountsWithoutUsage.
- **Usage in output:** tokens and cost
- **Limit messages:** Spec list matches packages/ai/src/error/rate-limit.ts:24-110. Missing: `usage credits are required` (Anthropic entitlement, :37; `credits_required` already covers the code), `prepaid balance` and `usage_pricing_required` (Cursor, :43-44), `spending cap` (Google, :48; `spend limit` misses it), `rate_limit` with underscore, `resource exhausted`/`resource_exhausted` (:63), `clinepass limit` and `free limit reached` (:59), `too many concurrent` (:64-68), and the Chinese quota phrases (:82-83). `will reset` is broad: omp itself gates it on account wording (:73-76) because transient per-minute limits say it too.

Source: github:can1357/oh-my-pi main, @oh-my-pi/pi-coding-agent 18.20.3 (packages/coding-agent/package.json:3). Not installed, nothing run.

- **Login folder:** default `~/.omp/agent` (PI_CONFIG_DIR renames `.omp`). `PI_CODING_AGENT_DIR` moves agent.db, config.yml, models.yml, sessions; on Linux/macOS a migrated XDG layout is used only for the default dir (packages/utils/src/dirs.ts:384-404). Profiles live under `~/.omp/profiles/<name>` and ignore the override.
- **Storage:** credentials are JSON in `auth_credentials.data` rows of agent.db (SQLite, WAL), several per provider (credential pool with rotation), tombstoned on failure via `disabled_cause` (packages/ai/src/auth/sqlite-credential-store.ts:456-501). An auth broker (OMP_AUTH_BROKER_URL/TOKEN, config.yml auth.broker, `~/.omp/auth-broker.token`) replaces local storage (packages/coding-agent/src/session/auth-broker-config.ts:10-13).
- **Status:** no `auth status`. `omp usage --provider <p>` (text mode) exits 1 when no credential is stored for that provider (packages/coding-agent/src/cli/usage-cli.ts:1735-1756) and 0 otherwise; it fetches live usage, so it needs network, and a stored but revoked credential still exits 0. `--json` always exits 0, so do not use it for the check.
- **Identity:** OAuth rows store `email`, `accountId`, `orgId`/`orgName` (sqlite-credential-store.ts:130-224, login prints `Logged in to X as <email>`, packages/coding-agent/src/cli/oauth-terminal.ts:197-205), but in SQLite, so no file/pointer. `omp usage --json` exposes it under reports[].metadata / accountsWithoutUsage[].email.
- **Keys:** stored login beats env (cascade.ts:575-590 `Deliberate login credentials win; then an explicit env var`).
- **Usage in output:** assistant `message_end` usage `{input,output,cacheRead,cacheWrite,totalTokens,cost{total}}`; spec pointers right.
- **Limits:** see limit_notes.
