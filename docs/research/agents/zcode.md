# ZCode (`zcode`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `ZCODE` in `src/agent/specs.rs`.

- **Source:** github:zai-org/ZCode (main, repo 3.18.2; Agent CLI in apps/zcode-cli 0.17.1, entry packages/cli). npm `zcode-cli` 0.0.1 is an unrelated name-squat placeholder.
- **License:** Apache-2.0
- **Version checked:** 3.18.2 (apps/zcode-cli 0.17.1)
- **Install:** Build the CLI distribution from source: pnpm bootstrap && pnpm build:zcode (installer install.sh puts the runtime in ~/.zcode/runtime and `zcode` in ~/.local/bin); the ZCode desktop app embeds the same CLI. No official public npm package.
- **Headless call:** `zcode --output-format stream-json --no-color`, prompt flag eq `--prompt=`
- **Runs tools without asking when headless:** no

## Notes

**What it is.** Z.ai (Zhipu) **ZCode**, an AI coding workspace (desktop, web, TUI) open-sourced under Apache-2.0 at `github:zai-org/ZCode`; the Agent CLI/runtime is `apps/zcode-cli` (own TypeScript code, not a Claude Code or Gemini CLI fork; JSON is not Claude Code's result object). One `zcode` command: no args starts the TUI, a leading `--web` starts the web UI, everything else goes to the Agent CLI (README.en.md:88-111). There is no public prebuilt CLI download URL in the repo (the distribution installer reads a configured `ZCODE_DIST_BASE_URL`, `scripts/build-zcode.mjs:17,236`); the desktop app bundles it. npm `zcode-cli` 0.0.1 is a third-party placeholder that only prints help. Fixtures derived from source, not run.

**Headless.** `zcode --prompt <text>` (`-p` takes a value; it is NOT a print switch; a bare positional is not a prompt) (`apps/zcode-cli/docs/usage/02-cli-reference.md`). Parser is `node:util` `parseArgs` with strict options (`packages/cli/src/arguments.ts:1-60`), so `--prompt=<text>` keeps a value that starts with `-` intact; `flag_eq` is used. Unsupported flags (`--print`, `--model`, `--settings`, `--max-turns`, `--allowed-tools`, `--permission-mode`) are rejected (02-cli-reference.md). Working dir defaults to `process.cwd()` (`prompt-command.ts:177`); `--cwd` overrides. The CLI loads `.env` files walking **up** from the working directory without overriding existing env (`docs/usage/03-config.md:94`): gotcha for workspaces.

**Permissions (important).** `--mode plan|build|edit|yolo|guarded`; **`--prompt` defaults to `yolo`** (02-cli-reference.md, 05-permissions.md:3), i.e. tools run unasked. Hashi pins `--mode build`: writes and side-effecting tools need approval, and with no approval client an `ask` makes the tool fail ("ask + no approval client -> tool fails", 05-permissions.md:22-28), which is the deny-by-default behaviour. `plan` is more restrictive but not an OS sandbox; `edit` auto-approves workspace edits; `guarded` still asks for dangerous commands. No OS sandbox. Tool denials do not fail the process (exit can be 0).

**Output.** `--output-format text|json|stream-json` (overrides `--json`). `json` prints a **pretty-printed multi-line** object (`formatJson`, prompt-command.ts:409-449), which a last-line parser cannot read, so `stream-json` is chosen: NDJSON events, then a terminating result line `{type:"result", sessionId, traceId, turnId?, response, turnResponses? (only when >1 turn), usage?, eventCount, projection{status, turnCount, totalTokenCount, contextUsed, contextWindow}}` (prompt-command.ts:383-407). `response` is the last turn's text. Event line shapes are unspecified ("keep unknown event types", 02-cli-reference.md); Hashi only needs the last line. `usage` is the runtime's usage object (`ModelUsage`/`ModelUsageSummary` in `packages/contracts/src/model/index.ts:556-578`: `inputTokens`, `outputTokens`, `totalTokens`, `cacheReadTokens`, ...; exact type of `result.usage` UNVERIFIED) and is omitted when absent; `projection.totalTokenCount` is a fallback. No cost. **Errors:** stdout gets nothing; stderr gets `Error: <message> (traceId: ...)` and exit 1 (prompt-command.ts:457-467), so `fixture_error` is null (empty stdout) and Hashi must use the exit code plus stderr.

**Resume.** `--resume <sessionId>` (ids like `sess_...`), `-c/--continue` for the most recent in the cwd; mutually exclusive. Keep the same data dir; ids are assigned by ZCode.

**Model.** No `--model` flag (explicitly unsupported). The model comes from `~/.zcode/v2/provider_config.json` (personal provider file, override path with `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE`); legacy `ZCODE_MODEL`/`ZCODE_API_KEY` are no longer general overrides (03-config.md:8-14,63-70).

**Config/env.** Provider and credential data under `ZCODE_DATA_BASE_DIR`; runtime/permission/MCP/hooks config in `~/.zcode/cli/config.json` (HOME based). Full isolation per account needs `HOME` + `ZCODE_DATA_BASE_DIR` (13-automation.md sets `HOME`, `USERPROFILE`, `ZCODE_DATA_BASE_DIR`). Other env: `ZCODE_STORAGE_DIR`, `ZCODE_SESSION_DB_PATH`, `ZCODE_LOG_DIR`, `ZCODE_HTTP_PROXY`, `ZCODE_NO_PROXY`, `ZCODE_AGENT_CA_CERT` (03-config.md:92). The official automation recipe also passes `--disallowed-tools "AskUserQuestion EnterPlanMode ExitPlanMode"` for unattended runs (13-automation.md); Hashi may add that.

**Slash commands.** `--prompt "/help"` and custom commands are routed to a local command centre (prompt-command.ts:296-316), so a leading `/` is a command.

**Process tree.** The docs warn that killing only the parent does not end Bash/MCP/background children (13-automation.md); Hashi's `kill_on_drop` kills only the direct child.

**Rate limits.** Not characterised in source read; patterns are generic (UNVERIFIED). Requires Node.js 24.14 for the distribution runtime.

## Success output (derived from source or docs, not recorded)

```
{"type":"result","sessionId":"sess_01JDA9Z3K0Hashi","traceId":"trace_01JDA9Z3K1","turnId":"turn_01JDA9Z3K2","response":"Paris.","usage":{"inputTokens":8120,"outputTokens":5,"totalTokens":8125},"eventCount":4,"projection":{"status":"idle","turnCount":1,"totalTokenCount":8125,"contextUsed":8125,"contextWindow":200000}}
```
