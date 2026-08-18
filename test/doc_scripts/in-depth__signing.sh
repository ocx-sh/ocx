#!/usr/bin/env bash
# state: setup:signing
# cast: true
# title: Signing a package and verifying it against a pinned identity
# doc: in-depth/signing
# description: Keyless-sign a published package against a self-hosted Sigstore stack, verify it against a [[trust.policy]] pin, then verify again offline from pinned trust material.
set -euo pipefail

# The setup:signing provider has already published acme/mytool, brought up the
# local Sigstore stack (dex, Fulcio, Rekor, TesseraCT, Trillian), written the
# [[trust.policy]] pin that makes the verify lines flag-free, and put the
# dex identity token in OCX_IDENTITY_TOKEN and the stack's CA in
# OCX_SIGSTORE_TUF_ROOT — the two channels a CI job uses, which is why the
# lines below carry neither flag.
# $FULCIO / $REKOR address the stack; the docs page carries the concrete values.

# region cast
ocx package sign -p linux/amd64 --fulcio-url "$FULCIO" --rekor-url "$REKOR" "$PKG_ACME_MYTOOL"
ocx package verify -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_MYTOOL"
ocx --offline package verify -p linux/amd64 "$PKG_ACME_MYTOOL"
# endregion cast

# Verification — outside the displayed region, drift-gated on every run.
report=$(ocx --format json package verify -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_MYTOOL")
echo "$report" | grep -q '"certificate_identity": *"ocx-test@example.com"'
