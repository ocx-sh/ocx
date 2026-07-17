#!/usr/bin/env bash
# title: package push -n publishes a fresh single-layer package and index lists it
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

# Per-test repo so parallel xdist workers do not collide.
repo="t_$(uuidgen | tr -d '-' | head -c 8)_pushdemo"
fq="$REGISTRY/$repo:1.0.0"

ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64
ocx package push -n -i "$fq" mytool-1.0.0.tar.xz
ocx index update "$repo"

out="$(ocx index list "$repo")"
if [[ "$out" != *"$repo"* ]]; then
    echo "expected '$repo' in index list output, got: $out" >&2
    exit 1
fi
