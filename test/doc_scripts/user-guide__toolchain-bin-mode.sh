#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: user-guide/toolchain-bin-mode
# title: Tools on PATH, nothing else composed
# description: activate = "bin" renders one directory of launchers; putting it on PATH is the whole of what a shell does in this mode.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
printf 'activate = "bin"\n[tools]\n' >ocx.toml
ocx add "$PKG_KITWARE_CMAKE"
ocx pull
export PATH="$PWD/.ocx/toolchain/bin:$PATH"
command -v cmake
cmake --version
# endregion cast
