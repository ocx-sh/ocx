#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/oci-primitives
# title: OCI-tier package primitives
# description: Install, select, deselect, uninstall, exec, and query env for a package at the OCI tier.
set -euo pipefail

# region cast
ocx package install "$PKG_NODEJS_NODE"
ocx package select "$PKG_NODEJS_NODE"
ocx package exec "$PKG_NODEJS_NODE" -- node --version
ocx package env "$PKG_NODEJS_NODE"
# endregion cast
# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx package exec "$PKG_NODEJS_NODE" -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}
ocx package deselect "$REPO_NODEJS_NODE"
ocx package uninstall "$PKG_NODEJS_NODE"
