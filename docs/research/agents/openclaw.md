# OpenClaw (`openclaw`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `OPENCLAW` in `src/agent/specs.rs`.

- **Source:** github:openclaw/openclaw
- **License:** MIT
- **Version checked:** 2026.11.4
- **Install:** curl -fsSL https://openclaw.ai/install.sh | bash | npm install -g openclaw@latest --allow-scripts=openclaw
- **Headless call:** `openclaw agent exec --json`, prompt dashdash
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

**Project:** OpenClaw (`openclaw/openclaw`, MIT, npm `openclaw` 2026.11.4), the multi-channel personal assistant gateway formerly known as Clawdbot/Moltbot. It is not primarily a coding CLI, but it ships a purpose-built headless one-shot: `openclaw agent exec`. Not installed here; nothing was run. Everything below is from source at `~/.opensrc/repos/github.com/openclaw/openclaw/main`.

**Two entry points; use `agent exec`.**
- `openclaw agent --message <text> [--to|--session-id|--session-key|--agent] [--local] [--json]` (src/cli/program/register.agent-turn.ts:15-101) talks to a running **Gateway daemon** by default; `--local` runs the embedded agent in-process but takes the shared state lock and refuses if a Gateway owns it (src/commands/agent-via-gateway.ts:896-919). Needs a session selector (docs/cli/agent.md:18). Its `--json` output is `{payloads, meta, deliveryStatus?}` (src/agents/command/delivery.ts:538-544) with session id and usage under `meta.agentMeta`.
- `openclaw agent exec [message]` (register.agent-turn.ts:103-195, src/commands/agent-exec.ts) runs ONE embedded turn with **no Gateway**, in a fresh temp state dir (`mkdtemp(openclaw-agent-exec-)`, agent-exec.ts:130-134) that is deleted afterwards (409-415), and prints a stable envelope. Docs call it "the recommended headless entry point for CI and coding automation" (docs/cli/agent.md:36).

**Prompt passing:** commander 15 with `enablePositionalOptions` (src/cli/program/build-program.ts:14). `agent` is a lazily registered command whose action reparses the root raw argv (src/cli/program/register-command-groups.ts:150-170); the test at src/cli/program/action-reparse.test.ts:34-48 proves `-- --help` reaches a positional literally, and the early help/version sniffer stops at `--` (src/cli/argv.ts:41-44). So `agent exec <flags> -- <prompt>` is the right form. Alternative: `--message-file -` reads stdin (agent-exec-input.ts:32-77), max 4 MiB. Blank prompt: `Missing prompt. Pass text or use --message-file <path>.` (agent-exec-input.ts:74).

**cwd:** `--cwd` defaults to `process.cwd()` (agent-exec.ts:125) and also becomes the agent workspace (`OPENCLAW_WORKSPACE_DIR`, agent-exec.ts:220-222; agent-exec-input.ts:195-201, `skipBootstrap: true` so no bootstrap files are written). Hashi's spawn cwd is enough. Not a git repo: fine (no git requirement found).

