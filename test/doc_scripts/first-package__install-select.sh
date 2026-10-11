#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/install-select
# title: Install a second version and select it
# description: Install CMake 4.0 and point the current symlink at it.
set -euo pipefail

ocx package install $PKG_KITWARE_CMAKE
# region cast
ocx package install --select $REPO_KITWARE_CMAKE:4.0
# endregion cast
