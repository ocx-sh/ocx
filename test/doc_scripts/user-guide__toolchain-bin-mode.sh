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
ocx add "$PKG_NODEJS_NODE"
ocx pull
export PATH="$PWD/.ocx/toolchain/active/bin:$PATH"
command -v node
node --version
# endregion cast

# `command -v node` above is not a gate on its own.  The harness never scrubs
# PATH, so the script inherits the runner's — and `ubuntu-latest` ships node.
# If the prepended directory stopped existing, the host's node would answer,
# the script would still exit 0, and the cast would record a path that is not
# ocx's.  Assert the *resolved* path, not that some node resolved.
resolved="$(command -v node)"
expected="$PWD/.ocx/toolchain/active/bin/node"
[[ $resolved == "$expected" ]] || {
    echo "expected node to resolve to the rendered trampoline" >&2
    echo "  expected: $expected" >&2
    echo "  resolved: $resolved" >&2
    exit 1
}
