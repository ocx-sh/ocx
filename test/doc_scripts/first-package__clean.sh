#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-package/clean
# title: Remove the other version and clean up
# description: Uninstall a version, then delete every package nothing references.
set -euo pipefail

ocx package install $REPO_KITWARE_CMAKE:4.0
# region cast
ocx package uninstall $REPO_KITWARE_CMAKE:4.0
ocx clean
# endregion cast
