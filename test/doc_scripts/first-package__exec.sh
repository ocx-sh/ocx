#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/exec
# title: Run a package once
# description: Execute CMake straight from the registry without installing it.
set -euo pipefail

# region cast
ocx package exec $PKG_KITWARE_CMAKE -- cmake --version
# endregion cast
