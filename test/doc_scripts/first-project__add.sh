#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-project/add
# title: Pin a tool
# description: Add Node.js to the project: resolve the tag, write ocx.lock and download the package.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir my-project
cd my-project
ocx init
# region cast
ocx add $REPO_NODEJS_NODE:24
# endregion cast
