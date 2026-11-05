# Performance: memory, startup and binary size

Measured for 0.3.0 on release builds, before (`4829217`) and after this pass. macOS is
an Apple Silicon laptop (10 cores, about 650 processes running); Linux is
`rust:1-bookworm` in Docker on the same machine. Every server ran with a
fresh temp `HASHI_HOME` and `HOME`, fake agent scripts for all six agents,
usage polling and the update check off. No real agent CLI ran.

## Results

| Measure | macOS before | macOS after | Linux before | Linux after |
|---|---|---|---|---|
| Binary size | 8.18 MB | 9.56 MB | 9.00 MB | 11.06 MB |
| Binary size, gzip -9 | 4.21 MB | 4.47 MB | 4.60 MB | 4.92 MB |
| First `/healthz`, restart | 24 ms | 12 ms | 3.5 ms | 2.7 ms |
| First `/healthz`, first start (creates the DB) | 40 ms | 22 ms | 28 ms | 35 ms |
| First `/healthz`, each `--version` takes 100 ms | 720 ms | 140 ms | 638 ms | 115 ms |
| Memory per held long poll, 30 KB prompt | 88.7 KiB | 24.5 KiB | 80 KiB | 16 to 30 KiB |
| Memory per open SSE stream, 30 KB prompt | 105.7 KiB | 73.3 KiB | 126 to 145 KiB | 78 to 85 KiB |
| Memory per held long poll, short prompt | 24.8 KiB | 24.5 KiB | 14.7 KiB | 17.6 KiB |
| Runner peak while capturing 10 MB of stdout | 23.4 MiB | 13.9 MiB | 20.3 MiB | 10.6 MiB |
| Watchdog sample, 300 to 650 processes (CPU) | 4.5 ms | 4.3 ms | 1.25 to 2.06 ms | 0.78 to 1.11 ms |
| RSS idle | 13.5 MiB | 13.4 MiB | 11.5 MiB | 11.7 MiB |
| RSS after 1000 completed requests | 17.1 MiB | 18.2 MiB | 13.8 MiB | 13.7 MiB |
| RSS after 1000, then 10k more requests | 16.8 to 19.3 MiB | 17.0 to 19.0 MiB | 13.6 to 15.7 MiB | 13.7 to 15.9 MiB |

Memory per held connection is live heap on macOS (`heap`) and RSS on Linux,
as the difference before and after opening 100 long polls or 50 streams,
divided by their number. Linux RSS is noisier, so it shows the range of two
runs. Idle and post-load RSS, and the Linux first start (which is mostly
creating and syncing the database), moved within run-to-run noise.

**Leaks.** None found. Over 10k completed requests after a 1000-request warm
up, RSS rose by 1 to 2.5 MiB in the first few thousand and then held flat on
Linux (15.7, 15.7, 15.9, 15.9 MiB at each 2000) and on macOS (18.8, 19.0,
19.0, 19.0 MiB), which is allocator warm-up,
not growth per request.

## What changed

1. **Agent version probes run together** (`src/server.rs`). Startup waited
   for each agent's `--version` in turn, so it cost the sum of their start
   times; it now costs the slowest. With real Node based CLIs (each 100 ms
   or more to start) this is most of startup: 720 ms became 140 ms with six
   agents at 100 ms each. On macOS the very first run of a new executable is
   also scanned by the system, which made a first start with new scripts
   386 ms before and 81 ms after. The results keep their order.
2. **Agent output is decoded in place** (`src/agent/runner.rs`).
   `String::from_utf8` reuses the capture buffer when the output is valid
   UTF-8 and only invalid output takes the lossy copy. The runner's peak for
   10 MB of stdout fell from 23 MiB to 14 MiB (macOS) and from 20 MiB to
   11 MiB (Linux). The whole request's peak for a 9 MB answer did not move
   (36.8 MiB footprint on macOS, 65.6 MiB VmHWM on Linux): it is set later,
   while the answer is parsed, stored and serialized, which is on the
   request path, not here.
3. **Long polls and streams do not hold a request copy while they wait**
   (`src/api/ask.rs`, `src/api/openai.rs`). Each held wait kept the request
   it had read, prompt included, until the wait ended and then read it
   again; `GET /v1/ask/{id}?wait=` held two copies. With 30 KB prompts a held
   long poll fell from 89 KiB to 25 KiB, the same as with a short prompt,
   and a stream from 106 KiB to 73 KiB (what remains is the queued request
   itself). The non-streaming chat completion also drops its body (the whole
   transcript) before waiting.
4. **The watchdog reads memory only for the run's processes**
   (`src/guard.rs`). See the sampler section.
5. **Release binaries keep their symbol names** (`Cargo.toml`). See below.

## Request path, database and dashboard

These were timed in process with the ignored benches below, on a seeded
store of 200 tokens, 10 projects, 30 accounts with usage and 20,000
finished requests (medians, before and after):

| Path | Before | After | Change |
|---|---|---|---|
| Token lookup on every authenticated request | 8.8 µs | 3.9 µs | prepared statements cached per connection (64 slots) |
| `GET /v1/ask/{id}` | 19.8 µs | 10.1 µs | the same |
| MCP `tools/list` | 28 µs | 22 µs | the same |
| `GET /v1/stats` | 0.50 ms | 0.05 ms | counts index pages and small status ranges instead of every entry |
| `GET /v1/projects` (10 projects) | 3.6 ms | 0.53 ms | schema 9 index on `(project, created_at, cost_usd)` |
| `GET /v1/accounts` (30 accounts) | 8.7 ms | 2.1 ms | one read of every token's account list instead of one per account |
| Dashboard refresh | 5.8 ms | 2.0 ms | store data read once per refresh into a frame cache, not once per frame |
| Accounts popup, one frame | 5.4 ms | 0.25 ms | the same |

