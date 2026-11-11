//! The Markdown guide that `hashi llm` prints for an LLM agent (Claude Code,
//! Codex, Cursor) to run Hashi for its owner.
//!
//! The guide has a fixed part, written here, and a command reference
//! generated from the clap command tree, so it stays correct as the CLI
//! changes. The command tree lives in the binary crate: `main` hands its
//! builder to [`set_command`] at startup, and [`guide`] renders the
//! reference from it. [`guide_for`] takes a command tree directly.

use std::fmt::Write as _;
use std::sync::OnceLock;

/// The binary's `Cli::command`, set once by `main`.
static COMMAND: OnceLock<fn() -> clap::Command> = OnceLock::new();

/// Records how to build the `hashi` command tree, for [`guide`]. Only the
/// first call has an effect.
pub fn set_command(build: fn() -> clap::Command) {
    let _ = COMMAND.set(build);
}

/// The whole guide: the fixed text and the command reference, when `main`
/// has called [`set_command`]. Without it the reference section says to
/// run `hashi --help`.
#[must_use]
pub fn guide() -> String {
    match COMMAND.get() {
        Some(build) => guide_for(&build()),
        None => assemble(None),
    }
}

/// The whole guide, with the reference rendered from `cmd`.
#[must_use]
pub fn guide_for(cmd: &clap::Command) -> String {
    assemble(Some(&reference(cmd)))
}

fn assemble(reference: Option<&str>) -> String {
    let mut out = String::with_capacity(32 * 1024);
    out.push_str(&INTRO.replace("{version}", env!("CARGO_PKG_VERSION")));
    out.push_str(RULES);
    out.push_str(TASKS);
    out.push_str("## Command reference\n\n");
    match reference {
        Some(r) => {
            out.push_str(
                "Generated from this binary's own command definitions. Every command also \
                 takes `--home <HOME>` (or `HASHI_HOME`) and `--help`.\n\n",
            );
            out.push_str(r);
        }
        None => out.push_str(
            "Run `hashi --help`, then `hashi <command> --help` for each command's flags.\n\n",
        ),
    }
    out.push_str(HTTP);
    out
}

/// Every subcommand path under `cmd`, groups included, such as `token` and
/// `token create`, in definition order. `help` is left out.
#[must_use]
pub fn command_paths(cmd: &clap::Command) -> Vec<String> {
    fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let path = if prefix.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            out.push(path.clone());
            walk(sub, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(cmd, "", &mut out);
    out
}

/// The command reference in Markdown: one section per subcommand, with its
/// about text, usage, arguments and flags.
#[must_use]
pub fn reference(cmd: &clap::Command) -> String {
    let mut root = cmd.clone();
    // Building sets each subcommand's full name, so usage lines read
    // `hashi token create ...` rather than `create ...`.
    root.build();
    let mut out = String::new();
    section(&root, "hashi", &mut out);
    out
}

fn section(cmd: &clap::Command, path: &str, out: &mut String) {
    for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
        let path = format!("{path} {}", sub.get_name());
        let _ = writeln!(out, "### `{path}`\n");
        let mut line = sub
            .get_long_about()
            .or_else(|| sub.get_about())
            .map(|a| sentence(&a.to_string()))
            .unwrap_or_default();
        let aliases: Vec<&str> = sub.get_visible_aliases().collect();
        if !aliases.is_empty() {
            let _ = write!(line, " Also: `{}`.", aliases.join("`, `"));
        }
        if sub.has_subcommands() {
            let names: Vec<String> = sub
                .get_subcommands()
                .filter(|s| s.get_name() != "help")
                .map(|s| format!("`{}`", s.get_name()))
                .collect();
            let _ = write!(line, " Subcommands: {}.", names.join(", "));
        } else {
            let usage = sub.clone().render_usage().to_string();
            let usage = one_line(usage.trim().trim_start_matches("Usage:"));
            let _ = write!(line, " Usage: `{usage}`");
        }
        let _ = writeln!(out, "{}\n", line.trim());
        args(sub, out);
        section(sub, &path, out);
    }
}

