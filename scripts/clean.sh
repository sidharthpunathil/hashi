#!/bin/sh
# Frees the disk space this repository's builds take.
#
#   scripts/clean.sh                 show what would be removed and its size
#   scripts/clean.sh --yes           remove it
#   scripts/clean.sh --yes --no-docker --no-worktrees
#
# It removes:
#   - build output: target/ in this checkout and in every git worktree of it,
#     and scripts/perf/micro/target/ (Cargo rebuilds it on the next build);
#   - the Docker volumes the e2e run uses (hashi-target, hashi-cargo-registry),
#     unless a container still uses them;
#   - linked worktrees with nothing uncommitted and nothing unpushed. Their
#     branches stay, and the main checkout is never removed.
#
# Source, git history, ~/.hashi and every other project are left alone.

set -eu

yes=0
docker=1
worktrees=1
for arg in "$@"; do
    case "$arg" in
        --yes | -y) yes=1 ;;
        --no-docker) docker=0 ;;
        --no-worktrees) worktrees=0 ;;
        -h | --help)
            sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg (see --help)" >&2
            exit 2
            ;;
    esac
done

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
main=$(git -C "$root" worktree list --porcelain | sed -n '1s/^worktree //p')

size() { du -sh "$1" 2>/dev/null | cut -f1 | tr -d ' '; }

if [ "$yes" = 1 ]; then
    verb="removed"
else
    verb="would remove"
fi

# A cargo build writing into target/ while it is removed fails confusingly.
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
    if [ "$yes" = 1 ]; then
        echo "cargo or rustc is running; wait for it to finish, then run this again" >&2
        exit 1
    fi
    echo "note: cargo or rustc is running; --yes waits for nothing and refuses"
fi

remove_dir() {
    [ -d "$1" ] || return 0
    echo "  $verb $1 ($(size "$1"))"
    if [ "$yes" = 1 ]; then
        rm -rf "$1"
    fi
}

# A worktree can go when nothing in it would be lost.
removable() {
    wt=$1
    [ "$wt" != "$main" ] || return 1
    [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || return 1
    branch=$(git -C "$wt" symbolic-ref --quiet --short HEAD 2>/dev/null) || return 1
    upstream=$(git -C "$wt" rev-parse --abbrev-ref "$branch@{upstream}" 2>/dev/null) || return 1
    [ "$(git -C "$wt" rev-list --count "$upstream..$branch")" = 0 ]
}

echo "build output"
git -C "$root" worktree list --porcelain | sed -n 's/^worktree //p' | while read -r wt; do
    remove_dir "$wt/target"
    remove_dir "$wt/scripts/perf/micro/target"
done

if [ "$worktrees" = 1 ]; then
    echo "worktrees"
    git -C "$root" worktree list --porcelain | sed -n 's/^worktree //p' | while read -r wt; do
        [ "$wt" != "$main" ] || continue
        if removable "$wt"; then
            echo "  $verb $wt (clean and pushed; its branch stays)"
            if [ "$yes" = 1 ]; then
                git -C "$main" worktree remove "$wt"
            fi
        else
            echo "  kept $wt: it has uncommitted or unpushed work"
        fi
    done
    if [ "$yes" = 1 ]; then
        git -C "$main" worktree prune
    fi
fi

if [ "$docker" = 1 ] && command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    echo "docker"
    for vol in hashi-target hashi-cargo-registry; do
        docker volume inspect "$vol" >/dev/null 2>&1 || continue
        if [ -n "$(docker ps -aq --filter "volume=$vol")" ]; then
            echo "  kept volume $vol: a container uses it"
            continue
        fi
        echo "  $verb volume $vol"
        if [ "$yes" = 1 ]; then
            docker volume rm "$vol" >/dev/null
        fi
    done
fi

if [ "$yes" = 0 ]; then
    echo
    echo "nothing was removed; run with --yes to remove it"
fi
