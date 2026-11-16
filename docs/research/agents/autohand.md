# Autohand Code (`autohand`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `AUTOHAND` in `src/agent/specs.rs`.

- **Source:** github:autohandai/code-cli
- **License:** Apache-2.0
- **Version checked:** 0.9.11
- **Install:** npm i -g autohand-cli | curl -fsSL https://autohand.ai/install.sh | bash | brew install autohandai/code/autohand-code
- **Headless call:** `autohand --json local`, prompt flag eq `--prompt=`
- **Runs tools without asking when headless:** no

## Notes

Autohand Code CLI (Autohand AI LLC), open source TypeScript/commander 15 CLI. Read at ~/.opensrc/repos/github.com/autohandai/code-cli/main (package.json version 0.9.11, same as npm `autohand-cli`). Not run here; fixtures derived from source.

**Binaries.** package.json `bin`: `autohand`, `autohand-code`, `agent`, `ah` (and `ahtraces`). `agent` collides with Cursor CLI's `agent`, so Hashi should not list it.

**Headless.** `[prompt]` positional or `-p, --prompt [text]` runs command mode (src/index.ts:434-435; positional copied into `prompt` by src/startup/cliOptions.ts:121-127). Prompt style is `--prompt=<text>`: the root command registers subcommands (`login`, `config`, `mcp`, `squad`, `resume`, ...; src/index.ts:966-1079) and commander dispatches the first operand to a subcommand even after `--`, so a positional prompt like `login` would misroute. Commander parses `--prompt=<value>` with the whole tail as the value. The root command does not call allowUnknownOption (only subcommands do, src/index.ts:999, 1025, 1539).

**Output.** `--output-format` only accepts `stream-json`; `--json [stream|local]` (src/modes/commandOutput.ts:32-63). `--json local` writes exactly one line at the end: `{"type":"result","content":<final assistant message>}` on success or `{"type":"error","message":...}` on failure (commandOutput.ts:109-136). `stream-json` writes raw AgentOutputEvent objects (`thinking`, `tool_start`, `tool_end`, `file_modified`, ... with fields `content`, `toolName`, `toolArgs`, `toolOutput`; src/types.ts:2273-2306), a `{"type":"result","content"}` line per assistant `message` event, and `{"type":"error","message"}` lines (commandOutput.ts:81-107). In structured modes all console output is redirected to stderr (commandOutput.ts:147-170; src/index.ts:1622-1627). Text mode prints only the final message on success (commandOutput.ts:113-119).

**No session id and no usage on stdout.** Neither output mode carries a session id, token counts or cost. The session id and `tokensUsed` only reach the `stop` hook payload (src/core/agent/AgentLifecycleRunner.ts, finalizeCommandTurn). `-r, --resume [reference]` and `-c, --continue` (latest session in this folder, works with `-p`) exist (src/startup/resumeCommand.ts:20-22, 44-60), so with Hashi's per-token workspace `-c` resumes that token's last session without needing the id (resume_flag `--resume` is listed for completeness but Hashi cannot learn the id). The hidden `--sdk-session-reference` is RPC-mode only (src/modes/rpc/index.ts:679-711). `--ephemeral` keeps the run out of session history (src/index.ts:528).