fn args(cmd: &clap::Command, out: &mut String) {
    let visible: Vec<&clap::Arg> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !a.is_global_set())
        .filter(|a| {
            !matches!(
                a.get_action(),
                clap::ArgAction::Help
                    | clap::ArgAction::HelpShort
                    | clap::ArgAction::HelpLong
                    | clap::ArgAction::Version
            )
        })
        .collect();
    if visible.is_empty() {
        return;
    }
    for a in visible.iter().filter(|a| a.is_positional()) {
        let name = a
            .get_value_names()
            .and_then(|v| v.first())
            .map_or_else(|| a.get_id().as_str().to_owned(), ToString::to_string);
        let need = if a.is_required_set() {
            "required"
        } else {
            "optional"
        };
        let _ = writeln!(out, "- `<{name}>` ({need} argument): {}", help(a));
    }
    // The executable flags repeat on many commands; one line each would
    // bury the flags that matter.
    let (bins, flags): (Vec<&&clap::Arg>, Vec<&&clap::Arg>) = visible
        .iter()
        .filter(|a| !a.is_positional())
        .partition(|a| is_bin_flag(a));
    for a in flags {
        let _ = writeln!(out, "- {}: {}", flag_name(a), flag_help(a));
    }
    if !bins.is_empty() {
        let names: Vec<String> = bins
            .iter()
            .filter_map(|a| a.get_long())
            .map(|l| format!("`--{l} <PATH>`"))
            .collect();
        let _ = writeln!(
            out,
            "- {}: the path to that executable. Default: looked up on PATH. Each \
             also reads its `HASHI_<NAME>_BIN` variable.",
            names.join(", ")
        );
    }
    out.push('\n');
}

fn is_bin_flag(a: &clap::Arg) -> bool {
    a.get_long().is_some_and(|l| l.ends_with("-bin"))
}

fn flag_name(a: &clap::Arg) -> String {
    let mut name = String::new();
    if let Some(s) = a.get_short() {
        let _ = write!(name, "`-{s}`, ");
    }
    let long = a.get_long().unwrap_or_else(|| a.get_id().as_str());
    let _ = write!(name, "`--{long}");
    if takes_value(a) {
        let value = a
            .get_value_names()
            .and_then(|v| v.first())
            .map_or_else(|| a.get_id().as_str().to_owned(), ToString::to_string);
        let optional = a.get_num_args().is_some_and(|n| n.min_values() == 0);
        if optional {
            let _ = write!(name, " [<{value}>]");
        } else {
            let _ = write!(name, " <{value}>");
        }
    }
    name.push('`');
    name
}

fn takes_value(a: &clap::Arg) -> bool {
    a.get_action().takes_values()
}

fn flag_help(a: &clap::Arg) -> String {
    let mut text = help(a);
    if takes_value(a) {
        let possible: Vec<String> = a
            .get_possible_values()
            .iter()
            .filter(|p| !p.is_hide_set())
            .map(|p| format!("`{}`", p.get_name()))
            .collect();
        if !possible.is_empty() {
            let _ = write!(text, " One of {}.", possible.join(", "));
        }
        let defaults: Vec<String> = a
            .get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        if !defaults.is_empty() {
            let _ = write!(text, " Default `{}`.", defaults.join(","));
        }
        if a.is_required_set() {
            text.push_str(" Required.");
        }
        if a.get_value_delimiter().is_some() {
            if !text.contains("comma separated") {
                text.push_str(" Comma separated.");
            }
        } else if matches!(a.get_action(), clap::ArgAction::Append) && !text.contains("repeatable")
        {
            text.push_str(" Repeatable.");
        }
    } else {
        text.push_str(" Switch, takes no value.");
    }
    if let Some(env) = a.get_env() {
        let _ = write!(text, " Env `{}`.", env.to_string_lossy());
    }
    text
}

