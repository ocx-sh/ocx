#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: getting-started/find-candidate
# title: Find an installed package
# description: Return the stable candidate path for an installed package version.
set -euo pipefail

# region cast
ocx package install "$PKG_UV"
ocx package which --candidate "$PKG_UV"
# endregion cast
candidate="$(ocx package which --candidate "$PKG_UV")"
[[ -n "$candidate" ]] || {
    echo "expected a candidate path" >&2
    exit 1
}
