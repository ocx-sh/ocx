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

import json
import os
import subprocess
from datetime import UTC, datetime
from pathlib import Path

from src.registry import (
    IMAGE_INDEX_MEDIA_TYPE,
    IMAGE_MANIFEST_MEDIA_TYPE,
    delete_manifest,
    get_manifest,
    list_referrers,
    push_manifest,
)
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial, cosign_artifacts
from tests.fixtures.sigstore_stack import SigstoreStack

# Sigstore bundle v0.3 artifact type — mirrors the Rust constant
# `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"


def _iso8601(epoch_seconds: int) -> str:
    """Render a Rekor `integratedTime` the way the report does.

    Derived from the fixture rather than transcribed, so a re-capture that moved
    the log entry cannot leave a stale literal agreeing with nothing.
    """
    return datetime.fromtimestamp(epoch_seconds, UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> dict:
    """Sign ``pkg`` against the real stack; return the JSON envelope's ``data``."""
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True, text=True, env=ocx.env,
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"
    return json.loads(result.stdout)["data"]


def _referrer_digest(data: dict) -> str:
    """The digest of the OCI referrer manifest the ``bundle`` leg pushed.

    The sign report is per-leg since ``--signature-format`` landed: a leg's
    ``manifest_digest`` is the manifest its payload hangs from — the OCI
    referrer under ``bundle``, the ``sha256-<hex>.sig`` sidecar under
    ``simplesigning`` — so the two are not interchangeable and the surgery below
    needs the ``bundle`` one specifically. Selected by format and pinned to
    exactly one match rather than taken at ``[0]``: a run that grew a second leg
    would otherwise silently point the surgery at the sidecar.
    """
    legs = [leg for leg in data["legs"] if leg["format"] == "bundle"]
    assert len(legs) == 1, f"expected exactly one `bundle` leg in the sign report, got {data['legs']}"
    return legs[0]["manifest_digest"]


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
# Registry capability — no referrers API, no fallback tag → exit 79
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_without_referrers_api_or_fallback_tag_exits_79(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path,
    sigstore_stack: SigstoreStack,
) -> None:
    """Registry with no referrers API and no fallback tag → exit 79.

    ``legacy_registry`` (``registry:2``, #106/#195 negative fixture) does not
    implement ``/v2/<name>/referrers/``, so this is the only registry in the
    suite that reaches the OCI referrers tag-schema fallback at all — a green
    run against zot proves nothing about this path.

    Discovery reads *both* sources: the Referrers API, then the
    ``sha256-<hex>`` fallback tag. Nothing was signed here, so neither answers
    and the honest verdict is "no signatures found" (79) — not a capability
    refusal (84). 84 is now write-path only; ``ocx package sign`` still raises
    it when the fallback index itself is refused.
    """
    from src.helpers import make_package

    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    result = _verify(
        legacy_ocx, sigstore_stack, pkg,
        identity="anyone@example.com", issuer="https://anywhere.example",
        json_format=True,
    )
    assert result.returncode == 79, (
        f"expected exit 79 (NotFound / NoSignaturesFound), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    # 79 alone cannot tell this from TargetNotFound or any other NotFound, and
    # the whole point of the flip is *which* 79 this is. Assert the frozen slug.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "no_signatures_found", (
        f"a registry with no referrers API and no fallback tag must report "
        f"no_signatures_found, not a capability verdict; got {envelope['error']}"
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
    - ``data.signatures`` present, one row per discovered signature.
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
    # S-006 is populated now (D6): the discovery pipeline fills ``signatures``,
    # so a *successful* verify carries the key rather than omitting it. The
    # omit-while-empty rule it replaces still holds on the failing branches --
    # those never render ``data`` at all.
    assert "signatures" in data, (
        f"a successful verify reports the signatures it verified, got {data!r}"
    )
    entry = data["signatures"][0]
    # None of the three is `skip_serializing_if`, so a row always carries all
    # three: a missing one is a regression, never a mode. Their *values* are
    # pinned per shape by the cosign-fixture cells below; the golden envelope
    # asserts the shape.
    assert {"signature_format", "discovery_method", "key_backend"} <= entry.keys(), entry
    # The rows and the flat fields are projected from the same verified list,
    # the verdict first -- so row 0 must describe the signature the flat fields
    # describe, not a different one that also passed.
    assert entry["referrer_digest"] == data["referrer_digest"], (
        f"`signatures[0]` and the flat fields must be one signature; got {entry!r}"
    )


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
    """Flip a byte in the published bundle's signature → exit 65 (SignatureInvalid).

    The bundle is content-addressed, so this is registry surgery: sign normally,
    then corrupt ``dsseEnvelope.signatures[0].sig`` — an image signature is a
    DSSE envelope, so that is the slot the pre-DSSE ``messageSignature.signature``
    became — and swap the referrer for the corrupted bundle, DELETING the
    original, so exactly one referrer exists for the subject and it is the
    tampered one. Everything else in the bundle is what the real stack signed.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    adversarial.tamper_bundle_signature(ocx.registry, pkg.repo, digest, size)

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
    v2 must reject it: the in-toto Statement inside the bundle's DSSE envelope
    names v1 in its ``subject``, not the v2 digest being verified, so
    ``binds_subject`` fails closed with ``StatementSubjectMismatch`` (65) —
    re-pointing the referrer's ``subject`` moves nothing that is actually
    signed. This is the acceptance-level counterpart to
    the unit ``transparency_body_binding_rejects_spliced_subject`` test — a bundle
    lifted from one artifact cannot be laundered onto another.
    """
    v1, v2 = published_two_versions

    referrer_v1 = _referrer_digest(_sign(ocx, sigstore_stack, identity_token, v1))
    referrer_v2 = _referrer_digest(_sign(ocx, sigstore_stack, identity_token, v2))

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
    valid_manifest = get_manifest(ocx.registry, pkg.repo, _referrer_digest(data))
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


# ──────────────────────────────────────────────────────────────────────────────
# `--platform` is optional — C-010
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_without_platform_runs_against_what_resolved(
    ocx: OcxRunner, published_package: PackageInfo, sigstore_stack: SigstoreStack
) -> None:
    """C-010. ``package verify`` with no ``--platform`` is not a usage error.

    ``--platform`` used to be ``required = true``; a cosign-signed
    multi-platform tag carries its signature on the *index*, which is what the
    reference resolves to when nothing narrows it, so demanding the flag made
    that signature unreachable.

    Written out longhand rather than through ``_verify``, which always passes
    the flag. The envelope is the discriminator, not the exit code: clap writes
    a usage refusal to stderr and leaves stdout empty, so a parseable envelope
    naming a *pipeline* verdict proves the grammar accepted the invocation and
    the run reached the registry.
    """
    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "verify",
            "--rekor-url", sigstore_stack.rekor_url,
            "--sigstore-trusted-root", str(sigstore_stack.trust_root),
            "--certificate-identity", "anyone@example.com",
            "--certificate-oidc-issuer", "https://anywhere.example",
            published_package.short,
        ],
        capture_output=True, text=True, env=dict(ocx.env),
    )
    assert result.returncode != 64, (
        f"--platform must be optional; clap refused the invocation\n"
        f"stderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "no_signatures_found", (
        f"the run must reach the pipeline and report on the resolved object, "
        f"not fail earlier; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# `--key` and `--signature-format` grammar — exits 85 / 64
# ──────────────────────────────────────────────────────────────────────────────


def _verify_flags(ocx: OcxRunner, *flags: str) -> subprocess.CompletedProcess[str]:
    """Run ``package verify`` with only the flags under test.

    Deliberately not ``_verify``: that helper always passes the certificate
    pair, which is exactly what ``--key`` conflicts with. Every case below is
    refused before the first registry request, so the identifier need not
    resolve to anything and no package is published for it.
    """
    return subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "verify",
            "--platform", current_platform(),
            *flags,
            "localhost:5000/never-published:1.0.0",
        ],
        capture_output=True, text=True, env=ocx.env,
    )


