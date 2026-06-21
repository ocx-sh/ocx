#!/usr/bin/env bash
# state: setup:patches-consumer
# cast: true
# title: Running packages with patch overlays
# doc: user-guide/patches-consumer
# description: Sync site patches for installed tools, inspect the composed environment with --show-patches, and run the tool with the overlay applied.
set -euo pipefail

# The setup:patches-consumer provider has already configured the [patches]
# tier, published a corp-ca companion, installed cmake, and published the
# descriptor. The region below is what an operator actually runs; `ocx patch
# sync` is what installs the companion on screen.

# region cast
ocx patch sync
ocx package env "$PKG_CMAKE" --show-patches
ocx package exec "$PKG_CMAKE" -- cmake --version
# endregion cast
