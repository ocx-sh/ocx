#!/usr/bin/env bash
# title: package test runs a freshly-built single-layer bundle locally
# description: No scenario state needed — exercises ocx package create + ocx package test end-to-end.
set -euo pipefail

cd "$SCENARIO_TMP"
mkdir -p build/bin
cat >build/bin/mytool <<'EOF'
#!/bin/sh
echo mytool-ok
EOF
chmod +x build/bin/mytool

cat >metadata.json <<'EOF'
{ "type": "bundle", "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin", "visibility": "public" }
  ] }
EOF

ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64

out="$(ocx package test -i "$REGISTRY"/mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool)"
if [[ "$out" != *"mytool-ok"* ]]; then
    echo "expected 'mytool-ok' in output, got: $out" >&2
    exit 1
fi
