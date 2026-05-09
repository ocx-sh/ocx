#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline exec re-assembles package from cache after rm -rf $OCX_HOME/packages
# description: Bug-fix regression. The package install creates blob/layer caches
#   under $OCX_HOME/{blobs,layers}/. After deleting only the assembled
#   $OCX_HOME/packages/ tree, --offline exec must reuse the caches to re-materialise
#   the package without any registry hop.
set -euo pipefail

ocx install --select "$PKG_HELLO"

# Sanity: package and blobs both present after install.
[[ -d "$OCX_HOME/packages" ]] || { echo "expected packages dir after install" >&2; exit 1; }
[[ -d "$OCX_HOME/blobs"    ]] || { echo "expected blobs dir after install"   >&2; exit 1; }

rm -rf "$OCX_HOME/packages"

out="$(ocx --offline exec "$PKG_HELLO" -- hello)"
if [[ "$out" != *"$MARKER_HELLO"* ]]; then
    echo "expected marker '$MARKER_HELLO' in offline-rebuilt exec output, got: $out" >&2
    exit 1
fi
if [[ "$out" != *"$MARKER_HELLO"* ]]; then
    echo "expected marker '$MARKER_HELLO' in offline-rebuilt exec output, got: $out" >&2
    exit 1
fi
