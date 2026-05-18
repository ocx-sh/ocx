#!/usr/bin/env bash
# state: setup:multi-version
# cast: true
# doc: getting-started/env
# title: Package environment
# description: Resolve and display the environment variables declared by a package.
set -euo pipefail

# region cast
ocx package install "$PKG_CORRETTO"
ocx package env "$PKG_CORRETTO"
# endregion cast
