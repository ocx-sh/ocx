#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/isolation
# title: Project vs OCI-tier isolation
# description: Demonstrate that ocx exec reads only the project file and ocx package exec reads no project file at all.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_NODEJS_NODE"
ocx exec -- node --version
ocx package exec "$PKG_NODEJS_NODE" -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
[[ "$(ocx package exec "$PKG_NODEJS_NODE" -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
