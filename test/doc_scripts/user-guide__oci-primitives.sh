#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/oci-primitives
# title: OCI-tier package primitives
# description: Install, select, deselect, uninstall, exec, and query env for a package at the OCI tier.
set -euo pipefail

# region cast
ocx package install "$PKG_CMAKE"
ocx package select "$PKG_CMAKE"
ocx package exec "$PKG_CMAKE" -- cmake --version
ocx package env "$PKG_CMAKE"
# endregion cast
ocx package deselect "$REPO_CMAKE"
ocx package uninstall "$PKG_CMAKE"
