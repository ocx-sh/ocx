#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: user-guide/toolchain-activation
# title: A locked toolchain reaches the shell
# description: Declare a tool, render the toolchain with ocx pull, and the tool answers to its own name through the rendered links.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
ocx add "$PKG_KITWARE_CMAKE"
ocx pull
eval "$(ocx env --shell=bash)"
command -v cmake
cmake --version
# endregion cast
