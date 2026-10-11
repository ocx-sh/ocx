#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-project/pull
# title: Pull the locked tools
# description: Download the locked packages, as a teammate does after cloning, then run the tool.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add --no-pull $PKG_KITWARE_CMAKE
# region cast
ocx pull
ocx exec -- cmake --version
# endregion cast
