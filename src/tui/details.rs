//! The long form of each Commands row: what it is, how it works and what
//! to know before using it. `d` on the Commands view opens it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use super::app::{Action, App};
use super::theme as t;
use super::ui::{action_cli, centered, key_hint, modal, plain};

/// What a Commands row does, at length.
#[derive(Debug, Clone, Copy)]
pub struct Details {
    /// What it is, in a paragraph.
    pub about: &'static str,
    /// How it works, step by step.
    pub how: &'static [&'static str],
    /// What to know before using it.
    pub know: &'static [&'static str],
}

/// The long form of `action`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub const fn details(action: Action) -> Details {
    match action {
        Action::Cloudflare => Details {
            about: "Puts this Hashi on the internet through a Cloudflare quick tunnel, so people \
                    outside your network can open your links and send prompts. It needs \
                    cloudflared on this machine and no Cloudflare account.",
            how: &[
                "Hashi starts cloudflared, which connects out to Cloudflare and gets a random \
                 https://<words>.trycloudflare.com address. Nothing on your router changes.",
                "The header shows the address once it is up, and new invite, login share and \
                 pool links use it.",
                "Requests reach Cloudflare over HTTPS and come down the tunnel to this machine. \
                 Every request still needs a Hashi token.",
                "While it is up, the address is kept in ~/.hashi/public-url, so `hashi share` in \
                 another terminal prints a public link too.",
            ],
            know: &[
                "The address changes every time the tunnel starts, so links made with an old \
                 one stop working after a restart.",
                "One public route at a time: starting this while Tailscale is up asks to switch.",
                "Without cloudflared, this opens its install command instead.",
                "Stop going public turns it off; quitting Hashi does too.",
            ],
        },
        Action::TailscaleServe => Details {
            about: "Serves this Hashi on your tailnet at https://<machine>.<tailnet>.ts.net: a \
                    stable HTTPS name that only devices on your Tailscale network can reach.",
            how: &[
                "Hashi runs `tailscale serve --bg --https=443` pointing at this server, and on \
                 stop turns off only that listener.",
                "Links use the .ts.net address while it is up. Tailscale handles the \
                 certificate.",
                "Every request still needs a Hashi token; Hashi ignores Tailscale's identity \
                 headers.",
            ],
            know: &[
                "Needs the tailscale CLI, installed and signed in.",
                "A friend can open your links only if they are on your tailnet, or you share \
                 this machine with them in the Tailscale admin console. For anyone else, use \
                 Funnel or the Cloudflare tunnel.",
                "Hashi never resets the rest of your Serve config.",
                "The server must listen over plain HTTP on 127.0.0.1 or 0.0.0.0, because \
                 Tailscale proxies to http://127.0.0.1.",
            ],
        },
        Action::TailscaleFunnel => Details {
            about: "Like Tailscale serve, but anyone on the internet can reach the .ts.net \
                    address, with no Tailscale of their own.",
            how: &[
                "Hashi runs `tailscale funnel --bg --https=443` pointing at this server.",
                "If your tailnet has not allowed Funnel yet, Tailscale prints an approval link. \
                 Hashi shows it in a popup and waits up to 60 seconds.",
                "Links use the .ts.net address while it is up.",
            ],
            know: &[
                "The address stays the same across restarts, unlike a quick tunnel, so links \
                 keep working.",
                "Needs MagicDNS, HTTPS certificates and the funnel attribute in your tailnet \
                 policy. Funnel only takes ports 443, 8443 and 10000.",
                "Anyone with the address reaches the server, but nothing works without a token.",
            ],
        },
        Action::StopPublic => Details {
            about: "Turns off whichever public route is up (the Cloudflare tunnel, Tailscale \
                    serve or funnel) without stopping Hashi.",
            how: &[
                "For the tunnel, Hashi stops cloudflared. For Tailscale, it runs only the off \
                 command for the listener it turned on.",
                "~/.hashi/public-url is removed and new links go back to this machine's address.",
                "The server keeps running, and queued and running prompts carry on.",
            ],
            know: &[
                "Links that used the public address stop working until a route is up again, and \
                 a quick tunnel comes back at a new address.",
                "Tokens people already claimed keep working once you go public again.",
            ],
        },
        Action::NewInvite => Details {
            about: "Makes a one-time link that gives someone their own token for this Hashi. \
                    They send prompts here and your agents answer, within the limits you set. \
                    Your logins and keys never leave this machine.",
            how: &[
                "Choose a label, the agents, limits (prompts a day or a month, a budget, how \
                 many at once), how long access lasts and an optional project.",
                "Stop at: a percent of your own 5-hour or weekly quota. Past it their prompts \
                 are refused, so you keep the rest for yourself.",
                "Link via: the Cloudflare tunnel, Tailscale, this wifi or this machine. The \
                 dashboard starts the route when it is not up.",
                "The link works once and expires after 1, 24 or 168 hours. Its page shows what \
                 they get; claiming gives them the token once, with setup snippets.",
                "With Hashi installed, they can run `hashi connect <link>` and use your agents \
                 from their own terminal.",
            ],
            know: &[
                "You can see their prompts and answers, and the claim page tells them so.",
                "Pending invites are in Tokens (press i); x cancels one. The claimed token lives \
                 there too: e edits, p pauses, d revokes.",
                "At most 100 invites can be pending.",
            ],
        },
        Action::ShareLogin => Details {
            about: "Hands the key of an API-key or OAuth-token account to someone you trust, \
                    through a one-time link, so they run the agent on their own machine with \
                    your account.",
            how: &[
                "Choose the account, how many days the share lasts and a percent to be \
                 reminded at.",
                "When they redeem the link (`hashi connect` or Connect to a bridge), their \
                 Hashi adds the key as one of its accounts.",
                "Their Hashi reads your account's usage through the link. Both of you get a \
                 reminder in the log when it passes the agreed percent.",
                "When the days run out, their copy of the account is switched off.",
            ],
            know: &[
                "Nothing is enforced: they hold the real key. Share only with someone you trust, \
                 and replace the key with the provider if that changes.",
                "Only key accounts can be shared this way. A login account's sign-in stays on \
                 this machine; share it with New invite instead.",
                "If their Hashi cannot add the key as an account, it saves it to an owner-only \
                 file and says where.",
            ],
        },
        Action::Pool => Details {
            about: "Hosts a party: people join with their own Hashi and lend their agents, and \
                    everyone in the pool can use all of them through one endpoint. From here \
                    you can also join someone else's pool, or leave it.",
            how: &[
                "o opens a join link for a number of hours and people, over the route you pick. \
                 c closes it; members stay.",
                "Each member's Hashi keeps asking yours for work and runs prompts on its own \
                 accounts, within its own stop percent and end time, then sends the answer \
                 back. Members need no public URL.",
                "Each member gets a pool token, saved as a bridge, so `hashi ask --remote \
                 <host>-pool` sends a prompt to the whole pool.",
                "The popup (and GET /v1/pool) shows everyone, online or not, with each agent's \
                 headroom. x removes a member, a joins another pool, l leaves it.",
            ],
            know: &[
                "The host sees every prompt that goes through the pool, and a member sees the \
                 prompts its agents run.",
                "Keys never leave a member's machine: only prompts and answers travel.",
                "A member that has not checked in for 75 seconds counts as offline and gets no \
                 work.",
            ],
        },
        Action::NewToken => Details {
            about: "Creates an API token for a script, an app or yourself. There is no link: \
                    the token is shown right here, once.",
            how: &[
                "Choose a name, a role, whether it keeps conversations, the agents, the \
                 accounts, a project, and whether it can see how much quota is left.",
                "Use it as a Bearer token on /v1/ask, the OpenAI-compatible API under /v1, or \
                 the MCP server at /mcp.",
                "Copy it when it is shown: Hashi keeps only a hash and cannot show it again.",
            ],
            know: &[
                "A user token made without limits may have 3 prompts waiting or running at \
                 once. Set limits with e in Tokens.",
                "Tokens are in the Tokens view: e edits, p pauses, d revokes. Revoking stops its \
                 running prompts at once.",
                "For a person, New invite is friendlier: they get the token through a claim \
                 page with setup help.",
            ],
        },
        Action::AccessRequests => Details {
            about: "People can ask you for access through a link instead of waiting for an \
                    invite, and people who already have a token can ask for more. This lists \
                    those requests and lets you decide.",
            how: &[
                "o opens an ask-access link (for a week) and x closes it. Nobody can ask until \
                 a link is open.",
                "Each request shows who asked, their message and what they want. a accepts \
                 with a form filled from what they asked for; d declines with an optional \
                 reason they see.",
                "Accepting a new person makes a one-time invite their page collects by itself. \
                 Accepting a request for more updates their token.",
                "s also shows decided and expired requests.",
            ],
            know: &[
                "Nothing is granted until you accept.",
                "The tab shows how many are waiting, and a toast appears when one arrives.",
                "A request nobody decides expires after 7 days.",
            ],
        },
        Action::Ask => Details {
            about: "Sends a prompt to an agent from the dashboard, here or on a bridge you \
                    connected to: to check that things work, or to get an answer without \
                    leaving the terminal.",
            how: &[
                "Choose this Hashi or a saved bridge, the agent and, here, optionally one \
                 account. Type the prompt; enter adds a line and ctrl-s sends.",
                "A local prompt goes through the normal queue as the admin token, so it shows \
                 in Requests with its answer in the detail.",
                "A bridge's answer opens in its own popup.",
            ],
            know: &[
                "Picking an account runs the prompt on that account only: a quick way to test \
                 one.",
            ],
        },
        Action::Accounts => Details {
            about: "The accounts this Hashi runs prompts on: each agent's default sign-in, \
                    extra logins, API keys, OAuth tokens, bridges used as accounts, logins \
                    shared with you and pool members.",
            how: &[
                "a adds one. A login account steps aside for the agent's own sign-in; a key \
                 account takes the key in a masked field and stores it owner-only.",
                "l signs an account in again, d disables, e enables, x removes, u reads its \
                 usage now.",
                "Each row shows how much of its usage windows is used and when they reset.",
                "Each Claude Code and Codex account gets terminal commands, such as \
                 work-claude and work-dclaude; n changes their names.",
            ],
            know: &[
                "When an account hits its limit, Hashi moves to the next one, as the account \
                 strategy says.",
                "A shared login shows who shared it, the agreed percent and when the share ends.",
            ],
        },
        Action::Projects => Details {
            about: "Folders on this machine with their own agents, accounts, models and account \
                    strategy. Tokens and invites bound to a project run their prompts in that \
                    folder.",
            how: &[
                "a adds a project from a folder, e edits it and x removes it.",
                "A bound token may use only the agents both it and the project allow, and the \
                 project's accounts.",
                "The strategy picks how runs choose accounts (in order, most left, soonest \
                 reset) and the percent at which they move off one.",
            ],
            know: &[
                "Removing a project never touches the folder. Its tokens go back to their own \
                 workspace.",
                "The folder must exist and sit outside the Hashi home.",
            ],
        },
        Action::Bridges => Details {
            about: "Other people's Hashi that this machine connected to with an invite link. \
                    You can send them prompts, or use them as accounts.",
            how: &[
                "The list shows names and URLs, never tokens. Tokens stay in \
                 ~/.hashi/remotes.json, readable only by you.",
                "c connects a new one, a asks it, x forgets one, r checks the requests for \
                 access you sent.",
                "`hashi account add <agent> <name> --remote <bridge>` makes a bridge an \
                 account, so it joins failover and strategies.",
            ],
            know: &[
                "The person running a bridge sees every prompt you send it.",
                "Forgetting a bridge switches off the accounts that used it. The owner can \
                 still revoke your token on their side.",
            ],
        },
        Action::Connect => Details {
            about: "Redeems a link someone sent you: an invite to their Hashi, or a login share.",
            how: &[
                "Paste the link. Hashi claims it and saves the token under a name, by default \
                 their server's name.",
                "An invite becomes a bridge in Bridges I use. A login share adds their key as \
                 an account here.",
                "Then Ask can send prompts to it.",
            ],
            know: &[
                "Links must be https, or plain http on this machine or a private network (for \
                 sharing on one wifi).",
                "A link works once. If it has expired or was used, ask for a new one.",
            ],
        },
        Action::Listen => Details {
            about: "Moves the running server to another port or bind address without stopping \
                    it.",
            how: &[
                "Choose a port and a bind address: 127.0.0.1 for this machine only, 0.0.0.0 or \
                 :: for every network, or a typed IP such as a Tailscale address.",
                "The new address is bound first. If that fails, the form says why and nothing \
                 changes.",
                "The old address then finishes its open requests within 10 seconds and closes. \
                 Queued and running prompts carry on.",
                "A tunnel or Tailscale route that is up restarts on the new port.",
            ],
            know: &[
                "Listening on 0.0.0.0 without TLS asks first: anyone on your network can reach \
                 the server, though still only with a token.",
                "Sharing over this wifi needs 0.0.0.0, so people on the network can reach you.",
                "The choice is saved for the next start.",
            ],
        },
        Action::CopyUrl => Details {
            about: "Copies the base URL clients should use: the public address when a route is \
                    up, otherwise this machine's.",
            how: &[
                "Use it as the base for /v1/ask, with /v1 as the OpenAI base URL, or with /mcp \
                 for the MCP server.",
                "c copies it from any view.",
            ],
            know: &[
                "A 127.0.0.1 URL works only on this machine. Go public, or listen on the \
                 network, to reach it from elsewhere.",
            ],
        },
        Action::InstallCloudflared => Details {
            about: "Installs cloudflared, the Cloudflare program the quick tunnel needs.",
            how: &[
                "Shows Cloudflare's own install command for this machine, such as `brew \
                 install cloudflared` on macOS, or its package repository on Linux.",
                "enter runs it with the terminal attached, so sudo can ask for your password, \
                 then the dashboard comes back. c copies it instead.",
                "Without a package manager it shows the download page.",
            ],
            know: &[
                "Hashi never downloads and runs a program itself.",
                "When it is installed, the row shows where.",
            ],
        },
        Action::InstallTailscale => Details {
            about: "Installs tailscale, which Tailscale serve and funnel need.",
            how: &[
                "Shows Tailscale's own install command for this machine, such as its install \
                 script on Linux or Homebrew on macOS.",
                "enter runs it with the terminal attached, so sudo can ask for your password, \
                 then the dashboard comes back. c copies it instead.",
                "Then sign in: `sudo tailscale up` on Linux, or the Tailscale app on macOS and \
                 Windows.",
            ],
            know: &[
                "Hashi never downloads and runs a program itself.",
                "Doctor shows whether Tailscale is signed in.",
            ],
        },
        Action::InstallCrush => Details {
            about: "Installs Crush, Charm's coding agent, so Hashi can run prompts on it too.",
            how: &[
                "Shows Charm's own install command. enter runs it with the terminal attached; \
                 c copies it.",
                "Hashi uses Crush only when started with --enable-crush.",
            ],
            know: &[
                "Crush approves its own tool use, so Hashi switches off every tool that can \
                 write, run code or reach the network, and checks the Crush version first.",
                "Crush can still read any file your user can read.",
            ],
        },
        Action::CheckUpdate => Details {
            about: "Asks GitHub whether a newer Hashi release is out.",
            how: &[
                "One request to api.github.com, carrying only the Hashi version.",
                "Hashi also checks once a day by itself and remembers the answer.",
                "If one is out, Install the update installs it.",
            ],
            know: &["Checking downloads nothing."],
        },
        Action::InstallUpdate => Details {
            about: "Downloads the latest release and replaces this binary with it.",
            how: &[
                "Asks first. Then downloads this platform's archive from the GitHub release, \
                 verifies its SHA-256 checksum and checks the new binary reports the expected \
                 version.",
                "Then swaps it in. Quit and start Hashi again to run the new version.",
            ],
            know: &[
                "It refuses a binary installed by Cargo, or one in a folder you cannot write \
                 to.",
                "Everything in ~/.hashi stays.",
            ],
        },
        Action::Doctor => Details {
            about: "A health check of this machine for Hashi. Start here when something does \
                    not work.",
            how: &[
                "Shows each agent found and its version, Crush, cloudflared, Tailscale and its \
                 sign-in, the accounts, the update state, the home folder and the resource \
                 guard.",
                "u checks for updates and r checks everything again.",
            ],
            know: &["The same checks as `hashi doctor` in a terminal."],
        },
        Action::Report => Details {
            about: "Writes a file to attach to a bug report.",
            how: &[
                "It holds the version, OS, the agents and helpers found, HASHI_ settings, \
                 database counts, the last 20 failed requests, the last 200 log lines and any \
                 crash file.",
                "Tokens, invite codes, provider keys and your home folder are blanked out. \
                 Prompts are left out unless you turn include prompts on.",
                "The file is saved owner-only and its path shown; c copies the path.",
            ],
            know: &["Nothing is uploaded. Read the file, then attach it to a GitHub issue."],
        },
        Action::Audit => Details {
            about: "A record of every change to who can use this Hashi: tokens, invites, \
                    shares, pools, access requests and accounts.",
            how: &[
                "Shows the last 500 entries, newest first, with who made each change. Changes \
                 made here show as dashboard.",
                "↑↓ and PgUp PgDn scroll.",
            ],
            know: &["The same list as `hashi audit`."],
        },
        Action::CopyGuide => Details {
            about: "Copies a Markdown guide that tells an LLM agent how to install, run and \
                    manage Hashi. Paste it into Claude Code, Codex or another agent and let it \
                    do the setup.",
            how: &["Copies the text of `hashi llm` and says how many lines it copied."],
            know: &["It holds no tokens or secrets, only how Hashi works."],
        },
        Action::Help => Details {
            about: "Every key the dashboard knows, in one list.",
            how: &["? opens it from any view, and any key closes it."],
            know: &[
                "`hashi --help` lists the terminal commands, and `hashi <command> --help` \
                 explains each one.",
            ],
        },
    }
}

