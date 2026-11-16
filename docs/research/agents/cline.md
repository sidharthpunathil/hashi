# Cline CLI (`cline`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `CLINE` in `src/agent/specs.rs`.

- **Source:** github:cline/cline
- **License:** Apache-2.0
- **Version checked:** @cline/cli 3.0.74 (source only, not installed)
- **Install:** npm i -g cline
- **Headless call:** `cline --json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

Source: `~/.opensrc/repos/github.com/cline/cline/main/apps/cli` (`@cline/cli` 3.0.74), plus `sdk/packages/shared`. Not installed here; nothing was run, fixtures are DERIVED from source. Complements docs/research/agent-clis.md section 11.

**Headless**: root command with positional `[prompt]` and `--json` (src/commands/program.ts:23-126). The prompt must be **one argv element containing whitespace**, else `Unknown command or unquoted prompt` and exit 1 (`promptArgLooksQuoted`, main.ts:133-142, checked at 785-791). Hashi must append a trailing space to a one-word prompt. `--` goes through commander, which drops the separator and treats the rest as operands (UNVERIFIED by a run). `normalizeCliArgs` (utils/helpers.ts:509-557) rewrites `--auto-approve`, `--autoapprove=`, `--thinking`, `--reasoning-effort` tokens anywhere in argv, even after `--`; since a valid prompt contains whitespace it can only collide through the `--autoapprove=`/`--reasoning-effort=` prefixes, which Hashi should refuse. Piped stdin is read only when it is a FIFO/file (`stdinHasPipedInput`, main.ts:67, 1138-1144).

**Output (`--json`)**: NDJSON, each line `{ts, type, ...}` (`emitJsonLine`, utils/output.ts:129-139). Types: `run_start {providerId, modelId, catalog, thinking, mode, sessionId?}` (runtime/run-agent.ts:49-62), `agent_event {event}` for every agent event (utils/events.ts:98-101), `team_event`, `run_abort_requested`, `run_result {finishReason, iterations, usage{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, totalCost}, aggregateUsage?, durationMs, text, model}` (run-agent.ts:369-380), `run_aborted {reason, message}` (387-391). Errors and warnings go to **stderr** as `{ts, type:"error"|"warning", message}` (output.ts:189-197).

**Session id**: `run_start.sessionId` reads the active session, but `run_start` is printed (run-agent.ts:276) before `sessionManager.start` (285) and `setActiveCliSession` (315), so it is probably absent. `session_ptr` is set to `/sessionId` in case it appears; UNVERIFIED. `agent_event.event.conversationId` may be the session id (UNVERIFIED). Irrelevant in practice: **no headless resume**: `--id` forces the interactive chat target (main.ts:727-745) and `--json` with interactive errors (793-799). `resume_flag` is null.

**Failure detection**: `run_result.finishReason !== "completed"` means failure: the error text is in `run_result.text`, it is written to stderr, and exit code is 1 (run-agent.ts:404-415). The generic `error_kind` cannot test `finishReason`, so Hashi must rely on the non-zero exit (or special-case `finishReason`). `run_aborted` covers aborts; a `--timeout` hit prints `run timed out after Ns` to stderr with exit 1 (396-399).

**Permissions**: default tool auto-approval is **true** (main.ts:885, resolved with persisted global settings via `resolveStartupToolAutoApprove`). An explicit `--auto-approve false` wins over persisted settings; then each tool approval in a non-TTY is denied with `Tool "<name>" requires approval in a TTY session` (utils/approval.ts:69-77). Whether read-only tools bypass approval is UNVERIFIED. `-y/--yolo` auto-approves a limited tool set. `-p/--plan` runs plan mode.

**Slash commands**: the CLI has chat commands (utils/chat-commands.ts) and workflows; whether a `/`-prefixed prompt is expanded in a headless `--json` run is UNVERIFIED, so it is marked true to be safe (space-prefix).

**Config dir**: `CLINE_DIR` relocates `~/.cline`, and `CLINE_DATA_DIR` defaults to `$CLINE_DIR/data` (sdk/packages/shared/src/storage/paths.ts:158-192). Rules and workflows are also read from `~/Documents/Cline` (paths.ts:169-171), which `CLINE_DIR` does not move. `--config`/`--data-dir` flags do the same per run. Auto-update on startup unless `CLINE_NO_AUTO_UPDATE=1` (src/commands/update.ts:309-313). Telemetry opt-out env: UNVERIFIED.

**Credentials**: `CLINE_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY` are read by the SDK provider code; `-P/--provider` and `-k/--key` select per run (program.ts:49-53; `-k` also persists the key, so it is listed as unsafe for shared tokens).

**Limit patterns**: UNVERIFIED; provider errors are passed through (formatted by `formatCliErrorMessage`), and the only Cline-specific quota string found is ClinePass UI copy.

## Success output (derived from source or docs, not recorded)

```
{"ts":"2026-11-16T10:00:00.000Z","type":"run_start","providerId":"anthropic","modelId":"claude-sonnet-4-5","catalog":"bundled","thinking":"off","mode":"act"}
{"ts":"2026-11-16T10:00:01.000Z","type":"agent_event","event":{"type":"content_end","contentType":"text","text":"OK","agentId":"a1","conversationId":"1763287200000_ab12c"}}
{"ts":"2026-11-16T10:00:01.100Z","type":"run_result","finishReason":"completed","iterations":1,"usage":{"inputTokens":900,"outputTokens":2,"cacheReadTokens":0,"cacheWriteTokens":0,"totalCost":0.0027},"durationMs":2100,"text":"OK","model":{"id":"claude-sonnet-4-5","provider":"anthropic"}}
```

## Error output

```
{"ts":"2026-11-16T10:00:00.000Z","type":"run_start","providerId":"anthropic","modelId":"claude-sonnet-4-5","catalog":"bundled","thinking":"off","mode":"act"}
{"ts":"2026-11-16T10:00:01.100Z","type":"run_result","finishReason":"error","iterations":1,"usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"totalCost":0},"durationMs":800,"text":"429 rate limit exceeded","model":{"id":"claude-sonnet-4-5","provider":"anthropic"}}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. All login state is one JSON file, data/settings/providers.json, under resolveClineDataDir() = $CLINE_DATA_DIR or $CLINE_DIR/data (sdk/packages/shared/src/storage/paths.ts:158-192, 431-437). Written with mode 0600, no OS keyring anywhere in apps/cli or sdk/packages (provider-settings-manager.ts:194-211; grep for keytar/keychain/safeStorage/keyring finds nothing). CLINE_DATA_DIR and CLINE_PROVIDER_SETTINGS_PATH override it, so both must be unset (they are already in credential_vars). ~/Documents/Cline (rules, workflows, hooks, plugins; paths.ts:169-184) is not moved, but holds no login.
- **API-key accounts:** no. The run picks provider = --provider, else providers.json lastUsedProvider, else cline (apps/cli/src/main.ts:940-946), then apiKey = --key || the persisted key/OAuth token for that provider (main.ts:967-973). The env key is only a runtime fallback when nothing is persisted (comment main.ts:977-979; configured-provider-registry.ts:32-44, 123). So with the owner's ~/.cline, a stored Cline login (or a lastUsedProvider of anthropic etc.) outranks CLINE_API_KEY. It works only together with an empty per-account CLINE_DIR, or by passing `-P cline -k <key>`, which also persists the key (program.ts:49-53).
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": "data/settings/providers.json", "pointer": "/providers/cline/settings/auth/metadata/userInfo/email", "jwt_claim": null}
- **Switch:** yes. CLINE_DIR replaces ~/.cline itself (paths.ts:158-167), so sub is empty and the login is data/settings/providers.json. Cline OAuth writes auth {accessToken (prefixed workos:), refreshToken, accountId, expiresAt (epoch ms), metadata.userInfo} under providers.cline.settings (provider-auth-registry.ts:123-165; cline.ts:143-178), and refreshes rewrite the same file, so the expiry pointer orders copies. Caveat: the same file also holds every other provider's key, lastUsedProvider and model choice, so a switch swaps provider selection too, and an expiry exists only for the cline (or cline-pass, stored as cline, provider-auth-registry.ts:244-247) OAuth entry; a ChatGPT/Codex OAuth login lives under its own provider key.
- **Live quota:** yes. ClineAccountService.fetchMe/fetchBalance/fetchCurrentPlan/fetchOrganizationBalance (sdk/packages/core/src/account/cline-account-service.ts:95, 115-121, 161, 184, request at 270-300); base URL https://api.cline.bot (sdk/packages/shared/src/runtime/cline-environment.ts:20); the CLI's account dialog reads these (apps/cli/src/tui/cline-account.ts:178-196). Balance is Cline credits (formatClineCredits), not a percentage; only meaningful for the Cline provider, not for BYO provider keys. Response field names beyond `balance` UNVERIFIED by a call.
- **Usage in output:** tokens and cost
- **Limit messages:** Spec has `insufficient credits`, but the Cline API 402 says code `insufficient_credits` and message `Not enough credits available` (apps/cli/src/tui/cline-account.ts:55-67), so neither is matched. Add: `insufficient_credits`, `not enough credits`, `clinepass limit` (`You have reached your ClinePass limit ... please try again later.`, sdk/packages/llms/src/providers/errors.ts:10-12; CLI prints `ClinePass limit reached`, apps/cli/src/utils/cline-pass-errors.ts:32-41), `free limit reached on model` and `daily free model limit reached` (errors.ts:13; cline-pass-errors.ts:45). Rate limits are classified by HTTP 429 or /rate[\s_-]?limit/ and /per[\s_-]?minute/ (sdk/packages/llms/src/providers/error-classification.ts:33-37, 170-175), so `rate limit` should become `rate_limit` too or rely on 429. Note these arrive in run_result.text with finishReason != completed and exit 1 (apps/cli/src/runtime/run-agent.ts:369-415).

Source: github:cline/cline main, apps/cli (@cline/cli 3.0.74) and sdk/packages. Not installed; nothing run.

- **Login store:** `ProviderSettingsManager` reads/writes `resolveProviderSettingsPath()` = `$CLINE_PROVIDER_SETTINGS_PATH` or `<dataDir>/settings/providers.json` (sdk/packages/shared/src/storage/paths.ts:431-437; sdk/packages/core/src/services/storage/provider-settings-manager.ts:133-137, 194-211, atomic temp write, mode 0600). Shape `{version, lastUsedProvider, providers: {<id>: {settings: {provider, apiKey?, model?, baseUrl?, auth?: {accessToken, refreshToken, accountId, expiresAt, metadata}}}}}` (provider-settings-manager.ts:173, 216-248; provider-auth-registry.ts:132-160).
- **OAuth providers:** cline, cline-pass (stored as cline), openai-codex, oca (provider-auth-registry.ts:244-247; auth.ts:431-436). Cline login uses https://api.cline.bot/api/v1/auth/{authorize,token,refresh} (cline.ts:27-30); `expiresAt` is ISO from the server converted to epoch ms (cline.ts:143-178). If expiresAt is missing the expiry comes from the JWT `exp` (provider-auth-registry.ts:69-96). Login requires a TTY (auth.ts:199) and headless runs never start OAuth (main.ts:977-999).
- **Identity:** `metadata.userInfo = {subject, email, name, clineUserId, accounts}` (cline.ts:56-62, 162-167). `auth.accountId` is clineUserId. Email is not a top-level auth field.
- **Status:** no non-interactive auth status command; subcommands are auth, config, plugin, skill, connect, mcp, doctor, hook, schedule, hub, update, version, kanban (apps/cli/src/main.ts:198-659). `doctor` only kills processes / opens the log (main.ts:547-559). A status check would be reading providers.json or calling /api/v1/users/me.
- **Usage:** `run_result.usage {inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, totalCost}` (run-agent.ts:369-380), so tokens and cost; Hashi's spec pointers match.
- **Quota:** see quota field; balance only, no reset window.
- **Keyring:** none; plain file.
