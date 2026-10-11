#!/usr/bin/env bash
# state: setup:basic
# doc: install-ocx/about
# title: Check the installation
# description: Print the installed version and build details.
set -euo pipefail

# region cast
ocx about
# endregion cast