**Permissions (important).** In command mode, a tool needing approval is denied (`deny_once`, reason `non_interactive`) because the prompt callback receives `nonInteractive: true` when `-p` is set or stdin is not a TTY (src/core/agent/AgentCommandRuntime.ts:384, 397; src/ui/promptCallback.ts:113-115). The run then fails with exit code 77 and the stderr summary `Denied (this run can't ask for approval): <tools>. Rerun with --yes, or with --allowed-tools <tools>.` (src/permissions/unattended.ts:1-26; src/index.ts:2302). Read tools (read_file, find, list_tree, ...) are on the default allow list (src/permissions/PermissionManager.ts:39). **But** the callback auto-allows everything first if `AUTOHAND_NON_INTERACTIVE=1`, `CI=1` or `AUTOHAND_YES=1` (src/ui/promptCallback.ts:106-111), and `CI=1` also auto-accepts plans and auto-commit (AgentCommandRuntime.ts:593, src/core/actionExecutor.ts:2681-2688). Hashi must therefore override these: env sets `CI=false`, `AUTOHAND_NON_INTERACTIVE=0`, `AUTOHAND_YES=0` (the project's own unattended test does exactly this: src/testing/scenarios/unattendedApprovalScenario.ts:6-8). `--restricted` denies all dangerous operations outright (PermissionManager.ts:490); not pinned because default command mode already denies, but it is an option for a strictly read-only backend. `--patch` forces yes+unrestricted into preview mode (src/index.ts:2796-2800). Workspace safety refuses only exact system dirs or home (src/startup/workspaceSafety.ts:180-200), so a per-token workspace is fine; no git repo is required.

**Auth (blocks unattended first run).** Startup requires an Autohand account token even for command mode; with no TTY or with CI set it prints `Not signed in. Run `autohand login`, or in CI set AUTOHAND_API_KEY and add --bare.` and exits 1 (src/auth/ensureAuth.ts:202-208). `--bare` mode reads `AUTOHAND_API_KEY` (or `auth.apiKeyHelper`) instead (ensureAuth.ts:101-113, 133-156). Note `--bare` also skips hooks, AGENTS.md discovery, keychain reads (src/index.ts:439). Whether a BYO-provider-only config skips the account gate is UNVERIFIED. Provider env overrides are merged in src/config.ts:1264-1330 (AUTOHAND_PROVIDER, AUTOHAND_AI_API_KEY, AUTOHAND_AI_BASE_URL, AUTOHAND_AI_PLAN, AUTOHAND_API_URL, AUTOHAND_SECRET, AZURE_*). Other provider keys come from the config file.

**Config dir.** `AUTOHAND_HOME` relocates ~/.autohand (config.json with auth token, sessions, memory, telemetry; src/constants.ts:36-73, 76-165). `AUTOHAND_CONFIG` / `--config` pick a config file (src/config.ts:245).

**Model.** `--model <model>` (src/index.ts:448), applied via applyCliModelOverride. Hidden `--provider`.

**Exit codes.** 0 success, 1 failure, 2 invalid option normalization (src/index.ts:546-548), 77 unattended approval denied.

**Limits.** Autohand gateway: `Rate limit exceeded. Please wait a moment and try again, or choose a different model.` and quota messages `Autohand AI <5-hour|24-hour|weekly|monthly> request quota reached.` (src/providers/LLMGatewayClient.ts:91, 141-145, 229-246); Ollama `usage limit` (src/providers/OllamaProvider.ts:386, 538-539); Bedrock `quota was exceeded` (BedrockProvider.ts:448).

**Slash commands.** A prompt starting with `/` is handled as a local slash command, not sent to the model (src/core/agent/AgentLifecycleRunner.ts:1877-1882). `--bare` disables slash commands.

**Env.** AUTOHAND_SKIP_UPDATE_CHECK (src/utils/versionCheck.ts:296), AUTOHAND_OFFLINE / `--offline` (model catalog refresh off), AUTOHAND_NO_BANNER, AUTOHAND_SKIP_PING, AUTOHAND_DISABLE_AUTO_REPORT, AUTOHAND_NO_BROWSER, AUTOHAND_DISABLE_COMPUTER_USE are the set the project's own headless test uses. `--no-daemon` disables background services and child processes (src/index.ts:440), pinned so kill_on_drop does not orphan helpers (UNVERIFIED that any would otherwise outlive the run).

UNVERIFIED: exact stdout bytes (fixtures derived from CommandOutputWriter); whether other code writes straight to process.stdout in `--json local` mode bypassing the console redirect.

## Success output (derived from source or docs, not recorded)

```
{"type":"result","content":"`add` returns the sum of its two arguments."}
```

## Error output

```
{"type":"error","message":"Rate limit exceeded. Please wait a moment and try again, or choose a different model."}
```
