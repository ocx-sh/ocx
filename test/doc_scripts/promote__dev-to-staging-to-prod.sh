#!/usr/bin/env bash
# state: setup:promotion
# cast: true
# title: Promoting one build from dev to staging to production
# doc: user-guide/promoting-packages
# description: Copy an already-published package along the dev, staging and production registries without rebuilding it, so the platform manifest digest - and anything signed against it - is the same at every hop.
set -euo pipefail

# The setup:promotion provider published acme/mytool:1.4.2 to the dev registry
# and put the two downstream addresses in $STAGING and $PROD. The dev registry
# is the runner's default, which is why the first source is a bare reference.

# region cast
ocx package copy --to "$STAGING" --cascade "$PKG_ACME_MYTOOL"
ocx package copy --to "$PROD" --cascade "$STAGING/$REPO_ACME_MYTOOL:1.4.2"
ocx package inspect --resolve -p linux/amd64 "$PROD/$REPO_ACME_MYTOOL:1.4.2"
# endregion cast

# Verification — outside the displayed region, drift-gated on every run.
# The digest production resolves to must be the one the dev registry published:
# that identity is the whole reason to copy rather than rebuild.
dev=$(ocx --format json package inspect --resolve -p linux/amd64 "$PKG_ACME_MYTOOL" |
    grep -o 'sha256:[0-9a-f]\{64\}' | head -1)
live=$(ocx --format json package inspect --resolve -p linux/amd64 "$PROD/$REPO_ACME_MYTOOL:1.4.2" |
    grep -o 'sha256:[0-9a-f]\{64\}' | head -1)
test -n "$dev"
test "$dev" = "$live"

# A repeat of a finished promotion is a no-op, not a re-upload.
echo "$(ocx --format json package copy --to "$PROD" "$STAGING/$REPO_ACME_MYTOOL:1.4.2")" |
    grep -q '"disposition": *"unchanged"'
