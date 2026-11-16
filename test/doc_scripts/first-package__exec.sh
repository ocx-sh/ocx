#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/exec
# title: Run a package once
# description: Execute Node.js straight from the registry without installing it.
set -euo pipefail

# region cast
ocx package exec $PKG_NODEJS_NODE -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx package exec $PKG_NODEJS_NODE -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
