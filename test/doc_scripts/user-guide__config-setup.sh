#!/usr/bin/env bash
# state: setup:managed-config-onboard
# doc: user-guide/config-setup
# title: Adopt managed config without a full install
# description: Configuration-only adoption of the [managed] tier for automation hosts - no binary bootstrap, no env shims, no shell profiles.
set -euo pipefail

# The setup:managed-config-onboard provider has already published the
# managed-config artifact and aliased "internal.company.com" (via [mirrors])
# to the local test registry. `ocx config setup` runs the identical adoption
# sequence as `ocx self setup --managed-config` but skips the binary
# bootstrap, env shims, and shell profiles entirely.

# region cast
ocx config setup --managed-config internal.company.com/ocx-config:user
# endregion cast

grep -q '\[managed\]' "$OCX_HOME/config.toml"
test -f "$OCX_HOME/state/managed-config/snapshot.json"
