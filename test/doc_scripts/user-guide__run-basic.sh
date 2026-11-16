#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-basic
# title: Run a command in the project toolchain environment
# description: Invoke a tool from the default [tools] group with ocx exec.
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
