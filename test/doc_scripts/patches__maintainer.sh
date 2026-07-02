#!/usr/bin/env bash
# state: setup:patches-maintainer
# cast: true
# title: Publishing patch descriptors
# doc: user-guide/patches-maintainer
# description: Preview a patch descriptor with ocx patch test, publish it to the patch registry, and freeze companion digests for reproducible builds.
set -euo pipefail
cd "$SCENARIO_TMP"

# The setup:patches-maintainer provider has already published the base tool
# and the corp-ca companion, configured the [patches] tier, and written
# descriptor.json into this work dir. The region below is the maintainer's
# author → test → publish → freeze flow.

# region cast
ocx patch test --descriptor descriptor.json "$PKG_MYTOOL"
ocx patch publish --descriptor descriptor.json "$PKG_MYTOOL"
ocx --global patch freeze
# endregion cast