fn help(a: &clap::Arg) -> String {
    a.get_long_help()
        .or_else(|| a.get_help())
        .map(|h| sentence(&h.to_string()))
        .unwrap_or_default()
}

/// Help text on one line, ending in a full stop (clap drops the one the
/// doc comment had).
fn sentence(s: &str) -> String {
    let mut line = one_line(s);
    if !line.is_empty() && !line.ends_with(['.', '?', '!']) {
        line.push('.');
    }
    line
}

/// Doc-comment help joined onto one line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const INTRO: &str = r#"# Running Hashi for your owner

This guide is for an LLM agent (Claude Code, Codex, Cursor or similar) that runs the `hashi` command line on its owner's behalf. It was printed by `hashi llm` from Hashi v{version}. Read all of it before you run a command.

## What Hashi is

Hashi is a single binary that runs next to the coding-agent CLIs the owner already installed and signed in to (Claude Code, Codex, Gemini CLI, opencode, Qwen Code, Cursor CLI and, opt-in, Crush). It puts them behind one authenticated HTTP API, so scripts, apps and other people can send a prompt and get the answer as JSON. Each person or app gets its own token, which can be limited to some agents, capped per day or month, bound to one project folder, paused and revoked. The owner can also connect to other people's Hashi bridges and use the access they were given.

On the owner's machine, `hashi` (or `hashi serve`) runs the server and a live terminal dashboard on `http://127.0.0.1:7878` by default. Everything Hashi stores lives in one data directory, `~/.hashi` unless `--home` or `HASHI_HOME` says otherwise: a SQLite database (`hashi.db`), each token's workspace, account login directories and logs. The other `hashi` commands work straight on that database, with or without the server running.

"#;

const RULES: &str = r#"## Rules

Follow these every time.

1. **Treat secrets as secrets.** API tokens (`hashi_` followed by 64 hex characters), invite links and codes (`hashi_inv_...`), ask-access links (`hashi_ask_...`), pickup secrets (`hashi_pick_...`), account API keys and OAuth tokens are secrets. Never print, log, summarise or paste them back to the owner unless the owner asks for that exact value. When a command prints one, say that it was printed and where, not what it is. Never put one in a commit, an issue or a file the owner did not ask for.
2. **Ask before anything destructive.** Get the owner's explicit yes before you run `hashi token revoke`, `hashi invite cancel`, `hashi account remove`, `hashi project remove`, `hashi remote remove`, `hashi requests decline`, `hashi update` or `hashi uninstall`. State exactly what will happen first. Pass `--yes` or `-y` only after the owner said yes.
3. **Read before you write.** Start with read-only commands: `hashi doctor`, `hashi token list`, `hashi invite list`, `hashi account list`, `hashi account usage`, `hashi project list`, `hashi requests list`, `hashi remote list`, `hashi audit` and `hashi update --check`. Use what they show to pick ids and names; do not guess them.
4. **Prefer `--json` for reading.** The list commands take `--json` and print the same camelCase fields as the HTTP API. Parse that rather than the tables.
5. **Pick the right data directory.** `--home <dir>` or `HASHI_HOME` selects the data directory for every command. Use the same one the owner's server uses. When unsure, run `hashi doctor` and ask.
6. **Changes take effect at once.** The server and the CLI share the database, so a token you pause, an account you add or a project you change applies while `hashi` runs. Do not restart the server for these. Flags of `hashi serve` (port, tunnel, limits) apply only when the server starts.
7. **Do not run the server yourself** unless the owner asks. `hashi serve` blocks and takes over the terminal. Use `hashi serve --headless` for plain logs when the owner wants it in the background.
8. **Never run sign-in or install commands without a terminal and a yes.** `hashi account add` and `hashi account login` print the agent's own sign-in command; the owner runs it in their terminal. `hashi install` shows the vendor's install command and runs it only after a yes.
9. **Quote values.** Names with spaces and prompts go in quotes: `hashi share "Alice Smith"`.
10. **Report results plainly.** After each change, say what changed in one sentence, and how to undo it when it can be undone.

