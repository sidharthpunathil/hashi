#!/bin/sh
# Fixture tests for scripts/upstream-check.sh. No network: gh, npm and curl
# are replaced by fakes that print canned outputs from
# tests/fixtures/upstream/ and record what they were asked to do.
#
# Usage: sh scripts/test-upstream-check.sh

# Backticks in expected issue text are literal.
# shellcheck disable=SC2016

set -u

here=$(cd "$(dirname "$0")" && pwd)
script="$here/upstream-check.sh"
FIX="$here/../tests/fixtures/upstream"
export FIX
fails=0
passes=0

ok() { passes=$((passes + 1)); }
bad() {
    fails=$((fails + 1))
    printf 'FAIL: %s\n' "$*"
}
eq() {
    # eq NAME EXPECTED ACTUAL
    if [ "$2" = "$3" ]; then ok; else bad "$1: expected [$2], got [$3]"; fi
}
has() {
    # has NAME NEEDLE HAYSTACK
    case $3 in
        *"$2"*) ok ;;
        *) bad "$1: [$2] not found in [$3]" ;;
    esac
}
lacks() {
    case $3 in
        *"$2"*) bad "$1: [$2] unexpectedly found" ;;
        *) ok ;;
    esac
}

run() { sh "$script" "$@"; }

# normalize and compare
eq "normalize codex tag" "0.166.0" "$(run normalize rust-v0.166.0)"
eq "normalize v tag" "0.67.0" "$(run normalize v0.67.0)"
eq "normalize cursor" "2026.11.05-86047df" "$(run normalize 2026.11.05-86047df)"
eq "compare newer minor" newer "$(run compare 0.166.0 0.170.1)"
eq "compare newer patch" newer "$(run compare 2.1.318 2.1.319)"
eq "compare same through prefix" same "$(run compare 0.166.0 rust-v0.166.0)"
eq "compare older" older "$(run compare 0.67.0 0.66.9)"
eq "compare numeric not lexical" newer "$(run compare 0.9.0 0.10.0)"
eq "compare cursor by date" newer "$(run compare 2026.11.05-86047df 2026.11.20-3f9c2ab)"
eq "compare cursor same date" same "$(run compare 2026.11.05-86047df 2026.11.05-ffffff0)"
eq "compare cursor older" older "$(run compare 2026.11.05-86047df 2026.10.31-0000000)"
eq "compare ignores prerelease suffix" same "$(run compare 0.68.0 0.68.0-nightly.1)"

# gh api releases
eq "github skips drafts and prereleases" \
    "0.170.1|rust-v0.170.1|https://github.com/openai/codex/releases/tag/rust-v0.170.1" \
    "$(run parse-github <"$FIX/gh-releases-codex.json")"
eq "github tag_pattern filters other packages" \
    "0.27.0|v0.27.0|https://github.com/QwenLM/qwen-code/releases/tag/v0.27.0" \
    "$(run parse-github '^v[0-9]' <"$FIX/gh-releases-qwen.json")"
has "github without pattern takes the newest" "sdk-typescript-v0.3.0" \
    "$(run parse-github <"$FIX/gh-releases-qwen.json")"
eq "github no releases" "" "$(run parse-github <"$FIX/gh-releases-empty.json")"

# npm view
eq "npm plain" "2.1.318" "$(run parse-npm <"$FIX/npm-view.txt")"
eq "npm json" "2.2.0" "$(run parse-npm <"$FIX/npm-view-json.txt")"
if printf 'npm ERR! 404\n' | run parse-npm >/dev/null; then
    bad "npm error output parsed as a version"
else ok; fi

# Cursor install script (text only)
eq "cursor package url" "2026.11.20-3f9c2ab" "$(run parse-cursor <"$FIX/cursor-install.sh")"
if run parse-cursor <"$FIX/cursor-install-no-url.sh" >/dev/null; then
    bad "cursor script without a package url parsed"
else ok; fi

# The committed JSON has a known source type for every agent.
real=$(run list)
eq "real json lists seven agents" 7 "$(printf '%s\n' "$real" | grep -c .)"
for a in claude codex gemini opencode qwen cursor crush; do
    has "real json has $a" "$a|" "$real"
done
eq "real json sources are known" "" \
    "$(printf '%s\n' "$real" | cut -d'|' -f3 | grep -Ev '^(npm|github|installer):' || true)"

