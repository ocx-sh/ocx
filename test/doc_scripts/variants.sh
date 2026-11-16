#!/usr/bin/env bash
# state: setup:variants
# cast: true
# title: Working with variants
# doc: user-guide/variants
set -euo pipefail
# region cast
ocx index list "$REPO_ASTRAL_SH_PYTHON_BUILD_STANDALONE" --variants
ocx package install "$REPO_ASTRAL_SH_PYTHON_BUILD_STANDALONE:slim-3.14.7"
ocx package exec "$REPO_ASTRAL_SH_PYTHON_BUILD_STANDALONE:slim-3.14.7" -- python3 --version
# endregion cast
