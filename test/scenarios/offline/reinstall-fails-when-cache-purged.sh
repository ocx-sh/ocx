#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline exec fails clearly when both packages/ and blobs/ are gone
# description: Offline reinstall must surface a non-zero exit when the cache
#   is incomplete — never silently fall back to a network fetch.
set -euo pipefail

ocx package install --select "$PKG_HELLO"

rm -rf "$OCX_HOME/packages" "$OCX_HOME/blobs" "$OCX_HOME/layers"

if ocx --offline package exec "$PKG_HELLO" -- hello; then
    echo "expected --offline exec to fail with empty cache" >&2
    exit 1
fi