def test_verify_key_with_unimplemented_backend_exits_85(ocx: OcxRunner) -> None:
    """S-011. ``--key awskms://...`` → exit 85, naming the scheme.

    The failure mode this pins is not the exit code but the *message*: read as
    a filename, ``awskms://alias/release`` fails with "no such file or
    directory" and sends the operator looking for a missing key file that was
    never meant to exist. The refusal happens at the reference parser, before
    anything treats the value as a path.
    """
    result = _verify_flags(ocx, "--key", "awskms://alias/release")
    assert result.returncode == 85, (
        f"expected exit 85 (UnsupportedKeyBackend), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "unsupported_key_backend", (
        f"85 must be the key-backend refusal; got {envelope['error']}"
    )
    assert "awskms" in envelope["error"]["message"], (
        f"the message must name the scheme so the operator knows what is "
        f"unimplemented; got {envelope['error']['message']!r}"
    )
    assert "no such file" not in envelope["error"]["message"].lower(), (
        f"a KMS reference must never be reported as a missing file; got "
        f"{envelope['error']['message']!r}"
    )


def test_verify_malformed_key_reference_exits_64(ocx: OcxRunner) -> None:
    """S-011, the other half. An unrecognised scheme → exit 64.

    Same parser, different verdict: ``vault://`` is not a backend OCX knows at
    all, so it is a bad invocation (64) rather than an unimplemented backend
    (85). Sharing one code would tell an operator to wait for a feature that is
    never coming.
    """
    result = _verify_flags(ocx, "--key", "vault://secret/cosign")
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / KeyReferenceInvalid), got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "key_reference_invalid", (
        f"64 must be the reference-grammar refusal, not some other usage "
        f"error; got {envelope['error']}"
    )


