#!/usr/bin/env bash
# state: setup:patches-maintainer
# cast: true
# title: Testing a patch descriptor locally
# doc: user-guide/patches-test
# description: Preview a patch descriptor with ocx patch test, run a command in the composed environment, and preview an unpublished companion via --companion-archive.
set -euo pipefail
cd "$SCENARIO_TMP"

# The setup:patches-maintainer provider has already published the base tool
# and the corp-ca companion, configured the [patches] tier, written
# descriptor.json into this work dir, and built (but never pushed) a
# corp-ca:2.0.0 preview-companion.tar.xz archive with its own
# descriptor-preview.json. The region below is the maintainer's local preview
# loop: compose the descriptor onto the base, run a command in the composed
# environment, then preview an unpublished companion before it exists on the
# registry.

# region cast
ocx patch test --descriptor descriptor.json "$PKG_MYTOOL"
ocx patch test --descriptor descriptor.json "$PKG_MYTOOL" -- mytool --version
ocx patch test --descriptor descriptor-preview.json --companion-archive preview-companion.tar.xz "$PKG_MYTOOL"
# endregion cast
