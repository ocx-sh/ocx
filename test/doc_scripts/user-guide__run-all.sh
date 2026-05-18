#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-all
# title: Run with all groups composed
# description: Compose the environment from every declared group using the all keyword.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx add -g ci "$PKG_UV"
ocx run -g all -- cmake --version
# endregion cast
