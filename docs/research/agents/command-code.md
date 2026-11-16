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

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. All login state is one plain JSON file, <HOME or USERPROFILE>/.commandcode/auth.json (homeDir cli.fmt.js:5227-5230, resolveCommandAuthDir 5259-5262, co='.commandcode' 77888, mo prod 'auth.json' 77890-77894; storeCommandAuthCredentials 5485-5505 writes apiKey,userId,userName,keyName,authenticatedAt with mode 384). No keychain or keyring code exists. There is no dedicated dir override, so the account folder must be a per-account HOME (and USERPROFILE on Windows); HOME also moves settings, skills, mods, sessions and caches under ~/.commandcode. Keep COMMANDCODE_API_ENV and --staging/--local unset: they switch the file to auth.staging.json/auth.local.json (getApiEnv 3947-3951, apiEnvFromEnvVar 3937-3940).
- **API-key accounts:** yes. getCommandAuthKey (cli.fmt.js:5329-5334) returns the trimmed env key first (getCommandApiKeyFromEnv 5313-5318, uo='COMMAND_CODE_API_KEY' 77889) and only falls back to auth.json apiKey when the env var is empty, so the owner's stored login cannot outrank it. Caveat: BYOK provider keys and provider OAuth from the owner's ~/.commandcode (config.json, auth.json provider entries) still apply when HOME is shared, so a key account is cleanest with its own HOME too.
- **Status check:** {"args": ["whoami"], "signed_in_when": "exit 0"}
- **Identity:** {"file": ".commandcode/auth.json", "pointer": "/userName", "jwt_claim": null}
- **Switch:** yes. Login is a long-lived Command Code API key in auth.json with no expiry field (fields apiKey, userId, userName, keyName, authenticatedAt; storeCommandAuthCredentials cli.fmt.js:5485-5505), so swapping the file swaps the account and a run never refreshes it. Under the account dir (a per-account HOME) the same folder is .commandcode. Caveat: auth.json also holds provider (anthropic/copilot/openai) credentials (createAuthFileCredentialStore over createCommandAuthFileStore, ~97588 and the provider auth service), which move with it; refreshed provider OAuth tokens would be written into the live file during a run.
- **Live quota:** yes. Remaining plan credits and per-window limits come from the billing endpoints, read with the same API key the CLI uses. Response field shapes are taken from the consumer code (getUsageViewModel), not from a recorded response; UNVERIFIED by running. Not available in --local-only / CMD_LOCAL_ONLY mode (5547).
- **Usage in output:** tokens
- **Limit messages:** Spec patterns match the bundle: 'Rate limit exceeded. Please wait a moment and try again.' and the window message "You've reached today's limit on <model>" / "You've reached your <window> usage limit." (classifyPrintModeError cli.fmt.js:56126-56200, formatWindowLimitMessage 9834-9842), 'Insufficient credits for Command Code' (buildInsufficientCreditsMessage 36788-36796, exit 10). The spend cap message is server text read from the error body (parseSpendCapError, code Pk), and the in-app label is 'Spend limit reached' (89414), so 'spend limit' is plausible but its exact server wording is UNVERIFIED. Each of these also sets a distinct exit code (5 rate limited, 4 spend cap, 10 credits; aI 92296-92309), which is more reliable than text. Optional addition: 'premium requests used up' is a silent model fallback, not a failure, so do not add it.

Source: npm command-code 1.89.1 (UNLICENSED, closed), dist/cli.mjs formatted with prettier 3.3.3 into scratchpad/cmdcfmt/cli.fmt.js (149,696 lines, same numbering as docs/research/agents/command-code.md). Not installed, nothing run.

**Login storage.** `${HOME ?? USERPROFILE}/.commandcode/auth.json` (5227-5230, 5259-5268, 77888-77894). The file is plain JSON written with mode 0600 (5485-5505); `command-code login` is a browser OAuth flow via `https://commandcode.ai/studio/auth/cli?callback=http://127.0.0.1:<port>/callback` (buildCommandAuthUrl 5482-5484). `login [provider]` also accepts anthropic, copilot, openai (resolveProviderAuthTarget 106382-106397), stored through the same auth file store. No keychain/keytar/keyring code in the bundle.

**API key precedence.** COMMAND_CODE_API_KEY env wins over auth.json (getCommandAuthKey 5329-5334, checkCommandAuthStatus 5340-5345 marks fromEnv).

**Status.** Two commands, both top-level (cX.addCommand list 149676-149694; the `auth` group at 107160 is built but never added):
- `command-code status --json` prints `{authenticated, version, user?, provider?, model?, context_window?, error?}` and exits 1 when not authenticated or on error (statusAction ~106957-106963, buildCommandAuthStatusJson 5421-5442). It only checks that an API key exists (env or file); it never calls the server, so a revoked key still reports authenticated.
- `command-code whoami` calls GET /alpha/whoami with the key (getAuthenticatedEntity 8094-8096, oa 79304) and exits 1 when there is no key, the request fails, or no user comes back (whoamiAction 149631-149644); on success it prints Name, Email, Username (projectWhoamiUserRows 8103-8109) as text. Chosen as status_check because it actually validates the key; `status --json` with pointer /authenticated true is the offline alternative.

**Identity.** auth.json `/userName` is the Command Code username (not the email); generic values 'API Key'/'Manual API Key' mean a pasted key with no real name (isGenericCommandAuthUserName 5405-5407). The email is only available from the whoami API (`user.email`).

**Quota.** /usage overlay data path: fetchUsageData (6774-6806) calls whoami?limits=1, billing/credits, billing/subscriptions and usage/summary on https://api.commandcode.ai with the key; getUsageViewModel (6836-6880) derives monthlyRemaining, purchasedRemaining, freeRemaining, usagePercent, windowLimits, orgLimits.

**Usage in output.** Final result line `{type:'result', subtype, sessionId?, stopReason?, usage:{inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens}, durationMs, finalText, error?}` (buildPrintResultLine 56245-56256); usage is run_end usage else summed model_request_end usage (56265-56280). Spec pointers /usage/inputTokens and /usage/outputTokens are right; no cost field, so 'tokens'. Cached input is separate (cacheReadTokens), UNVERIFIED whether inputTokens excludes it.

**Limits.** See limit_notes; exit codes in aI (92296-92309).
