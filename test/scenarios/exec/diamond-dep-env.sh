#!/usr/bin/env bash
# scenario: DiamondDeps
# title: diamond app reaches shared leaf exactly once via either branch
set -euo pipefail

ocx package install --select "$PKG_APP"

env_out="$(ocx package exec "$PKG_APP" -- env)"

# Leaf's HOME var must be present and appear exactly once (cross-root dedup).
count="$(grep -c "^${HOME_KEY_LEAF}=" <<<"$env_out" || true)"
if [[ "$count" != "1" ]]; then
    echo "expected exactly one '$HOME_KEY_LEAF=...' line, got $count" >&2
    echo "$env_out" >&2
    exit 1
fi
