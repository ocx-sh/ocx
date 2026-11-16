#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/project-lifecycle
# title: Project toolchain lifecycle
# description: Scaffold, add, lock, pre-warm, run, and remove a project toolchain binding.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_NODEJS_NODE"
ocx lock
ocx pull
ocx exec -- node --version
# endregion cast
# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
ocx remove "$REPO_NODEJS_NODE"
