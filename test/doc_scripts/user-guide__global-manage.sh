#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/global-manage
# title: Manage the global toolchain
# description: Add, remove, lock, and update packages in the global toolchain with the root --global flag.
set -euo pipefail

# region cast
ocx --global add "$PKG_NODEJS_NODE"
ocx --global add "$PKG_ASTRAL_SH_UV"
# endregion cast
ocx --global remove "$REPO_NODEJS_NODE"
ocx --global lock
ocx --global update
