#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline exec runs the entrypoint after online install
set -euo pipefail

ocx install --select "$PKG_HELLO"

out="$(ocx --offline exec "$PKG_HELLO" -- hello)"
if [[ "$out" != *"$MARKER_HELLO"* ]]; then
    echo "expected marker '$MARKER_HELLO', got: $out" >&2
    exit 1
fi
