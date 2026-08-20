# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package verify`` (Slice 1 — referrers verify).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md``
(specifically C-S1-1 frozen envelope + C-S1-2 VerifyErrorKind variant set) and
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

Trust-root seam: verify runs against the local stack's trusted-root JSON
(``--sigstore-trusted-root``), which carries the Fulcio CA and the pinned Rekor key. The
one exception is the Rekor-unavailable test, which supplies a document with no
pinned Rekor key so the key fetch actually happens and can fail.
"""
from __future__ import annotations

import base64
import json
import os
import subprocess
from pathlib import Path

from src.registry import delete_manifest, get_manifest, list_referrers, push_manifest
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack

# Sigstore bundle v0.3 artifact type — mirrors the Rust constant
# `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> dict:
    """Sign ``pkg`` against the real stack; return the JSON envelope's ``data``."""
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True, text=True, env=ocx.env,
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"
    return json.loads(result.stdout)["data"]


def _verify(
    ocx: OcxRunner,
    stack: SigstoreStack,
    pkg: PackageInfo,
    *,
    identity: str | None = None,
    issuer: str | None = None,
    rekor_url: str | None = None,
    trusted_root: Path | None = None,
    platform: str | None = None,
    json_format: bool = False,
    attestation: bool = False,
    predicate_type: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``package verify``, defaulting every knob to the stack's own values.

    ``trusted_root`` points at a different trusted-root document — the way to
    vary what the trust material carries without losing the CT log key.

    ``attestation`` / ``predicate_type`` select the DSSE search and narrow it.
    Passing ``predicate_type`` without ``attestation`` is rejected by clap
    (``requires``), so the two are spelled together at every call site.
    """
    env = dict(ocx.env)
    root = ["--sigstore-trusted-root", str(trusted_root or stack.trust_root)]
    return subprocess.run(
        [
            str(ocx.binary),
            *(["--format", "json"] if json_format else []),
            "package", "verify",
            *(["--attestation"] if attestation else []),
            *(["--type", predicate_type] if predicate_type else []),
            "--platform", platform or current_platform(),
            "--rekor-url", rekor_url or stack.rekor_url,
            *root,
            "--certificate-identity", identity or stack.identity,
            "--certificate-oidc-issuer", issuer or stack.issuer,
            pkg.short,
        ],
        capture_output=True, text=True, env=env,
    )


# ──────────────────────────────────────────────────────────────────────────────
# Identity mismatch — exit 77 (PermissionDenied)
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_unknown_signer_fails_identity_mismatch(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Sign with signer A; verify against signer B → exit 77.

    C-S1-2: ``IdentityMismatch`` is the "verified, but not by the signer you
    expected" signal. Distinct from ``NoSignaturesFound`` (79) — the bundle
    exists and cryptographically verifies, but the cert SAN doesn't match the
    caller's ``--certificate-identity``.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(
        ocx, sigstore_stack, pkg, identity="someone-else@example.com", json_format=True,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityMismatch), "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )
    # The exit code alone cannot tell this from IssuerMismatch (also 77) or an
    # unrelated PermissionDenied (e.g. a genuine filesystem EPERM) — see
    # VerifyErrorKind::exit_code. Assert the frozen slug so a regression that
    # rejects for the wrong reason cannot pass as "verify correctly rejected".
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "identity_mismatch", (
        f"exit 77 must be the identity check, not a different PermissionDenied "
        f"cause; got {envelope['error']}"
    )


def test_verify_issuer_mismatch_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Cert-issuer mismatch → exit 77. Distinct variant, same code as identity."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(
        ocx, sigstore_stack, pkg, issuer="https://wrong-issuer.example", json_format=True,
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (IssuerMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    # Same reasoning as the identity-mismatch sibling above: 77 alone cannot
    # distinguish this from IdentityMismatch or an unrelated PermissionDenied.
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "issuer_mismatch", (
        f"exit 77 must be the issuer check, not a different PermissionDenied "
        f"cause; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# No signatures found — exit 79 (NotFound)
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_no_signatures_exits_79(
    ocx: OcxRunner, published_package: PackageInfo, sigstore_stack: SigstoreStack
) -> None:
    """A package with no referrers → exit 79.

    C-S1-2: ``NoSignaturesFound`` maps to 79 so CI scripts can distinguish
    "not signed" (retryable: sign first) from "bad signature" (terminal) via
    ``$?`` alone. Fails before reaching crypto, so the trust root is harmless
    here — passed for consistency with every other verify call.
    """
    pkg = published_package
    result = _verify(
        ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example",
        json_format=True,
    )
    assert result.returncode == 79, (
        f"expected exit 79 (NotFound / NoSignaturesFound), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    # The exit code alone cannot carry this: `target_not_found` is also 79, and
    # the two mean opposite things to someone deciding whether to trust an
    # artifact. Assert the slug so this test and its sibling below can never
    # both pass against a pipeline that collapsed them.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "no_signatures_found", (
        f"the package exists and is unsigned; got {envelope['error']}"
    )


def test_verify_absent_platform_reports_target_not_found(
    ocx: OcxRunner, published_package: PackageInfo, sigstore_stack: SigstoreStack
) -> None:
    """A platform the package does not ship for → exit 79, ``target_not_found``.

    Shares exit 79 with ``no_signatures_found`` on purpose — both are "not
    here" — but never its slug. Reporting "this package is unsigned" for a
    mistyped ``--platform`` is how a typo becomes a belief about supply-chain
    posture, so the discriminator is the slug, not the code.
    """
    pkg = published_package
    result = _verify(
        ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example",
        # A supported OS the fixture does not ship for. An unparseable string
        # (`plan9/sparc64`) would exit at the CLI boundary and never reach the
        # resolver this test is about.
        platform="windows/arm64",
        json_format=True,
    )
    assert result.returncode == 79, (
        f"expected exit 79 (NotFound / TargetNotFound), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "target_not_found", (
        f"an absent platform is not an unsigned package; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — no referrers API → exit 84
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_referrers_unsupported_exits_84(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path,
    sigstore_stack: SigstoreStack,
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
    result = _verify(
        legacy_ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example",
    )
    assert result.returncode == 84, (
        f"expected exit 84 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# JSON envelope golden contract — error + success branches
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_error_envelope_golden_shape(
    ocx: OcxRunner, published_package: PackageInfo, sigstore_stack: SigstoreStack
) -> None:
    """Error-branch JSON envelope matches frozen envelope contract (C-S1-1).

    Shape check (order-independent, key-presence):
    - Root keys: ``schema_version``, ``command``, ``exit_code``, ``error``.
    - ``error.kind`` is ``not_found`` for an unsigned package.
    - ``error.message`` is non-empty.
    - ``error.context`` is a JSON object (may be empty).
    - No ``data`` key on error branches.
    """
    pkg = published_package
    result = _verify(
        ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example", json_format=True,
    )
    assert result.returncode != 0, "unsigned package must fail verify"
    # No `or result.stderr` fallback: the envelope must land on stdout. A
    # fallback here would let a regression that moved the envelope to stderr
    # pass silently — see test_verify_json_format_emits_single_envelope_on_stdout
    # for the dedicated single-stream contract test.
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package verify"
    assert envelope["exit_code"] == 79
    assert "data" not in envelope, "error branch must not carry data"
    error = envelope["error"]
    assert error["kind"] == "not_found"
    assert isinstance(error["message"], str) and error["message"]
    assert isinstance(error["context"], dict)


def test_verify_json_format_emits_single_envelope_on_stdout(
    ocx: OcxRunner, published_package: PackageInfo, sigstore_stack: SigstoreStack
) -> None:
    """Under ``--format json``, a failing verify emits exactly one JSON
    document on stdout — never a second document, never stray diagnostic text,
    never the envelope on stderr instead.

    For a command that reported nothing before failing (an unsigned package),
    that one document IS the error envelope. ``json.loads`` on the raw
    ``result.stdout`` string parses strictly: it only succeeds when the whole
    stream is exactly one JSON value, so a stray extra document or leaked text
    fails this test with a JSON decode error rather than silently passing.
    """
    pkg = published_package
    result = _verify(
        ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example", json_format=True,
    )
    assert result.returncode != 0, "unsigned package must fail verify"
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package verify"
    assert "error" in envelope
    assert "data" not in envelope, "error branch must not carry data"


def test_verify_success_envelope_golden_shape(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Success-branch JSON envelope matches frozen envelope contract.

    Shape check:
    - Root keys: ``schema_version``, ``command``, ``exit_code``, ``data``.
    - ``exit_code`` is 0 on success.
    - ``data.subject_digest`` and ``data.referrer_digest`` start with ``sha256:``.
    - ``data.certificate_identity`` and ``data.certificate_oidc_issuer`` present.
    - No ``error`` key on success branches.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(ocx, sigstore_stack, pkg, json_format=True)
    assert verify.returncode == 0, verify.stderr
    envelope = json.loads(verify.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package verify"
    assert envelope["exit_code"] == 0
    assert "error" not in envelope, "success branch must not carry error"
    data = envelope["data"]
    assert data["subject_digest"].startswith("sha256:")
    assert data["referrer_digest"].startswith("sha256:")
    assert data["certificate_identity"] == sigstore_stack.identity
    assert data["certificate_oidc_issuer"] == sigstore_stack.issuer


# ──────────────────────────────────────────────────────────────────────────────
# Tampered Rekor SET — exit 65 (DataError)
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_detects_tampered_rekor_set(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A tampered Rekor SET → exit 65 (DataError), not exit 83.

    RekorSetInvalid is a data-integrity failure (the bundle has been altered) —
    retry will not help, so it must map to ``DataError`` not ``TransparencyLogUnavailable``.
    The real log signs what it signs, so the bad SET is made after the fact, by
    corrupting the one field in a bundle that is otherwise entirely authentic.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    adversarial.tamper_signed_entry_timestamp(ocx.registry, pkg.repo, digest, size)

    verify = _verify(ocx, sigstore_stack, pkg)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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
    referrer_digest = _sign(ocx, sigstore_stack, identity_token, pkg)["referrer_digest"]

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

    verify = _verify(ocx, sigstore_stack, pkg)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A bundle whose leaf does not chain to the trusted CA → exit 65.

    The spliced leaf carries the same identity and issuer extensions the real
    one does, so a pass proves the chain is validated rather than the identity
    merely being read off whatever certificate is present.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    leaf = adversarial.throwaway_leaf_der(sigstore_stack.identity, sigstore_stack.issuer)
    adversarial.splice_foreign_certificate(ocx.registry, pkg.repo, digest, size, leaf)

    verify = _verify(ocx, sigstore_stack, pkg)
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / CertChainInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rekor unavailable during verify — exit 83
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_transparency_log_unavailable_exits_83(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Rekor unreachable during the verify key lookup → exit 83.

    Distinguished from ``RekorSetInvalid`` (exit 65) because retry MAY help
    here — the service is down, not a crypto failure. The trust root carries the
    CA and the CT log key but no pinned Rekor key, on purpose: a pinned key
    would make the lookup unnecessary and there would be nothing to fail.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=adversarial.unreachable_rekor_url(),
        trusted_root=sigstore_stack.trusted_root_without_rekor_key(tmp_path),
    )
    assert verify.returncode == 83, (
        f"expected exit 83 (TransparencyLogUnavailable), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# ANY-of key rotation — a later valid referrer is reached past a wrong one
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_any_of_rotation_reaches_valid_referrer(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Two referrers on one subject — a failing one + the valid one → verify passes.

    Verify against the expected identity must succeed: the pipeline's ANY-of loop
    has to reach the valid referrer rather than stop at the failing one. Without
    ANY-of, an unusable first candidate would mask the good signature. The
    referrer count is asserted first so the test genuinely exercises rotation
    (two candidates), not a single trivially-valid signature.

    The losing candidate carries an untrusted leaf rather than a rotated-away
    identity: the local dex mints exactly one identity, so a second *trusted*
    signer does not exist to sign with.
    """
    pkg = published_package
    subject_digest = _sign(ocx, sigstore_stack, identity_token, pkg)["subject_digest"]

    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    leaf = adversarial.throwaway_leaf_der("rotated-away@example.com", sigstore_stack.issuer)
    adversarial.add_rival_bundle_with_foreign_certificate(ocx.registry, pkg.repo, digest, size, leaf)

    # Precondition: two distinct bundle referrers now hang off the subject, so the
    # verify below actually exercises ANY-of (not a single valid candidate).
    status, index = list_referrers(ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03)
    assert status == 200, f"referrers listing failed with status {status}"
    bundles = [m for m in (index or {}).get("manifests", []) if m.get("artifactType") == SIGSTORE_BUNDLE_V03]
    assert len(bundles) >= 2, (
        f"rotation needs two candidate referrers on {subject_digest}, found {len(bundles)}: {bundles}"
    )

    verify = _verify(ocx, sigstore_stack, pkg)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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

    referrer_v1 = _sign(ocx, sigstore_stack, identity_token, v1)["referrer_digest"]
    referrer_v2 = _sign(ocx, sigstore_stack, identity_token, v2)["referrer_digest"]

    manifest_v1 = get_manifest(ocx.registry, v1.repo, referrer_v1)  # subject=v1, layers[0]=v1 bundle
    manifest_v2 = get_manifest(ocx.registry, v2.repo, referrer_v2)  # carries the exact v2 subject descriptor

    # Splice: v1's bundle referrer, re-pointed at v2's subject.
    spliced = dict(manifest_v1)
    spliced["subject"] = manifest_v2["subject"]
    delete_manifest(ocx.registry, v2.repo, referrer_v2)  # drop v2's own valid referrer
    push_manifest(ocx.registry, v2.repo, spliced)        # v2 now has only the spliced one

    verify = _verify(ocx, sigstore_stack, v2)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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
    data = _sign(ocx, sigstore_stack, identity_token, pkg)
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

    verify = _verify(ocx, sigstore_stack, pkg)
    assert verify.returncode == 0, (
        f"a malformed referrer must not block the valid signature (ANY-of), "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Attestation mode — S-009, S-016, S-017
#
# `--attestation` and the bare signature mode look for two different kinds of
# signed content under one artifactType. Every row below asserts the exit code
# AND the frozen slug: the two modes share exit 79 for "nothing of my kind is
# here", so the code alone cannot say which search actually ran.
# ──────────────────────────────────────────────────────────────────────────────


def _attest(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    predicate: Path,
    *,
    predicate_type: str = "cyclonedx",
    env: dict[str, str] | None = None,
) -> dict:
    """Attach a signed attestation to ``pkg``; return the JSON envelope's ``data``."""
    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "attest",
            "--platform", current_platform(),
            "--predicate", str(predicate),
            "--type", predicate_type,
            "--fulcio-url", stack.fulcio_url,
            "--rekor-url", stack.rekor_url,
            "--identity-token-file", str(token),
            pkg.short,
        ],
        capture_output=True, text=True, env=env or ocx.env,
    )
    assert result.returncode == 0, f"attest setup failed: {result.stderr}"
    return json.loads(result.stdout)["data"]


