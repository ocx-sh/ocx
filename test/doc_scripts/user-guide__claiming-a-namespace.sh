#!/usr/bin/env bash
# state: setup:basic
# doc: user-guide/claiming-a-namespace
# cast: true
# title: The claim grammar
# description: Read the grammar ocx package claim accepts. Its argv refusals are exercised outside the displayed region, where they cost no network call and need no credential.
set -euo pipefail

# Why this script shows the grammar rather than a claim: every mode of
# `ocx package claim`, `--out` included, reads the index repository's committed
# entry first (that is what makes an already-claimed namespace exit 65), so
# there is no forge-free run to record. What IS forge-free is the argv-fault
# block, which decides every refusal below from the command line alone --
# before a credential is resolved and before anything is dialled.

# region cast
ocx package claim --help
# endregion cast

# Verification -- outside the displayed region (drift-gated, never shown/cast).
#
# `--transport git` and `--out` cannot both hold: one writes over a clone, the
# other opens no request at all. The refusal is exit 64 and it costs no network
# call, which is the property the troubleshooting section on the page promises.
# `./never-written` is deliberately not created: the refusal lands before any
# `--out` directory is touched, so the name existing would prove less, and no
# temp file is needed to say so.
status=0
ocx package claim \
    --repository oci://ghcr.io/acme/widget \
    --transport git \
    --out ./never-written \
    acme/widget >/dev/null 2>&1 || status=$?
[[ "$status" -eq 64 ]] || {
    echo "ERROR: --transport git with --out must exit 64, got $status" >&2
    exit 1
}

# The same block refuses an `--upstream-*` flag with no `--upstream-org` anchor.
status=0
ocx package claim \
    --repository oci://ghcr.io/acme/widget \
    --upstream-repository-url https://github.com/WidgetProject/widget \
    --out ./never-written \
    acme/widget >/dev/null 2>&1 || status=$?
[[ "$status" -eq 64 ]] || {
    echo "ERROR: --upstream-repository-url without its anchor must exit 64, got $status" >&2
    exit 1
}
