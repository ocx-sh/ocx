#!/usr/bin/env bash
# state: setup:basic
# doc: install-ocx/version-json
# title: Read the build channel as JSON
# description: Print the version as a JSON document for scripts.
set -euo pipefail

# region cast
ocx --format json version
# endregion cast
