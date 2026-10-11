#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-project/exec
# title: Run a command with the pinned tools
# description: Run CMake from the lock with ocx exec.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add $PKG_KITWARE_CMAKE
# region cast
ocx exec -- cmake --version
# endregion cast
