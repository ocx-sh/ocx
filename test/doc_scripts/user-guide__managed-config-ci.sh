#!/usr/bin/env bash
# state: setup:managed-config-ci
# doc: user-guide/managed-config-ci
# title: Sync managed config in a CI recipe
# description: Point an ephemeral CI runner at the managed-config artifact via an environment variable, sync it explicitly, then install a locked tool -- no seed ever touches disk.
set -euo pipefail

# The setup:managed-config-ci provider has already published the
# managed-config artifact, aliased "internal.company.com" (via [mirrors]) to
# the local test registry, and published cmake for the install step below.

# region cast
export OCX_MANAGED_CONFIG=internal.company.com/ocx-config:ci
ocx config update
ocx package install "$PKG_CMAKE"
# endregion cast
