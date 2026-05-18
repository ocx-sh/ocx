#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/exec-once
# title: Run a tool once without installing
# description: Execute a package on demand without creating a persistent candidate symlink.
set -euo pipefail

ocx package exec "$PKG_CMAKE" -- cmake --version
