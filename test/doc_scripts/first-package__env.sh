#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/env
# title: See the package environment
# description: List the environment variables an installed package declares.
set -euo pipefail

ocx package install $PKG_KITWARE_CMAKE
# region cast
ocx package env $PKG_KITWARE_CMAKE
# endregion cast
