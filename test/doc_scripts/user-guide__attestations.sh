#!/usr/bin/env bash
# state: setup:signing
# cast: true
# title: Attaching a signed SBOM attestation and reading it back
# doc: user-guide/attestations
# description: Attach a CycloneDX SBOM to a published package as a signed in-toto attestation, then list and extract it back through the same verification pipeline signatures use.
set -euo pipefail

# The setup:signing provider has already published acme/mytool, brought up
# the local Sigstore stack (dex, Fulcio, Rekor, TesseraCT, Trillian), written
# the [[trust.policy]] pin, and put the dex identity token in
# OCX_IDENTITY_TOKEN and the stack's CA in OCX_SIGSTORE_TRUSTED_ROOT — the two
# channels a CI job uses, which is why the lines below carry neither flag.
# $FULCIO / $REKOR address the stack; the docs page carries the concrete
# values.

# sbom.json is written by the setup:signing provider (recordings/setups.py)
# rather than a heredoc here: CA5 only replays lines inside the cast region,
# so a pre-region shell command would never run during cast recording.

# region cast
ocx package attest -p linux/amd64 --predicate sbom.json --type cyclonedx --fulcio-url "$FULCIO" --rekor-url "$REKOR" "$PKG_ACME_MYTOOL"
ocx package sbom -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_MYTOOL"
ocx package sbom -p linux/amd64 --rekor-url "$REKOR" --output extracted-sbom.json "$PKG_ACME_MYTOOL"
# endregion cast

# Verification — outside the displayed region, drift-gated on every run.
# A second `sbom` listing is read-only and does not mint a second
# attestation, so it is safe to re-run for evidence without disturbing the
# one-attestation state the extraction above relied on.
list_report=$(ocx --format json package sbom -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_MYTOOL")
echo "$list_report" | grep -q '"status": *"success"'
echo "$list_report" | grep -q '"predicate_type": *"https://cyclonedx.org/bom"'
echo "$list_report" | grep -q '"certificate_identity": *"ocx-test@example.com"'

cmp -s sbom.json extracted-sbom.json
