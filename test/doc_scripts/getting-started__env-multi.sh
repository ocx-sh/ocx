#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: getting-started/env-multi
# title: Compose environments from multiple packages
# description: Pass multiple packages to merge their environments in declaration order.
set -euo pipefail

ocx package install "$PKG_NODEJS"
ocx package install "$PKG_BUN"
# region cast
ocx package env "$PKG_NODEJS" "$PKG_BUN"
# endregion cast
