# Factory Droid (`droid`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `DROID` in `src/agent/specs.rs`.

- **Source:** npm:droid (docs repo github:Factory-AI/factory + embedded bundle of @factory/cli-darwin-arm64; CLI itself closed)
- **License:** UNLICENSED (proprietary)
- **Version checked:** 1.12.1
- **Install:** curl -fsSL https://app.factory.ai/cli | sh | npm i -g droid | irm https://app.factory.ai/cli/windows | iex
- **Headless call:** `droid exec -o json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

Source: Factory docs repo `github:Factory-AI/factory` (opensrc fetched via npm `droid@1.12.1`, docs only, no CLI source) at `~/.opensrc/repos/github.com/Factory-AI/factory/1.12.1/docs/cli/droid-exec/overview.mdx`, plus the embedded JS bundle in the native binary `@factory/cli-darwin-arm64@1.12.1` (pkgs/_factory_cli-darwin-arm64_1.12.1/package/bin/droid (bun-compiled; embedded JS read with `strings`, offsets are line numbers in the `strings -n 8` dump)). Not executed; fixtures are derived from the bundle's serializers.

**Headless:** `droid exec [options] [prompt]` (commander `new Xn("exec")`, single optional `[prompt]` argument). Prompt sources, in order: `-f/--file`, positional, then stdin only when no prompt was given and stdin is not a TTY (exec chunk `Z()`). A prompt of exactly `-` is rejected (`dashSyntaxDeprecated`). Passing both `-f` and a prompt is an error. With a positional prompt, stdin (/dev/null) is never read.

**`--`:** the bundled commander `parseOptions` treats `--` as end of options and pushes the rest to operands (standard commander, seen verbatim in the bundle). So `exec ... -- <prompt>` is safe. Not run end to end (UNVERIFIED in practice).

**Output `-o json`:** one compact line (`JSON.stringify`, not pretty, despite the pretty example in overview.mdx:247-256). Success object `X()`: `{type:"result", subtype:"success"|"failure", is_error, duration_ms, num_turns, result, session_id, usage}`. Failure object `K()`: same keys with `subtype:"failure"`, `is_error:true`, `result` = error message, `num_turns:0`. Usage `pw()`: `{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, factory_credits, thinking_tokens, ttft_ms?}` (inclusive of subagents: `getInclusiveTokenUsage`). This is Claude Code's result shape minus `total_cost_usd`; cost is Factory credits, not USD, so cost is left null. Note `subtype` on failure is `failure`, not Claude's `error_during_execution`. Errors before a session exists (auth, bad flags, MCP bootstrap) go to **stderr** in red and stdout is empty; Hashi's stderr fallback covers that.

**`-o stream-json`:** JSONL via `k9()`: `{type:"system",subtype:"init",cwd,session_id,tools,model,reasoning_effort}`, `{type:"message",role:"user"|"assistant",id,text,timestamp,session_id}`, `{type:"reasoning",...}`, `{type:"tool_call",id,messageId,toolId,toolName,parameters,timestamp,session_id}`, `{type:"tool_result",id,messageId,toolId,isError,value?,error?{type:"tool_error",message},timestamp,session_id}`, `{type:"error",source:"cli"|"agent_loop",message,timestamp,session_id}`, `{type:"system",subtype:"hook_settings_changed",...}` and the terminal `{type:"completion",finalText,numTurns,durationMs,session_id,timestamp,usage}`. `json` was chosen because it is the simplest stable contract. Also `--output-format stream-jsonrpc` (long-lived JSON-RPC; docs say `--auto` and `--model` flags do NOT configure sessions there, overview.mdx:273-277): never use it.

**Model:** `-m, --model <id>` (also `custom:<Display-Name>-<index>` for BYOK models from `~/.factory/settings.json`). `-r` is reasoning effort, not resume.

**Resume:** `-s, --session-id <id>` continues an existing session (`ensureSessionLoaded`), requires a prompt. `--fork <id>` branches. Hidden `--init-session-id <id>` ("internal use, for subagent pre-generation") creates a new session with a caller-chosen id, so Hashi could pre-assign ids (UNVERIFIED end to end; internal flag may change). Session ids are UUIDs.

**Permissions (good default):** no `--auto` means read-only spec-like mode (overview.mdx:37-41, 127-150). Allowed without flags: reads and read-only shell commands (cat, ls, find without -exec, git status/log/diff, whoami, ps...). Anything above the autonomy level makes the whole run **fail fast**: `Exec ended early: insufficient permission to proceed. Re-run with --auto low|medium|high ...` (strings ~159707-159714) with non-zero exit and an `is_error:true` result. That means a prompt that makes the model try to edit kills the run instead of just denying the tool. Caveat: read-only shell still runs (e.g. `cat` outside the workspace is classed read-only), so the agent can read any file the Hashi user can. Pinned `--remove-tools execute-cli` removes the shell tool entirely: the id `execute-cli` is the shell tool constant in the bundle (`s6="execute-cli"`, docs example `--disabled-tools execute-cli` overview.mdx:382), but `--remove-tools` rejects unknown ids, so verify the id with `droid exec --list-tools -o json` before shipping (UNVERIFIED). `--auto low|medium|high`, `--skip-permissions-unsafe` and `--mission` (requires high) raise autonomy; org-managed maximum can cap them. MCP servers from user settings are started for exec (`mcpStartFailed` warning); there is no flag to skip MCP (UNVERIFIED whether project `.factory/` settings in the workspace load; assume yes).

**Slash commands:** the prompt is passed through `resolveExecPromptForAgent` -> `AL()`, which resolves any `/command` token (regex `(^|\s)/`) including custom commands and skills. Treat leading `/` as a slash command.

**Env/config:** `FACTORY_API_KEY` (docs overview.mdx:103-111; error `Authentication failed. FACTORY_API_KEY is set but appears to be invalid.`). `FACTORY_API_BASE_URL` selects the API host. Home/config root: `FACTORY_HOME_OVERRIDE` replaces `HOME` when resolving `~/.factory` (`Cje(){return process.env.FACTORY_HOME_OVERRIDE??HOME??USERPROFILE??cwd}`), but interactive logins are stored in the OS keychain unless `FACTORY_DISABLE_KEYRING=1` (bundle message: "set FACTORY_DISABLE_KEYRING=1 to store credentials on disk"), so both are set for per-account isolation. `FACTORY_DROID_AUTO_UPDATE_ENABLED=false` disables auto-update (function `l()`; the Factory desktop app itself spawns droid with this set to "false"). No interactive `login` subcommand exists in 1.12.1's subcommand list; sign-in is `/login` inside the TUI (or an API key).

**Limits:** Factory plan messages: `Usage limit reached`, `Your standard credit limits are exhausted`, `Rate limited by Factory`, `Provider rate limited`; BYOK: `Your model provider's usage quota is exhausted`; ChatGPT plan: `has reached its usage limit`. Exact wording reaching `result` on exec is UNVERIFIED.

