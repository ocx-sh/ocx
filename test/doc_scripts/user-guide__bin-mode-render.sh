#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/bin-mode-render
# title: Render a bin-mode project toolchain
# description: ocx pull writes the render stamp a prompt checks before it puts a project's launcher directory on PATH.
set -euo pipefail

cd "$SCENARIO_TMP"
printf 'activate = "bin"\n[tools]\n' >ocx.toml
ocx add "$PKG_KITWARE_CMAKE"
# region cast
ocx pull
# endregion cast
[[ -x .ocx/toolchain/active/bin/cmake ]] || {
    echo "expected ocx pull to render .ocx/toolchain/active/bin/cmake" >&2
    exit 1
}
# `active` is the PATH-facing indirection, so assert it is a link and not a
# directory that happens to carry the same name — a copy made with `cp -rL`
# leaves the second, and the trampoline check above passes on both.
[[ -L .ocx/toolchain/active ]] || {
    echo "expected .ocx/toolchain/active to be a link to shells/default" >&2
    exit 1
}
