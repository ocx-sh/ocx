#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-group
# title: Run with a named group
# description: Scope the toolchain environment to a named group with -g.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add -g ci "$PKG_UV"
ocx run -g ci -- uv --version
# endregion cast
