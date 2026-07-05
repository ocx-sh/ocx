#!/usr/bin/env bash
# state: setup:managed-config-onboard
# doc: user-guide/managed-config-setup
# title: Onboard a workstation to centrally managed config
# description: Adopt the [managed] tier so mirrors, patch-registry, and default-registry settings sync from one operator-published config artifact.
set -euo pipefail

# The setup:managed-config-onboard provider has already published the
# managed-config artifact, aliased "internal.company.com" (via [mirrors]) to
# the local test registry, and published a stand-in ocx/cli release for the
# bootstrap seam below. The region is exactly what an operator runs.
export __OCX_SELF_IMAGE="$REGISTRY/$REPO_SELF"

# region cast
ocx self setup --managed-config internal.company.com/ocx-config:user --no-modify-path
# endregion cast

grep -q '\[managed\]' "$OCX_HOME/config.toml"
