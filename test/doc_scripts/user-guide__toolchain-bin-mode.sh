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
export PATH="$PWD/.ocx/toolchain/active/bin:$PATH"
command -v cmake
cmake --version
# endregion cast

# `command -v cmake` above is not a gate on its own.  The harness never scrubs
# PATH, so the script inherits the runner's — and `ubuntu-latest` ships cmake.
# If the prepended directory stopped existing, the host's cmake would answer,
# the script would still exit 0, and the cast would record a path that is not
# ocx's.  Assert the *resolved* path, not that some cmake resolved.
resolved="$(command -v cmake)"
expected="$PWD/.ocx/toolchain/active/bin/cmake"
[[ $resolved == "$expected" ]] || {
    echo "expected cmake to resolve to the rendered trampoline" >&2
    echo "  expected: $expected" >&2
    echo "  resolved: $resolved" >&2
    exit 1
}
