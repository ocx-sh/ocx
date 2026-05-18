#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-named
# title: Run a specific named binding
# description: Pass a binding name to run only that tool from the composed scope.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx run "$REPO_CMAKE" -- cmake --version
# endregion cast
