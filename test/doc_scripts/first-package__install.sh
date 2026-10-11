#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/install
# title: Install a package
# description: Install CMake and get a stable candidate path.
set -euo pipefail

# region cast
ocx package install $PKG_KITWARE_CMAKE
# endregion cast
