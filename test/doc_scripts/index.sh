#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# title: Browsing the index
# doc: getting-started/index
set -euo pipefail
# region cast
ocx index catalog
ocx index list "$REPO_CORRETTO"
# endregion cast
