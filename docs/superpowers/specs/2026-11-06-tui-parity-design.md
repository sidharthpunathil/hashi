# Everything from the dashboard: design

## Intent

The owner should be able to do nearly everything the CLI does without leaving
the dashboard. The CLI stays the scriptable interface. The dashboard is
where people actually live while Hashi runs.

## Rule

Every subcommand and option of `hashi` (read `hashi --help` and each
subcommand's `--help` from the clap definitions in `src/main.rs` and
`src/admin_cli.rs`) is put in exactly one bucket:

- **In the dashboard**: a key, a form, a popup or a Commands-tab row does the
  same thing, through the same library function the CLI calls. Logic must not
  be duplicated.
- **Excluded**: `serve` (the dashboard is the running server, and its flags only apply at start) and `uninstall`. They get no dashboard entry, no Commands-tab row and no copy-the-command row. Plain `hashi ask` without a server is covered by the dashboard prompt popup.

Add a parity test that walks the clap command tree, compares it with a table
in the TUI code that maps every subcommand to its dashboard entry or to
"excluded", and fails when a new subcommand has no entry.

## Gaps to close

These are the expected gaps. The inventory may find more.

1. **Edit a token** (`token update`):
   - A form on the selected token: name, note, expiry (days from now, or
     never), limits (daily and monthly requests and budget, max pending), and
     the agent account lists (from the accounts feature).
   - Pause, resume and revoke stay on their keys.
2. **Audit** (`audit`): a scrollable audit list, either a section in Logs or
   a Commands row that opens a popup, whichever fits the current layout.
3. **Ask** (`ask`, local and `--remote`):
   - A prompt popup: choose the agent (and the account, for admins), type a
     multi-line prompt, and send.
   - It goes through the normal queue as the owner's admin token, so it shows
     in Requests like any other.
   - The answer streams into the request detail.
4. **Accounts** (`account add|login|list|disable|enable|remove`):
   - An Accounts section, either in the Commands tab or a popup listing
     accounts with their state, with the same actions.
   - Signing in uses the existing "step aside" terminal flow from the
     installer popup.
   - Adding a key account uses a masked input field, so the key is never
     shown.
5. **Doctor** (`doctor`): a status popup listing:
   - the agents found, with versions;
   - cloudflared and tailscale, with Tailscale's sign-in state;
   - the update state;
   - the home path and the resource guard settings.
6. **Report** (`report`): writes the redacted report into the Hashi home's
   `reports/` directory, or the current directory, matching the CLI's
   default. It shows the path, with `c` to copy it. `--include-prompts` is
   an explicit toggle in the popup, off by default.
7. **Update** (`update`, `update --check`):
   - Check now, and show the result.
   - Install runs through the step-aside flow, then tells the owner to quit
     and restart.
8. **Bridges I use** (`connect`, `remote list|remove`, `ask --remote`):
   - List saved remotes, and remove one.
   - Connect takes a pasted invite link.
   - Ask on a remote uses the prompt popup with a target selector.
10. **Port and bind address, live.**
    - A Commands-tab row opens a form with the port (1 to 65535) and the bind
      address. The bind choices are loopback `127.0.0.1`, all interfaces
      `0.0.0.0` or `::`, or a typed IP such as a Tailscale address.
    - **Applying:**
      1. Bind the new listener first. If that fails (port in use, address
         not on this machine, permission denied), show the error next to
         the field and keep serving on the old address.
      2. Move to the new listener with the same TLS settings, connection
         cap and limits, and update the URL in the header.
      3. Let the old listener finish its open requests (the existing
         10 second graceful drain), then close it.
    - Running agent requests are not interrupted: they live in the queue,
      not the listener.
    - Any active public route (a Cloudflare quick tunnel, or Tailscale serve
      or funnel) is restarted to point at the new local port. A named
      tunnel's public hostname cannot be retargeted by Hashi, so changing the
      port while one is configured warns that the Cloudflare dashboard
      hostname must be updated too.
    - Choosing a non-loopback bind without TLS asks for confirmation with
      the same warning the CLI prints at startup.
    - **Keeping the change:**
      - A "keep for next start" toggle (on by default) saves the choice in
        an owner-only settings file, `<home>/settings.json`.
      - At startup the order is: flag, then environment variable, then
        settings file, then default. When a flag or environment variable
        overrides the saved value, the form says so.
      - `hashi uninstall` treats the file as Hashi's own (add it to
        `is_hashi_entry`).

9. **Already there**: share, invites (list and cancel), new token, revoke,
   pause, copy URL, go public (Cloudflare and Tailscale), stop, install
   helpers, and help. Verify that each is reachable and that its
   Commands-tab row shows its CLI equivalent.

## Constraints

- Forms follow the existing share and token forms: field navigation, inline
  validation errors next to the field, and Esc to cancel.
- Secrets typed into forms are masked and never reach the log.
- Every Commands-tab row shows its CLI equivalent.
- Keys stay discoverable. The `?` help and footer hints are updated, and no
  two actions in the same view share a key.
- HASHI_REDUCED_MOTION and light and dark terminals keep working.
- Tests use the existing TUI test style (`tests/tui.rs`). They never run real
  agents, the network or real installers.
