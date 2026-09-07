#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/bin-mode-render
# title: Render a bin-mode project toolchain
# description: ocx pull writes the render stamp a prompt checks before it puts a project's bin/ on PATH.
set -euo pipefail

cd "$SCENARIO_TMP"
printf 'activate = "bin"\n[tools]\n' >ocx.toml
ocx add "$PKG_KITWARE_CMAKE"
# region cast
ocx pull
# endregion cast
[[ -x .ocx/toolchain/bin/cmake ]] || {
    echo "expected ocx pull to render .ocx/toolchain/bin/cmake" >&2
    exit 1
}
