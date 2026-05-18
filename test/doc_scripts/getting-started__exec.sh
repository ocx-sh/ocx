#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: getting-started/exec
# title: Run a package once
# description: Execute a package without installing it; the binary is fetched on demand.
set -euo pipefail

# region cast
ocx package exec "$PKG_UV" -- uv --version
# endregion cast
