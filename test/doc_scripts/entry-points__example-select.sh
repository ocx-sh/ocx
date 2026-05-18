#!/usr/bin/env bash
# state: setup:multi-version
# doc: entry-points/example-select
# title: End-to-end example — switch to a different version
# description: Install both versions of a package and select a newer one; the current symlink re-points without re-sourcing the shell profile.
set -euo pipefail

# region cast
# Install the first (primary) version and select it.
ocx package install --select "$PKG_CORRETTO"
# endregion cast

# Install the second version (no --select yet).
# The tag for the second version pushed by the multi-version setup is 25.0.0.
ocx package install "$REPO_CORRETTO:25.0.0"

# Selecting the new version re-points current; no dotfile re-sourcing needed.
ocx package select "$REPO_CORRETTO:25.0.0"

# Verify current now points at the new version — which --current with the
# versioned identifier resolves through the current symlink to the candidate path.
current="$(ocx package which --current "$REPO_CORRETTO:25.0.0")"
[[ "$current" == *"25"* ]] || {
    echo "expected current to point at 25.x, got: $current" >&2
    exit 1
}
