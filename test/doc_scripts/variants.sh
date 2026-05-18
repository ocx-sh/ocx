#!/usr/bin/env bash
# state: setup:variants
# cast: true
# title: Working with variants
# doc: user-guide/variants
set -euo pipefail
# region cast
ocx index list "$REPO_PYTHON" --variants
ocx package install "$REPO_PYTHON:debug-3.13.0"
ocx package exec "$REPO_PYTHON:debug-3.13.0" -- python3 --version
# endregion cast
