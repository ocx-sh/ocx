#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-project/pull
# title: Pull the locked tools
# description: Download the locked packages, as a teammate does after cloning, then run the tool.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add --no-pull $REPO_NODEJS_NODE:24
# region cast
ocx pull
ocx exec -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
