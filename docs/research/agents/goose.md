# Goose (`goose`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **ran**. The spec Hashi runs is `GOOSE` in `src/agent/specs.rs`.

- **Source:** github:block/goose
- **License:** Apache-2.0
- **Version checked:** 1.44.0 (installed, run here); source read at main 1.58.0
- **Install:** curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | CONFIGURE=false bash | brew install block-goose-cli
- **Headless call:** `goose run -q --output-format stream-json`, prompt flag eq `--text=`
- **Runs tools without asking when headless:** no

## Notes

Source: `~/.opensrc/repos/github.com/block/goose/main` (Cargo.toml version 1.58.0). Installed binary is 1.44.0 (`goose --version`). The earlier write-up in docs/research/agent-clis.md section 6 covers 1.44.0; this file adds what changed on main.

**Real run (1.44.0)**: `cd $(mktemp -d) && env NO_COLOR=1 TERM=dumb GOOSE_MODE=chat GOOSE_TELEMETRY_OFF=1 timeout 120 goose run -q --output-format stream-json --no-profile -n hashi-test1 '--text=Reply with exactly: OK' </dev/null` exited 1 with the stdout recorded as `fixture_error` and empty stderr: no provider is configured on this machine (`~/.config/goose` holds only `skills`). So `fixture_success` is DERIVED from source, not recorded. Note the setup error is plain text on **stdout** even in stream-json mode (`render_error` is `println!`, crates/goose-cli/src/session/output.rs:765-767; called at session/builder.rs:308 and 759). Hashi must treat non-JSON stdout plus exit 1 as failure and use stdout, not stderr, as the message.

**Prompt**: `-t/--text <TEXT>` (crates/goose-cli/src/cli.rs:238-245), no `allow_hyphen_values`, so use the attached `--text=<prompt>` form. No positional prompt exists.

**Stream events** (`StreamEvent`, session/mod.rs:150-176, serde tag `type`, snake_case, one JSON object per line via `emit_stream_event` mod.rs:2290-2294): `message {message}` (mod.rs:1534-1535, every agent message including tool responses with role `user`), `notification {extension_id, ...}`, `error {error: string}`, `complete {total_tokens, input_tokens?, output_tokens?, cache_read_input_tokens?, cache_write_input_tokens?, cost_usd?}`. Usage is the session's accumulated total (mod.rs:1652-1689), so on a resumed session it is cumulative.

**Behaviour change on main vs 1.44.0**: on main a terminal failure (stream error, cancel, or an error content block in the last message, `headless_run_error` mod.rs:2743-2761) emits `error` and **no** `complete` (mod.rs:1610-1615, 1650) and makes the run fail. In 1.44.0 an `error` is followed by `complete` and a mid-run provider failure exited 0 (agent-clis.md section 6). Treat any `error` line as `Reported` regardless.

**Text pointer caveats**: `/message/content/0/text` assumes the first content block is text. Thinking blocks (`type: thinking`) or a `toolRequest` first would hide the text; in `GOOSE_MODE=chat` there are no tool requests, but a reasoning model can still emit a thinking block first. Whether streamed assistant chunks arrive as deltas (so `concat` is right) or as cumulative snapshots is UNVERIFIED; concat matches how the text renderer prints them.

**Session id**: not in any stream event, only in the banner that `-q` suppresses. Resume needs two flags (`-r -n <name>` or `-r --session-id <id>`, cli.rs `--resume`/`--name`/`--session-id`), which `resume_flag` cannot express, so it is null. Practical approach: always start with `-n hashi-<uuid>` and resume with `-r -n hashi-<uuid>`.

**Permissions**: `GOOSE_MODE=chat` disables tools (prompt_manager.rs:153, configure.rs:2464). Default mode is `auto`, which runs every tool headless; `approve`/`smart_approve` abort headless with "Tool approval required in non-interactive mode" (mod.rs:1410-1423); other confirmations are auto-allowed with a warning (mod.rs:1424-1427). Elicitations are cancelled (mod.rs:1466-1468). `--no-profile` keeps the owner's configured extensions from loading. No OS sandbox.

**Slash commands**: the agent treats a user message starting with `/` as a built-in command (`/compact`, `/clear`, `/prompt`, `/goal`, `/status`, ...; crates/goose/src/agents/execute_commands.rs:20-69).

