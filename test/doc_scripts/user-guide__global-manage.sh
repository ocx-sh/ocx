#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/global-manage
# title: Manage the global toolchain
# description: Add, remove, lock, and upgrade packages in the global toolchain with the root --global flag.
set -euo pipefail

# region cast
ocx --global add "$PKG_CMAKE"
ocx --global add "$PKG_UV"
# endregion cast
ocx --global remove "$REPO_CMAKE"
ocx --global lock
ocx --global upgrade
