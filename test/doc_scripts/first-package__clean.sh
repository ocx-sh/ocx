#!/usr/bin/env bash
# state: setup:node-versions
# doc: first-package/clean
# title: Remove the other version and clean up
# description: Uninstall a version, then delete every package nothing references.
set -euo pipefail

ocx package install $REPO_NODEJS_NODE:24
# region cast
ocx package uninstall $REPO_NODEJS_NODE:24
ocx clean
# endregion cast
