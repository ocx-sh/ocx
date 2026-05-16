#!/usr/bin/env bash
# scenario: MultiEntrypoints
# title: each entrypoint of the toolkit launches its own binary
set -euo pipefail

ocx package install --select "$PKG_TOOLKIT"

for tool in tool-a tool-b tool-c tool-d; do
    out="$(ocx package exec "$PKG_TOOLKIT" -- "$tool")"
    if [[ "$out" != *"entry-point-${tool}"* ]]; then
        echo "expected '$tool' to print 'entry-point-${tool} ...', got: $out" >&2
        exit 1
    fi
done
