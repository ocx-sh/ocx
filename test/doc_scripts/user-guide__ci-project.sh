#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/ci-project
# title: CI with a project toolchain
# description: Check the lock, pre-warm the package store from ocx.lock and run the locked toolchain in CI.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
ocx add --no-pull "$PKG_NODEJS_NODE"
# region cast
ocx lock --check
ocx --frozen pull
ocx exec -- node --version
# endregion cast

# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
