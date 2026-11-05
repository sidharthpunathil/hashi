# Upstream tracking: design

## Intent

Hashi depends on details of seven CLIs it does not control: flags, env vars,
output formats and limit messages. When one of them releases, the owner
should find out, see which Hashi behaviour might be affected, and see at a
glance in the README what each agent was last verified against.

## 1. Single source of truth

`docs/research/verified-versions.json` (already committed) records, per
agent, the upstream source, the last verified version and tag, and the date.
It is embedded in the binary with `include_str!` and parsed at build time
(through a const or a lazy parse that is tested).

## 2. README

The Supported agents table gets a "Verified with" column, for example
`0.166.0 (2026-11-06)`. A test fails when that column disagrees with the
JSON, so the two cannot drift. A sentence under the table explains what
"verified" means and links to the research notes.

## 3. Weekly upstream check (GitHub Actions)

- **Workflow.** `.github/workflows/upstream.yml` runs on a weekly schedule
  and on `workflow_dispatch`.
- **Hardening, same as the existing workflows:**
  - actions pinned by SHA;
  - `permissions: contents: read, issues: write` at job level only;
  - `persist-credentials: false`.
- **Steps:**
  1. Read the JSON.
  2. For each agent, look up the latest release:
     - GitHub releases through `gh api`, filtered by `tag_pattern` when set;
     - `npm view <pkg> version` for npm sources;
     - for Cursor, fetch `https://cursor.com/install` and extract the
       version in the package URL. Only grep the script text, never run it.
  3. Compare the latest release with `version`.
- **Reporting, one issue per agent:**
  - When an agent is newer, open a GitHub issue titled "Upstream: <Name>
    <old> to <new>", or update the existing open one with that title prefix.
  - The issue body links the release notes or changelog, and lists the
    Hashi files that depend on that agent: `src/agent/<agent>.rs`, the limit
    patterns, the account env vars and the research section.
  - It includes the re-verify checklist from docs/research/agent-clis.md.
  - A newer upstream never fails CI. It only reports.
- **Labels.** The issue uses an `upstream` label, which the workflow creates
  if it is missing.

## 4. Runtime warning

- `hashi doctor` and agent discovery compare each installed agent's version
  (already detected) with the verified version, parsing loose semver.
- **When the installed version is newer** (compared by major and minor only;
  a newer patch is fine), doctor prints:
  > `codex 0.170.1 is newer than the version Hashi was verified with
  > (0.166.0). If runs fail, run hashi report and check for an update with
  > hashi update --check.`
- The dashboard agent card shows a small "newer than verified" tag, and
  `/v1/stats` (admin) includes it.
- **When the installed version is older**, Hashi only enforces the minimums
  it already has (opencode 2.2 and others). For unknown versions it says
  nothing.
- This never blocks runs. Crush is the exception: it keeps its own stricter
  version gate.

## 5. Release checklist

`CONTRIBUTING.md` gains a "Re-verifying an agent" section:

1. Fetch the new source with opensrc.
2. Re-check the research notes for that agent.
3. Run the tests.
4. Bump the JSON entry and the README column.
5. Close the upstream issue.

## Anti-goals

- Auto-bumping the verified version without a human check.
- Network access in tests. The workflow script's parsing is unit-testable
  with canned API responses: write it as a small Rust xtask binary or a
  POSIX shell script with fixture tests, whichever fits the repo.
