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

from src.registry import delete_manifest, get_manifest, list_referrers, push_manifest
from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor, FakeSigstoreStack

# Sigstore bundle v0.3 artifact type — mirrors the Rust constant
# `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"


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


# ──────────────────────────────────────────────────────────────────────────────
# ANY-of key rotation — a later valid referrer is reached past a wrong one
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_any_of_rotation_reaches_valid_referrer(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_sigstore_stack: FakeSigstoreStack,
) -> None:
    """Two referrers on one subject — a wrong identity + the valid one → verify passes.

    Sign the same subject twice under two different identities (per-signer
    idempotency keeps both bundles). Verify against the *expected* identity must
    succeed: the pipeline's ANY-of loop has to reach the second (valid) referrer
    rather than fail on the first (rotated-away) one. Without ANY-of, a
    non-matching first candidate would mask the valid signature. The referrer
    count is asserted first so the test genuinely exercises rotation (two
    candidates), not a single trivially-valid signature.
    """
    pkg = published_package
    fulcio_rekor = ("--fulcio-url", fake_fulcio.url, "--rekor-url", fake_rekor.url)

    # Referrer #1 — a rotated-away / non-matching identity.
    wrong = subprocess.run(
        [str(ocx.binary), "--format", "json", "package", "sign", *fulcio_rekor,
         "--platform", "linux/amd64", pkg.short],
        capture_output=True, text=True,
        env={**ocx.env, "OCX_IDENTITY_TOKEN": fake_sigstore_stack.oidc_token(subject="rotated-away@example.com")},
    )
    assert wrong.returncode == 0, f"wrong-identity sign failed: {wrong.stderr}"
    subject_digest = json.loads(wrong.stdout)["data"]["subject_digest"]

    # Referrer #2 — the valid, expected identity.
    good = subprocess.run(
        [str(ocx.binary), "--format", "json", "package", "sign", *fulcio_rekor,
         "--platform", "linux/amd64", pkg.short],
        capture_output=True, text=True,
        env={**ocx.env, "OCX_IDENTITY_TOKEN": fake_sigstore_stack.oidc_token()},
    )
    assert good.returncode == 0, f"valid-identity sign failed: {good.stderr}"

    # Precondition: two distinct bundle referrers now hang off the subject, so the
    # verify below actually exercises ANY-of (not a single valid candidate).
    status, index = list_referrers(ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03)
    assert status == 200, f"referrers listing failed with status {status}"
    bundles = [m for m in (index or {}).get("manifests", []) if m.get("artifactType") == SIGSTORE_BUNDLE_V03]
    assert len(bundles) >= 2, (
        f"rotation needs two candidate referrers on {subject_digest}, found {len(bundles)}: {bundles}"
    )

    verify = subprocess.run(
        [str(ocx.binary), "package", "verify",
         "--certificate-identity", "test-signer@example.com",
         "--certificate-oidc-issuer", "https://fake-oidc.test",
         "--rekor-url", fake_rekor.url, "--platform", "linux/amd64", pkg.short],
        capture_output=True, text=True,
        env={**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)},
    )
    assert verify.returncode == 0, (
        f"ANY-of verify must reach the valid second referrer past the wrong first one, "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Cross-subject splice — a valid bundle re-attached to a foreign subject → 65
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_spliced_bundle_onto_foreign_subject_rejected(
    ocx: OcxRunner,
    published_two_versions: tuple[PackageInfo, PackageInfo],
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A valid bundle spliced onto a DIFFERENT subject must fail verify (exit 65).

    Registry surgery: sign v1 and v2, then delete v2's own referrer and attach
    v1's (valid) bundle as v2's only referrer — re-pointing the referrer's
    ``subject`` to v2's digest while its bundle still binds v1's digest. Verify
    v2 must reject it: the bundle's ``messageSignature.messageDigest`` binds v1,
    not the v2 subject being verified, so the subject-binding check fails closed
    with ``SignatureInvalid`` (65). This is the acceptance-level counterpart to
    the unit ``transparency_body_binding_rejects_spliced_subject`` test — a bundle
    lifted from one artifact cannot be laundered onto another.
    """
    v1, v2 = published_two_versions
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}

    def _sign(pkg: PackageInfo) -> str:
        result = subprocess.run(
            [str(ocx.binary), "--format", "json", "package", "sign",
             "--fulcio-url", fake_fulcio.url, "--rekor-url", fake_rekor.url,
             "--platform", "linux/amd64", pkg.short],
            capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0, f"sign setup failed for {pkg.short}: {result.stderr}"
        return json.loads(result.stdout)["data"]["referrer_digest"]

    referrer_v1 = _sign(v1)
    referrer_v2 = _sign(v2)

    manifest_v1 = get_manifest(ocx.registry, v1.repo, referrer_v1)  # subject=v1, layers[0]=v1 bundle
    manifest_v2 = get_manifest(ocx.registry, v2.repo, referrer_v2)  # carries the exact v2 subject descriptor

    # Splice: v1's bundle referrer, re-pointed at v2's subject.
    spliced = dict(manifest_v1)
    spliced["subject"] = manifest_v2["subject"]
    delete_manifest(ocx.registry, v2.repo, referrer_v2)  # drop v2's own valid referrer
    push_manifest(ocx.registry, v2.repo, spliced)        # v2 now has only the spliced one

    verify = subprocess.run(
        [str(ocx.binary), "package", "verify",
         "--certificate-identity", "test-signer@example.com",
         "--certificate-oidc-issuer", "https://fake-oidc.test",
         "--rekor-url", fake_rekor.url, "--platform", "linux/amd64", v2.short],
        capture_output=True, text=True,
        env={**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)},
    )
    assert verify.returncode == 65, (
        f"a bundle spliced onto a foreign subject must fail verify with exit 65, "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Malformed-referrer DoS — a junk candidate must not block the valid signature
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_malformed_referrer_does_not_block_valid_one(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A junk Sigstore-typed referrer alongside a valid one → verify still passes.

    Sign normally, then push a second referrer of the same subject whose bundle
    layer is unparseable garbage (same ``artifactType`` so it IS a candidate).
    The ANY-of loop must treat the junk candidate as one failed verdict
    (``BundleParseFailed``) and go on to the valid referrer — an unparseable
    first candidate cannot deny service to a genuine signature. Without ANY-of a
    malformed candidate could mask the valid one.
    """
    from src.registry import push_referrer

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
        [str(ocx.binary), "--format", "json", "package", "sign",
         "--fulcio-url", fake_fulcio.url, "--rekor-url", fake_rekor.url,
         "--platform", "linux/amd64", pkg.short],
        capture_output=True, text=True, env=env,
    )
    assert sign.returncode == 0, f"valid sign failed: {sign.stderr}"
    data = json.loads(sign.stdout)["data"]
    subject_digest = data["subject_digest"]

    # The valid referrer carries the exact subject descriptor; reuse its size for
    # the junk referrer so both attach to the same subject.
    valid_manifest = get_manifest(ocx.registry, pkg.repo, data["referrer_digest"])
    subject_size = valid_manifest["subject"]["size"]
    push_referrer(
        ocx.registry, pkg.repo, subject_digest, subject_size,
        artifact_type=SIGSTORE_BUNDLE_V03,
        payload=b"this is not a valid sigstore bundle at all",
    )

    verify = subprocess.run(
        [str(ocx.binary), "package", "verify",
         "--certificate-identity", "test-signer@example.com",
         "--certificate-oidc-issuer", "https://fake-oidc.test",
         "--rekor-url", fake_rekor.url, "--platform", "linux/amd64", pkg.short],
        capture_output=True, text=True,
        env={**ocx.env, "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)},
    )
    assert verify.returncode == 0, (
        f"a malformed referrer must not block the valid signature (ANY-of), "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )
