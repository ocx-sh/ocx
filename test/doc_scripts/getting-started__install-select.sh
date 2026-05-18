#!/usr/bin/env bash
# state: setup:multi-version
# cast: true
# doc: getting-started/install-select
# title: Install and select a version
# description: Install a package and set it as the current version in one step.
set -euo pipefail

# region cast
ocx package install --select "$PKG_CORRETTO"
ocx package which --current "$PKG_CORRETTO"
# endregion cast