/// `text` broken into lines of at most `width` columns, at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn heading(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(text, t::bold(t::ORCHID)))
}

/// Points, each wrapped under its own bullet.
fn points(items: &[&str], width: usize, out: &mut Vec<Line<'static>>) {
    for item in items {
        for (i, l) in wrap(item, width.saturating_sub(2)).into_iter().enumerate() {
            let mark = if i == 0 { "• " } else { "  " };
            out.push(Line::from(vec![
                Span::styled(mark, t::fg(t::PINK)),
                Span::styled(l, plain()),
            ]));
        }
    }
}

/// Every line of `action`'s details, wrapped to `width` columns.
#[must_use]
pub fn lines(app: &App, action: Action, width: usize) -> Vec<Line<'static>> {
    let d = details(action);
    let mut out = Vec::new();
    for l in wrap(action.description(), width) {
        out.push(Line::from(Span::styled(l, t::muted())));
    }
    out.push(Line::raw(""));
    out.push(heading("What it is"));
    for l in wrap(d.about, width) {
        out.push(Line::from(Span::styled(l, plain())));
    }
    out.push(Line::raw(""));
    out.push(heading("How it works"));
    points(d.how, width, &mut out);
    out.push(Line::raw(""));
    out.push(heading("Good to know"));
    points(d.know, width, &mut out);
    out.push(Line::raw(""));
    out.push(heading("Run it"));
    let key = match action.key() {
        "enter" => "enter on this row".to_owned(),
        k @ ("s" | "n") => format!("enter, or {k} here and in Tokens"),
        k => format!("enter, or {k} from anywhere"),
    };
    out.push(Line::from(vec![
        Span::styled("here      ", t::muted()),
        Span::styled(key, plain()),
    ]));
    out.push(Line::from(vec![
        Span::styled("terminal  ", t::muted()),
        Span::styled(action_cli(app, action), t::fg(t::ORCHID)),
    ]));
    out
}

/// Draws the details popup of `action`, scrolled down `scroll` lines.
pub fn render(app: &App, frame: &mut Frame, action: Action, scroll: u16) {
    let area = frame.area();
    // As tall as the text needs: the border, the top padding, the text, a
    // line for "more" and the keys. Taller than the screen, it scrolls.
    let width = 92.min(area.width.saturating_sub(4));
    let text_w = usize::from(width.saturating_sub(6));
    let need = u16::try_from(lines(app, action, text_w).len() + 5).unwrap_or(u16::MAX);
    let rect = centered(area, width, need.min(area.height.saturating_sub(2)).max(10));
    let block = modal(action.title());
    let inner = block.inner(rect);
    let [text, gap, keys] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let body = lines(app, action, usize::from(text.width));
    let max = u16::try_from(body.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(text.height);
    app.details_max_scroll.set(max);
    let scroll = scroll.min(max);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(body).scroll((scroll, 0)), text);
    if scroll < max {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("↓ more", t::muted()))),
            gap,
        );
    }
    frame.render_widget(
        Paragraph::new(key_hint(&[
            ("↑↓", "scroll"),
            ("←→", "other command"),
            ("enter", "run it"),
            ("esc", "close"),
        ])),
        keys,
    );
}
