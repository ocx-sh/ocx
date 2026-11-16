#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-project/exec
# title: Run a command with the pinned tools
# description: Run Node.js from the lock with ocx exec.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add $REPO_NODEJS_NODE:24
# region cast
ocx exec -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
