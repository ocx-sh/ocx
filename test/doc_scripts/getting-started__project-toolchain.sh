#!/usr/bin/env bash
# state: setup:full-catalog
# doc: getting-started/project-toolchain
# title: Project toolchain quick-start
# description: Declare, lock, and run a project toolchain in three commands.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_NODEJS_NODE"
ocx exec -- node --version
# endregion cast
# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
