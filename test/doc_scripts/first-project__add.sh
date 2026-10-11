#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-project/add
# title: Pin a tool
# description: Add CMake to the project: resolve the tag, write ocx.lock and download the package.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
# region cast
ocx add $PKG_KITWARE_CMAKE
# endregion cast
