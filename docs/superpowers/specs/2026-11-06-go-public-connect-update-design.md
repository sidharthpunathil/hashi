# Go public, friend setup and updates: design

## Intent

Three gaps make sharing Hashi harder than it should be:

1. The owner starts `hashi` and gets `http://127.0.0.1:7878`. An invite made
   then only works on the owner's machine, and the only fix today is to quit
   and restart with `--tunnel`.
2. A friend who receives an invite link has to read snippets and wire their
   tools by hand. The owner wants one link that sets the friend up.
3. Nothing tells the owner a new release exists, and upgrading means rerunning
   the installer by hand.

The friend never needs to run a Hashi server. They need a token, and their own
tools (Claude Code, Cursor, VS Code, a terminal) pointed at the owner's bridge.

## Goals

### A. Go public without a restart (owner)

- Dashboard key `t` starts a Cloudflare quick tunnel on a running server when
  none is running. The header shows progress, then the URL. When `cloudflared`
  is missing, a toast names the install command for the platform.
- The tunnel started this way behaves exactly like `--tunnel`: the public URL
  becomes the base for invite links unless `--public-url` is set, and
  `CF-Connecting-IP` is trusted from loopback only while a tunnel is
  configured (this flag becomes runtime state instead of a startup constant).
- The invite popup keeps the invite code, not a frozen URL. When the link is
  local only it says "press t to make this link public", and when the tunnel
  comes up while the popup is open the shown URL switches to the public one.
- The running server writes its current public base URL to
  `~/.hashi/public-url` (owner only) when it has one and removes it on
  shutdown, so `hashi share` from another terminal prints a public link.

### B. One-link friend setup

- After claiming, the claim page offers one-click MCP install buttons for
  Cursor and VS Code (their documented MCP install deep links) next to the
  existing `claude mcp add` command. Only commands verified against each
  tool's own documentation or source are shown.
- Before claiming, the claim page also offers "Set up from a terminal": a
  single command that runs the official installer from GitHub with
  `HASHI_CONNECT='<this invite link>'` (sh and PowerShell forms). The
  installers, after installing, run `hashi connect "$HASHI_CONNECT"`.
- `hashi connect <invite-url> [--name <name>]` redeems the invite over HTTPS
  (plain HTTP only for loopback hosts), stores `{name, baseUrl, token}` in
  `~/.hashi/remotes.json` (owner only), and prints the ready-to-run setup
  commands for the friend's tools plus a first `hashi ask` example. It never
  runs another program without the user asking (`--setup claude` runs
  `claude mcp add` for them).
- `hashi ask --remote [<name>] "<prompt>"` sends the prompt to a saved remote
  (waits and polls until done) instead of running a local agent. With one
  saved remote the name is optional. `hashi remote list` and
  `hashi remote remove <name>` manage them.

### C. Updates

- On server start (background, never blocking startup) and in
  `hashi doctor`, Hashi asks GitHub for the latest release at most once per
  24 hours, caching the answer in `~/.hashi/update-check.json`. A newer
  version raises a bus event; the dashboard header shows
  "update vX.Y.Z: run hashi update", and headless mode logs one line.
- `--no-update-check` / `HASHI_NO_UPDATE_CHECK=1` turns the check off. The
  request carries only a `hashi/<version>` User-Agent.
- `hashi update [--version vX.Y.Z] [--check]` downloads the archive for the
  current platform and its `.sha256` from the GitHub release, verifies the
  checksum, extracts it with the system `tar`, and replaces the running
  binary (rename on Unix; on Windows move the running exe aside and clean it
  up on the next start). It refuses with a clear message when the binary is
  not writable or was installed by Cargo (`~/.cargo/bin`), and tells the
  user to restart Hashi afterwards.

## Non-goals

- Silent automatic installs. Hashi tells the owner and the owner runs
  `hashi update`.
- Starting a tunnel without the owner asking: tunnels stay opt-in.
- Hosting an installer script on the owner's bridge. The terminal setup
  always runs the installer from GitHub, which verifies checksums.

## Anti-goals

- Any network request during tests. Tests use local servers and injected
  base URLs.
- Any `unsafe` code, em dash characters, or AI attribution anywhere.
- Storing a friend's token anywhere but the owner-only `remotes.json`.

## Calls made by the builder

- `ureq` (rustls, no default features) is the HTTP client for CLI commands.
- Release asset names follow `.github/workflows/release.yml`:
  `hashi-<target>.tar.gz` and `.zip` on Windows, each with `.sha256`.