**Exit codes:** 0 success, non-zero on any failure (docs overview.mdx:599-602); earlier research recorded 1 runtime, 2 bad args (UNVERIFIED in 1.12.1; `useSignalExitCodes` is enabled for exec). Git repo not required (only `-w/--worktree` needs one).

## Success output (derived from source or docs, not recorded)

```
{"type":"result","subtype":"success","is_error":false,"duration_ms":5657,"num_turns":1,"result":"`add` returns the sum of its two arguments.","session_id":"8af22e0a-d222-42c6-8c7e-7a059e391b0b","usage":{"input_tokens":11234,"output_tokens":187,"cache_read_input_tokens":8192,"cache_creation_input_tokens":0,"factory_credits":2,"thinking_tokens":0,"ttft_ms":812}}
```

## Error output

```
{"type":"result","subtype":"failure","is_error":true,"duration_ms":2310,"num_turns":0,"result":"Exec ended early: insufficient permission to proceed. Re-run with --auto low|medium|high (to raise autonomy), or use --skip-permissions-unsafe for dangerous commands.","session_id":"8af22e0a-d222-42c6-8c7e-7a059e391b0b","usage":{"input_tokens":5120,"output_tokens":64,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"factory_credits":1,"thinking_tokens":0}}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. FACTORY_HOME_OVERRIDE replaces HOME when resolving ~/.factory (docs/research/agents/droid.md, Cje()); the credential store dir is qS() = join(home, .factory) and a login exists when auth.v2.keyring, auth.v2.loginkeychain or auth.v2.file is present (droid byte 197148530, B7()). Keyring mode keeps the encryption key in the macOS Keychain under service "Factory CLI" / "auth-encryption-key" shared by every home (byte 197178235, cr/di constants); FACTORY_DISABLE_KEYRING=1 stores on disk (message at byte 7912270).
- **API-key accounts:** yes. The auth doctor check reports "using FACTORY_API_KEY (overrides any stored login session)" (droid byte 218579077) and runAuthChecks labels the source "FACTORY_API_KEY environment variable" vs "stored login session" (byte 218582312).
- **Status check:** {"args": ["doctor", "--checks", "auth", "--json"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. The stored login is ciphertext ({version, backend: keyfile|keyring|security-cli, ciphertext}, byte 197188581), so there is no readable expiry. The owner's plain droid normally uses keyring mode (auth.v2.keyring + Keychain key) and a secure login hides a file one ("a login saved to disk instead would be hidden behind it", byte 7912270), so dropping auth.v2.file into ~/.factory would not take effect unless the owner also runs with FACTORY_DISABLE_KEYRING=1.
- **Live quota:** yes. og() fetches /api/billing/limits; j() reads limits.standard and extraUsageBalanceCents; C() treats any of fiveHour/weekly/monthly as exhausted; le() reads usedPercent and windowEnd (droid bytes 210162322 for C() and 210165431 for og() and j()). The doctor connectivity probes list the same endpoint (byte 218650320). Easy for FACTORY_API_KEY accounts; an OAuth login's token is encrypted on disk.
- **Usage in output:** tokens
- **Limit messages:** The strings in the spec are the app messages (usage limit reached, standard credit limits exhausted, rate limited by Factory, provider rate limited, provider usage quota exhausted, ChatGPT plan usage limit). The limit service also classifies "standard-exhausted" and "out-of-credits" states (fe(), byte 210165598); consider adding "out of credits" (exact user-facing text unverified).

Closed bun binary; JS read from the embedded bundle by byte offset (not executed). Docs: ~/.opensrc/repos/github.com/Factory-AI/factory/1.12.1.

- `droid doctor` options: --json, --verbose, --timeout, --checks <names>; check names include config, environment, system, connectivity, auth, ... (RA list at byte 198374591); the action sets process.exitCode=1 when any selected check fails (doctor command at byte 198462834). `--checks auth` does not spend credits.
- Identity: the decrypted credential holds the WorkOS id token with an `email` claim (schema ft, byte 197172001) and a whoami object, and the API has GET /api/cli/whoami (byte 218650320), but nothing readable sits in a plain file.
- Output: `-o json` usage has input/output/cache tokens and factory_credits but no USD cost, so cost stays null.