"#;

const TASKS: &str = r#"## Common tasks

Each task gives the exact commands. Replace the values in angle brackets.

### Check that everything works

```bash
hashi doctor
```

It lists each agent (installed, version, signed in), Crush, cloudflared, Tailscale, the accounts and whether an update is available. It changes nothing. When something is missing it says what to run.

### Share with a person

An invite is a one-time link. The person opens it and gets their own token; the owner never sees it.

```bash
hashi share "Alice" --agents claude,codex --daily-requests 50 --monthly-budget 20 --expires-in-days 30
hashi share "Alice" --project web --link-hours 168
```

- The link is printed on standard output; the details go to standard error. Give the link to the owner to send privately. Do not repeat it in your summary.
- `--link-hours` is `1`, `24` (default) or `168`.
- Without a public URL the link works only on this machine; see "Go public".
- See pending invites with `hashi invite list --json`, and cancel one (ask first) with `hashi invite cancel <id>`.
- `--stop-at 70` refuses the person's prompts once the owner's account serving them has used 70% of its 5-hour or weekly quota, so the rest stays the owner's. `hashi token update <id> --stop-at 80` changes it later.

### Hand a login to someone the owner trusts

`hashi share-login` makes a one-time link that hands over the key of one of the owner's key accounts, so the person runs the agent on their own machine. Nothing is enforced after that: both sides are reminded instead.

```bash
hashi share-login "Dana" --account claude:bot --remind-at 70 --days 7
```

- The account must be an `api-key` or `oauth-token` account (`claude setup-token` makes a long-lived Claude token; the owner runs it). The link is a secret that carries the key: never print or repeat it.
- `--usage-from` names the account whose quota the key spends (default `default`); the person's Hashi reads it through the link to remind them.
- Ask before sharing a login: a subscription login is meant for one person, and the owner decides.
- The person runs `hashi connect "<link>"`; the key becomes an account named after the remote, switched off when `--days` ends.

### Pools: a party where everyone lends their agents

```bash
hashi pool open --via lan            # host: a join link for this wifi (or --via cloudflare, tailscale)
hashi pool join "<link>" --name sid --agents claude --stop-at 70 --hours 4   # member
hashi pool status                    # host: who is in, online, headroom per agent
```

- Members need no public URL: their Hashi asks the host for work and runs it with their own accounts and limits. `hashi serve` must run on their machine. Pausing their local `pool ...` token takes them out of rotation.
- Each member gets a pool token on the host (saved as the remote `<host>-pool`, so `hashi ask --remote <host>-pool` works), and `GET /v1/pool` shows everyone's agents and headroom.
- Every share takes `--via auto|cloudflare|tailscale|lan|local`. `lan` is plain http on the local network: say so to the owner.
- Ask before kicking someone (`hashi pool kick <name>`) or leaving (`hashi pool leave`).

### Create a token for an app

```bash
hashi token create "ci-bot" --agents claude --daily-requests 100 --note "GitHub Actions"
hashi token create "docs-site" --project docs --stateful --expires-in-days 90
```

The raw token is printed once on standard output, and never again. Tell the owner to store it now (for example in their secret manager). Do not echo it. `--admin` creates an admin token: only do that when the owner asks.

### Pause, resume, change and revoke a token

Find the token first. Tokens are addressed by id, display prefix (the `prefix` field, which is not secret) or unique name.

```bash
hashi token list --json
hashi token update <id-or-prefix> --pause
hashi token update <id-or-prefix> --resume
hashi token update <id-or-prefix> --daily-requests 20 --expires-in-days 7
hashi token update <id-or-prefix> --clear-limits
hashi token revoke <id-or-prefix>
```

Pausing is reversible; revoking is not. Revoking stops the token's running prompts and deletes its workspace. Ask before revoking. The last admin token cannot be revoked.

