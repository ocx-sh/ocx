#!/usr/bin/env bash
# state: setup:deps-visibility
# cast: true
# title: Flat view with visibility
# doc: user-guide/deps-flat
set -euo pipefail
# region cast
ocx package deps --flat "$PKG_ACME_WEBAPP"
# endregion cast
