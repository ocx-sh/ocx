#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/uninstall
# title: Remove the selected version
# description: Deselect, uninstall and purge the selected version in one command.
set -euo pipefail

ocx package install --select $PKG_KITWARE_CMAKE
# region cast
ocx package uninstall --deselect --purge $PKG_KITWARE_CMAKE
# endregion cast
