#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/fresh-clone
# title: Warm the store after a fresh clone
# description: Pull every locked tool into the local object store so direnv can activate the environment without a registry round-trip.
set -euo pipefail

cd "$SCENARIO_TMP"
# Set up a project so `ocx pull` has a lock to materialize from. In a real
# fresh clone this state is already on disk.
ocx init
ocx add --no-pull "$PKG_CMAKE"
# region cast
ocx pull
# endregion cast
