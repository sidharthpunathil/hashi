#!/bin/sh
# Hashi installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | sh
#
# Options (environment variables):
#   HASHI_VERSION=v0.3.1        install a specific release (default: latest)
#   HASHI_INSTALL_DIR=~/bin     where to put the binary (default: ~/.local/bin)
#   HASHI_NO_DOCTOR=1           skip the post-install `hashi doctor` check
#   HASHI_CONNECT='<invite>'    after installing, run `hashi connect` with this
#                               invite link (instead of `hashi doctor`)
#
# It downloads a prebuilt binary from GitHub Releases, verifies its SHA-256
# checksum, and installs it for the current user. No sudo is needed. If no
# prebuilt binary fits this machine, it falls back to `cargo install`.

set -eu

REPO="sidharthpunathil/hashi"
VERSION="${HASHI_VERSION:-latest}"
INSTALL_DIR="${HASHI_INSTALL_DIR:-$HOME/.local/bin}"

if [ -t 1 ]; then
    BOLD="$(printf '\033[1m')"; DIM="$(printf '\033[2m')"; RESET="$(printf '\033[0m')"
    PURPLE="$(printf '\033[38;5;141m')"; GREEN="$(printf '\033[32m')"; RED="$(printf '\033[31m')"
else
    BOLD=""; DIM=""; RESET=""; PURPLE=""; GREEN=""; RED=""
fi

say()  { printf '  %s\n' "$*"; }
step() { printf '  %s›%s %s\n' "$PURPLE" "$RESET" "$*"; }
ok()   { printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
fail() { printf '  %s✗ %s%s\n' "$RED" "$*" "$RESET" >&2; exit 1; }
has()  { command -v "$1" >/dev/null 2>&1; }

# An invite link of exactly the shape the claim page builds (SAFE_ORIGIN and
# SAFE_CODE in src/web/hashi.js, joined by /claim/). Nothing else reaches
# `hashi connect`.
CONNECT_PATTERN='^(https://([A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?|\[[0-9A-Fa-f:.]+\])|http://(localhost|127(\.[0-9]{1,3}){3}|\[::1\]))(:[0-9]{1,5})?/claim/hashi_inv_[0-9a-f]{16,118}$'

valid_connect_link() {
    case "$1" in
        *"
"*) return 1 ;;
    esac
    printf '%s\n' "$1" | LC_ALL=C grep -Eq "$CONNECT_PATTERN"
}

download() {
    # download <url> <file>
    if has curl; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"
    elif has wget; then
        wget -q --https-only -O "$2" "$1"
    else
        fail "curl or wget is required"
    fi
}

sha256_of() {
    if has sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        fail "sha256sum or shasum is required to verify the download"
    fi
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$arch" in
        x86_64 | amd64) arch="x86_64" ;;
        arm64 | aarch64) arch="aarch64" ;;
        *) return 1 ;;
    esac
    case "$os" in
        Linux) printf '%s-unknown-linux-gnu' "$arch" ;;
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple Silicon.
            if [ "$arch" = "x86_64" ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = "1" ]; then
                arch="aarch64"
            fi
            printf '%s-apple-darwin' "$arch"
            ;;
        *) return 1 ;;
    esac
}

install_with_cargo() {
    if ! has cargo; then
        fail "no prebuilt binary for $(uname -s)/$(uname -m), and cargo is not installed. Install Rust from https://rustup.rs and run this again."
    fi
    step "building from source with cargo (this takes a few minutes)"
    if [ "$VERSION" = "latest" ]; then
        cargo install --locked --git "https://github.com/$REPO" --root "$tmp/cargo"
    else
        cargo install --locked --git "https://github.com/$REPO" --tag "$VERSION" --root "$tmp/cargo"
    fi
    mkdir -p "$INSTALL_DIR"
    install -m 755 "$tmp/cargo/bin/hashi" "$INSTALL_DIR/hashi"
}

path_hint() {
    case ":${PATH}:" in
        *":$INSTALL_DIR:"*) return 0 ;;
    esac
    say ""
    say "${BOLD}Add Hashi to your PATH${RESET} (then open a new terminal):"
    case "${SHELL:-}" in
        */zsh) say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
        */fish) say "  fish_add_path $INSTALL_DIR" ;;
        *) say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
    esac
}

main() {
    printf '\n  %s✦ hashi%s %sthe bridge to Claude Code and Codex%s\n\n' "$BOLD$PURPLE" "$RESET" "$DIM" "$RESET"

    if [ -n "${HASHI_CONNECT:-}" ] && ! valid_connect_link "$HASHI_CONNECT"; then
        fail "HASHI_CONNECT is not an invite link like https://<host>/claim/hashi_inv_<code>; copy the command from the claim page again"
    fi

    tmp="$(mktemp -d 2>/dev/null || mktemp -d -t hashi)"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    if target="$(detect_target)"; then
        asset="hashi-$target.tar.gz"
        if [ "$VERSION" = "latest" ]; then
            base="https://github.com/$REPO/releases/latest/download"
        else
            base="https://github.com/$REPO/releases/download/$VERSION"
        fi

        step "downloading $asset ($VERSION)"
        if download "$base/$asset" "$tmp/$asset"; then
            # The archive exists, so never fall back to an unverified build:
            # a missing or wrong checksum is an error.
            download "$base/$asset.sha256" "$tmp/$asset.sha256" || fail "could not download the checksum for $asset; refusing to install"
            expected="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
            actual="$(sha256_of "$tmp/$asset")"
            [ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset; refusing to install"
            ok "checksum verified"

            tar -xzf "$tmp/$asset" -C "$tmp"
            mkdir -p "$INSTALL_DIR"
            install -m 755 "$tmp/hashi-$target/hashi" "$INSTALL_DIR/hashi"
        else
            say "${DIM}no prebuilt binary found for $target${RESET}"
            install_with_cargo
        fi
    else
        install_with_cargo
    fi

    ok "installed $("$INSTALL_DIR/hashi" --version) to $INSTALL_DIR"
    path_hint

    if [ -n "${HASHI_CONNECT:-}" ]; then
        # A friend's setup: redeem the invite link instead of checking for
        # local agents, which they do not need.
        step "connecting with your invite link"
        if ! "$INSTALL_DIR/hashi" connect -- "$HASHI_CONNECT"; then
            fail "Hashi is installed, but connecting with the invite link failed (the reason is above). An invite link works once: if it was already used or has expired, ask for a new one, then run: hashi connect -- '<new link>'"
        fi
        return 0
    fi

    if [ -z "${HASHI_NO_DOCTOR:-}" ]; then
        "$INSTALL_DIR/hashi" doctor || true
    fi

    say "${BOLD}Next:${RESET} run ${PURPLE}hashi${RESET} to start the bridge and open the dashboard."
    if ! has claude && ! has codex; then
        say "${DIM}Install and sign in to Claude Code or Codex first; Hashi drives them for you.${RESET}"
    fi
    say ""
}

main "$@"
