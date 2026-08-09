#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: getting-started/env-multi
# title: Compose environments from multiple packages
# description: Pass multiple packages to merge their environments in declaration order.
set -euo pipefail

ocx package install "$PKG_NODEJS_NODE"
ocx package install "$PKG_OVEN_SH_BUN"
# region cast
ocx package env "$PKG_NODEJS_NODE" "$PKG_OVEN_SH_BUN"
# endregion cast
