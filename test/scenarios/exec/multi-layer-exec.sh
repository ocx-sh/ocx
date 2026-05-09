#!/usr/bin/env bash
# scenario: MultiLayer
# title: exec on a 3-layer package finds bin from the top layer
set -euo pipefail

ocx install --select "$PKG_PKG"

out="$(ocx exec "$PKG_PKG" -- myapp)"
if [[ "$out" != *"$MARKER_PKG"* ]]; then
    echo "expected marker '$MARKER_PKG' in output, got: $out" >&2
    exit 1
fi
