# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package verify`` (Slice 1 — referrers verify).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md``
(specifically C-S1-1 frozen envelope + C-S1-2 VerifyErrorKind variant set) and
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

Test strategy
=============

- **Envelope golden tests** run today against the unimplemented CLI and
  ``xfail(strict=True)`` until Phase 5. They pin byte-level v1 contract shape.
- **Signer-mismatch + unknown-signer tests** depend on ``fake_fulcio``
  minting leaf certs with controllable SANs; xfail until fixtures land.
- **No-signatures tests** can potentially run today (registry has no
  referrers at all) but exit 79 requires the error classifier to route
  ``VerifyErrorKind::NoSignaturesFound`` correctly — xfail until Phase 5.
"""
from __future__ import annotations

import json
import subprocess

import pytest

from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor


# ──────────────────────────────────────────────────────────────────────────────
# Identity mismatch — exit 77 (PermissionDenied)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: SignPipeline::run and VerifyPipeline::run not yet implemented",
)
def test_verify_unknown_signer_fails_identity_mismatch(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Sign with signer A; verify against signer B → exit 77.

    C-S1-2: ``IdentityMismatch`` is the "verified, but not by the signer you
    expected" signal. Distinct from ``NoSignaturesFound`` (79) — the bundle
    exists and cryptographically verifies, but the cert SAN doesn't match the
    caller's ``--certificate-identity``.
    Xfails until Phase 5c wires both Rust pipelines.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    # First publish a signature as signer A (the token's claim).
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0, f"sign setup failed: {sign.stderr}"

    # Now verify as signer B — different identity, same bundle.
    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "someone-else@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityMismatch), "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: SignPipeline::run and VerifyPipeline::run not yet implemented",
)
def test_verify_issuer_mismatch_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Cert-issuer mismatch → exit 77. Distinct variant, same code as identity.

    Xfails until Phase 5c wires both Rust pipelines.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0

    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://wrong-issuer.example",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (IssuerMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# No signatures found — exit 79 (NotFound)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5: unsigned package yields VerifyErrorKind::NoSignaturesFound → "
    "exit 79; stub currently panics with unimplemented!()",
)
def test_verify_no_signatures_exits_79(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A package with no referrers → exit 79.

    C-S1-2: ``NoSignaturesFound`` maps to 79 so CI scripts can distinguish
    "not signed" (retryable: sign first) from "bad signature" (terminal) via
    ``$?`` alone.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "anyone@example.com",
            "--certificate-oidc-issuer", "https://anywhere.example",
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 79, (
        f"expected exit 79 (NotFound / NoSignaturesFound), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — no referrers API → exit 83
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5: registry:2 does not implement /v2/<name>/referrers/; "
    "verify must detect 404 via capability probe and exit 83",
)
def test_verify_referrers_unsupported_exits_83(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Registry without referrers API → exit 83.

    Discovery must fail hard — silently returning an empty result set when
    the registry doesn't support the endpoint would masquerade as
    ``NoSignaturesFound``, muddying the exit-code contract.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "anyone@example.com",
            "--certificate-oidc-issuer", "https://anywhere.example",
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 83, (
        f"expected exit 83 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# JSON envelope golden contract — error + success branches
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5: render_error_envelope is unimplemented; error branch emits "
    "v1 schema with `error.kind=not_found` and `exit_code=79` for unsigned pkg",
)
def test_verify_error_envelope_golden_shape(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Error-branch JSON envelope matches frozen v1 contract (C-S1-1).

    Shape check (order-independent, key-presence):
    - Root keys: ``schema_version``, ``command``, ``exit_code``, ``error``.
    - ``error.kind`` is ``not_found`` for an unsigned package.
    - ``error.message`` is non-empty.
    - ``error.context`` is a JSON object (may be empty).
    - No ``data`` key on error branches.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "verify",
            "--certificate-identity", "anyone@example.com",
            "--certificate-oidc-issuer", "https://anywhere.example",
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode != 0, "unsigned package must fail verify"
    envelope = json.loads(result.stdout or result.stderr)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package verify"
    assert envelope["exit_code"] == 79
    assert "data" not in envelope, "error branch must not carry data"
    error = envelope["error"]
    assert error["kind"] == "not_found"
    assert isinstance(error["message"], str) and error["message"]
    assert isinstance(error["context"], dict)


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5: success-branch envelope emits v1 shape with top-level data "
    "wrapping VerifyResult (subject_digest + referrer_digest + cert identity)",
)
def test_verify_success_envelope_golden_shape(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Success-branch JSON envelope matches frozen v1 contract.

    Shape check:
    - Root keys: ``schema_version``, ``command``, ``exit_code``, ``data``.
    - ``exit_code`` is 0 on success.
    - ``data.subject_digest`` and ``data.referrer_digest`` start with ``sha256:``.
    - ``data.certificate_identity`` and ``data.certificate_oidc_issuer`` present.
    - No ``error`` key on success branches.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0

    verify = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 0, verify.stderr
    envelope = json.loads(verify.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package verify"
    assert envelope["exit_code"] == 0
    assert "error" not in envelope, "success branch must not carry error"
    data = envelope["data"]
    assert data["subject_digest"].startswith("sha256:")
    assert data["referrer_digest"].startswith("sha256:")
    assert data["certificate_identity"] == "test-signer@example.com"
    assert data["certificate_oidc_issuer"] == "https://fake-oidc.test"


# ──────────────────────────────────────────────────────────────────────────────
# Tampered Rekor SET — exit 65 (DataError)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: VerifyPipeline SET canonicalization not yet wired",
)
def test_verify_detects_tampered_rekor_set(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A tampered Rekor SET → exit 65 (DataError), not exit 82.

    Locks the TODO at ``fake_sigstore.py:551-562``. RekorSetInvalid is a
    data-integrity failure (the bundle has been altered) — retry will not
    help, so it must map to ``DataError`` not ``RekorUnavailable``. Phase 5c
    must canonicalize the SET payload in a way that catches single-bit
    tampering during ``VerifyPipeline::run``.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0, sign.stderr

    # Toggle the fake Rekor into tampered-SET mode (Phase 5c adds the toggle).
    fake_rekor.set_tampered_set(True)  # type: ignore[attr-defined]

    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / RekorSetInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Tampered bundle signature — exit 65 (DataError / SignatureInvalid)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: VerifyPipeline::run unimplemented",
)
def test_verify_detects_tampered_bundle_signature_exits_65(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
    tmp_path,
) -> None:
    """Flip a byte in the published bundle blob → exit 65 (SignatureInvalid).

    The bundle is content-addressed, so altering the OCI blob will fail the
    subject-digest signature check. Phase 5c implements the full verify
    pipeline; pre-5c the test xfails strictly.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0, sign.stderr

    # Phase 5c wires the bundle-tamper path. The probe — flipping a single
    # byte in the bundle blob and re-pushing — runs at Phase 5c time.

    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / SignatureInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Invalid cert chain — exit 65 (DataError / CertChainInvalid)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: VerifyPipeline::run unimplemented",
)
def test_verify_invalid_cert_chain_exits_65(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Fulcio returns a cert chain that fails validation → exit 65.

    Toggles ``fake_fulcio.set_invalid_chain(True)`` before signing so the
    bundle carries a chain that the verify pipeline rejects via
    ``CertChainInvalid``. Phase 5c implements the trust-root chain check;
    pre-5c xfail strictly.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    fake_fulcio.set_invalid_chain(True)
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0, sign.stderr

    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / CertChainInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rekor unavailable during verify — exit 82
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5c: VerifyPipeline::run unimplemented",
)
def test_verify_rekor_unavailable_exits_82(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Fake Rekor returns 503 during the verify SET lookup → exit 82.

    Distinguished from ``RekorSetInvalid`` (exit 65) because retry MAY help
    here — the service is transiently down, not a crypto failure. Phase 5c
    implements the SET fetch + classification.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert sign.returncode == 0, sign.stderr

    from tests.fixtures.fake_sigstore import HttpStatus
    fake_rekor.set_failure_mode(HttpStatus(503))

    verify = subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify.returncode == 82, (
        f"expected exit 82 (RekorUnavailable), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
