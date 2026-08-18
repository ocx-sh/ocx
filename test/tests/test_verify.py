# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package verify`` (Slice 1 — referrers verify).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md``
(specifically C-S1-1 frozen envelope + C-S1-2 VerifyErrorKind variant set) and
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

Trust-root seam: verify runs against the local stack's trusted-root JSON
(``--trusted-root``), which carries the Fulcio CA and the pinned Rekor key. The
one exception is the Rekor-unavailable test, which supplies a document with no
pinned Rekor key so the key fetch actually happens and can fail.
"""
from __future__ import annotations

import base64
import json
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
) -> subprocess.CompletedProcess[str]:
    """Run ``package verify``, defaulting every knob to the stack's own values.

    ``trusted_root`` points at a different trusted-root document — the way to
    vary what the trust material carries without losing the CT log key.
    """
    env = dict(ocx.env)
    root = ["--trusted-root", str(trusted_root or stack.trust_root)]
    return subprocess.run(
        [
            str(ocx.binary),
            *(["--format", "json"] if json_format else []),
            "package", "verify",
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
    """Error-branch JSON envelope matches frozen v1 contract (C-S1-1).

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
    """Success-branch JSON envelope matches frozen v1 contract.

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
    retry will not help, so it must map to ``DataError`` not ``RekorUnavailable``.
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


def test_verify_rekor_unavailable_exits_83(
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
        f"expected exit 83 (RekorUnavailable), got {verify.returncode}\n"
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