### Add an account and see usage

An account is another login, API key or OAuth token for an agent. Runs move to the next account when one hits its limit.

```bash
hashi account add claude work
hashi account add codex ci --kind api-key --key-env OPENAI_API_KEY
hashi account add claude bot --kind oauth-token --key-stdin
hashi account list --json
hashi account usage
hashi account usage --refresh --json
```

- A login account prints its sign-in command. The owner runs it in a terminal; then check with `hashi account list`.
- `hashi switch <claude|codex> <name|default>` points the owner's plain `claude` or `codex` command at a login account (the desktop app is not affected); without a name it shows who is signed in. `hashi account reset` undoes every switch and command. `hashi account commands --agent <claude|codex> --name <word>` makes a command with that name that runs the CLI without permission prompts; nothing is set by default, and an empty name removes it.
- Never ask the owner to paste a key into the chat. Use `--key-env <VAR>` with a variable they set, or `--key-stdin` in their own terminal.
- Put an account on a token with `hashi token update <id> --account claude=work,personal`.
- `hashi account disable`, `enable` and `clear-limit` take `<agent> <name>`. `hashi account remove` deletes the account and its key (ask first).

### Set up a project with accounts and a strategy

A project is a folder that tokens and invites can be bound to. Bound tokens run in that folder.

```bash
hashi project add web /abs/path/to/web --agents claude,codex --account claude=work,personal --default-agent claude
hashi project update web --account-strategy most-left --rotate-at 85 --model claude=sonnet
hashi project list --json
hashi share "Bob" --project web
hashi token create "web-ci" --project web
```

- The path must be absolute and must exist. The folder is never deleted by Hashi.
- `--account-strategy` is `in-order`, `most-left` or `soonest-reset`. `--rotate-at` is a percent from 50 to 99.
- `hashi project remove <name>` refuses while tokens or invites are bound; `--unbind` moves them back to their own workspaces first. Ask before removing.

### Go public

Remote people need a public URL. These are start-up flags of the server, so the owner restarts `hashi` with them (or presses `t` in the dashboard).

```bash
hashi serve --tunnel
hashi serve --tailscale serve
hashi serve --tailscale funnel
hashi serve --tunnel-token <cloudflare-tunnel-token> --public-url https://hashi.example.com
hashi serve --public-url https://hashi.example.com
```

- `--tunnel` starts a Cloudflare quick tunnel (needs `cloudflared`; `hashi install cloudflared`).
- `--tailscale serve` reaches the owner's tailnet only; `funnel` reaches the internet. Both need `tailscale`, signed in.
- Pass a tunnel token through `HASHI_TUNNEL_TOKEN` rather than the command line, so it stays out of the process list.
- Once the server is public, `hashi share` and `hashi requests open` print public links.

### Respond to access requests

People can ask for access through the ask-access link, and token holders can ask for more.

```bash
hashi requests open --project web --note "ask for the docs" --hours 72
hashi requests list --json
hashi requests list --all
hashi requests accept <id> --daily-requests 30
hashi requests decline <id> --reason "not this week"
hashi requests close
```

- Show the owner each pending request (name, message, what they asked for) and let the owner decide. Do not accept or decline on your own.
- Accepting a new person creates a single-use invite only they can collect. Accepting a token's request changes that token.
- Request ids can be shortened to a unique start of at least 4 characters.

### Connect to someone else's bridge and ask remotely

```bash
hashi connect "<invite link>" --name alice
hashi remote list
hashi ask --remote alice "Summarise the README"
hashi ask --remote "Summarise the README"
hashi request "<ask-access link>" --name "Owner Name" --message "for the docs"
hashi request status
```

- `hashi connect` redeems the invite and saves the token in the data directory; it is never printed.
- `--remote` without a name works when only one bridge is saved.
- `hashi account add claude via-alice --remote alice` turns that access into an account the owner's own tokens can fail over to.

### Ask a local agent directly