def _cyclonedx(padding_bytes: int = 0) -> str:
    """A minimal valid CycloneDX 1.6 document, optionally padded to a size.

    The padding rides in a string field rather than as trailing bytes so the
    document stays parseable at every size — a refusal at a cap is then
    attributable to size alone, never to a parse error arriving with it.
    """
    return json.dumps(
        {
            "bomFormat": "CycloneDX",
            "specVersion": "1.6",
            "components": [],
            "padding": "a" * padding_bytes,
        }
    )


def test_verify_attestation_verifies_a_published_attestation(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-009 happy path: what `attest` published, `verify --attestation` accepts.

    The closing assertion on `VerifyOptions.content` threading (WP8b M4): a
    revert that hardcodes `VerifyContentMode::Signature` leaves every other
    attestation row still green — they all assert *refusals*, which a signature-
    mode search also produces — and reds only here, where a DSSE candidate must
    actually be found, parsed and verified.
    """
    pkg = published_package
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_text(_cyclonedx())
    published = _attest(ocx, sigstore_stack, identity_token, pkg, predicate)

    verify = _verify(ocx, sigstore_stack, pkg, attestation=True, json_format=True)
    assert verify.returncode == 0, (
        f"verify --attestation must accept the attestation `ocx package attest` "
        f"just published, got {verify.returncode}\n"
        f"stdout: {verify.stdout.strip()}\nstderr: {verify.stderr.strip()}"
    )
    # Exit 0 alone would also be satisfied by a run that verified some *other*
    # candidate on the subject; the referrer digest names which one was read,
    # and `attest` reported it, so the two ends are tied together by identity
    # rather than by both merely succeeding.
    assert json.loads(verify.stdout)["data"]["referrer_digest"] == published["referrer_digest"], (
        f"the verified referrer must be the attestation just published "
        f"({published['referrer_digest']}); got {verify.stdout.strip()}"
    )


def test_verify_attestation_on_a_signature_only_subject_exits_79(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A subject carrying only a signature has no attestation → 79.

    The signature referrer is a candidate by ``artifactType`` — same media type,
    same subject — so this is a discrimination test, not a listing one: the
    bundle is fetched and its content oneof says `messageSignature`, which the
    attestation mode must refuse to count. The slug separates that from the
    signature mode's own empty answer, which is `no_signatures_found`.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(ocx, sigstore_stack, pkg, attestation=True, json_format=True)
    assert verify.returncode == 79, (
        f"expected exit 79 (NotFound / AttestationNotFound), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "attestation_not_found", (
        f"a signature must not satisfy an attestation search; got {envelope['error']}"
    )


def test_verify_signature_mode_on_an_attestations_only_subject_exits_79(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The converse: attestations present, no signature → 79 `no_signatures_found`.

    Renamed from `no_usable_bundle` at WP6 (exit unchanged, wording accurate).
    Asserted here because the rename is a wire contract — a script branching on
    the slug breaks silently if it drifts back.
    """
    pkg = published_package
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_text(_cyclonedx())
    _attest(ocx, sigstore_stack, identity_token, pkg, predicate)

    verify = _verify(ocx, sigstore_stack, pkg, json_format=True)
    assert verify.returncode == 79, (
        f"expected exit 79 (NotFound), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "no_signatures_found", (
        f"an attestation must not satisfy a signature search; got {envelope['error']}"
    )


def test_verify_attestation_type_narrowing_miss_exits_79(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-017: `--type` naming a type nothing carries → 79 `attestation_not_found`.

    A CycloneDX attestation is published and asked for as SPDX. Narrowing is by
    the *signed* payload, so the miss is decided after the envelope is verified,
    and it records nothing — no `predicate_type_mismatch` reaches the caller,
    the aggregate is simply "no attestation of that type". Asserting the slug is
    what distinguishes a genuine narrowing miss from a verification failure that
    happens to share exit 65's neighbourhood.
    """
    pkg = published_package
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_text(_cyclonedx())
    _attest(ocx, sigstore_stack, identity_token, pkg, predicate)

    verify = _verify(
        ocx, sigstore_stack, pkg, attestation=True, predicate_type="spdx", json_format=True,
    )
    assert verify.returncode == 79, (
        f"expected exit 79 for a --type narrowing miss, got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "attestation_not_found", (
        f"S-017: a narrowing miss reports attestation_not_found, not a mismatch "
        f"kind; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-016 — the byte caps are chosen by mode, not shared
# ──────────────────────────────────────────────────────────────────────────────

#: `MAX_BUNDLE_SIZE_BYTES` (`oci/sign/bundle.rs`) — the per-candidate bundle cap
#: a signature-mode run enforces. The attestation mode's own cap
#: (`MAX_ATTESTATION_ENVELOPE_BYTES`, 32 MiB) is 64x larger; a bundle between the
#: two is the only input that can tell which one is in force.
SIGNATURE_BUNDLE_CAP_BYTES = 512 * 1024


def test_verify_attestation_cap_is_selected_by_mode_not_shared(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-016: one bundle, over the signature cap and under the attestation cap.

    A padded CycloneDX predicate makes the published bundle exceed
    ``MAX_BUNDLE_SIZE_BYTES`` while staying far under
    ``MAX_ATTESTATION_ENVELOPE_BYTES``. That single artifact is then asked for
    both ways, and the two answers are the assertion:

    * ``--attestation`` verifies it — only possible if the 32 MiB cap is in
      force, since the blob is refused before parsing under the 512 KiB one;
    * the bare signature mode refuses it as a **bundle read**, not as an empty
      search — the blob is over *its* cap, so the read is cut short.

    The slug on that second answer is the whole discrimination, which is why it
    is asserted rather than a bare non-zero. Under a shared (hoisted) cap the
    signature run still fails — it reaches the bundle, finds a DSSE envelope
    where it wanted a message signature, and reports the ordinary
    ``no_signatures_found`` (79). A non-zero assertion passes in both worlds
    and has no reachable red for the property this test exists to prove.

    Hoisting the attestation numbers into the shared path would silently relax
    `ocx package verify`, and nothing else in this file would notice: every
    other bundle here is a few kilobytes, so both caps accept them.
    """
    pkg = published_package
    predicate = tmp_path / "big.cdx.json"
    # 1 MiB of padding: comfortably past the 512 KiB signature cap even before
    # base64 expands the payload, and three orders of magnitude below 32 MiB.
    predicate.write_text(_cyclonedx(padding_bytes=1024 * 1024))
    data = _attest(ocx, sigstore_stack, identity_token, pkg, predicate)

    manifest = get_manifest(ocx.registry, pkg.repo, data["referrer_digest"])
    bundle_size = manifest["layers"][0]["size"]
    assert bundle_size > SIGNATURE_BUNDLE_CAP_BYTES, (
        f"the fixture must exceed the signature cap or it proves nothing about "
        f"cap selection: bundle is {bundle_size} bytes, cap is "
        f"{SIGNATURE_BUNDLE_CAP_BYTES}"
    )

    attestation_mode = _verify(ocx, sigstore_stack, pkg, attestation=True, json_format=True)
    assert attestation_mode.returncode == 0, (
        f"a {bundle_size}-byte bundle is under the attestation cap and must "
        f"verify; got {attestation_mode.returncode}\n"
        f"stdout: {attestation_mode.stdout.strip()}\n"
        f"stderr: {attestation_mode.stderr.strip()}"
    )

    signature_mode = _verify(ocx, sigstore_stack, pkg, json_format=True)
    assert signature_mode.returncode == 65, (
        f"the same over-cap bundle must be refused by a signature-mode run as a "
        f"data error; got {signature_mode.returncode}\n"
        f"stdout: {signature_mode.stdout.strip()}"
    )
    detail = json.loads(signature_mode.stdout)["error"]["detail"]
    assert detail == "bundle_parse_failed", (
        f"the refusal must come from the capped bundle read, which is what "
        f"proves the signature cap is still 512 KiB; got {detail!r}. "
        f"`no_signatures_found` here means both modes share one cap — the run "
        f"read the whole blob and only then rejected its content kind."
    )


# ──────────────────────────────────────────────────────────────────────────────
# Certificate validity window — integratedTime outside [notBefore, notAfter]
# ──────────────────────────────────────────────────────────────────────────────

#: The two slugs that can carry a window refusal, and which layer each names.
#: `cert_chain_invalid` is `sigstore`'s own check (`verify_digest` step 7);
#: `certificate_validity_window` is ocx's re-assertion in `verify_one_referrer`.
WINDOW_REFUSAL_SLUGS = ("cert_chain_invalid", "certificate_validity_window")


def test_verify_integrated_time_outside_certificate_window_is_refused(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A log entry timestamped after the certificate expired → exit 65.

    CVE-2024-55655: a signature is only evidence if the log says it was made
    while the signing certificate was valid. ``integratedTime`` is the one entry
    field the logged body does not contain, so shifting it past ``notAfter``
    leaves every body-consistency and Merkle check intact and the run reaches a
    real window comparison.

    Two independent implementations enforce that window over byte-identical
    inputs — `sigstore`'s `verify_digest` step 7 and ocx's own row-13
    re-assertion (`tlog::verify_integrated_time_within_certificate`). The
    security property is that the artifact is refused, and that is asserted
    strictly. Which of the two catches it is an implementation fact, pinned
    separately and loudly: with `sigstore` 0.14 the delegated check runs first
    (`pipeline.rs`, `verifier.verify` ahead of `verify_rekor_set` and the
    recheck), so the slug is `cert_chain_invalid`. A flip to
    `certificate_validity_window` is not a regression — it means `sigstore`
    stopped checking and ocx's backstop earned its keep, which is exactly what
    it exists for. Update the pin, do not weaken the assertion.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    subject_digest, subject_size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    shifted_to = adversarial.shift_integrated_time_outside_certificate_window(
        ocx.registry, pkg.repo, subject_digest, subject_size,
    )

    verify = _verify(ocx, sigstore_stack, pkg, json_format=True)
    assert verify.returncode == 65, (
        f"an integratedTime of {shifted_to} lies past the certificate's notAfter "
        f"and must be refused as a data error; got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    detail = json.loads(verify.stdout)["error"]["detail"]
    assert detail in WINDOW_REFUSAL_SLUGS, (
        f"exit 65 must be the validity-window refusal, not an unrelated data "
        f"error; got {detail!r}"
    )
    assert detail == "cert_chain_invalid", (
        f"expected `sigstore`'s own step-7 check to catch this first, got "
        f"{detail!r}. If this is `certificate_validity_window`, `sigstore` no "
        f"longer performs the check and ocx's row-13 re-assertion is now the "
        f"only one — re-pin this line and say so in the changelog."
    )


# ──────────────────────────────────────────────────────────────────────────────
# S-008 / S-019 — `package sbom` refusals, and the envelope that reports them
#
# Both rows are about one property: the JSON envelope's `exit_code` is the code
# the process actually returns. Before WP9b these disagreed — a CLI-local
# `CommandError` rendered as `1`/`internal` while the process exited 64 or 65
# (CLI-04) — so a consumer branching on the envelope read a different outcome
# than a consumer branching on `$?`.
# ──────────────────────────────────────────────────────────────────────────────


def _sbom_args(stack: SigstoreStack, pkg: PackageInfo) -> list[str]:
    """`package sbom` with the stack's trust material — the shared prefix."""
    return [
        "package", "sbom",
        "--platform", current_platform(),
        "--rekor-url", stack.rekor_url,
        "--sigstore-trusted-root", str(stack.trust_root),
        "--certificate-identity", stack.identity,
        "--certificate-oidc-issuer", stack.issuer,
        pkg.short,
    ]


def _run_with_stdout_on_a_terminal(ocx: OcxRunner, *args: str) -> tuple[int, str]:
    """Run ocx with a pseudo-terminal on **stdout**; return `(exit code, stdout)`.

    The mirror of `test_self_activate.py`'s interactive helper, which puts the
    pty on stderr: the branch under test here asks `stdout().is_terminal()`, and
    that is false in every pipe — so a plain captured subprocess exercises the
    allow branch while reading as though it covered the refusal.

    stderr stays a pipe and is discarded, which is what keeps the returned
    string parseable: diagnostics share the terminal with the payload the
    moment both are pointed at one pty.
    """
    import pty

    primary, secondary = pty.openpty()
    try:
        result = subprocess.run(
            [str(ocx.binary), *args],
            stdout=secondary, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL,
            env=ocx.env,
        )
        # Close the write end first: the child has exited, and while any writer
        # remains open the read below would block instead of reporting EOF.
        os.close(secondary)
        secondary = -1
        chunks: list[bytes] = []
        while True:
            try:
                data = os.read(primary, 4096)
            except OSError:  # EIO — every writer is gone, which is this pty's EOF
                break
            if not data:
                break
            chunks.append(data)
    finally:
        if secondary != -1:
            os.close(secondary)
        os.close(primary)
    # A pty's line discipline maps \n to \r\n on the way out and does nothing
    # else, so dropping both rejoins the single-line envelope losslessly.
    return result.returncode, b"".join(chunks).decode(errors="replace").replace("\r", "").replace("\n", "")


def test_sbom_output_to_a_terminal_is_refused_and_the_envelope_agrees(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
) -> None:
    """S-008 + S-019: `--output -` on a TTY → exit 64, and the envelope says 64.

    Predicate bytes are publisher-authored and unsanitized, so a terminal is the
    one destination they may not reach (CWE-150). The refusal is a *usage*
    error, not a data one: the document is fine, the destination is not.

    Driven through a real pseudo-terminal because `is_terminal()` is false in
    every pipe, which is exactly what a captured-subprocess test gives — a test
    that ran without a pty would assert against the allow branch while reading
    as if it covered the refusal. Nothing needs to be published first: the check
    runs before the network round-trip, so this also pins that ordering.
    """
    returncode, terminal = _run_with_stdout_on_a_terminal(
        ocx, "--format", "json", *_sbom_args(sigstore_stack, published_package), "--output", "-",
    )
    assert returncode == 64, (
        f"writing raw predicate bytes to a terminal must be refused as a usage "
        f"error (64), got {returncode}\nterminal: {terminal[:400]}"
    )
    envelope = json.loads(terminal)
    assert envelope["exit_code"] == 64, (
        f"the envelope's exit_code must be the code the process returned "
        f"({returncode}); got {envelope['exit_code']} — CLI-04"
    )
    assert envelope["error"]["kind"] == "usage_error", (
        f"the destination is the problem, not the data; got {envelope['error']}"
    )


def test_sbom_summary_on_a_non_cyclonedx_predicate_is_partial_failure_not_a_hard_error(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """S-019: `--summary` over a predicate the CycloneDX reader cannot parse
    exits 0 with that one entry moved to ``refused`` -- never a hard failure.

    WP-R5 correction: this test previously asserted exit 65 with a top-level
    ``error`` envelope, a contract `--summary` no longer has -- per-candidate
    independence (PKG-22) means one unreadable document costs its OWN entry,
    not the run, the same correction already applied to
    ``test_sbom.py::test_sbom_summary_refuses_a_non_cyclonedx_predicate_but_listing_still_works``.
    That test covers an SPDX predicate; this one is kept as the DISTINCT
    ``custom`` predicate-type case -- proving the partial-failure treatment
    is not special-cased to one foreign type.

    A `custom` attestation is the vehicle: it is signed and verifiable like any
    other, so the run reaches the summary step and refuses there rather than
    earlier -- which is what makes the refusal attributable to the reader.
    """
    pkg = published_package
    predicate = tmp_path / "not-a-bom.json"
    predicate.write_text(json.dumps({"hello": "world"}))
    attested = _attest(ocx, sigstore_stack, identity_token, pkg, predicate, predicate_type="custom")

    result = subprocess.run(
        [str(ocx.binary), "--format", "json", *_sbom_args(sigstore_stack, pkg), "--summary"],
        capture_output=True, text=True, env=ocx.env,
    )
    assert result.returncode == 0, (
        f"an unreadable document refuses its own entry, not the run, got "
        f"{result.returncode}\nstdout: {result.stdout.strip()}\n"
        f"stderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["exit_code"] == 0, (
        f"the envelope's exit_code must match the process code; got "
        f"{envelope['exit_code']} — CLI-04"
    )
    data = envelope["data"]
    assert data["summary"] == {
        "status": "partial_failure", "verification": "verified", "exit_code": 0, "total": 1, "verified": 0, "unverified": 0, "refused": 1,
    }
    assert data["entries"] == [], "nothing summarized, so nothing is listed as summarized"

    [refusal] = data["refused"]
    assert refusal["reason_kind"] == "sbom_summary_failed", (
        f"a script branches on the slug, got: {refusal!r}"
    )
    assert refusal["referrer_digest"] == attested["referrer_digest"]
    # Without the offending type in the message the operator cannot tell which
    # of several attestations was unreadable.
    assert "cosign.sigstore.dev/attestation" in refusal["reason"], (
        f"the refusal must name the predicate type it could not read; got "
        f"{refusal['reason']!r}"
    )
