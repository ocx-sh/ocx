#!/usr/bin/env bash
# state: setup:deps-visibility
# cast: true
# title: Tracing why a dependency is pulled in
# doc: user-guide/deps-why
set -euo pipefail
# region cast
ocx deps --why "$PKG_NODEJS" "$PKG_WEBAPP"
# endregion cast
