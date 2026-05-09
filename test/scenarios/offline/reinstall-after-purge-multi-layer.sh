#!/usr/bin/env bash
# scenario: MultiLayer
# title: --offline exec re-assembles 3-layer package after rm packages/
set -euo pipefail

ocx install --select "$PKG_PKG"

rm -rf "$OCX_HOME/packages"

out="$(ocx --offline exec "$PKG_PKG" -- myapp)"
if [[ "$out" != *"$MARKER_PKG"* ]]; then
    echo "expected marker '$MARKER_PKG' in offline-rebuilt multi-layer exec, got: $out" >&2
    exit 1
fi
