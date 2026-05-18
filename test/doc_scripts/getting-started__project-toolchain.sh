#!/usr/bin/env bash
# state: setup:full-catalog
# doc: getting-started/project-toolchain
# title: Project toolchain quick-start
# description: Declare, lock, and run a project toolchain in three commands.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx run -- cmake --version
# endregion cast