```bash
hashi ask "What does src/main.rs do?" --agent codex
```

This runs the agent CLI once, without the server or the queue.

### Check for updates

```bash
hashi update --check
```

This only reports. Installing is `hashi update`, which replaces the binary: ask first, and tell the owner to restart `hashi` afterwards.

### File a bug

```bash
hashi report
```

It writes a redacted report file and uploads nothing. Ask the owner to read it before sharing.

"#;

const HTTP: &str = r#"## HTTP API, if the CLI is not available

Use these when `hashi` is not on PATH but a server and a token are. The base URL is `http://127.0.0.1:7878` locally, or the public URL. Keep the token in an environment variable, such as `HASHI_TOKEN`, and never paste it into a command you show.

Send a prompt and wait up to 60 seconds (`POST /v1/ask`):

```bash
curl http://127.0.0.1:7878/v1/ask -H "Authorization: Bearer $HASHI_TOKEN" -d '{"message": "Explain src/main.rs", "agent": "claude", "wait": 60}'
```

A `202` answer has a `poll` path: read it with `GET /v1/ask/{id}?wait=30`.

List tokens with an admin token (`GET /v1/tokens`; `POST` creates one, `PATCH /v1/tokens/{id}` updates, `DELETE` revokes):

```bash
curl http://127.0.0.1:7878/v1/tokens -H "Authorization: Bearer $HASHI_TOKEN"
```

See the token's own status, limits and usage (`GET /v1/me`):

```bash
curl http://127.0.0.1:7878/v1/me -H "Authorization: Bearer $HASHI_TOKEN"
```

OpenAI-compatible chat (`POST /v1/chat/completions`; `GET /v1/models` lists the agents). `model` is the agent, optionally with its own model after a slash, such as `claude/opus`:

```bash
curl http://127.0.0.1:7878/v1/chat/completions -H "Authorization: Bearer $HASHI_TOKEN" -d '{"model": "claude", "messages": [{"role": "user", "content": "Hello"}]}'
```

The full reference, with every endpoint, field and error code, is `docs/api.md` in the repository: https://github.com/sidharthpunathil/hashi/blob/main/docs/api.md
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> clap::Command {
        clap::Command::new("hashi")
            .subcommand(
                clap::Command::new("token").about("Tokens.").subcommand(
                    clap::Command::new("create")
                        .about("Create a token.")
                        .arg(clap::Arg::new("name").required(true).help("Label."))
                        .arg(
                            clap::Arg::new("admin")
                                .long("admin")
                                .action(clap::ArgAction::SetTrue)
                                .help("Grant admin rights."),
                        )
                        .arg(clap::Arg::new("note").long("note").help("Who it is for.")),
                ),
            )
            .subcommand(clap::Command::new("doctor").about("Check things."))
    }

    #[test]
    fn reference_lists_paths_flags_and_whether_they_take_a_value() {
        let r = reference(&sample());
        for text in [
            "### `hashi token`",
            "### `hashi token create`",
            "### `hashi doctor`",
            "Create a token.",
            "`<name>` (required argument): Label.",
            "`--admin`: Grant admin rights. Switch, takes no value.",
            "`--note <note>`: Who it is for.",
            "Usage: `hashi token create",
        ] {
            assert!(r.contains(text), "missing {text:?} in\n{r}");
        }
        assert_eq!(
            command_paths(&sample()),
            ["token", "token create", "doctor"]
        );
    }

    #[test]
    fn the_fixed_text_has_no_dash_substitutes() {
        let text = assemble(Some(""));
        for c in [
            '\u{2014}', '\u{2012}', '\u{2015}', '\u{2212}', '\u{2E3A}', '\u{2E3B}', '\u{FE58}',
            '\u{FE63}', '\u{FF0D}',
        ] {
            assert!(!text.contains(c), "found U+{:04X}", u32::from(c));
        }
        assert!(!text.contains(" \u{2013} "));
        assert!(!text.contains(" -- "));
    }
}
