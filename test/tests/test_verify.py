# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package verify`` (Slice 1 — referrers verify).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md``
(specifically C-S1-1 frozen envelope + C-S1-2 VerifyErrorKind variant set) and
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

Trust-root seam: every verify subprocess that must succeed (or reach crypto)
sets ``OCX_SIGSTORE_TRUST_ROOT`` to the fake Fulcio CA (``fake_fulcio.root_pem``)
so the leaf cert chain validates against the fake root rather than a real
Sigstore trust bundle.
"""
from __future__ import annotations

import base64
import json
import subprocess

from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor


# ──────────────────────────────────────────────────────────────────────────────
# Identity mismatch — exit 77 (PermissionDenied)
# ──────────────────────────────────────────────────────────────────────────────


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
    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityMismatch), "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


def test_verify_issuer_mismatch_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Cert-issuer mismatch → exit 77. Distinct variant, same code as identity."""
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

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (IssuerMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# No signatures found — exit 79 (NotFound)
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_no_signatures_exits_79(
    ocx: OcxRunner, published_package: PackageInfo, fake_fulcio: FakeFulcio
) -> None:
    """A package with no referrers → exit 79.

    C-S1-2: ``NoSignaturesFound`` maps to 79 so CI scripts can distinguish
    "not signed" (retryable: sign first) from "bad signature" (terminal) via
    ``$?`` alone. Fails before reaching crypto, so the trust-root env is
    harmless here — added for consistency with every other verify call.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=env,
    )
    assert result.returncode == 79, (
        f"expected exit 79 (NotFound / NoSignaturesFound), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — no referrers API → exit 84
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_referrers_unsupported_exits_84(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path,
    fake_fulcio: FakeFulcio,
) -> None:
    """Registry without referrers API → exit 84.

    ``legacy_registry`` (``registry:2``, #106/#195 negative fixture) does not
    implement ``/v2/<name>/referrers/``. Discovery must fail hard — silently
    returning an empty result set when the registry doesn't support the
    endpoint would masquerade as ``NoSignaturesFound``, muddying the
    exit-code contract.
    """
    from src.helpers import make_package

    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    env = {**legacy_ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=env,
    )
    assert result.returncode == 84, (
        f"expected exit 84 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# JSON envelope golden contract — error + success branches
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_error_envelope_golden_shape(
    ocx: OcxRunner, published_package: PackageInfo, fake_fulcio: FakeFulcio
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
    env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=env,
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

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
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


def test_verify_detects_tampered_rekor_set(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A tampered Rekor SET → exit 65 (DataError), not exit 83.

    RekorSetInvalid is a data-integrity failure (the bundle has been
    altered) — retry will not help, so it must map to ``DataError`` not
    ``RekorUnavailable``. The tamper toggle is set BEFORE signing so the
    produced bundle carries the bad SET (see ``FakeRekor.set_tampered_set``
    docstring).
    """
    pkg = published_package
    fake_rekor.set_tampered_set(True)

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

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / RekorSetInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Tampered bundle signature — exit 65 (DataError / SignatureInvalid)
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_detects_tampered_bundle_signature_exits_65(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Flip a byte in the published bundle blob → exit 65 (SignatureInvalid).

    The bundle is content-addressed, so this is registry surgery: sign
    normally, fetch the referrer manifest + its bundle-blob layer, corrupt
    ``messageSignature.signature`` by flipping one byte, push the corrupted
    blob under a new digest, then DELETE the original referrer manifest and
    push a replacement pointing at the corrupted blob — so exactly one
    referrer exists for the subject and it is the tampered one.
    """
    from src.registry import delete_manifest, get_blob, get_manifest, push_blob, push_manifest

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
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
    referrer_digest = json.loads(sign.stdout)["data"]["referrer_digest"]

    manifest = get_manifest(ocx.registry, pkg.repo, referrer_digest)
    bundle_layer = manifest["layers"][0]
    bundle = json.loads(get_blob(ocx.registry, pkg.repo, bundle_layer["digest"]))
    signature = bytearray(base64.b64decode(bundle["messageSignature"]["signature"]))
    signature[0] ^= 0xFF  # flip a byte — deterministically invalidates the signature
    bundle["messageSignature"]["signature"] = base64.b64encode(bytes(signature)).decode()
    corrupted_bytes = json.dumps(bundle).encode()

    new_blob_digest = push_blob(ocx.registry, pkg.repo, corrupted_bytes)
    manifest["layers"][0] = {**bundle_layer, "digest": new_blob_digest, "size": len(corrupted_bytes)}
    delete_manifest(ocx.registry, pkg.repo, referrer_digest)
    push_manifest(ocx.registry, pkg.repo, manifest)

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / SignatureInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Invalid cert chain — exit 65 (DataError / CertChainInvalid)
# ──────────────────────────────────────────────────────────────────────────────


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
    ``CertChainInvalid``.
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

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / CertChainInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rekor unavailable during verify — exit 83
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_rekor_unavailable_exits_83(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Fake Rekor returns 503 during the verify SET lookup → exit 83.

    Distinguished from ``RekorSetInvalid`` (exit 65) because retry MAY help
    here — the service is transiently down, not a crypto failure.
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

    verify_env = {**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)}
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
        env=verify_env,
    )
    assert verify.returncode == 83, (
        f"expected exit 83 (RekorUnavailable), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
