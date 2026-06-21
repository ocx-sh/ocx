#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Publishing patch descriptors
# doc: user-guide/patches-maintainer
# description: Author a patch descriptor, preview it locally with ocx patch test, publish it to the patch registry, and freeze companion digests for reproducible builds.
set -euo pipefail
cd "$SCENARIO_TMP"

# Write a config.toml so the [patches] tier is configured before patch commands.
# OCX_CONFIG overrides the default config search path (like OCX_MIRRORS for mirrors).
mkdir -p config
cat >config/config.toml <<'TOML'
[patches]
registry = "localhost:5000"
path = "site-patches/{registry}/{repository}"
required = true
TOML
export OCX_CONFIG="$SCENARIO_TMP/config/config.toml"

# Publish the base package so the patch-test step can materialise it.
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json -i "$PKG_MYTOOL" mytool-1.0.0.tar.xz

# Build a companion package — carries the env overlay (e.g. a CA bundle or proxy var).
cat >companion-metadata.json <<'JSON'
{
  "type": "bundle",
  "version": 1,
  "identifier": "localhost:5000/site-patches/corp-ca:1.0",
  "env": [
    {
      "key": "SSL_CERT_FILE",
      "type": "constant",
      "required": true,
      "value": "/etc/ssl/corp/ca-bundle.pem",
      "visibility": "interface"
    }
  ]
}
JSON
mkdir -p companion/etc/ssl/corp
echo "# corp CA bundle" >companion/etc/ssl/corp/ca-bundle.pem
ocx package create companion -m companion-metadata.json -o corp-ca-1.0.tar.xz

# region cast
cat >descriptor.json <<'JSON'
{
  "version": 1,
  "rules": [
    {
      "match": "*",
      "packages": ["localhost:5000/site-patches/corp-ca:1.0"],
      "required": true
    }
  ]
}
JSON

ocx package push -n -p linux/amd64 \
    -m companion-metadata.json \
    -i "localhost:5000/site-patches/corp-ca:1.0" \
    corp-ca-1.0.tar.xz

ocx patch test \
    --descriptor-file descriptor.json \
    "$PKG_MYTOOL"

ocx patch publish \
    --descriptor-file descriptor.json \
    "$PKG_MYTOOL"

ocx --global patch freeze
# endregion cast

# Verification — outside the displayed region.
test -f "$OCX_HOME/patches.snapshot.json" || {
    echo "ERROR: patches.snapshot.json not written to OCX_HOME" >&2
    exit 1
}
