#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/select-back
# title: Point current back at the first version
# description: Select the 22 install again after 24 was selected.
set -euo pipefail

ocx package install $PKG_NODEJS_NODE
ocx package install --select $REPO_NODEJS_NODE:24
# region cast
ocx package select $PKG_NODEJS_NODE
# endregion cast
