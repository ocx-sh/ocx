#!/usr/bin/env bash
# scenario: TwoLevelDeps
# title: --offline exec re-assembles app + transitive leaf after rm packages/
# description: Offline reinstall must walk transitive deps and rehydrate each
#   from cached blobs/layers, not just the root.
set -euo pipefail

ocx install --select "$PKG_APP"

rm -rf "$OCX_HOME/packages"

env_out="$(ocx --offline exec "$PKG_APP" -- env)"
if ! grep -q "^${HOME_KEY_LEAF}=" <<<"$env_out"; then
    echo "expected leaf's home env key '$HOME_KEY_LEAF' after offline reinstall" >&2
    echo "$env_out" >&2
    exit 1
fi