def test_verify_key_with_a_file_colon_prefix_exits_64_naming_the_bare_path(
    ocx: OcxRunner,
) -> None:
    """The removed single-colon spelling, at the CLI boundary.

    ``file:etc/acme.pub`` is a string cosign *resolves* — to a file literally
    named ``file:etc/acme.pub``. OCX used to strip the prefix and open
    ``etc/acme.pub`` instead, so one value named two different files depending
    on which tool read it. It is now refused.

    There is no deprecation window, so the message is the whole migration: it
    has to carry the replacement text. And 64, never 74 — reporting it as a
    missing file would send the operator hunting for a path they must not
    write, which is the exact failure the refusal exists to prevent.
    """
    result = _verify_flags(ocx, "--key", "file:etc/acme-release.pub")
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / KeyReferenceInvalid) for the removed "
        f"spelling, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "key_reference_invalid", (
        f"the grammar refusal, not an I/O one; got {envelope['error']}"
    )
    message = envelope["error"]["message"]
    assert "etc/acme-release.pub" in message, (
        f"the message must name the bare path to write instead; got {message!r}"
    )
    assert "no such file" not in message.lower(), (
        f"the removed spelling must never be reported as a missing file; got "
        f"{message!r}"
    )


def test_verify_key_conflicts_with_certificate_identity(ocx: OcxRunner) -> None:
    """S-003's error case. ``--key`` with ``--certificate-identity`` → exit 64.

    A key signature carries no certificate, so there is no SAN for the flag to
    match. clap refuses the pair, and the message must name both flags — an
    "unexpected argument" that names only one leaves the operator guessing
    which of the two to drop.
    """
    result = _verify_flags(
        ocx,
        "--key", "cosign.pub",
        "--certificate-identity", "you@example.com",
        "--certificate-oidc-issuer", "https://issuer.example",
    )
    assert result.returncode == 64, (
        f"expected exit 64 for --key with the certificate flags, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
    # Presence alone would pass off the usage line, which repeats every flag
    # that was parsed. Assert clap's conflict sentence and BOTH partners: the
    # two `conflicts_with` declarations are independent, so a test satisfied by
    # either one cannot tell a half-declared conflict from a whole one.
    assert "'--key <REF>' cannot be used with" in result.stderr, (
        f"the refusal must be the conflict, not some other usage error; got "
        f"{result.stderr!r}"
    )
    conflict = result.stderr.split("cannot be used with", 1)[1]
    for flag in ("--certificate-identity", "--certificate-oidc-issuer"):
        assert flag in conflict.split("Usage:", 1)[0], (
            f"the conflict list must name {flag}; got {result.stderr!r}"
        )


def test_verify_signature_format_both_exits_64(ocx: OcxRunner) -> None:
    """S-012. ``--signature-format both`` on verify → exit 64.

    ``both`` selects what to *write*. A verification result cannot say "either
    of these two signatures satisfied me", so pinning it is a bad invocation
    rather than a silent choice of one shape — silently picking one is the
    failure this refusal exists to prevent.

    The discriminator here is the message, not ``error.detail``: this refusal
    carries no slug, because it is a bare usage error raised by the shared
    option group rather than a verify-taxonomy variant. Exit 64 alone would
    match the identity refusal the sibling test below lands on.
    """
    result = _verify_flags(ocx, "--signature-format", "both")
    assert result.returncode == 64, (
        f"expected exit 64 for --signature-format both, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["kind"] == "usage_error", (
        f"the pin refusal is a bad invocation; got {envelope['error']}"
    )
    assert "single format" in envelope["error"]["message"], (
        f"the refusal must say what `both` is for and what a verify needs; "
        f"got {envelope['error']['message']!r}"
    )


def test_verify_accepts_a_single_signature_format_pin(ocx: OcxRunner) -> None:
    """The green half of S-012: ``bundle`` and ``simplesigning`` are pins.

    Without this the refusal above is indistinguishable from
    ``--signature-format`` being rejected outright. Both values are accepted
    and the run advances to the identity gate — which is what ``detail`` proves
    and the exit code cannot, since that gate also answers 64.
    """
    for value in ("bundle", "simplesigning"):
        result = _verify_flags(ocx, "--signature-format", value)
        envelope = json.loads(result.stdout)
        assert envelope["error"].get("detail") == "no_identity_provided", (
            f"--signature-format {value} is a legal pin, so the run must reach "
            f"the identity gate rather than stopping at the flag; got "
            f"{envelope['error']}"
        )


# ──────────────────────────────────────────────────────────────────────────────
# D-8 — OCX verifies what cosign published
#
# Every cell below pushes **cosign v3.1.1's own committed bytes**
# (`tests/fixtures/golden/`) into a real registry and runs the shipped `ocx`
# binary against them. Nothing here signs anything: these are the only cells in
# the suite whose subject was produced by another implementation, which is what
# makes them interop evidence rather than a round trip of OCX against itself.
#
# They are also the reason the loop's headline claim is testable at all while
# `ocx package sign` is red for a reason outside loop D — verifying an artifact
# cosign wrote needs no OCX signer, no Fulcio, no dex and no Rekor. The trust
# root is committed (`test/sigstore/`), and `--rekor-url` deliberately names a
# dead loopback port so a run that *did* reach for the transparency log would
# fail rather than quietly depend on a stack being up.
#
# **Each cell carries its negative.** A green that never invoked the path is
# indistinguishable from the path not existing, so every cell has a twin that
# pushes the same artifact with one signature byte flipped, reads the corrupted
# bytes back off the wire to prove the mutation landed, and asserts the same
# command refuses them with a named `error.detail`.
# ──────────────────────────────────────────────────────────────────────────────


def _verify_golden(
    runner: OcxRunner,
    reference: str,
    *,
    identity: str | None = None,
    issuer: str | None = None,
    key: Path | None = None,
    platform: str | None = None,
    allow_unlogged: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run the real binary against a pushed golden artifact, JSON on stdout.

    Keyless and key mode are mutually exclusive at the CLI (`--key` conflicts
    with the certificate flags), so exactly one of ``identity``/``key`` is
    spelled per call site.

    ``allow_unlogged`` adds `--allow-unlogged-signature`, the opt-out from the
    keyless sidecar's transparency-log requirement. Default off, because off is
    the contract and a helper that quietly passed it would make every keyless
    sidecar cell say nothing.
    """
    assert (identity is None) != (key is None), "a cell verifies keyless or by key, never both"
    return subprocess.run(
        [
            str(runner.binary), "--format", "json", "package", "verify",
            *(["--platform", platform] if platform else []),
            *(["--allow-unlogged-signature"] if allow_unlogged else []),
            "--rekor-url", cosign_artifacts.DEAD_REKOR_URL,
            "--sigstore-trusted-root", str(cosign_artifacts.TRUST_ROOT),
            *(["--certificate-identity", identity, "--certificate-oidc-issuer", issuer] if identity else []),
            *(["--key", str(key)] if key else []),
            reference,
        ],
        capture_output=True, text=True, env=runner.env,
    )


def _refusal(result: subprocess.CompletedProcess[str], exit_code: int, detail: str, what: str) -> None:
    """Assert a refusal by exit code **and** slug.

    The code alone cannot tell one refusal from another — 65 is every data
    error and 77 is every permission denial — so a corrupted artifact rejected
    for an unrelated reason would read as a passing negative control.
    """
    assert result.returncode == exit_code, (
        f"{what}: expected exit {exit_code}, got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    )
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == detail, f"{what}: got {envelope['error']}"


def test_verify_accepts_cosigns_keyless_dsse_bundle(ocx: OcxRunner, unique_repo: str) -> None:
    """A cosign-written **keyless** DSSE image signature verifies through the CLI.

    Criterion 1, end to end: cosign's referrer, cosign's bundle, cosign's Fulcio
    leaf and cosign's Rekor entry, read by the shipped binary out of a live
    registry. The reported identity, issuer and `signed_at` are asserted against
    the values inside *this fixture* — the certificate's SAN and OIDC-issuer
    extension, and the log entry's `integratedTime` — rather than against
    hand-written constants, so two transcriptions cannot agree with each other
    while both disagree with cosign.

    The leaf **is expired** (its window was about ten minutes, last August).
    That is the point: the validity check anchors on the transparency-log
    instant, never on a clock, so this cell does not rot.
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    referrer = cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "keyless",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
    )
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == subject
    assert data["referrer_digest"] == referrer.digest
    assert data["certificate_identity"] == identity
    assert data["certificate_oidc_issuer"] == issuer
    assert data["signed_at"] == _iso8601(cosign_artifacts.golden_integrated_time("keyless")), (
        "signed_at must be the Rekor entry's integratedTime, not a wall clock"
    )
    [entry] = data["signatures"]
    assert (entry["signature_format"], entry["discovery_method"], entry["key_backend"]) == (
        "bundle", "referrers_api", "keyless",
    ), entry


def test_verify_refuses_cosigns_keyless_bundle_with_one_signature_byte_flipped(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """The negative control for the cell above: same push, one byte different.

    Its own repository rather than an in-place mutation — verification is
    ANY-of, so a corrupted referrer sitting beside the intact one would be
    stepped over and the cell would pass without ever refusing anything.
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    referrer = cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "keyless",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
        corrupt=True,
    )
    served = cosign_artifacts.served_bundle_signature(ocx.registry, unique_repo, referrer.blob_digest)
    assert served != cosign_artifacts.golden_bundle_signature("keyless"), (
        "the registry is serving cosign's signature unchanged — the mutation did "
        "not land, so a refusal below would be about something else"
    )

    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")
    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    _refusal(result, 65, "signature_invalid", "a flipped DSSE signature byte")


def test_verify_accepts_cosigns_key_mode_dsse_bundle(ocx: OcxRunner, unique_repo: str) -> None:
    """Criterion 1, key mode: `--key` against cosign's committed public key.

    A key-mode bundle carries `verificationMaterial.publicKey` — a bare hint,
    no certificate — so there is no chain to walk, no SCT and no SAN. The
    absence has to be *visible*: `certificate_identity` must not appear on the
    row at all rather than appearing empty, because an empty string in a
    provenance column reads as "signed by nobody" instead of "not applicable".
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    referrer = cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "key",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
    )

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        key=cosign_artifacts.GOLDEN / "keys" / "cosign.pub",
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == subject
    [entry] = data["signatures"]
    assert entry["key_backend"] == "file", entry
    assert entry["signature_format"] == "bundle", entry
    assert entry["referrer_digest"] == referrer.digest
    assert "certificate_identity" not in entry, (
        f"a key signature carries no certificate; the row must omit the field: {entry}"
    )
    assert "certificate_oidc_issuer" not in entry, entry
    assert entry["signed_at"] == _iso8601(cosign_artifacts.golden_integrated_time("key")), (
        "cosign uploads a key-mode signature to Rekor too, so the instant is proved here"
    )


def test_verify_refuses_cosigns_key_mode_bundle_with_one_signature_byte_flipped(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """The key-mode negative control.

    The refusal is **65 / signature_invalid**. Under a key, "did this signature
    verify" and "does a trusted key accept it" are one question — a verifier
    holding only public keys cannot tell a tampered signature from an untrusted
    signer — and `signature_invalid` is the sentence true of both readings: a
    trusted key was tried and did not accept these bytes. It answered 77 /
    `identity_mismatch` until `identity::matching_key_policies` was fixed, which
    named a certificate this path does not carry and hid the failure from every
    caller scripting 65.
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    referrer = cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "key",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
        corrupt=True,
    )
    served = cosign_artifacts.served_bundle_signature(ocx.registry, unique_repo, referrer.blob_digest)
    assert served != cosign_artifacts.golden_bundle_signature("key"), (
        "the mutation did not land; the registry still serves cosign's signature"
    )

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        key=cosign_artifacts.GOLDEN / "keys" / "cosign.pub",
    )
    _refusal(result, 65, "signature_invalid", "a flipped key-mode signature byte")


def test_verify_accepts_cosigns_key_mode_simplesigning_sidecar(ocx: OcxRunner, unique_repo: str) -> None:
    """Criterion 2: the `sha256-<hex>.sig` shape, with no verification material at all.

    No certificate, no chain, no `dev.sigstore.cosign/bundle` — one base64
    signature annotation over the payload layer, and that is the whole artifact.
    It is what `cosign generate | sign-blob | attach signature` writes in key
    mode, and refusing it as malformed would be refusing a legal cosign output.
    """
    subject, _ = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    _, layer_digest = cosign_artifacts.push_sidecar(
        ocx.registry, unique_repo, subject,
        cosign_artifacts.GOLDEN / "simplesigning_key_manifest.json",
    )

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        key=cosign_artifacts.GOLDEN / "keys" / "cosign.pub",
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    [entry] = data["signatures"]
    assert entry["signature_format"] == "simplesigning", entry
    assert entry["discovery_method"] == "sidecar_tag", entry
    assert entry["key_backend"] == "file", entry
    assert entry["referrer_digest"] == layer_digest, (
        "one layer is one signature, so the row names the layer, not the sidecar manifest"
    )
    assert "signed_at" not in entry, (
        "cosign v3.1.1 attaches no offline transparency material to a sidecar, so "
        f"there is no proved signing instant to report: {entry}"
    )


def test_verify_refuses_a_key_mode_sidecar_whose_signature_was_tampered(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """The key-mode sidecar negative — committed bytes, not a mutation minted here.

    `simplesigning/tampered_key_signature_manifest.json` is the golden key-mode
    manifest with one byte flipped inside the DER ECDSA signature; the layer it
    names is unchanged, so the signed message is still cosign's byte-exact
    payload and the flipped byte is the only difference.
    """
    subject, _ = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    tag, _ = cosign_artifacts.push_sidecar(
        ocx.registry, unique_repo, subject,
        cosign_artifacts.NEGATIVES / "tampered_key_signature_manifest.json",
    )
    served = cosign_artifacts.served_sidecar_signature(ocx.registry, unique_repo, tag)
    assert served != cosign_artifacts.golden_sidecar_signature("key"), (
        "the sidecar the registry serves still carries cosign's signature"
    )

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        key=cosign_artifacts.GOLDEN / "keys" / "cosign.pub",
    )
    _refusal(result, 65, "signature_invalid", "a tampered key-mode sidecar signature")


def test_verify_refuses_a_keyless_simplesigning_sidecar_with_no_transparency_evidence(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """A keyless `.sig` with a certificate and **no** bundle annotation is refused.

    This is the normal cosign-authored keyless sidecar — `attach signature
    --rekor-response` writes no `dev.sigstore.cosign/bundle` annotation in
    v3.1.1 — and this cell used to assert ocx **accepted** it, exit 0 with
    `signed_at` absent, "the honest report" for a signature nothing timestamps.

    That was the defect, and this fixture is what made it durable rather than
    theoretical: the committed certificate is valid
    `2026-08-29T02:07:58Z .. 02:17:58Z`, ten minutes, and the window check was
    anchored on that certificate's own `notBefore` — so it could never fail and
    the leaf stayed acceptable for ever. A keyless certificate lives minutes;
    without a log entry there is no evidence the signature happened while it was
    live, which is exactly why cosign refuses this artifact by default (rc 12,
    "signature not found in transparency log").

    The exact pair is asserted, not a bare non-zero: 79 would mean the sidecar
    was never discovered, and this cell must fail *after* finding it. Its
    sibling below proves the opt-out brings the same artifact back, so neither
    half can be satisfied by a gate that is simply stuck.
    """
    subject, _ = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    _, layer_digest = cosign_artifacts.push_sidecar(
        ocx.registry, unique_repo, subject,
        cosign_artifacts.GOLDEN / "simplesigning_keyless_manifest.json",
    )
    assert layer_digest, "the sidecar layer is what the refusal must be about"
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    _refusal(result, 65, "signature_invalid", "a keyless sidecar carrying no transparency-log entry")


def test_verify_accepts_that_same_sidecar_under_allow_unlogged_signature(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """`--allow-unlogged-signature` is reachable: the same artifact, accepted.

    The other half of the pair above, on the same fixture through the same
    binary, one flag apart. Without it the refusal above is indistinguishable
    from a keyless sidecar path that never verifies anything, and the flag would
    be one nobody can use.

    The identity gate still runs in full — the reported SAN and issuer are read
    back off the annotation certificate, which a parse-only acceptance could not
    produce — and both transparency fields stay **absent**. That absence is the
    contract of the flag: it buys acceptance of a signature nothing timestamps,
    never an invented instant. It also accepts an expired certificate, because
    with no logged instant there is nothing to check the window against; that is
    what "insecure opt-out for air-gapped CI" means, and it is why it is off by
    default.
    """
    subject, _ = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    _, layer_digest = cosign_artifacts.push_sidecar(
        ocx.registry, unique_repo, subject,
        cosign_artifacts.GOLDEN / "simplesigning_keyless_manifest.json",
    )
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        identity=identity, issuer=issuer, allow_unlogged=True,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    [entry] = data["signatures"]
    assert entry["signature_format"] == "simplesigning", entry
    assert entry["discovery_method"] == "sidecar_tag", entry
    assert entry["key_backend"] == "keyless", entry
    assert entry["referrer_digest"] == layer_digest
    assert (entry["certificate_identity"], entry["certificate_oidc_issuer"]) == (identity, issuer), entry
    assert "signed_at" not in entry, f"nothing proves when this was signed: {entry}"
    assert "rekor_log_index" not in entry, f"no log holds this signature: {entry}"


def test_verify_refuses_a_keyless_sidecar_whose_signature_was_tampered(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """The keyless sidecar negative, from the committed `simplesigning/` set.

    Gives `tampered_signature_manifest.json` — until now read only by a Rust
    unit test — an end-to-end consumer: the same bytes, through the shipped
    binary and a real registry.

    Asked **twice**, and the second call is what keeps it a signature test. This
    fixture carries no `dev.sigstore.cosign/bundle` either, so since keyless
    sidecars require transparency-log evidence a plain run would answer 65 /
    `signature_invalid` whether or not the signature was tampered — the same
    pair for two different causes. Re-asking under
    `--allow-unlogged-signature` lifts the evidence requirement and leaves the
    flipped byte as the only thing left to refuse.
    """
    subject, _ = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    tag, _ = cosign_artifacts.push_sidecar(
        ocx.registry, unique_repo, subject,
        cosign_artifacts.NEGATIVES / "tampered_signature_manifest.json",
    )
    served = cosign_artifacts.served_sidecar_signature(ocx.registry, unique_repo, tag)
    assert served != cosign_artifacts.golden_sidecar_signature("keyless"), (
        "the sidecar the registry serves still carries cosign's signature"
    )

    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")
    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    _refusal(result, 65, "signature_invalid", "a tampered keyless sidecar signature")

    lifted = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}@{subject}",
        identity=identity, issuer=issuer, allow_unlogged=True,
    )
    _refusal(
        lifted, 65, "signature_invalid",
        "a tampered keyless sidecar signature with the transparency requirement lifted",
    )


# ──────────────────────────────────────────────────────────────────────────────
# C-009 / criterion 8 — the registry with no Referrers API
#
# The only cells in the suite that reach the OCI tag-schema fallback at all. A
# green against zot proves nothing here: zot answers
# `/v2/<name>/referrers/<digest>` natively, so the fallback never runs.
# ──────────────────────────────────────────────────────────────────────────────


def _legacy(ocx: OcxRunner, legacy_registry: str) -> OcxRunner:
    """The same binary and OCX_HOME, pointed at `registry:2`."""
    return OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)


def test_verify_reads_cosigns_bundle_through_the_fallback_tag_on_a_registry_without_referrers(
    ocx: OcxRunner, legacy_registry: str, unique_repo: str
) -> None:
    """Criterion 8. The same keyless bundle, discovered through the fallback index.

    `registry:2` has no Referrers API, so the only door open is the
    `sha256-<hex>` tag the OCI tag-schema fallback defines — and the index
    pushed here is cosign's own (`fallback_index.json`), whose child descriptor
    keeps `artifactType` and carries **no** annotations (cosign#4641). A reader
    that filtered candidates by `dev.sigstore.bundle.content` would find
    nothing.

    The absence of the API is asserted rather than assumed: without that, a
    `fallback_tag` verdict could not be told from a harness pointed at the
    wrong registry.
    """
    runner = _legacy(ocx, legacy_registry)
    subject, size = cosign_artifacts.push_subject(legacy_registry, unique_repo)
    status, _ = list_referrers(legacy_registry, unique_repo, subject)
    assert status != 200, (
        f"{legacy_registry} answered the Referrers API with {status}; this cell needs "
        "the registry that does not, or the fallback path is never reached"
    )

    referrer = cosign_artifacts.push_bundle_referrer(
        legacy_registry, unique_repo, "keyless",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
    )
    cosign_artifacts.push_fallback_index(legacy_registry, unique_repo, subject, referrer)
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")

    result = _verify_golden(
        runner, f"{legacy_registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    [entry] = data["signatures"]
    assert entry["discovery_method"] == "fallback_tag", (
        "the Referrers API 404s on this registry, so anything else means the "
        f"verdict came from a door that is not open here: {entry}"
    )
    assert entry["signature_format"] == "bundle", entry
    assert entry["referrer_digest"] == referrer.digest
    assert data["certificate_identity"] == identity


def test_verify_refuses_a_corrupted_bundle_reached_through_the_fallback_tag(
    ocx: OcxRunner, legacy_registry: str, unique_repo: str
) -> None:
    """The fallback door's negative control.

    `signature_invalid` rather than `no_signatures_found` is itself the proof
    that discovery worked: the bundle had to be *found and pulled* through the
    fallback index before its signature could be judged, and on this registry
    there is no other way to find it.
    """
    runner = _legacy(ocx, legacy_registry)
    subject, size = cosign_artifacts.push_subject(legacy_registry, unique_repo)
    referrer = cosign_artifacts.push_bundle_referrer(
        legacy_registry, unique_repo, "keyless",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
        corrupt=True,
    )
    cosign_artifacts.push_fallback_index(legacy_registry, unique_repo, subject, referrer)
    served = cosign_artifacts.served_bundle_signature(legacy_registry, unique_repo, referrer.blob_digest)
    assert served != cosign_artifacts.golden_bundle_signature("keyless"), (
        "the mutation did not land; the registry still serves cosign's signature"
    )

    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")
    result = _verify_golden(
        runner, f"{legacy_registry}/{unique_repo}@{subject}", identity=identity, issuer=issuer,
    )
    _refusal(result, 65, "signature_invalid", "a corrupted bundle behind the fallback tag")


# ──────────────────────────────────────────────────────────────────────────────
# Criterion 3 — `--platform` narrowing into an index
#
# **Documented gap.** The plan's index-*level* positive — a signature on the
# enclosing index satisfying a pinned platform manifest — is not constructible
# from the committed bytes. Every golden fixture signs the same OCI **image
# manifest** (`sha256:47f8439…`), and its DSSE statement names that digest; an
# image *index* can never hash to it, so no committed cosign artifact carries a
# signature over an index. Building one would mean signing at test time, which
# is exactly the dependency these fixtures exist to remove. C-008's containment
# rule is pinned at its own seam instead — `pipeline.rs`'s
# `index_signature_subject` table and the fall-through guard added in 6decb6e4.
#
# What is reachable with cosign's bytes is the pair below: the narrowing path
# that a cosign user on a multi-platform tag actually takes, and the fail-closed
# half — a signature parked on the index is not credited to a child it does not
# name.
# ──────────────────────────────────────────────────────────────────────────────


def _push_index_over(registry: str, repo: str, child: str, child_size: int, tag: str) -> tuple[str, int]:
    """Tag a single-platform image index over ``child``; return ``(digest, size)``."""
    index = {
        "schemaVersion": 2,
        "mediaType": IMAGE_INDEX_MEDIA_TYPE,
        "manifests": [{
            "mediaType": IMAGE_MANIFEST_MEDIA_TYPE,
            "digest": child,
            "size": child_size,
            "platform": {"os": "linux", "architecture": "amd64"},
        }],
    }
    digest, _ = push_manifest(registry, repo, index, reference=tag)
    return digest, len(json.dumps(index).encode())


def test_verify_narrows_into_an_index_and_accepts_cosigns_signature_on_the_platform_manifest(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """Criterion 3, the reachable half: `--platform` selects the signed child.

    The reference resolves to an index; `--platform linux/amd64` narrows to the
    manifest cosign signed. `subject_digest` must be that child and never the
    index — reporting the index would mean the verdict was attributed to an
    object whose bytes no signature covers.
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    index_digest, _ = _push_index_over(ocx.registry, unique_repo, subject, size, "v1")
    assert index_digest != subject
    referrer = cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "keyless",
        {"mediaType": IMAGE_MANIFEST_MEDIA_TYPE, "digest": subject, "size": size},
    )
    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")

    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}:v1",
        identity=identity, issuer=issuer, platform="linux/amd64",
    )
    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr.strip()}"
    data = json.loads(result.stdout)["data"]
    assert data["subject_digest"] == subject, (
        f"the verdict must name the narrowed platform manifest, not the index {index_digest}"
    )
    assert data["referrer_digest"] == referrer.digest


