#!/bin/sh
# Weekly upstream check for the agent CLIs Hashi drives.
#
# Reads docs/research/verified-versions.json, looks up each agent's latest
# release and, with --report, opens or updates one GitHub issue per agent
# that is newer than the verified version. A newer upstream never fails:
# the exit status is non-zero only when a lookup itself failed.
#
# Usage:
#   upstream-check.sh check [--report] [JSON]
#   upstream-check.sh list [JSON]
#   upstream-check.sh normalize VERSION
#   upstream-check.sh compare VERIFIED LATEST       prints newer, same or older
#   upstream-check.sh parse-github [TAG_PATTERN]    stdin: gh api .../releases
#   upstream-check.sh parse-npm                     stdin: npm view PKG version
#   upstream-check.sh parse-cursor                  stdin: the install script text
#   upstream-check.sh issue-body AGENT LATEST URL [JSON]
#
# The commands it runs can be replaced for tests: GH (default gh), NPM
# (default npm) and CURL (default curl). Cursor's installer is only fetched
# and searched as text; it is never run.

set -eu

GH=${GH:-gh}
NPM=${NPM:-npm}
CURL=${CURL:-curl}
here=$(cd "$(dirname "$0")" && pwd)
default_json="$here/../docs/research/verified-versions.json"
sep=$(printf '\037')

die() {
    printf 'upstream-check: %s\n' "$*" >&2
    exit 2
}

# Strips a tag prefix such as `v` or `rust-v`: rust-v0.166.0 -> 0.166.0.
normalize() {
    printf '%s\n' "$1" | sed 's/^[^0-9]*//'
}

# Compares the numeric part of two versions, ignoring a pre-release or
# build suffix (Cursor's commit hash). 2026.11.05-86047df compares by date.
compare() {
    a=$(normalize "$1")
    b=$(normalize "$2")
    awk -v a="$a" -v b="$b" 'BEGIN {
        sub(/[-+].*/, "", a); sub(/[-+].*/, "", b)
        na = split(a, x, "."); nb = split(b, y, ".")
        n = na > nb ? na : nb
        for (i = 1; i <= n; i++) {
            p = (i <= na) ? x[i] + 0 : 0
            q = (i <= nb) ? y[i] + 0 : 0
            if (q > p) { print "newer"; exit }
            if (q < p) { print "older"; exit }
        }
        print "same"
    }'
}

# stdin: the JSON array from `gh api repos/OWNER/REPO/releases`.
# Prints `version<US>tag<US>url` for the newest published release whose tag
# matches the pattern. Drafts and pre-releases are skipped.
parse_github() {
    jq -r --arg pat "${1:-.}" --arg sep "$sep" '
        [ .[] | select((.draft | not) and (.prerelease | not))
              | select(.tag_name | test($pat)) ]
        | first // empty
        | [.tag_name, .html_url] | join($sep)' |
        while IFS="$sep" read -r tag url; do
            printf '%s%s%s%s%s\n' "$(normalize "$tag")" "$sep" "$tag" "$sep" "$url"
        done
}

# stdin: `npm view PKG version` (plain or --json). Prints the version.
parse_npm() {
    tr -d '"[:space:]' | grep -E '^[0-9]+\.[0-9]+' || return 1
}

# stdin: the text of https://cursor.com/install. Prints the version in the
# package download URL, such as 2026.11.05-86047df.
parse_cursor() {
    grep -Eo 'downloads\.cursor\.com/[A-Za-z0-9_/.-]*[0-9]{4}\.[0-9]{2}\.[0-9]{2}-[0-9a-f]+' |
        grep -Eo '[0-9]{4}\.[0-9]{2}\.[0-9]{2}-[0-9a-f]+' | head -n 1 | grep . || return 1
}

# Prints one `agent<US>name<US>source<US>repo<US>version<US>tag_pattern` line
# per agent.
agents() {
    jq -r --arg sep "$sep" '.agents | to_entries[]
        | [.key, .value.name, .value.source, (.value.repo // ""),
           .value.version, (.value.tag_pattern // "")] | join($sep)' "$1"
}

# Prints `version<US>tag<US>url` for an agent's latest release.
latest() {
    source=$1 repo=$2 pattern=$3
    case $source in
        github:*)
            "$GH" api "repos/${source#github:}/releases?per_page=50" </dev/null |
                parse_github "${pattern:-.}" | grep . || return 1
            ;;
        npm:*)
            pkg=${source#npm:}
            v=$("$NPM" view "$pkg" version </dev/null | parse_npm) || return 1
            if [ -n "$repo" ]; then
                url="https://github.com/$repo/blob/main/CHANGELOG.md"
            else
                url="https://www.npmjs.com/package/$pkg/v/$v"
            fi
            printf '%s%s%s%s%s\n' "$v" "$sep" "$v" "$sep" "$url"
            ;;
        installer:*)
            # Fetched as text and searched; never executed.
            v=$("$CURL" -fsSL --max-time 30 "${source#installer:}" </dev/null | parse_cursor) ||
                return 1
            printf '%s%s%s%s%s\n' "$v" "$sep" "$v" "$sep" "https://cursor.com/changelog"
            ;;
        *) return 1 ;;
    esac
}

