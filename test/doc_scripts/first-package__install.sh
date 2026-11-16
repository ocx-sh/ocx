#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/install
# title: Install a package
# description: Install Node.js and get a stable candidate path.
set -euo pipefail

# region cast
ocx package install $PKG_NODEJS_NODE
# endregion cast
