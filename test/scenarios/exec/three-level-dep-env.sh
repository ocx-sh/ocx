#!/usr/bin/env bash
# scenario: ThreeLevelDeps
# title: exec on app surfaces transitive leaf via mid (all public)
set -euo pipefail

ocx package install --select "$PKG_APP"

env_out="$(ocx package exec "$PKG_APP" -- env)"

for key in "$HOME_KEY_MID" "$HOME_KEY_LEAF"; do
    if ! grep -q "^${key}=" <<<"$env_out"; then
        echo "expected '$key' in app's exec env" >&2
        echo "$env_out" >&2
        exit 1
    fi
done
