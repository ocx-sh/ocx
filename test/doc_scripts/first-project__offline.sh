#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-project/offline
# title: Build without the network
# description: Run the locked tool with all network access disabled, from the packages ocx add already downloaded.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add $REPO_NODEJS_NODE:24
# region cast
ocx --offline exec -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx --offline exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
