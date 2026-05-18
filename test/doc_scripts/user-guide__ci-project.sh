#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/ci-project
# title: CI with a project toolchain
# description: Pre-warm the package store from ocx.lock and run the locked toolchain in CI.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx pull
ocx run -- cmake --version
# endregion cast