# Issue body
body=$(run issue-body codex 0.170.1 https://example.test/notes "$FIX/verified-versions.json")
has "body names versions" "Codex 0.170.1 is out. Hashi was last verified with 0.166.0 (2026-11-06)." "$body"
has "body links notes" "Release notes: https://example.test/notes" "$body"
has "body agent file" '`src/agent/codex.rs`' "$body"
has "body limit patterns" '`src/agent/limit.rs`' "$body"
has "body account env" '`src/accounts.rs`' "$body"
has "body research section" "agent-clis.md#flags-hashi-pins-for-shared-tokens" "$body"
has "body checklist" "- [ ] Bump \`codex\` in \`docs/research/verified-versions.json\`" "$body"
body=$(run issue-body qwen 0.27.0 u "$FIX/verified-versions.json")
has "qwen research anchor" "agent-clis.md#3-qwen-code" "$body"

# End to end with fake gh, npm and curl.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
FAKE_DIR=$work
export FAKE_DIR
cat >"$work/gh" <<'EOF'
#!/bin/sh
echo "gh $*" >>"$FAKE_DIR/log"
case "$1 $2" in
    "api repos/openai/codex/"*) cat "$FIX/gh-releases-codex.json" ;;
    "api repos/google-gemini/gemini-cli/"*) cat "$FIX/gh-releases-gemini.json" ;;
    "api repos/QwenLM/qwen-code/"*) cat "$FIX/gh-releases-qwen.json" ;;
    "issue list") cat "$FIX/gh-issues.json" ;;
    "issue create" | "issue edit")
        while [ $# -gt 0 ]; do
            if [ "$1" = --body-file ]; then cat "$2" >>"$FAKE_DIR/bodies"; fi
            shift
        done
        ;;
    "label create") ;;
    *) echo "unexpected gh call" >&2; exit 1 ;;
esac
EOF
cat >"$work/npm" <<'EOF'
#!/bin/sh
echo "npm $*" >>"$FAKE_DIR/log"
[ -n "${FAKE_NPM_FAIL:-}" ] && { echo "npm ERR! network" >&2; exit 1; }
cat "$FIX/npm-view.txt"
EOF
cat >"$work/curl" <<'EOF'
#!/bin/sh
echo "curl $*" >>"$FAKE_DIR/log"
cat "$FIX/cursor-install.sh"
EOF
chmod +x "$work/gh" "$work/npm" "$work/curl"

out=$(GH="$work/gh" NPM="$work/npm" CURL="$work/curl" \
    sh "$script" check --report "$FIX/verified-versions.json")
status=$?
log=$(cat "$work/log")
eq "check exits 0" 0 "$status"
has "claude same" "claude 2.1.318 2.1.318 same" "$out"
has "codex newer" "codex 0.166.0 0.170.1 newer" "$out"
has "gemini same" "gemini 0.67.0 0.67.0 same" "$out"
has "qwen newer" "qwen 0.26.8 0.27.0 newer" "$out"
has "cursor newer" "cursor 2026.11.05-86047df 2026.11.20-3f9c2ab newer" "$out"
has "npm asked for the version" "npm view @anthropic-ai/claude-code version" "$log"
has "cursor installer fetched" "curl -fsSL --max-time 30 https://cursor.com/install" "$log"
has "label created" "gh label create upstream" "$log"
eq "label created once" 1 "$(grep -c 'label create' "$work/log")"
has "existing codex issue updated" "gh issue edit 41 --title Upstream: Codex 0.166.0 to 0.170.1" "$log"
has "qwen issue opened" "gh issue create --title Upstream: Qwen Code 0.26.8 to 0.27.0 --label upstream" "$log"
has "cursor issue opened" "gh issue create --title Upstream: Cursor CLI 2026.11.05-86047df to 2026.11.20-3f9c2ab" "$log"
lacks "no issue for an agent that is current" "Upstream: Gemini CLI" "$log"
lacks "no issue for claude" "Upstream: Claude Code" "$log"
lacks "different prefix untouched" "issue edit 7" "$log"
has "bodies written" "Qwen Code 0.27.0 is out." "$(cat "$work/bodies")"

# Without --report nothing is written to GitHub.
rm -f "$work/log"
GH="$work/gh" NPM="$work/npm" CURL="$work/curl" \
    sh "$script" check "$FIX/verified-versions.json" >/dev/null
lacks "dry run opens nothing" "issue" "$(cat "$work/log")"

# A failed lookup warns, carries on, and fails the run.
rm -f "$work/log"
out=$(FAKE_NPM_FAIL=1 GH="$work/gh" NPM="$work/npm" CURL="$work/curl" \
    sh "$script" check "$FIX/verified-versions.json" 2>/dev/null)
status=$?
eq "failed lookup exits 1" 1 "$status"
has "failed lookup warns" "::warning::Claude Code: could not look up" "$out"
has "later agents still checked" "cursor 2026.11.05-86047df" "$out"

printf '%s passed, %s failed\n' "$passes" "$fails"
[ "$fails" = 0 ]
