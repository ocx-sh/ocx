#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: getting-started/install
# title: Install a package
# description: Download a package into the content-addressed object store and create a candidate symlink.
set -euo pipefail

# region cast
ocx package install "$PKG_UV"
# endregion cast
