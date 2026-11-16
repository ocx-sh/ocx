#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/exec-once
# title: Run a package once without installing
# description: Execute a package on demand without creating a persistent candidate symlink.
set -euo pipefail

# region cast
ocx package exec "$PKG_NODEJS_NODE" -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx package exec "$PKG_NODEJS_NODE" -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
