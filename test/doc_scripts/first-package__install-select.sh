#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/install-select
# title: Install a second version and select it
# description: Install Node.js 24 and point the current symlink at it.
set -euo pipefail

ocx package install $PKG_NODEJS_NODE
# region cast
ocx package install --select $REPO_NODEJS_NODE:24
# endregion cast
