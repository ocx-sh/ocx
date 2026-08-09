#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: getting-started/uninstall
# title: Uninstall a package
# description: Remove the candidate symlink; the binary is preserved in the object store.
set -euo pipefail

# region cast
ocx package install "$PKG_ASTRAL_SH_UV"
ocx package uninstall "$PKG_ASTRAL_SH_UV"
# endregion cast
