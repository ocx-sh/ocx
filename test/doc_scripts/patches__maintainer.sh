#!/usr/bin/env bash
# state: setup:patches-maintainer
# cast: true
# title: Publishing patch descriptors
# doc: user-guide/patches-maintainer
# description: Preview a patch descriptor with ocx patch test, publish the descriptor, install the base to trigger patch discovery, and freeze companion digests for reproducible builds.
set -euo pipefail
cd "$SCENARIO_TMP"

# The setup:patches-maintainer provider has already published the base tool
# and the corp-ca companion, configured the [patches] tier, and written
# descriptor.json into this work dir. The region below is the maintainer's
# author -> test -> publish -> install -> freeze flow: publish makes the
# descriptor discoverable, install triggers lazy patch discovery (pulling in
# the now-published corp-ca companion), and freeze pins the digests install
# just discovered — `ocx patch freeze` itself never re-checks the registry,
# it only reads what discovery has already cached locally.

# region cast
ocx patch test --descriptor descriptor.json "$PKG_MYTOOL"
ocx patch publish --descriptor descriptor.json "$PKG_MYTOOL"
ocx package install "$PKG_MYTOOL"
ocx --global patch freeze
# endregion cast
