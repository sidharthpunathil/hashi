# Contributing to Hashi

Thanks for helping. Small fixes can go straight to a pull request. For larger changes, open an issue first so we can agree on the approach before you write the code.

## Setup

You need Rust 1.89 or newer. Claude Code and Codex are optional for development: the test suite drives fake agent scripts, so it runs anywhere.

```bash
git clone https://github.com/sidharthpunathil/hashi
cd hashi
cargo test
cargo run -- doctor
```

## Before you open a pull request

CI runs these, so running them locally saves a round trip:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo deny check          # cargo install --locked cargo-deny
```

## Guidelines

- No `unwrap()` or `expect()` outside tests. Return errors with context.
- `unsafe` is forbidden by the crate lints.
- Never hold the store mutex across an `.await`.
- Anything that spawns an agent goes through `agent::runner`, which enforces the timeout, output cap and kill-on-drop.
- Values passed to an agent as CLI options must pass `agent::is_safe_arg`. Prompts always go after `--`.
- New behaviour needs a test. API behaviour belongs in `tests/api.rs`, which runs a real server against fake agents. Parsers get fixture tests in `tests/fixtures/`.
- TUI changes: `HASHI_DUMP_TUI=1 cargo test --test tui -- --nocapture` prints the rendered frames. `HASHI_DUMP_HTML=<dir>` writes coloured HTML you can open in a browser.

## Adding an agent

1. Add a variant to `AgentKind` in `src/agent/mod.rs`, with its name, display name and binary names.
2. Create `src/agent/<name>.rs` with pure `build_args`, `env` and `parse_output` functions, plus fixture tests. Use the agent's safest unattended mode: never an auto-approve flag. Make sure no part of the prompt can be read as a flag, and check whether a leading `/` runs built-in commands.
3. Wire it into `agent::execute`, and give it a colour and glyph in `src/tui/theme.rs`.
4. Add `tests/<name>.rs` with a fake agent script, following `tests/gemini.rs`.

`docs/research/agent-clis.md` has notes on CLIs that are not supported yet.

## Commit messages

Use the imperative mood and keep the subject under about 72 characters, for example `Add per-token workspace cleanup`.

By contributing, you agree that your contributions are dual-licensed under MIT and Apache 2.0, the same as the project.
