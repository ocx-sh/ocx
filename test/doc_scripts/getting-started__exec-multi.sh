#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: getting-started/exec-multi
# title: Run multiple packages together
# description: Pass multiple packages before --; their environments are merged in declaration order.
set -euo pipefail

# region cast
ocx package exec "$PKG_NODEJS" "$PKG_BUN" -- bun --version
# endregion cast
