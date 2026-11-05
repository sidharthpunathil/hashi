# Resource guard: design

## Intent

Hashi runs heavyweight agent processes on the owner's own computer, and with
sharing, people other than the owner decide when they start. A Claude Code or
Codex run is a Node or native process that can take hundreds of megabytes to
several gigabytes of memory and spawn its own children (shells, language
servers, MCP servers). The owner's machine must stay usable no matter what a
shared token does. Hashi should know how much memory the machine has, keep a
reserve for the owner, and never start more work than fits.

## Abuse and accident scenarios

| # | Scenario | Today | Outcome without a guard |
|---|---|---|---|
| 1 | One sharer queues prompts for every agent at once | each agent has its own workers, so up to 6 runs at once (12 with `--concurrency 2`) | memory exhaustion, swapping, the owner's apps killed |
| 2 | Several sharers, each within their own `maxPending` | caps are per token only | same as 1, summed across tokens |
| 3 | A single run balloons (a large repo, a runaway tool loop) | only time and output-size limits | one process eats all memory |
| 4 | A run is killed on timeout but its children survive | `kill_on_drop` kills the direct child only | orphaned shells and servers keep using memory and CPU |
| 5 | The machine is already busy (the owner's own builds, a browser) | Hashi starts runs regardless | Hashi pushes the machine over the edge |
| 6 | Disk fills (responses in SQLite, logs, workspaces) | retention prunes requests; logs rotate | writes fail, the database cannot commit |
| 7 | Agent runs hog CPU | normal priority | the owner's machine feels slow |

## Goals

1. A **global cap on concurrent runs** across all agents, defaulting from the
   machine's memory and CPU count.
2. **Admission control by memory**: before starting a run, check available
   memory against a reserve kept for the owner; if short, leave the request
   queued and retry, instead of starting it.
3. **Per-run memory ceiling**: sample the run's whole process tree and stop it
   when it exceeds the ceiling, recording a clear error.
4. **No orphans**: start each run in its own process group (Unix) and kill the
   whole group on timeout, cancel, shutdown or memory stop; on Windows kill
   the process tree.
5. **Disk floor**: refuse to start runs when free disk space under the Hashi
   home is below a floor.
6. **Lower priority** for agent runs on Unix, so the owner's own work wins.
7. **Visibility**: the dashboard and admin stats show memory, running and
   waiting counts, and why work is held back.

## Non-goals

- Containers, VMs or cgroups management.
- Per-token memory quotas (the global guard protects the machine; per-token
  caps already bound fairness).

## Anti-goals

- Exposing machine memory figures to non-admin tokens or unauthenticated
  callers (it fingerprints the host).
- Any `unsafe` code. Use safe crates or child-process helpers.
- Dropping queued work: held requests wait and run later, they are not failed
  unless their own run exceeds a limit.

## Design

### Settings (flag, env, default)

| Setting | Default |
|---|---|
| `--max-running` / `HASHI_MAX_RUNNING` | `clamp(total_memory_gib / 8, 1, cpu_count)`, at least 1 |
| `--memory-reserve` / `HASHI_MEMORY_RESERVE` | `max(2 GiB, 20% of total)` kept free for the owner |
| `--run-memory-limit` / `HASHI_RUN_MEMORY_LIMIT` | `max(2 GiB, 40% of total)` per run, process tree |
| `--disk-floor` / `HASHI_DISK_FLOOR` | `1 GiB` free under the Hashi home |
| `--nice` / `HASHI_NICE` | `10` on Unix, `0` disables |

Sizes accept `512M`, `4G`, `25%`.

### Admission

A shared `ResourceGuard` owns a semaphore with `max_running` permits. A worker
that has claimed nothing yet asks the guard for a permit *before* claiming a
request, so requests stay `queued` (and keep their queue position) while the
machine is full. With a permit, it checks available memory and disk; if either
is short, it releases the permit, publishes a "held: low memory" event at most
once per minute, and waits a few seconds before trying again.

### Watchdog

While a run is active, a task samples the run's process tree memory every
2 seconds (the child and all descendants). Above the per-run limit, it kills
the group and the request fails with `run exceeded its memory limit (X of Y)`.

### Process groups

Each child is started with its own process group (`process_group(0)`, a safe
std API). Kill paths (timeout, memory stop, cancel, shutdown) signal the whole
group. On Windows, `taskkill /T /F /PID <pid>` ends the tree.

### Visibility

`GET /v1/stats` (admin) adds `resources: {memoryTotal, memoryAvailable,
reserve, running, maxRunning, held, diskFree}`. The dashboard Overview shows a
memory gauge and "held: low memory" when admission is waiting.

## Calls made by the builder

- Memory figures come from a cross-platform system-information crate.
- Defaults above are starting points chosen to leave the machine usable on a
  16 GB laptop (max 2 concurrent runs, 3.2 GB reserve).
