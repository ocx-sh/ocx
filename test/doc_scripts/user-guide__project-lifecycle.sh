#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/project-lifecycle
# title: Project toolchain lifecycle
# description: Scaffold, add, lock, pre-warm, run, and remove a project toolchain binding.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx lock
ocx pull
ocx run -- cmake --version
# endregion cast
ocx remove "$REPO_CMAKE"