# The research-notes heading for an agent, as a GitHub anchor.
research_anchor() {
    case $1 in
        opencode) echo "1-opencode" ;;
        gemini) echo "2-gemini-cli" ;;
        qwen) echo "3-qwen-code" ;;
        cursor) echo "4-cursor-cli" ;;
        crush) echo "7-crush" ;;
        *) echo "flags-hashi-pins-for-shared-tokens" ;;
    esac
}

issue_body() {
    agent=$1 new=$2 url=$3 json=$4
    name=$(jq -r --arg a "$agent" '.agents[$a].name' "$json")
    old=$(jq -r --arg a "$agent" '.agents[$a].version' "$json")
    verified=$(jq -r --arg a "$agent" '.agents[$a].verified' "$json")
    repo_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-sidharthpunathil/hashi}"
    cat <<EOF
$name $new is out. Hashi was last verified with $old ($verified).

Release notes: $url

A newer release usually works. This issue is a reminder to re-check the
behaviour Hashi depends on before bumping the verified version.

## Hashi files that depend on $name

- \`src/agent/$agent.rs\`: arguments, environment and output parsing
- \`src/agent/limit.rs\`: usage limit messages
- \`src/accounts.rs\`: account environment variables
- [\`docs/research/agent-clis.md\`]($repo_url/blob/main/docs/research/agent-clis.md#$(research_anchor "$agent")): research notes

## Re-verify checklist

From CONTRIBUTING.md, "Re-verifying an agent":

- [ ] Fetch the new source with opensrc (for a closed-source CLI, read the release notes and docs).
- [ ] Re-check the research notes for $name: headless flags, environment variables, account isolation and limit messages.
- [ ] Run \`cargo test --locked\`.
- [ ] Bump \`$agent\` in \`docs/research/verified-versions.json\` and the README "Verified with" column.
- [ ] Close this issue.

_Opened by the weekly upstream check (.github/workflows/upstream.yml)._
EOF
}

# Opens an issue for the agent, or updates the open one titled
# "Upstream: <Name> <old> to <new>" for any versions.
report() {
    agent=$1 name=$2 old=$3 new=$4 url=$5 json=$6
    title="Upstream: $name $old to $new"
    body=$(mktemp)
    issue_body "$agent" "$new" "$url" "$json" >"$body"
    existing=$("$GH" issue list --state open --limit 200 --json number,title </dev/null |
        jq -r --arg p "Upstream: $name " \
            '[.[] | select(.title | startswith($p) and (.[($p | length):] | test("^[^ ]+ to [^ ]+$")))]
            | first | .number // empty')
    if [ -n "$existing" ]; then
        "$GH" issue edit "$existing" --title "$title" --body-file "$body" </dev/null >/dev/null
        echo "  updated issue #$existing: $title"
    else
        "$GH" issue create --title "$title" --label upstream --body-file "$body" </dev/null >/dev/null
        echo "  opened issue: $title"
    fi
    rm -f "$body"
}

check() {
    do_report=0
    json=$default_json
    for arg in "$@"; do
        case $arg in
            --report) do_report=1 ;;
            *) json=$arg ;;
        esac
    done
    [ -f "$json" ] || die "no such file: $json"

    list=$(mktemp)
    agents "$json" >"$list"
    failed=0 labelled=0
    while IFS="$sep" read -r agent name source repo version pattern <&3; do
        if ! found=$(latest "$source" "$repo" "$pattern"); then
            echo "::warning::$name: could not look up the latest release ($source)"
            failed=1
            continue
        fi
        new=${found%%"$sep"*}
        url=${found##*"$sep"}
        state=$(compare "$version" "$new")
        echo "$agent $version $new $state"
        if [ "$state" = newer ] && [ "$do_report" = 1 ]; then
            if [ "$labelled" = 0 ]; then
                "$GH" label create upstream --color 5319e7 \
                    --description "A newer release of an agent CLI Hashi drives" --force \
                    </dev/null >/dev/null
                labelled=1
            fi
            report "$agent" "$name" "$version" "$new" "$url" "$json"
        fi
    done 3<"$list"
    rm -f "$list"
    return "$failed"
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case $cmd in
    check) check "$@" ;;
    list) agents "${1:-$default_json}" | tr "$sep" '|' ;;
    normalize) normalize "${1:?version}" ;;
    compare) compare "${1:?verified}" "${2:?latest}" ;;
    parse-github) parse_github "${1:-.}" | tr "$sep" '|' ;;
    parse-npm) parse_npm ;;
    parse-cursor) parse_cursor ;;
    issue-body) issue_body "${1:?agent}" "${2:?latest}" "${3:?url}" "${4:-$default_json}" ;;
    *) die "usage: upstream-check.sh check [--report] [JSON] (see the header for more)" ;;
esac
