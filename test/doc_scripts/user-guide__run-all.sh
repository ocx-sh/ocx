#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-all
# title: Run with all groups composed
# description: Compose the environment from every declared group using the all keyword.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_NODEJS_NODE"
ocx add -g ci "$PKG_ASTRAL_SH_UV"
ocx exec -g all -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -g all -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
