# `hashi uninstall`: design

## Intent

One command removes everything Hashi put on the machine, so trying it out is
safe to undo. It must never delete anything Hashi did not create.

## Behaviour

`hashi uninstall [--keep-data] [--yes] [--dry-run]`

1. **Inventory first.** List each item with its path and size, then act:
   - the running binary (`std::env::current_exe()`, canonicalised), plus a
     leftover `hashi.old.exe` from `hashi update` next to it;
   - the Hashi home (`--home` / `HASHI_HOME`, default `~/.hashi`): database,
     logs, crash files, workspaces, `remotes.json`, the update cache and the
     `public-url` file;
   - on Windows, the user PATH entry that `install.ps1` added (read
     install.ps1 for the exact directory), removed only if it matches that
     directory exactly.
2. **Refuse while a server runs.** The server writes `<home>/server.pid`
   (owner only) on start and removes it on shutdown. Uninstall refuses when
   that pid is alive and is a hashi process (check with sysinfo), and says to
   quit the dashboard or stop the service first. A stale file is ignored.
3. **Safety checks on the home.** Delete it only when it is a real directory
   (not a symlink) and looks like a Hashi home: it contains `hashi.db`, or it
   is the default `~/.hashi`. Never delete `/`, the user's home directory,
   or a directory with unrelated content. Otherwise list what was found and
   stop. Walk the tree without following symlinks, the same way the
   workspace removal in `src/server.rs` does.
4. **Binary.**
   - Installed by Cargo (under `.cargo/bin`): do not delete it. Print
     `cargo uninstall hashi`.
   - Unix: delete the file.
   - Windows: the running exe cannot delete itself, so use a vetted crate
     such as `self-replace` (self_delete) rather than hand-written tricks,
     provided its licence passes deny.toml.
5. **Confirm.** On a terminal, show the inventory and ask the user to type
   `uninstall`. `--yes` skips the prompt. Without a terminal and without
   `--yes`, refuse. `--dry-run` prints the inventory and exits.
   `--keep-data` removes only the binary and the PATH entry.
6. **Afterwards.** Print what was removed, and what Hashi leaves alone with
   the command to remove each:
   - cloudflared and tailscale, if installed (`brew uninstall cloudflared`
     and the like, matching what `hashi install` would have used);
   - MCP entries in the user's own tools for each saved remote
     (`claude mcp remove <name>`, `codex mcp remove <name>`), read from
     `remotes.json` before deleting it;
   - the tokens on other people's bridges, which only their owners can
     revoke.

## Anti-goals

- Deleting anything outside the inventory, or following symlinks.
- Touching Tailscale or Cloudflare state, or other programs' configs.
- Any test that deletes a real path outside a temp dir.
