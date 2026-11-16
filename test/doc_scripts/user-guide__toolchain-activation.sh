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
ocx add "$PKG_NODEJS_NODE"
ocx pull
eval "$(ocx env --shell=bash)"
command -v node
node --version
# endregion cast

# The runner ships its own node, so `command -v node` alone would pass on a host
# binary. Assert the resolved path is under the rendered toolchain.
resolved="$(command -v node)"
[[ $resolved == "$PWD/.ocx/toolchain/"* ]] || {
    echo "expected node to resolve under $PWD/.ocx/toolchain/" >&2
    echo "  resolved: $resolved" >&2
    exit 1
}
