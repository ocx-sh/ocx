#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/project-groups
# title: Project groups
# description: Declare named groups in ocx.toml and pull or lock only the needed subset.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_KITWARE_CMAKE"
ocx add -g ci "$PKG_ASTRAL_SH_UV"
ocx pull -g ci
ocx lock
# endregion cast
