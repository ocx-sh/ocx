#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/stable-paths-which
# title: Resolve stable symlink paths
# description: Print the current and candidate symlink paths for an installed package.
set -euo pipefail

# region cast
ocx package install --select "$PKG_CMAKE"
ocx package which --current "$PKG_CMAKE"
ocx package which --candidate "$PKG_CMAKE"
# endregion cast

current_path="$(ocx package which --current "$REPO_CMAKE")"
[[ -n "$current_path" ]] || {
    echo "ERROR: ocx package which --current returned empty output" >&2
    exit 1
}

candidate_path="$(ocx package which --candidate "$PKG_CMAKE")"
[[ -n "$candidate_path" ]] || {
    echo "ERROR: ocx package which --candidate returned empty output" >&2
    exit 1
}
