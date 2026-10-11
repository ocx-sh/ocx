#!/usr/bin/env bash
# state: setup:basic
# doc: install-ocx/version-verbose
# title: Show the build channel
# description: Print the verbose version, including the release channel.
set -euo pipefail

# region cast
ocx version --verbose
# endregion cast
