#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline install re-assembles package after clean + rm -rf packages/installs
# description: Regression for the GC + offline-rehydration architectural hole.
#   Before the fix, `ocx clean` deleted the OCX metadata config blob because it
#   had no refs/blobs/ back-edge; offline re-assembly then failed even though
#   the layers were still on disk.
set -euo pipefail
IFS=$'\n\t'

ocx package install --select "$PKG_HELLO"
ocx clean

# After clean, blobs and layers must still be on disk — they are reachable
# via the installed package's refs/*/ edges.
[[ -d "$OCX_HOME/blobs" ]] || {
    echo "blobs evicted by clean" >&2
    exit 1
}
[[ -d "$OCX_HOME/layers" ]] || {
    echo "layers evicted by clean" >&2
    exit 1
}

# Purge assembled packages + install symlinks; keep blobs/ and layers/.
rm -rf "$OCX_HOME/packages" "$OCX_HOME/installs"

# Re-assemble offline. Must succeed from cached blobs (incl. metadata config) + layers.
out="$(ocx --offline package exec "$PKG_HELLO" -- hello)"
if [[ "$out" != *"$MARKER_HELLO"* ]]; then
    echo "expected marker '$MARKER_HELLO' in offline-rebuilt exec output, got: $out" >&2
    exit 1
fi
