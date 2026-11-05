#!/usr/bin/env bash
# Canned excerpt shaped like https://cursor.com/install, for parsing tests only.
set -euo pipefail
detect_os() { case "$(uname -s)" in Linux*) echo linux;; Darwin*) echo darwin;; esac; }
OS=$(detect_os)
ARCH=$(uname -m)
DOWNLOAD_URL="https://downloads.cursor.com/lab/2026.11.20-3f9c2ab/${OS}/${ARCH}/agent-cli-package.tar.gz"
echo "Downloading Cursor Agent package..."
curl -fSL --progress-bar "$DOWNLOAD_URL" | tar --strip-components=1 -xzf - -C "$TEMP_EXTRACT_DIR"
