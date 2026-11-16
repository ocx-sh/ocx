#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/uninstall
# title: Remove the selected version
# description: Deselect, uninstall and purge the selected version in one command.
set -euo pipefail

ocx package install --select $PKG_NODEJS_NODE
# region cast
ocx package uninstall --deselect --purge $PKG_NODEJS_NODE
# endregion cast
