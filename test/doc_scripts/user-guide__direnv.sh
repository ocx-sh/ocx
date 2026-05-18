#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/direnv
# title: Set up direnv integration
# description: Write an .envrc that re-evaluates the project toolchain on each directory entry.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx direnv init
# endregion cast
[[ -f .envrc ]] || {
    echo "expected .envrc to be created" >&2
    exit 1
}
