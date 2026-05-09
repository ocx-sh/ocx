#!/usr/bin/env bash
# title: package test --keep preserves the materialised package on disk
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

ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz

# --keep prints the tempdir path to stderr before exec replaces the process.
# Run via subshell so we can capture stderr without ending the parent script.
stderr="$(
    ocx package test --keep -p linux/amd64 \
        -m metadata.json -i "$REGISTRY"/mytool:1.0.0 \
        mytool-1.0.0.tar.xz -- mytool 2>&1 1>/dev/null
)"

# Path should still exist after the command completes.
kept_path="$(grep -oE "${OCX_HOME}/temp/test/[a-zA-Z0-9._/-]+" <<<"$stderr" | head -n1 || true)"
if [[ -z "$kept_path" || ! -d "$kept_path" ]]; then
    echo "expected --keep to leave tempdir on disk; stderr was:" >&2
    echo "$stderr" >&2
    exit 1
fi
