#!/usr/bin/env bash
# scenario: TwoLevelDeps
# title: exec on app surfaces leaf's HOME env var (interface visibility)
set -euo pipefail

ocx package install --select "$PKG_APP"

env_out="$(ocx package exec "$PKG_APP" -- env)"

if ! grep -q "^${HOME_KEY_LEAF}=" <<<"$env_out"; then
    echo "expected leaf's home env key '$HOME_KEY_LEAF' in app's exec env" >&2
    echo "$env_out" >&2
    exit 1
fi
