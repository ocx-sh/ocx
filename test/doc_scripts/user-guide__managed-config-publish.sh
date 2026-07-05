#!/usr/bin/env bash
# state: setup:basic
# doc: user-guide/managed-config-publish
# title: Publish a managed-config update
# description: Validate and push a corporate config.toml as an ordinary versioned package with rolling cascade tags.
set -euo pipefail
cd "$SCENARIO_TMP"

# Outside the region: the payload an operator would already have on disk.
# `corp/ocx-config` resolves against the runner's default registry, exactly
# like a registry-less identifier does on an operator machine.
cat >config.toml <<'TOML'
[mirrors."ghcr.io"]
url = "https://ghcr.corp.example.com"
TOML

# region cast
ocx config push -i corp/ocx-config:user-1.4.2 ./config.toml --cascade --new
# endregion cast

# The push validated the payload and wrote the rolling variant tags: a host
# tracking the floating `:user` tag syncs the new content on its next update.
OCX_MANAGED_CONFIG="$REGISTRY/corp/ocx-config:user" ocx config update | grep -q updated
