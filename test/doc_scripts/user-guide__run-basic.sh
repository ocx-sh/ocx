#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-basic
# title: Run a command in the project toolchain environment
# description: Invoke a tool from the default [tools] group with ocx run.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx run -- cmake --version
# endregion cast
