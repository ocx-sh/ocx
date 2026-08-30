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
# Capture first, match second. A producer piped into `head -1` or `grep -q` is
# killed by SIGPIPE the moment the consumer leaves, and `pipefail` reports that
# as 141 — a race that passes whenever the producer's output is small enough to
# fit the pipe buffer before the consumer exits, and fails when it grows.
dev_json=$(ocx --format json package inspect --resolve -p linux/amd64 "$PKG_ACME_MYTOOL")
live_json=$(ocx --format json package inspect --resolve -p linux/amd64 "$PROD/$REPO_ACME_MYTOOL:1.4.2")
dev=$(grep -o -m1 'sha256:[0-9a-f]\{64\}' <<<"$dev_json")
live=$(grep -o -m1 'sha256:[0-9a-f]\{64\}' <<<"$live_json")
test -n "$dev"
test "$dev" = "$live"

# A repeat of a finished promotion is a no-op, not a re-upload.
repeat_json=$(ocx --format json package copy --to "$PROD" "$STAGING/$REPO_ACME_MYTOOL:1.4.2")
grep -q '"disposition": *"unchanged"' <<<"$repeat_json"
