#!/usr/bin/env bash
# scenario: BasicPackage
# title: Install the published BasicPackage and exec its hello entrypoint
# description: Asserts the marker echoed by the test binary matches MARKER_HELLO.
set -euo pipefail

ocx package install --select "$PKG_HELLO"

out="$(ocx package exec "$PKG_HELLO" -- hello)"

if [[ "$out" != *"$MARKER_HELLO"* ]]; then
    echo "expected marker '$MARKER_HELLO' in output, got: $out" >&2
    exit 1
fi
