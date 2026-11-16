#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-named
# title: Run a specific named binding
# description: Pass a binding name to run only that binary from the composed scope.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_NODEJS_NODE"
# `ocx add` names the binding after the repository basename, so a two-segment
# identifier still binds as `node` — the binding name is what `ocx exec` takes.
ocx exec node -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec node -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
