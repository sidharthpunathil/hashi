# GitHub Copilot CLI (`copilot`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `COPILOT` in `src/agent/specs.rs`.

- **Source:** npm:@github/copilot (closed; native SEA binary @github/copilot-darwin-arm64@1.0.112, embedded copilot.tgz extracted and read: cli-main.js, cli-native.node help strings, schemas/session-events.schema.json, changelog.json)
- **License:** Proprietary (GitHub Copilot CLI license, LICENSE.md in package)
- **Version checked:** 1.0.112
- **Install:** npm install -g @github/copilot | brew install copilot-cli | winget install GitHub.Copilot | curl -fsSL https://gh.io/copilot-install | bash
- **Headless call:** `copilot --output-format json`, prompt flag eq `--prompt=`
- **Runs tools without asking when headless:** no

## Notes

## How this was derived
Nothing was executed. The npm package `@github/copilot` is only a loader; the real CLI is a Node SEA binary in `@github/copilot-darwin-arm64@1.0.112`. Its `__NODE_SEA_BLOB` section (Mach-O offset 93323264) holds a gzip tar `copilot.tgz` (the loader extracts it via `getRawAsset("copilot.tgz")`). I extracted it without running anything: `package/cli-main.js` (8.8 MB bundle), `package/prebuilds/darwin-arm64/cli-native.node` (Rust clap front end; all CLI flag help text lives here), `package/schemas/session-events.schema.json`, `package/changelog.json`. Fixtures are DERIVED from these, not recorded.

## Headless
- `-p/--prompt <text>`: "Execute a prompt in non-interactive mode (exits after completion)" (cli-native.node help strings). Arg parsing is Rust clap, so `--prompt=<text>` keeps a leading `-` as the value. The prompt is a flag value, not a positional, so `--` is irrelevant; use flag_eq.
- `--output-format json`: "Output format: 'text' (default) or 'json' (JSONL, one JSON object per line)".
- stdin: `--fleet` help says prompt can come from `-p` or piped stdin. Whether an empty `/dev/null` stdin is appended to `-p` is UNVERIFIED (should be harmless).

