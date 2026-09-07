#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: reference/command-line/pinned
# title: Toolchain links versus digest paths
# description: The default composition follows the rendered toolchain links; --pinned names the digest roots ocx.lock pins right now.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
ocx add "$PKG_KITWARE_CMAKE"
ocx pull
ocx env
ocx env --pinned
# endregion cast
