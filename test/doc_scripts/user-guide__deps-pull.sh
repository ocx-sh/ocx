#!/usr/bin/env bash
# state: setup:dependencies
# doc: user-guide/deps-pull
# title: Pull a package with its dependency closure
# description: Download a package and all its declared dependencies into the object store.
set -euo pipefail

ocx package pull "$PKG_WEBAPP"
