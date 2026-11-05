# Tailscale, helper installs and the Commands tab: design

## Intent

The owner wants every way of going public to be something they can start,
stop and, if the helper is missing, install, from inside Hashi: the CLI and
the dashboard. Cloudflare quick tunnels already start with `t`. Tailscale is
the add-on: many people already run it, and it gives a stable
`https://<machine>.<tailnet>.ts.net` address with no Cloudflare account.

## Tailscale modes

| Mode | What it runs | Who can reach Hashi |
|---|---|---|
| `serve` | `tailscale serve --bg --https=<port> http://127.0.0.1:<hashi port>` | devices on the owner's tailnet, and people the machine is shared with |
| `funnel` | `tailscale funnel --bg --https=<port> http://127.0.0.1:<hashi port>` | anyone on the internet |

Facts from Tailscale's docs (kb/1242, kb/1223, CLI reference):

- Proxies only support `http://127.0.0.1` targets.
- Funnel listens only on 443, 8443 or 10000, and needs MagicDNS, HTTPS
  certificates and the `funnel` node attribute; when it is not enabled the CLI
  prints a URL that asks the owner to approve it.
- Turning one listener off is the enabling command plus `off`, with the same
  flags (`tailscale serve --https=443 off`). `reset` clears everything and must
  never be used, because it would delete the owner's other Serve config.
- Serve and Funnel forward from loopback and add `X-Forwarded-For`; Serve
  also adds Tailscale identity headers, which Hashi ignores (a Hashi token is
  still required).
- The device's name comes from `tailscale status --json` (`Self.DNSName`,
  trailing dot removed); `BackendState` is `Running` when signed in. The JSON
  format is documented as subject to change, so parse defensively and fail
  with a clear message.

## Goals

1. **Tailscale exposure.**
   - `hashi serve --tailscale serve|funnel` (env `HASHI_TAILSCALE`) and
     `--tailscale-port` (default 443) start it with the server.
   - The Commands tab starts and stops it at runtime.
   - On start, Hashi checks that the binary exists and that the backend is
     `Running`; otherwise it says how to sign in (`tailscale up`, or the app on
     macOS and Windows).
   - Hashi reads `tailscale serve status --json` first and refuses when the
     chosen port already has a handler Hashi did not create, suggesting
     another `--tailscale-port`.
   - With a 60 s timeout, any approval URL the CLI prints is surfaced to the
     owner as-is.
   - Once up, the `https://<DNSName>` URL becomes the public base (a
     configured `--public-url` still wins), its host is accepted by the Host
     check, and `X-Forwarded-For` from loopback is trusted while it is active.
   - On stop and on shutdown, Hashi runs only the matching `off` command for
     what it turned on.
   - The public-url file and the TunnelReady/TunnelDown events behave as they
     do for Cloudflare.
2. **Stop going public.**
   - The Cloudflare quick tunnel started with `t` or `--tunnel` can also be
     stopped at runtime.
   - Only one public route (Cloudflare or Tailscale) is active at a time.
     Starting another asks to switch.
3. **Helper installs the owner controls.**
   - `hashi install cloudflared|tailscale [--yes]` prints the exact official
     command for this platform and runs it only after the owner confirms. Each
     command must be verified against the vendor's install docs, never
     guessed: Homebrew on macOS, winget on Windows, Tailscale's official
     script on Linux, and Cloudflare's documented Linux method.
   - Where no supported package manager is present, Hashi prints the vendor's
     download page instead of acting.
   - Hashi never downloads and runs a binary itself.
   - In the dashboard, a missing helper opens a popup with the same command:
     `enter` runs it in the terminal (the dashboard steps aside, the command
     runs with the terminal attached so sudo prompts work, then the dashboard
     comes back), `c` copies it, `esc` cancels.
   - `hashi doctor` reports both helpers, and Tailscale's sign-in state.
4. **Tab 5: Commands.**
   - A fifth view lists what the owner can do, grouped:
     - Go public: Cloudflare quick tunnel; Tailscale serve; Tailscale funnel;
       stop.
     - Share: new invite; new token.
     - Install: cloudflared; tailscale.
     - Maintenance: check for updates (shows `hashi update` when one exists);
       copy the API URL.
     - Help: all key bindings.
   - Each row shows its key or Enter, a one-line description, its current
     state (running, not installed, signed out, active URL), and the
     equivalent CLI command so the owner learns it.
   - Arrow keys select, and Enter runs the row.
   - `5` opens the tab, and the tab bar shows it. The `?` popup stays.

## Non-goals

- Managing Tailscale ACLs, sharing machines, or signing the owner in.
- Named Cloudflare tunnels from the dashboard (they need a token and a
  hostname; the flags stay as they are).

## Anti-goals

- Running `tailscale serve reset` or `funnel reset`, or touching Serve
  listeners Hashi did not create.
- Installing anything without an explicit yes.
- Tests that run tailscale, cloudflared, package managers or the network.
  Tests use fake executables on a temp `PATH` and canned JSON.
