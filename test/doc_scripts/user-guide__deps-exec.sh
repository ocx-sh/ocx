#!/usr/bin/env bash
# state: setup:dependencies
# doc: user-guide/deps-exec
# title: Run a package with its dependency environments
# description: Execute a package with all declared dependency environments composed in topological order.
set -euo pipefail

# region cast
ocx package exec "$PKG_WEBAPP" -- serve --version
# endregion cast

# Verification — outside the displayed region (drift-gated, never shown/cast).
out="$(ocx package exec "$PKG_WEBAPP" -- serve --version)"
[[ -n "$out" ]] || {
    echo "ERROR: ocx package exec returned empty output" >&2
    exit 1
}
