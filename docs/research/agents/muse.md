# Muse Code (`muse`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **unverified**. The spec Hashi runs is `MUSE` in `src/agent/specs.rs`.

- **Source:** closed: Rust binary muse 1.4.20-R8619.1 (muse-aarch64-linux from lookaside.facebook.com via the public channel manifest), inspected with strings; bash launcher https://api.meta.ai/muse-launcher.sh read in full
- **License:** Proprietary (Meta)
- **Version checked:** 1.4.20-R8619.1
- **Install:** curl -fsSL https://dev.meta.ai/install.sh | bash (installs the bash launcher ~/.local/bin/muse, which downloads and self-updates the binary)
- **Headless call:** `muse exec --json`, prompt positional
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

**What it is.** Meta's **Muse Code** terminal agent (Meta Superintelligence Labs, beta 2026-08-05, model `muse-spark-1.2`). Closed source. Internally a Codex-style Rust harness ("tbh"; build paths `fbcode/musecode/...`), not a Claude Code or Gemini CLI fork, and its JSON is NOT Claude Code's result object. No public docs for the JSON schema exist; everything below comes from (a) the launcher script `https://api.meta.ai/muse-launcher.sh` (read in full), (b) strings in the 429 MB `muse-aarch64-linux` 1.4.20-R8619.1 binary (downloaded from the public manifest at `https://api.meta.ai/muse-code/channels/muse-stable`, not run), and (c) third-party write-ups (https://docs.e2b.dev/agents/muse.md, https://www.digitalapplied.com/blog/muse-code-deep-dive-fan-out-event-log-skills, https://codersera.com/blog/muse-code-complete-guide-2026/). Fixtures are reconstructed from schema strings and are UNVERIFIED.

**Headless.** `muse exec "<prompt>"` (binary strings: `muse exec --session-id <session-id> "<follow-up>"`, `muse exec --prompt-file ...`). `exec` flags from the binary's help text: `--json` ("Emit machine-readable JSONL events on stdout"), `--prompt-file <PATH>`, `--api-key-stdin`, `--provider echo|meta`, `--preset <NAME>` ("Model id for non-echo providers"), `-c/--config KEY=VALUE`, `--reasoning-effort none..ultra`, `--image`, `--workspace <PATH>`, `-w/--worktree`, `--output-schema <FILE>`, `--max-model-steps <N>`, `--max-tool-output-bytes`, `--session-id <UUID>` ("Use a specific session id"; also used to continue a saved session), `--user-input-auto-resolve`, `--disable-web-tools`, `--no-session-log`, `--yolo` ("Disable approval and sandbox, trust this workspace"), `--trust-workspace`, `--disable-approval`, `--disable-sandbox`, `--sandbox-network <MODE>` (default proxy-only), `--disable-write`, `--disable-shell`, `--approval-mode untrusted|on-request|never` (default on-request), `--approval-judge off|on` (LLM judge for prompt-bound calls, default on), `--permission-profile <ID>`. Exit codes (digitalapplied): 0 turn completed, 1 failed/cancelled (incl. `--max-model-steps`), 2 usage error, 130/143 signals.

**Prompt passing.** Positional after `exec`. Whether `--` is honoured is UNVERIFIED (the parser is not clap-looking help text), so `positional` is chosen; a prompt starting with `-` must be refused or passed via `--prompt-file` (not expressible in this schema).

**Output `--json`.** Binary schema text: "`view/page` and `muse exec --json` share this one definition": each line is an **unframed view notification** `{"method": <event>, "params": {sessionId, viewCursor, sourceRange, ...event fields}}` (the JSON-RPC notification minus `jsonrpc` and `emittedAtMs`). Methods seen in the binary: `session/started`, `turn/started`, `turn/completed` ("Turn terminal: completed | failed | cancelled, with usage and the settled error object"; `TurnError{message, retryable, quota{limitKind, resetsAtMs}}`), `item/started`, `item/updated`, `item/delta` (ephemeral streaming), `item/completed` ("the authoritative final object"; item kinds include `userMessage`, `agentMessage`, `reasoning`, `toolCall`, `subagent`, ...), `session/tokenUsage` (fields `turnId, modelId, usage{inputTokens, outputTokens, cachedTokens, cacheReadTokens, reasoningTokens}, promptTokens, totalTokens, finishReason, durationMs, cumulative{promptTokens, outputTokens, totalTokens, cacheReadTokens, cost{usd, partial}}`), `session/contextUsage`, `approval/requested`, `userInput/requested`. **UNVERIFIED:** the item discriminator name (`kind`) and the text field of an `agentMessage` item (`text` assumed); `text_ptr` only resolves on agentMessage items if tool items lack `/params/item/text`. Hashi should filter `item/completed` on `/params/item/kind == "agentMessage"` (needs a kind filter Hashi lacks) and treat `turn/completed` with `status != "completed"` or a non-null `params.error.message` as failure plus the exit code (no event type alone means failure, so `error_kind` is null). Session cost: `/params/cumulative/cost/usd` on the last `session/tokenUsage` (cumulative, so do not sum).

**Resume.** `muse exec --session-id <UUID> "<follow-up>"` continues a saved session headlessly (binary strings, digitalapplied). `muse resume` needs a TTY. `--no-session-log` makes the session unresumable.

**Permissions.** Default `on-request` with the OS sandbox ON and failing closed, network `proxy-only`; shell runs inside the sandbox without asking (Codex semantics, UNVERIFIED), hence `auto_approves_tools_headless: true` (sandboxed). The approval judge (default on) may approve prompt-bound calls automatically. For read-only use `--disable-write --disable-shell`. `--yolo` disables approvals AND the sandbox.

**Launcher gotchas.** `muse` on PATH is a bash script (fine without a shell: it has a shebang). It checks for updates at most hourly and, when due, **spawns a detached background updater** (`({ update_launcher; update_binary; } &)`) that `kill_on_drop` will not reap; set `MUSE_NO_AUTO_UPDATE=1`. It may run an interactive device login when the binary is missing; `MUSE_LOGIN=0` suppresses that (launcher.sh:650-656). Other launcher env: `MUSE_CHANNEL` (muse-stable|muse-canary), `MUSE_UPDATE_INTERVAL_SECONDS`, `MUSE_AUTH_PATH` (default `$XDG_CONFIG_HOME/muse/auth.json` or `~/.config/muse/auth.json`).

**Config/env.** User settings `~/.config/muse/settings.json` (codersera); data under `XDG_DATA_HOME` (binary: "set HOME or XDG_DATA_HOME and retry"). `XDG_CONFIG_HOME` moves auth.json and settings but not session data, so for a separate account set both `XDG_CONFIG_HOME` and `XDG_DATA_HOME` (or `HOME`). Provider env read by the binary: `META_API_KEY`, `ANTHROPIC_API_KEY/BASE_URL`, `OPENAI_API_KEY/BASE_URL`, `OPENROUTER_API_KEY/BASE_URL`, `MUSE_MODEL`. It also reads foreign context (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `.claude/skills`, `.codex/skills`, `CLAUDE.md`); `--no-foreign-personal-context` excludes personal rules and skills.

**Platforms.** macOS, Linux, Windows (manifest has x86_windows).

## Success output (derived from source or docs, not recorded)

```
{"method":"session/started","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"1","sourceRange":{"start":0,"end":1}}}
{"method":"turn/started","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"2","sourceRange":{"start":1,"end":2},"turnId":"t1"}}
{"method":"item/completed","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"3","sourceRange":{"start":2,"end":3},"turnId":"t1","item":{"id":"i1","kind":"userMessage","text":"what does src/lib.rs do?"}}}
{"method":"session/tokenUsage","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"4","sourceRange":{"start":3,"end":4},"turnId":"t1","modelId":"muse-spark-1.2","usage":{"inputTokens":8120,"outputTokens":30},"promptTokens":8120,"totalTokens":8150,"finishReason":"stop","durationMs":1310,"cumulative":{"promptTokens":8120,"outputTokens":30,"totalTokens":8150,"cost":{"usd":0.0008,"partial":false}}}}
{"method":"item/completed","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"5","sourceRange":{"start":4,"end":5},"turnId":"t1","item":{"id":"i2","kind":"agentMessage","text":"It defines `add`, which sums two integers."}}}
{"method":"turn/completed","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"6","sourceRange":{"start":5,"end":6},"turnId":"t1","status":"completed","durationMs":1400,"usage":{"inputTokens":8120,"outputTokens":30}}}
```

## Error output

```
{"method":"session/started","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"1","sourceRange":{"start":0,"end":1}}}
{"method":"turn/completed","params":{"sessionId":"0193a6c2-7b4d-7e2a-9c1f-0a8b7d6e5f41","viewCursor":"2","sourceRange":{"start":1,"end":2},"turnId":"t1","status":"failed","error":{"message":"usage limit reached for this window","retryable":false,"quota":{"limitKind":"window","resetsAtMs":1763305200000}}}}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. On macOS the login is stored as a Keychain item (one per OS user, not per folder); only off macOS, or when the Keychain is denied, is it written to $XDG_CONFIG_HOME/muse/auth.json. XDG_CONFIG_HOME moves auth.json and settings but not session data (XDG_DATA_HOME). TBH_CREDENTIAL_BACKEND exists but its accepted values are not visible in the binary, so forcing the file backend is UNVERIFIED. Works on Linux with XDG_CONFIG_HOME plus XDG_DATA_HOME.
- **API-key accounts:** yes. Binary help for `muse login`: "META_API_KEY always takes priority over the account login" (strings.txt:119749) and the TUI note "META_API_KEY overrides your Meta account login" (strings.txt:83001). Other provider keys (ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY) select non-Meta providers and are already in credential_vars.
- **Status check:** {"args": ["usage", "--json"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. auth.json shape per the launcher's reader: /providers/meta/{mechanism:"oauth", access_token, expires_at (number)} (launcher.sh:472-494); default path $XDG_CONFIG_HOME/muse/auth.json or ~/.config/muse/auth.json (launcher.sh:19-25). But on macOS the default credential backend is the Keychain (strings.txt:197533 "off-macOS stamps `file`"; Keychain error texts strings.txt:424471), so copying auth.json does not switch the live login there. Usable on Linux, or on macOS only if TBH_CREDENTIAL_BACKEND forces the file backend (UNVERIFIED). A login can also hold a minted API key instead of oauth (strings.txt:83001 "mint returned an empty api key"), in which case the expiry pointer is absent.
- **Live quota:** yes. Subscription logins only: errors "not logged in: run `muse login` first", reason codes api_key and no_allowance (strings.txt:82980), and "subscription allowance unavailable: the server sent no usage data (this can happen before your first use in a new window)" (strings.txt:82985). Exact top-level layout of the JSON is UNVERIFIED (field names only).
- **Usage in output:** tokens and cost
- **Limit messages:** Binary wording: "This turn stopped because your usage limit was reached." (strings.txt:123140), turn failure labels "usage limit reached" and "rate limited" (strings.txt:123203), "quota (resets at" (strings.txt:41816). "usage limit" and "rate limit" match these; "too many requests" does not appear in the binary (harmless). Optionally add "quota (resets at". turn/completed also carries a structured error.quota{limitKind, resetsAtMs} (strings.txt:415290).

Sources: closed binary muse 1.4.20-R8619.1 strings (scratchpad/pkgs/muse/strings.txt) and the bash launcher (scratchpad/pkgs/muse/launcher.sh). Nothing was run.

- **Login**: `muse login` is a device-code flow against auth.meta.com (launcher.sh:7-12; binary help strings.txt:119748). Success prints "Logged in. Credential saved to ..." (strings.txt:14319). `muse logout` removes the saved Meta credential (API key or account login) and leaves META_API_KEY alone (strings.txt:119750-119751).
- **Storage**: telemetry field credential_backend with values file, keychain, keychain_fallback_file and fallback reasons interaction_not_allowed / denied; "off-macOS stamps `file`" (strings.txt:197533). macOS Keychain errors: "macOS denied Muse Code access to your saved Meta credentials ... or run `muse login`" (strings.txt:424471). So on macOS the default is Keychain with file fallback; on Linux/Windows it is auth.json. TBH_CREDENTIAL_BACKEND is in the env list (strings.txt:424549); accepted values UNVERIFIED ("file" is a guess from the telemetry values).
- **auth.json**: /providers/meta/mechanism ("oauth"), /providers/meta/access_token, /providers/meta/expires_at (launcher.sh:472-494). No email or user name field was found, and no id_token, so identity is null.
- **Precedence**: META_API_KEY > stored credential (strings.txt:119749, 83001).
- **Status**: there is no `auth status` or `whoami`; `muse usage --json` exits non-zero with "not logged in: run `muse login` first" when signed out (strings.txt:82980). Caveat: it can also fail while signed in before first use in a new window (strings.txt:82985), and an API-key login reports reason api_key, so treat a failure as "unknown" rather than "signed out" unless the message says not logged in.
- **Quota**: `muse usage [--json]` (strings.txt:8770-8773); JSON keys schemaVersion, allowance, window, usedPercent, resetsAtMs, weekly, asOfMs (strings.txt:82785). TUI equivalents /usage and a subscription-plan command (strings.txt:122967).
- **Usage in --json**: session/tokenUsage with per-request usage and cumulative tokens and cost (strings.txt:123097, 160043 struct sizes: SessionTokenUsageParams 11 fields, CumulativeCost 2 fields usd/partial).
