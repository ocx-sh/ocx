#!/usr/bin/env bash
# state: setup:signing
# cast: true
# title: Attaching an unsigned SBOM and reading it back permissively
# doc: user-guide/attestations-unsigned
# description: Attach a signed CycloneDX SBOM, then attach a second SBOM with no signing identity present, and read both back with ocx package sbom --no-verify — the mode a consumer with no Sigstore setup reaches for.
set -euo pipefail

# The setup:signing provider has already published acme/mytool, brought up
# the local Sigstore stack, written a [[trust.policy]] pin SCOPED TO THIS
# PACKAGE in config.toml, and put the dex identity token in
# OCX_IDENTITY_TOKEN and the stack's CA in OCX_SIGSTORE_TRUSTED_ROOT.
# sbom.json (CycloneDX) and spdx.json (SPDX-JSON) are both written by the
# provider (recordings/setups.py) before this region runs (CA5: the recorder
# only replays cast-region lines).
#
# Because that policy matches the package, `ocx package sbom` demands
# verification BY DEFAULT here even with no identity flags typed on the
# command line -- dropping the flags is not what selects permissive mode.
# --no-verify is: it is the one flag that outranks a matching policy, which
# is why the listing below carries it explicitly rather than relying on
# omission.

# region cast
ocx package attest -p linux/amd64 --predicate sbom.json --type cyclonedx --fulcio-url "$FULCIO" --rekor-url "$REKOR" "$PKG_ACME_MYTOOL"
unset OCX_IDENTITY_TOKEN
ocx package attest -p linux/amd64 --predicate spdx.json --type spdxjson "$PKG_ACME_MYTOOL"
ocx package sbom -p linux/amd64 --no-verify "$PKG_ACME_MYTOOL"
# endregion cast

# Verification — outside the displayed region, drift-gated on every run.
# Re-listing is read-only and mints nothing, so it is safe evidence.
list_report=$(ocx --format json package sbom -p linux/amd64 --no-verify "$PKG_ACME_MYTOOL")
echo "$list_report" | grep -q '"status": *"success"'
echo "$list_report" | grep -q '"verification": *"unverified"'
echo "$list_report" | grep -q '"verified": *0'
echo "$list_report" | grep -q '"unverified": *2'
echo "$list_report" | grep -q '"predicate_type": *"https://spdx.dev/Document"'
echo "$list_report" | grep -q '"predicate_type": *"https://cyclonedx.org/bom"'

# Red proof this is not a foregone conclusion: the identical package, read
# with the [[trust.policy]] left to decide (no --no-verify), demands
# verification and refuses the unsigned referrer rather than listing it. The
# signed CycloneDX document still verifies, so the command still exits 0 -- a
# refusal beside a match is reported, not raised.
demanded_report=$(ocx --format json package sbom -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_MYTOOL")
echo "$demanded_report" | grep -q '"verification": *"verified"'
echo "$demanded_report" | grep -q '"verified": *1'
echo "$demanded_report" | grep -q '"unverified": *0'
echo "$demanded_report" | grep -q '"reason_kind": *"unsigned_rejected_by_policy"'
