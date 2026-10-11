#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/select-back
# title: Point current back at the first version
# description: Select the 3.31 install again after 4.0 was selected.
set -euo pipefail

ocx package install $PKG_KITWARE_CMAKE
ocx package install --select $REPO_KITWARE_CMAKE:4.0
# region cast
ocx package select $PKG_KITWARE_CMAKE
# endregion cast