**Config/credentials**: `GOOSE_PATH_ROOT` relocates config, data and state (used that way in crates/goose-cli/src/bin/mcp_conformance_driver.rs:115). Secrets: env first, then config.yaml, then keyring. `GOOSE_DISABLE_KEYRING=1` stores and reads secrets in a file under the config dir (configure.rs:701, 1660); for a separate Hashi account set it together with `GOOSE_PATH_ROOT`, but not for the owner's default account if their keys live in the keyring. Telemetry opt-out `GOOSE_TELEMETRY_OFF` (configure.rs:1594).

**Limit messages**: `ProviderError` variants `Rate limit exceeded: {details}` and `Credits exhausted: {details}` (crates/goose-provider-types/src/errors.rs:18, 50).

## Success output (recorded)

```
{"type":"message","message":{"id":"chatcmpl-01","role":"assistant","created":1763300000,"content":[{"type":"text","text":"OK"}],"metadata":{"userVisible":true,"agentVisible":true}}}
{"type":"complete","total_tokens":1214,"input_tokens":1212,"output_tokens":2,"cost_usd":0.003648}
```

## Error output

```
error: No provider configured. Run 'goose configure' first.
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. GOOSE_PATH_ROOT moves config, data and state to <root>/config, <root>/data, <root>/state (crates/goose/src/config/paths.rs:8-16) but only when the value is an absolute path (paths.rs:40-46). Secrets default to ONE keyring entry, service "goose", user "secrets" (base.rs:31-34, 401-412), which is the same for every root, so GOOSE_DISABLE_KEYRING (any value, env checked with is_ok at base.rs:219-223) is required; then secrets go to <config_dir>/secrets.yaml (base.rs:397-399).
- **API-key accounts:** no. Secrets are read env-first (Config::get_secret, base.rs:920-933), so a provider key in env does outrank the stored one, but there is no single key var: the provider comes from GOOSE_PROVIDER (plus GOOSE_MODEL) and the key name depends on it (ANTHROPIC_API_KEY, OPENAI_API_KEY, ...). An env-only account needs GOOSE_PROVIDER + GOOSE_MODEL + that provider's key.
- **Status check:** {"args": ["info", "--check"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. The plain goose (etcetera Xdg strategy, ~/.config/goose on macOS too, paths.rs:20-31) keeps secrets in the OS keyring unless GOOSE_DISABLE_KEYRING is in its env or in its config.yaml (base.rs:219-223), so copying secrets.yaml alone is ignored; copying config.yaml as well overwrites the owner's provider, model and extensions, and still only works if that config.yaml carries GOOSE_DISABLE_KEYRING: true. There is no account identity or expiry to compare.
- **Live quota:** no. Goose is bring-your-own-key across many providers; it has no plan or quota endpoint of its own.
- **Usage in output:** tokens and cost
- **Limit messages:** ProviderError Display strings are "Rate limit exceeded: {details}" and "Credits exhausted: {details}" (crates/goose-provider-types/src/errors.rs:18, 50); the spec's "rate limit exceeded" and "credits exhausted" match (case-insensitively). "too many requests" is a harmless extra. In 1.44.0 a setup failure is plain text on stdout, not JSON.

Source: ~/.opensrc/repos/github.com/block/goose/main (1.58.0); installed 1.44.0.

- `goose info --check` (crates/goose-cli/src/commands/info.rs:163-270) prints Provider/Model/Auth and returns Err("provider check failed") so the process exits 1 when not configured or auth fails (info.rs:266-270). It sends one tiny completion (check_provider calls provider_client.complete). Verified on the installed 1.44.0: with an empty absolute GOOSE_PATH_ROOT and GOOSE_DISABLE_KEYRING=1 it printed `Provider: not configured: Configuration value not found: GOOSE_PROVIDER` and exited 1. `goose doctor` also exists but is broader.
- Keyring layout: one JSON blob under service `goose`, user `secrets` (base.rs:31-34) shared by every GOOSE_PATH_ROOT, so two keyring-mode roots would overwrite each other's keys.
- Usage: the `complete` stream event carries total/input/output tokens and `cost_usd` (crates/goose-cli/src/session/mod.rs:164-176, 1639, 1682-1688); the spec pointers match.
- No identity: logins are provider API keys (a few providers do OAuth, but goose stores no user name).