The Tokens view also draws only the rows that fit, as the Requests view
does.

```sh
cargo test --release --test perf -- --ignored --nocapture
cargo test --release --test tui_bench -- --ignored --nocapture
```

`tests/perf.rs` never touches `~/.hashi`; set `HASHI_PERF_KEEP` to a file
path to keep the seeded database for `EXPLAIN QUERY PLAN`.

## Binary size and the release profile

The profile was already `lto = true` (fat), `codegen-units = 1`,
`panic = "abort"` and `strip = true`. `release.yml` builds with it as is.

`strip = true` removes the symbol table, and the crash report hook
(`src/diagnostics.rs`) prints `Backtrace::force_capture()`. A panic demo
with the same profile and hook printed `__mh_execute_header` for every frame
on macOS and `<unknown>` on Linux; with `strip = "debuginfo"` it printed
`handle_request`, `worker_loop` and `main`. A crash report without names
cannot be acted on, so the profile now uses `strip = "debuginfo"`. That
costs 1.4 MB raw and 0.25 MB gzipped on macOS, 2.1 MB raw and 0.32 MB
gzipped on Linux. Line numbers would need debug info and a much larger
binary, so they stay out. Windows is unchanged: its symbols live in a PDB,
which the release does not ship either way.

Other profile options, measured on macOS with the old code:

| Profile | Size | gzip -9 |
|---|---|---|
| fat LTO, `strip = true` (before) | 8,184,896 | 4,212,653 |
| fat LTO, `strip = "debuginfo"` (after) | 9,544,464 | 4,461,281 |
| thin LTO, `strip = "debuginfo"` | 10,333,360 | 4,640,007 |
| fat LTO, no strip | 10,058,184 | 4,537,963 |

Thin LTO is 8 % larger with no runtime gain to show for it, so fat stays.
`panic = "abort"` stays: the hook still runs before the abort, so the crash
report is written.

**Dependency features.** Replacing tokio's `full` with the features Hashi
uses (`fs`, `io-util`, `macros`, `net`, `process`, `rt-multi-thread`,
`signal`, `sync`, `time`) changed the binary by 192 bytes, larger, and
`parking_lot` stays in the tree through crossterm. Unused code is already
dropped by LTO, so nothing was changed.

## The watchdog sampler

Every run is sampled every 2 s (`guard::SAMPLE_EVERY`). A sample has to list
every process on the machine: a run's processes are found by parent, and
also by the process groups and sessions recorded for it, so a descendant
that called `setsid` and lost its parent is still found. sysinfo cannot
narrow that list, so refreshing only known pids would miss new descendants.
What it can narrow is what it reads: the sample used to read memory for
every process, and now lists them with nothing extra, then reads memory
only for the run's members. On Linux, where memory is a separate read per
process, a sample with 300 processes fell from 1.25 to 2.06 ms of CPU to
0.78 to 1.11 ms. On macOS one call returns both, and a sample stays about
4.5 ms with 650 processes. That is 0.2 % of one core per running run.
Threads are still skipped (`without_tasks`).

## Looked at, left alone

- **Usage poller** (`src/usage/poller.rs`): it wakes every 15 s, lists the
  eligible accounts and reads only the due ones. Its allocations per tick
  are a few small vectors; nothing to measure against. Its after-run channel
  is unbounded but carries one small key per finished run and is drained on
  every tick; bounding it would mean dropping refresh hints.
- **Maintenance** (`src/maintenance.rs`): one pass an hour on a blocking
  thread. Nothing to gain.
- **Startup work**: with the probes concurrent, a restart answers in 12 ms
  (macOS) and 3 ms (Linux); the rest is opening the database. The resource
  probe already measures nothing until asked.
- **Per-connection cost**: about 15 to 25 KiB per held connection with a
  short prompt is hyper's connection state and task, within the 512
  connection cap (about 12 MiB at most).

## Reproducing

Build a release binary of each version, then run the suite on each:

```sh
cargo build --release --locked
sh scripts/perf/suite.sh target/release/hashi
```

`scripts/perf/bench.py` starts each server with a temp `HASHI_HOME`, `HOME`
and `PATH`, writes fake agent scripts (the shape of `write_script` in
`tests/api.rs`) and points every `HASHI_<AGENT>_BIN` at them. It needs
Python 3 and, on macOS, `heap` (part of the system). Its docstring lists
the scenarios and knobs.

For Linux, export the source (never `target/`) and run in Docker:

```sh
mkdir -p /tmp/hashi-src && git archive HEAD | tar -x -C /tmp/hashi-src
docker run --rm -v /tmp/hashi-src:/src -v hashi-target:/target \
  -w /src -e CARGO_TARGET_DIR=/target rust:1-bookworm \
  sh -c 'cargo build --release --locked && sh scripts/perf/suite.sh /target/release/hashi'
```

The micro benches for the runner and the sampler build on their own:

```sh
cd scripts/perf/micro
cp ../../../Cargo.lock .
cargo run --release --bin runner -- 10000000
cargo run --release --bin sampler -- 200
```

Point the `hashi` path in `scripts/perf/micro/Cargo.toml` at a checkout of
the earlier commit to get the before numbers.
