#!/usr/bin/env bash
# state: setup:deps-visibility
# cast: true
# title: Dependency tree with visibility
# doc: user-guide/deps
set -euo pipefail
# region cast
ocx package deps "$PKG_WEBAPP"
# endregion cast