**Output (`--json`):** `writeRuntimeJson(runtime, envelope)` with 2-space indentation (src/runtime.ts:151-160), so stdout is ONE **pretty-printed multi-line** JSON object, not a single line. Parse the whole stdout. Envelope (src/commands/agent-exec-result.ts:99-121; docs/cli/agent.md:116-146): `ok`, `status` (`ok|error|timeout`), `final` (joined non-error, non-reasoning payload text), `payloads[]`, optional `usage {input, output, cacheRead?, cacheWrite?, total?, cost?}` (src/agents/usage.ts:20-31, 76-81), optional `costUsd` (USD), `assistantTurns`, `toolSummary`, `model`, `provider`, `sessionId`, and on failure `error {message, kind}` with kind `timeout|fallback_exhausted|aborted|error_payload|agent_error|exception|cleanup|<meta.error.kind>` (agent-exec-result.ts:74-86, agent-exec.ts:344-358, 418-427). Errors are also echoed to stderr (agent-exec.ts:447-449). Without `--json` stdout is only `final`.
- `error_ptr` `/error/message` is only present on failure, matching the schema rule. `ok`/`status` are the authoritative flags.
- Usage keys are `input`/`output`, not `input_tokens`; `cacheRead` is separate (Hashi's Claude convention adds cache tokens to input; UNVERIFIED whether `input` here already includes cache reads, provider-dependent normalisation in src/agents/usage.ts).

**Exit codes:** 0 ok, 1 error, 2 timeout (agent-exec.ts:100-104), 130/143 on SIGINT/SIGTERM after cleanup (agent-exec.ts:433-437). `--timeout` defaults to 600 s per model attempt (register.agent-turn.ts:135); set it below Hashi's own timeout.

**Session resume: NOT supported by `agent exec`.** It always mints `randomUUID()` (agent-exec.ts:93) and has no `--session-id`; with the default temp state dir the transcript is deleted. `--state-dir <existing dir>` retains state but still uses a new random session id per run, and it requires exclusive ownership (refuses while a Gateway owns it, agent-exec.ts:321-334). Resume exists only on `openclaw agent --local --session-id <id> --message ...` (docs/cli/agent.md:20-24), which uses the operator's shared state and a different JSON shape (`meta.agentMeta.sessionId`, `meta.agentMeta.usage`). So `resume_flag` is null; a resumable variant would need a second shape (UNVERIFIED end to end).

**Model:** `--model <provider/model>` (register.agent-turn.ts:120); repeatable `--fallback` requires `--model` (agent-exec.ts:181-184). Values like `anthropic/claude-sonnet-4-6` pass `is_safe_arg`. `--thinking <level>` exists.

**Permissions (important):** exec's defaults are dangerous for Hashi: sandbox `off`, tool profile `coding`, `tools.exec.mode: "full"` because "a headless one-shot has no approval channel" (agent-exec-input.ts:84-98). Shell (`exec`, `process`), `write`, `edit`, `apply_patch`, `web_fetch`, `web_search`, `code_execution`, `github_publish`, `plugins`, `gateway` and MCP (`bundle-mcp`) are all in the coding profile (src/agents/core-tool-facts.ts:50-135, 229-242). So `auto_approves_tools_headless` is true unless overridden. `fs.workspaceOnly: true` restricts file tools to `--cwd`, but shell is not contained. The ambient `~/.openclaw/openclaw.json` is merged over these defaults (agent-exec-input.ts:195), so an operator config can loosen or tighten it.
- Safe pin: `--config-override={"tools":{"allow":["ls","read"],"exec":{"mode":"deny"}}}`. `--config-override` is repeatable JSON deep-merged and schema-validated (agent-exec-input.ts:132-161; register.agent-turn.test at src/cli/program/register.agent.test.ts:108-120 shows `{"tools":{"deny":["exec"]}}`, also docs/cli/agent.md:64-68). `tools.allow` is "exact tool names allowed" and deny wins (src/config/zod-schema.agent-runtime.ts:58-65); `exec.mode` enum is `deny|allowlist|ask|auto|full` (line 223). That the profile allowlist and `tools.allow` intersect to exactly `ls,read` is UNVERIFIED end to end; a stricter fallback is adding `"profile":"minimal"` (presence, session_status, gateway only, src/agents/tool-catalog.test.ts:122-130), but note `gateway` is in minimal.
- `--isolated` ignores the ambient config entirely (agent-exec-input.ts:114-118), and `--auth-env-only` uses only provider env vars for credentials (agent-exec.ts:291-298) and skips installed plugins (line 172). Both are good for reproducibility; not pinned because they drop the operator's default model and stored logins. Hashi then must pass `--model`.
- `--config-override` with `--auth-env-only` is rejected (agent-exec-input.ts:107-109), so do not combine them.
- Provider auth env vars are stripped from tool subprocess env (`withHostExecInheritedEnvOmitted`, agent-exec.ts:311-315).

**Slash commands:** the Gateway path refuses `/compact` (agent-via-gateway.ts:880-887); whether `agent exec` interprets a leading `/` as a command is UNVERIFIED, so `slash_commands_in_prompt` is set true conservatively (the auto-reply pipeline handles slash commands elsewhere).

**Config/credentials:** `OPENCLAW_STATE_DIR` relocates the state dir, which also holds `openclaw.json` (src/config/paths.ts:163-165, 214) and auth profiles; `OPENCLAW_CONFIG_PATH` overrides the config file only; `OPENCLAW_HOME` overrides the home used for `~/.openclaw` (paths.ts:51, src/infra/home-dir.ts:38). Exec resolves stored credentials from the ambient state dir before redirecting state (agent-exec.ts:209-224), so `OPENCLAW_STATE_DIR` selects the login account. Provider keys come from env (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, ... list in src/secrets/provider-env-vars*.ts), the config file, or `openclaw onboard`/`openclaw models auth login`. Config loading can import a login shell env unless `env.shellEnv.enabled` is false (exec default sets it false, agent-exec-input.ts:86).

**Rate limits:** classified by regexes in src/agents/failover/retry-evidence.ts:29-34 (`rate.?limit|too many requests|resource[_ -]?exhausted|daily usage limit|quota[_ -]?exceeded|insufficient[_ -]?quota|usage limits?|out of budget`). Provider error text is passed through into `error.message`.

**Fixtures:** both are DERIVED from the documented envelope (docs/cli/agent.md:118-133) and agent-exec.ts:346-358, formatted with the same 2-space indentation the CLI uses. The failure message text is illustrative (UNVERIFIED).

**Other gotchas:** Node 24.16+ or 26.1+ required (README.md:38). Heavy CLI (plugins, SQLite state); cold start likely seconds. `--timeout` value must be a non-negative integer string (agent-exec.ts:177-180).

## Success output (derived from source or docs, not recorded)

```
{
  "ok": true,
  "status": "ok",
  "final": "OK",
  "payloads": [
    {
      "text": "OK"
    }
  ],
  "usage": {
    "input": 9120,
    "output": 2,
    "total": 9122
  },
  "costUsd": 0.0274,
  "assistantTurns": 1,
  "model": "claude-sonnet-4-6",
  "provider": "anthropic",
  "sessionId": "0193f2a8-5c1e-7d42-9b7a-3e8f1c2d4a6b"
}
```

## Error output

```
{
  "ok": false,
  "status": "error",
  "final": "",
  "payloads": [],
  "model": null,
  "provider": null,
  "sessionId": "6f0c2b7e-1d3a-4e5f-8a9b-0c1d2e3f4a5b",
  "error": {
    "message": "No API key found for provider \"anthropic\".",
    "kind": "exception"
  }
}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. OPENCLAW_STATE_DIR replaces ~/.openclaw outright (src/config/state-dir.ts:23-43). Auth profiles live in SQLite under it: shared store state/openclaw.sqlite (src/state/openclaw-state-db.paths.ts:35-44) or agents/main/agent/openclaw-agent.sqlite (src/agents/auth-profiles/path-resolve.ts:173-178, shared-main-dir.ts:7-12); legacy credentials/ also under it (src/config/paths.ts:318-327). agent exec resolves stored auth from the ambient state dir before redirecting to its temp dir (src/commands/agent-exec.ts:209-224, 290-298), so the account dir is used. OS keychain is only read for reusing an external Codex CLI login and only when a keychain prompt is allowed, which the provider-auth path disables by default (src/agents/cli-credentials.ts:188-198, src/agents/auth-profiles/external-cli-discovery.ts:46-58). Caveats: OPENCLAW_AGENT_DIR, OPENCLAW_HOME, OPENCLAW_PROFILE and OPENCLAW_OAUTH_DIR can also move auth, so they must be stripped; external CLI credential discovery (Codex auth.json under CODEX_HOME, Claude CLI) can still reach the owner's other CLIs' logins unless the account has its own profile for that provider.
- **API-key accounts:** no. Default precedence is profile-first: resolveApiKeyForProviderCore only reads env keys first when credentialPrecedence is env-first (src/agents/model-auth-provider.ts:502-510); otherwise it walks stored auth profiles (src/agents/model-auth-provider.ts:563-600) and falls back to the env key only after them (src/agents/model-auth-provider.ts:646-649). agent exec uses stored and external CLI credentials by default (docs/cli/agent.md:75, 379; src/cli/program/register.agent-turn.ts:133-134). So with the owner's ~/.openclaw holding an anthropic profile, the key does not win. `--auth-env-only` fixes precedence but is rejected together with the pinned --config-override (src/commands/agent-exec-input.ts:107-109) and loads no config, i.e. exec's full-tool defaults. Workable fix: run key accounts with OPENCLAW_STATE_DIR pointed at an empty Hashi-owned dir (no stored profiles), keeping the pinned override.
- **Status check:** {"args": ["models", "status", "--check", "--json"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. The login is not plain files: auth profiles are rows in SQLite (state/openclaw.sqlite or agents/main/agent/openclaw-agent.sqlite; src/agents/auth-profiles/path-resolve.ts:173-178, src/state/openclaw-state-db.paths.ts:35-44) that share the database with sessions, config machine state and usage stats, and auth-profiles.json/auth.json/oauth.json are only legacy names migrated away (src/agents/auth-profiles/file-state.ts:49, legacy-source-files.ts:37). A running Gateway daemon holds the database. Copying a DB file in and out would swap far more than the login and risk corrupting live WAL state, so file-copy switching does not fit.
- **Live quota:** yes. Plan windows are reported as usedPercent per window for subscription logins; API-key accounts get billing/balance only where the provider supports it. Not run here (not installed); also `models auth list --json` exposes blockedUntil/cooldownUntil per profile (src/commands/models/auth-list.ts:66-90).
- **Usage in output:** tokens and cost
- **Limit messages:** Spec patterns match the classifier regexes (src/agents/failover/retry-evidence.ts:29-34, src/agents/failover/message-patterns.ts:146-171). But the embedded runner rewrites classified failures into user copy that can become error.message in the exec envelope (errorPayloadMessage, src/commands/agent-exec-result.ts:69-73): rate limit becomes "The AI service needs a short break. Please try again in a few minutes." (src/agents/failover/user-copy.ts:42-43, 564-565; assistant-request-failure-copy.ts:92), which matches none of the spec patterns. Add "needs a short break". Billing copy "reported a billing problem ... usage limits" (assistant-request-failure-copy.ts:95) already hits "usage limit", and "returned a billing error" (user-copy.ts:93) does not: add "billing error" and "billing problem". Also worth adding raw forms "exceeded your current quota", "insufficient credits", "credit balance", "payment required" (message-patterns.ts:157, 250-258).

Source: ~/.opensrc/repos/github.com/openclaw/openclaw/main, not installed or run.

- **State dir**: OPENCLAW_STATE_DIR absolute path is taken as is (src/config/state-dir.ts:27-38); otherwise OPENCLAW_PROFILE picks ~/.openclaw-<profile> and OPENCLAW_HOME changes the home (state-dir.ts:46-54, src/config/paths.ts:51-53).
- **Auth store**: SQLite; auth-profiles.json is legacy (src/agents/auth-profiles/file-state.ts:49). OAuth credentials carry `email`, `expires` (ms) (src/agents/auth-profiles/types.ts:36-42).
- **Identity**: no JSON file to read; `openclaw models auth list --json` prints `profiles[].email` / `displayName` / `expiresAt` (src/commands/models/auth-list.ts:66-104), so a command-based identity read is possible, e.g. pointer /profiles/0/email.
- **Status**: `models status --check` exits 0 ok, 1 missing/expired/unavailable/indeterminate, 2 expiring (src/cli/models-cli.ts:297-306; src/commands/models/list.status-command.ts:1023-1066, 1110). It counts env keys too, so it reports signed in whenever a provider env key is present.
- **Exec output**: usage {input, output, cacheRead?, cacheWrite?, total?} and costUsd are copied from agentMeta when present (src/commands/agent-exec-result.ts:107-108); spec pointers /usage/input, /usage/output, /costUsd are correct; error at /error/message (agent-exec-result.ts:120).
- **Auth in exec**: `withAuthProfileStoreAgentDir(storedAuthAgentDir, storedAuthStateDir, ...)` unless --auth-env-only (src/commands/agent-exec.ts:290-298); provider env vars are omitted from tool subprocesses (agent-exec.ts:311-315).

## Real-binary check (2026-11-16)

OpenClaw 2026.11.4 (`npm i openclaw`, needs Node 24.16+ or 26.1+) has no `--config-override` on `agent exec`; only `--config <file>`. The pin was dropped, so OpenClaw runs with its own tool settings and stays opt-in. A run without a login fails cleanly with `No route-compatible authentication source is configured`, and `models status --check --json` exits 1 when signed out.
