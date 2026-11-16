# Aider (`aider`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `AIDER` in `src/agent/specs.rs`.

- **Source:** github:Aider-AI/aider
- **License:** Apache-2.0
- **Version checked:** 0.86.3.dev (source only, not installed)
- **Install:** python -m pip install aider-install && aider-install | pipx install aider-chat | uv tool install aider-chat
- **Headless call:** `aider`, prompt flag eq `--message=`
- **Runs tools without asking when headless:** no

## Notes

Source: `~/.opensrc/repos/github.com/Aider-AI/aider/main` (`aider/__init__.py`: 0.86.3.dev). Not installed here; nothing was run, both fixtures are DERIVED from source (the error fixture's litellm line is illustrative). Complements docs/research/agent-clis.md section 9.

**Headless**: `--message=<text>` (aliases `--msg`, `-m`; args.py:639) runs one turn and returns (main.py:1124-1133). Positionals, including after `--`, are files to add, never the prompt, so `flag_eq` is the only safe style. `-m` is the message, not the model; the model flag is `--model` (args.py:60).

**Output**: no JSON. Plain text on stdout: announcement lines, the reply, then `Tokens: <n> sent, <n> received. Cost: $x message, $y session.` (base_coder.py:2023-2068). Counts are abbreviated (`2.1k`), so usage can only be scraped approximately. Hashi's `text` format will include the banner and the Tokens line in the answer unless it strips them. API errors print to stdout/stderr and still **exit 0** (main.py:1130-1133 returns normally after `coder.run`).

**Safety**: `--chat-mode ask` (args.py:164-170, alias of `--edit-format`) uses the ask coder, which answers without editing files. Confirmations: `--yes-always` answers yes except where `explicit_yes_required` is set, which answers no (io.py:866-867); shell commands require explicit yes (base_coder.py:2450-2460), so they never run. Without `--yes-always`, EOF on /dev/null takes each prompt's default, which is usually `y` (io.py:884-886). `offer_url` opens a browser on yes (io.py:796-803); `BROWSER=true` makes Python's `webbrowser` run `/usr/bin/true` instead (UNVERIFIED end to end). **Always pass a model** (`--model` or `AIDER_MODEL`): with none and no key in the env, `select_default_model` offers an OpenRouter OAuth login that opens a browser and waits on a local callback (onboarding.py:116-146, 92-93).

**Slash and bang**: `is_command` is `inp[0] in "/!"` (commands.py:255-256), checked on the raw message (base_coder.py:912-917). `/run` and `!cmd` run shell commands, `/web` fetches, `/add` reads files. Hashi must prefix a space to prompts starting with `/` or `!` (a leading space defeats the check).

**Config hazards**: `.aider.conf.yml` is searched in the git root, cwd and home (main.py:464) and `.env` files are loaded (`--env-file`, args.py:801); a workspace `.aider.conf.yml` could re-enable shell or set `--test-cmd`. Every option can also be set by an `AIDER_<OPTION>` env var (configargparse `auto_env_var_prefix="AIDER_"`, args.py:41), so Hashi should strip inherited `AIDER_*` variables. Aider writes `.aider.chat.history.md`, `.aider.input.history` and `.aider.tags.cache.v4/` into the cwd.

**Resume**: none by id. `--restore-chat-history` replays `.aider.chat.history.md` from the cwd, which works only because Hashi reuses the per-token workspace (pinned off here).

**Config dir**: no single variable; user state lives under `~/.aider` and `~/.aider.conf.yml`, so only `HOME` relocates it. No sign-in command (keys come from env, `--api-key provider=key`, or `.env`).

**Limit messages**: `RateLimitError` maps to "The API provider has rate limited you. Try again later or check your quotas." (exceptions.py:44-47); the litellm exception text precedes it.

## Success output (derived from source or docs, not recorded)

```
Aider v0.86.3
Model: anthropic/claude-sonnet-4-5 with ask edit format
Git repo: none
Repo-map: disabled

OK

Tokens: 2.1k sent, 2 received. Cost: $0.0063 message, $0.0063 session.
```

## Error output

```
Aider v0.86.3
Model: anthropic/claude-sonnet-4-5 with ask edit format
Git repo: none
Repo-map: disabled

litellm.RateLimitError: AnthropicException - {"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization"}}
The API provider has rate limited you. Try again later or check your quotas.
```
