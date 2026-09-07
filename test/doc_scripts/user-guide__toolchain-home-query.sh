#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/toolchain-home-query
# title: Query the resolved toolchain home
# description: The JSON shell-state report is the supported way for an editor or a CI step to discover a project's toolchain home.
set -euo pipefail

cd "$SCENARIO_TMP"
ocx init
ocx add "$PKG_KITWARE_CMAKE"
# region cast
ocx pull
ocx --format json shell state
# endregion cast

# The displayed snippet pipes this through jq, which the page shows because it
# is what a reader actually types.  The gate does not: jq is not declared
# anywhere in this repo, so depending on it here would make the check ambient —
# green on a runner that happens to ship jq, red on a contributor's box, for a
# reason unrelated to ocx.  Parse with the suite's own interpreter instead, and
# with the real JSON parser rather than a grep over the text.
home="$(ocx --format json shell state |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["toolchain_home"])')"
[[ -d "$home" ]] || {
    echo "expected toolchain_home to name an existing directory, got: $home" >&2
    exit 1
}
