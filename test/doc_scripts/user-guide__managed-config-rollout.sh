#!/usr/bin/env bash
# state: setup:basic
# doc: user-guide/managed-config-rollout
# title: Pin, pause, and resume a managed-config host
# description: Roll a host back to a known-good managed-config version, hold the background tick while debugging, then rejoin the fleet.
set -euo pipefail
cd "$SCENARIO_TMP"

# Prologue (outside the region): the operator has published two versions and
# this host already tracks the floating `:user` tag.
cat >config-old.toml <<'TOML'
[mirrors."ghcr.io"]
url = "https://ghcr-old.corp.example.com"
TOML
cat >config-new.toml <<'TOML'
[mirrors."ghcr.io"]
url = "https://ghcr-new.corp.example.com"
TOML
ocx config push -i corp/ocx-config:user-1.4.1 ./config-old.toml --cascade --new
ocx config push -i corp/ocx-config:user-1.4.2 ./config-new.toml --cascade
export OCX_MANAGED_CONFIG="$REGISTRY/corp/ocx-config:user"
ocx config update

# region cast
# Roll back to a known-good version (any tag, digest, or tag@digest):
ocx config update user-1.4.1

# Hold the background tick for up to 7 days while you debug:
ocx config update --pause 3d user-1.4.1

# Rejoin the fleet:
ocx config update --resume
# endregion cast

# --resume cleared the pause and re-synced to the registry's current state.
[ ! -f "$OCX_HOME/state/managed-config/pause.json" ]
ocx --format json config update --check | grep -q '"status":"already_current"\|"status": "already_current"'