def test_verify_does_not_credit_a_signature_parked_on_the_index_to_the_platform_child(
    ocx: OcxRunner, unique_repo: str
) -> None:
    """Criterion 3, fail-closed: membership is not a substitute for the subject binding.

    Identical registry state to the cell above except for one field — the
    referrer's ``subject`` names the **index**, while the bundle inside it still
    attests the child. The child *is* a member of the index, so the second
    discovery pass runs and finds this referrer; it must still be refused,
    because a signature counts for the digest its statement names and for no
    other. Crediting it would let anyone attach an unrelated valid signature at
    the index and have every child inherit it.
    """
    subject, size = cosign_artifacts.push_subject(ocx.registry, unique_repo)
    index_digest, index_size = _push_index_over(ocx.registry, unique_repo, subject, size, "v1")
    cosign_artifacts.push_bundle_referrer(
        ocx.registry, unique_repo, "keyless",
        {"mediaType": IMAGE_INDEX_MEDIA_TYPE, "digest": index_digest, "size": index_size},
    )
    status, index = list_referrers(ocx.registry, unique_repo, index_digest)
    assert status == 200 and index and index["manifests"], (
        "the referrer must be attached to the index, or this cell refuses for the "
        "trivial reason that nothing was pushed"
    )

    identity, issuer = cosign_artifacts.golden_certificate_identity("keyless")
    result = _verify_golden(
        ocx, f"{ocx.registry}/{unique_repo}:v1",
        identity=identity, issuer=issuer, platform="linux/amd64",
    )
    _refusal(result, 79, "no_signatures_found", "a signature parked on the enclosing index")
