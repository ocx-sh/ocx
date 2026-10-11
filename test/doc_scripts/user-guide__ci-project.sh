#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/ci-project
# title: CI with a project toolchain
# description: Check the lock, pre-warm the package store from ocx.lock and run the locked toolchain in CI.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add --no-pull "$PKG_KITWARE_CMAKE"
# region cast
ocx lock --check
ocx --frozen pull
ocx exec -- cmake --version
# endregion cast
