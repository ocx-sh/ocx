#!/usr/bin/env bash
# state: setup:cosign-parity
# cast: true
# title: Signing with one tool and verifying with the other, in both directions
# doc: in-depth/cosign-parity
# description: Keyless-sign a package with ocx and verify it with upstream cosign, then sign a second package with cosign and verify it with ocx — same Fulcio, same Rekor, same registry.
set -euo pipefail

# The setup:cosign-parity provider has published acme/ocx-signed and
# acme/cosign-signed, brought up the local Sigstore stack (dex, Fulcio, Rekor,
# TesseraCT, Trillian), put the pinned upstream cosign on PATH as a plain
# binary, written the [[trust.policy]] that covers both repositories, and
# staged cosign's trust material in this directory.
# $FULCIO / $REKOR address the stack; the docs page carries the concrete values.
#
# $MANIFEST_OCX_SIGNED / $MANIFEST_COSIGN_SIGNED are `<registry>/<repo>@<digest>`
# references to each package's linux/amd64 manifest. cosign resolves a reference
# itself, and a package's tag resolves to its *index*, where no signature lives —
# `ocx package sign` signs the platform manifest under it. ocx is handed the tag
# plus `-p`, which is the same subject reached the other way round.
#
# The asymmetry in the flags is real, not cosmetic: ocx reads the pinned identity
# from [[trust.policy]], cosign has no policy file and must be told on the line.

# region cast
ocx package sign -p linux/amd64 --fulcio-url "$FULCIO" --rekor-url "$REKOR" "$PKG_ACME_OCX_SIGNED"
cosign verify --allow-http-registry --trusted-root trusted_root.json --certificate-identity ocx-test@example.com --certificate-oidc-issuer http://dex:5556/dex "$MANIFEST_OCX_SIGNED"
cosign sign --allow-http-registry --signing-config signing-config.json --trusted-root trusted_root.json --identity-token identity-token --yes "$MANIFEST_COSIGN_SIGNED"
ocx package verify -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_COSIGN_SIGNED"
# endregion cast

# Verification — outside the displayed region, drift-gated on every run.
#
# The two verifies are re-run here so their reports can be asserted on: inside
# the region only an exit code is observable, and a cast whose commands merely
# returned 0 is not evidence of interop. Both are read-only and idempotent.
cosign_report=$(cosign verify --allow-http-registry --trusted-root trusted_root.json \
    --certificate-identity ocx-test@example.com \
    --certificate-oidc-issuer http://dex:5556/dex "$MANIFEST_OCX_SIGNED" 2>&1)
echo "$cosign_report" | grep -q "The code-signing certificate was verified using trusted certificate authority certificates"
echo "$cosign_report" | grep -q "Existence of the claims in the transparency log was verified offline"

# acme/cosign-signed is never signed by ocx, so a signature ocx finds and
# accepts there is cosign's — which is what makes this half discriminating
# rather than a second reading of ocx's own work.
ocx_report=$(ocx --format json package verify -p linux/amd64 --rekor-url "$REKOR" "$PKG_ACME_COSIGN_SIGNED")
echo "$ocx_report" | grep -q '"certificate_identity": *"ocx-test@example.com"'
echo "$ocx_report" | grep -q '"rekor_log_index"'
