#!/usr/bin/env bash
# state: setup:patches-maintainer
# cast: true
# title: Publishing patch descriptors
# doc: user-guide/patches-maintainer
# description: Preview a patch descriptor with ocx patch test, preview an unpublished companion via --companion-archive, publish the descriptor, and freeze companion digests for reproducible builds.
set -euo pipefail
cd "$SCENARIO_TMP"

# The setup:patches-maintainer provider has already published the base tool
# and the corp-ca companion, configured the [patches] tier, written
# descriptor.json into this work dir, and built (but never pushed) a
# corp-ca:2.0.0 preview-companion.tar.xz archive with its own
# descriptor-preview.json. The region below is the maintainer's author ->
# test -> preview-unpublished -> publish -> freeze flow.

# region cast
ocx patch test --descriptor descriptor.json "$PKG_MYTOOL"
ocx patch test --descriptor descriptor-preview.json --companion-archive preview-companion.tar.xz "$PKG_MYTOOL"
ocx patch publish --descriptor descriptor.json "$PKG_MYTOOL"
ocx --global patch freeze
# endregion cast