## JSONL shape (cli-main.js around byte 5928903, `g4n`, `d$e`, `iAt`)
- Every session event is written as `JSON.stringify(event)` with secret redaction and C1 escaping: envelope `{id, timestamp, parentId, ephemeral?, agentId?, type, data}` (schema `*Event` definitions).
- Filtered OUT of stdout (`azi` set): `session.idle`, `session.usage_info`, `session.shutdown`, `assistant.streaming_delta`, `assistant.usage`, `assistant.intent`, `permission.*`, `user_input.*`, `elicitation.*`, `hook.*`, `system.message`, `skill.*`, `sandbox.decision`, and `model.<kind>` events (`AL`). So the per-call `assistant.usage` token event is NOT printed (the older doc's guess that tokens are absent is mostly right).
- `session.start {sessionId, version, producer:"copilot-agent", copilotVersion, startTime, selectedModel?, context{cwd,...}}` is replayed first (changelog: "Include session start, resume, and model-change events in -p --output-format json output").
- `assistant.message.data {messageId, content, toolRequests?, outputTokens?, model?, turnId?, parentToolCallId?}`; final answer = last `assistant.message` `data.content`. Sub-agent messages carry `parentToolCallId`; tool-call turns may have empty `content` (Hashi's `last` would then fail if the final message is empty; UNVERIFIED that this happens).
- `session.error.data {errorType, message, statusCode?, errorCode?, ...}`; in json mode it goes to stdout instead of stderr (`_n` handler, `Ir&&B9n(...)`).
- Terminal line, always written in prompt+json mode even after early failure (`d$e`, plus finally-block fallback): `{"type":"result","timestamp","sessionId","exitCode","usage":{"premiumRequests","totalApiDurationMs","sessionDurationMs","codeChanges":{"linesAdded","linesRemoved","filesModified"}}}`, plus `outcome:"blocked",blocker` when blocked. `sessionId` exists only on this line, so `session_ptr=/sessionId` works.
- Tokens: `session.usage_record` ("internal durable accounting receipt", `data.usage{model,inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens,reasoningTokens,cost,...}`) is not in the filter set, so it should reach stdout, but whether it is emitted live on the event bus during `-p` is UNVERIFIED. `assistant.message.data.outputTokens` is a fallback for output tokens only. The reliable path is `--usage-output-file <path>` (JSON written at exit with input/output/cache/reasoning tokens per model), which this schema cannot express. `inputTokens` may exclude cache reads (UNVERIFIED); `usage.cost` is a model multiplier, not USD, so cost_ptr is null.

## Session and model
- `--session-id <id>`: "Resume an existing session or task by ID, or set the UUID for a new session"; `--resume[=value]` takes id, prefix or name; `--continue` is the latest. Sessions live in `$COPILOT_HOME/session-state/<id>/` and `--resume` switches cwd to the session's recorded cwd unless `COPILOT_DISABLE_RESUME_AUTO_CD=1`.
- `--model <id>` (`auto` allowed); also `--effort`, `--context`. `COPILOT_MODEL` env.

## Permissions (help topic `permissions`)
- "In autopilot and -p runs, a tool permission request that would prompt is denied." `--allow-all-tools` help: "Non-interactive mode cannot ask, so it denies tool requests that no allow rule (or --assisted-approval) approves." So no hang, but "Commands it recognizes as read-only run without asking", hence the pinned `--deny-tool=shell` (no argument matches every shell command) and `--deny-tool=write`. Deny rules win over every allow, even `--allow-all`/`--yolo`. File access is limited to cwd plus the temp dir (`--disallow-temp-dir` drops the temp dir; on macOS your own /tmp files are allowed too).
- `COPILOT_ALLOW_ALL=true` auto-approves everything AND trusts the folder: Hashi must strip it from the inherited env.
- Folder trust: in `-p` an untrusted cwd skips workspace MCP config, hooks, skills and plugins (changelog 1.0.99+ adds `--trust-folder` for one run). Do not pass `--trust-folder`.
- Custom instructions (AGENTS.md, CLAUDE.md, .github/copilot-instructions.md in cwd) still load; add `--no-custom-instructions` if the workspace is untrusted. Copilot also reads `.claude/skills`, `.claude/commands` and `.claude/settings.json` hooks in a project.
- A prompt starting with `/` that names a skill is expanded as that skill (`z5n` in cli-main.js); Hashi should neutralise a leading `/`.

## Auth and config
- Token precedence: `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN` (help topic `environment`), taking precedence over stored credentials. `copilot login` stores the token in the OS keychain, falling back to plain text in config.json only after consent; with stdin not a TTY that consent prompt returns no and login fails with "Login succeeded, but the token was not saved". It also auto-detects a `gh` CLI login unless `--no-auto-login`.
- `COPILOT_HOME` (default `~/.copilot`) relocates settings, config.json, sessions, logs. Keychain entries are NOT under COPILOT_HOME, so two accounts that both use keychain login may collide (UNVERIFIED); per-account `COPILOT_GITHUB_TOKEN` is the safe separation. `--config-dir` is deprecated in favour of COPILOT_HOME. `COPILOT_CACHE_HOME` holds downloaded CLI versions.
- BYOK: `COPILOT_PROVIDER_BASE_URL` + `COPILOT_PROVIDER_TYPE` + `COPILOT_PROVIDER_API_KEY` bypasses GitHub auth.
- Auto-update downloads newer CLIs unless `COPILOT_AUTO_UPDATE=false` or `--no-auto-update` (disabled by default when `CI` is set).

## Limits
Help topic `limits` quotes: "You've hit the rate limit for this model.", "You've hit your session rate limit.", "You've reached your weekly rate limit.", "You've hit your rate limit.", plus plan budget "Plan: 100% used, resets <date>". `continueOnAutoMode` can auto-switch to Auto on rate limit. The `errorType` value in the error fixture is illustrative (UNVERIFIED).

## Exit codes
`exitCode` in the result line mirrors the process exit (0 ok, non-zero on error, interruption or a failed share/usage write); exact values beyond 0/1 UNVERIFIED. Git repo not required.

## Success output (derived from source or docs, not recorded)

```
{"id":"e1","timestamp":"2026-11-16T10:00:00.000Z","parentId":null,"type":"session.start","data":{"sessionId":"0cb916db-26aa-40f2-86b5-1ba81b225fd2","version":1,"producer":"copilot-agent","copilotVersion":"1.0.112","startTime":"2026-11-16T10:00:00.000Z","selectedModel":"claude-sonnet-4.5","context":{"cwd":"/srv/hashi/ws/u1"}}}
{"id":"e2","timestamp":"2026-11-16T10:00:00.050Z","parentId":"e1","type":"user.message","data":{"content":"what is 3 + 5?","source":"user"}}
{"id":"e3","timestamp":"2026-11-16T10:00:00.060Z","parentId":"e2","type":"assistant.turn_start","data":{"turnId":"0"}}
{"id":"e4","timestamp":"2026-11-16T10:00:01.900Z","parentId":"e3","type":"session.usage_record","data":{"usage":{"model":"claude-sonnet-4.5","inputTokens":9120,"outputTokens":6,"cacheReadTokens":0,"cacheWriteTokens":0,"cost":1,"duration":1830,"finishReason":"stop"}}}
{"id":"e5","timestamp":"2026-11-16T10:00:01.910Z","parentId":"e4","type":"assistant.message","data":{"messageId":"m1","model":"claude-sonnet-4.5","content":"8","toolRequests":[],"outputTokens":6,"turnId":"0"}}
{"id":"e6","timestamp":"2026-11-16T10:00:01.920Z","parentId":"e5","type":"assistant.turn_end","data":{"turnId":"0"}}
{"type":"result","timestamp":"2026-11-16T10:00:02.100Z","sessionId":"0cb916db-26aa-40f2-86b5-1ba81b225fd2","exitCode":0,"usage":{"premiumRequests":1,"totalApiDurationMs":1830,"sessionDurationMs":2100,"codeChanges":{"linesAdded":0,"linesRemoved":0,"filesModified":[]}}}
```

## Error output

```
{"id":"e1","timestamp":"2026-11-16T10:00:00.000Z","parentId":null,"type":"session.start","data":{"sessionId":"0cb916db-26aa-40f2-86b5-1ba81b225fd2","version":1,"producer":"copilot-agent","copilotVersion":"1.0.112","startTime":"2026-11-16T10:00:00.000Z","selectedModel":"claude-sonnet-4.5","context":{"cwd":"/srv/hashi/ws/u1"}}}
{"id":"e2","timestamp":"2026-11-16T10:00:00.050Z","parentId":"e1","type":"user.message","data":{"content":"what is 3 + 5?","source":"user"}}
{"id":"e3","timestamp":"2026-11-16T10:00:00.700Z","parentId":"e2","type":"session.error","data":{"errorType":"rate_limit","message":"You've hit your session rate limit. Please wait 41 minutes before trying again.","statusCode":429}}
{"type":"result","timestamp":"2026-11-16T10:00:00.800Z","sessionId":"0cb916db-26aa-40f2-86b5-1ba81b225fd2","exitCode":1,"usage":{"premiumRequests":0,"totalApiDurationMs":0,"sessionDurationMs":800,"codeChanges":{"linesAdded":0,"linesRemoved":0,"filesModified":[]}}}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. COPILOT_HOME (must be absolute) moves settings, state and sessions, but `copilot login` stores the token in the system credential store and falls back to a plain-text config file only when no store works or storeTokenPlaintext is set (cli-native.node login help at byte 9548848; settings schema entry storeTokenPlaintext at runtime.node byte 114955099; /logout help at cli-native.node byte 9209264 says the token is in the system keychain or in config.json under the active Copilot home). The keychain is not under COPILOT_HOME, so per-folder separation needs the plaintext setting pre-seeded in each account dir.
- **API-key accounts:** yes. Login help: tokens in COPILOT_GITHUB_TOKEN, GH_TOKEN, GITHUB_TOKEN are checked in that order and are suited to headless use (cli-native.node byte 9548848); /logout help: it does not sign out an env token, which stays active (byte 9209264). Needs a fine-grained PAT with Copilot Requests or a Copilot/gh OAuth token; classic ghp_ PATs are rejected.
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": "config.json", "pointer": "/lastLoggedInUser/login", "jwt_claim": null}
- **Switch:** no. By default the token is in the OS keychain, not a file. Even with storeTokenPlaintext the token shares config.json with the owner's machine-wide state (trusted folders, installed plugins, onboarding flags: api.schema.json GlobalStateLoadResult around 38270-38360), so copying it would clobber that state; GitHub OAuth tokens have no expiry field to compare.
- **Live quota:** yes. The URL is in runtime.node (bytes 103409854, 103527079) next to the device-code endpoints; the response fields are described in schemas/api.schema.json:31290-31340 (quota_snapshots, quota_reset_date, token_based_billing). Easy for COPILOT_GITHUB_TOKEN accounts; a keychain login needs the token read from the keychain. The exact header form is from the public VS Code extension, unverified for this CLI.
- **Usage in output:** tokens
- **Limit messages:** The spec's strings match the `copilot help limits` topic ("You've hit the rate limit for this model.", "...session rate limit.", "...weekly rate limit.", "...your rate limit.", "Plan: 100% used"). The runtime also has "GitHub API rate limit exceeded for the authenticated token" (runtime.node byte 103529134), already covered by "rate limit".

Closed source; read from the extracted 1.0.112 SEA package (cli-main.js is the UI bundle, the auth and config logic is in the Rust prebuilds runtime.node and cli-native.node, offsets are byte offsets).

- No non-interactive auth status command: subcommands do not include status or whoami; /login, /logout, /user exist only in the TUI (cli-main.js byte 2191312 `You are not logged in.`).
- Identity: global state (lastLoggedInUser {host, login, kind?}, loggedInUsers) per api.schema.json:38322-38333 and LoggedInUser at 42585-42605; that it lives in $COPILOT_HOME/config.json is inferred from the /logout help and the `config.json` string beside the auth keys `authTokens`, `copilotTokens_direct` in runtime.node (byte 104056474); unverified. Older releases used snake_case last_logged_in_user.
- COPILOT_DISABLE_KEYTAR appears in the env lists of runtime.node (byte 114906193) and cli-native.node (byte 9665946); it probably forces file storage but its semantics are unverified, so it is not proposed as keyring_off_env.
- COPILOT_HOME override must be absolute (runtime.node byte 101183150).
