#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline which fails when package was never installed
set -euo pipefail

# Do NOT install the published package. --offline which must fail.
if ocx --offline which "$PKG_HELLO"; then
    echo "expected --offline which to fail for uninstalled package" >&2
    exit 1
fi
