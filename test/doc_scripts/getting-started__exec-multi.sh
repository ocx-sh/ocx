#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: getting-started/exec-multi
# title: Run multiple packages together
# description: Pass multiple packages before --; their environments are merged in declaration order.
set -euo pipefail

# region cast
ocx package exec "$PKG_NODEJS_NODE" "$PKG_OVEN_SH_BUN" -- bun --version
# endregion cast
