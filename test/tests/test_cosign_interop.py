"""Bidirectional bundle interop between ocx and cosign 3.x (issue #197).

What is under test is the Sigstore bundle, in both directions: cosign accepts
what ocx produced, and ocx accepts what cosign produced. Both run against the
same local Fulcio and Rekor, so a pass means the two implementations agree on
the certificate chain, the message signature, and the transparency-log entry —
not merely that both parse the same JSON.

These five tests are the payload-agreement layer, not the discovery layer: each
hands the bundle to its consumer as a file — `--bundle` on a cosign blob
command, a referrer pushed straight into the registry for ocx — so a pass
proves the two implementations agree on the bytes of a signature, independent
of how either one would have found it. `test_cosign_matrix_*.py` is where
discovery is asserted: it drives `cosign verify <ref>` and `ocx package verify`
against real registries, across the referrers-API and fallback-tag paths
measured in `analysis_cosign_interop_probes.md`.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path

import pytest

from src import registry as reg
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import adversarial, attestations, cosign
from tests.fixtures.sigstore_stack import SigstoreStack

#: `--trusted-root` below is cosign's own flag, not an ocx one. If these ever
#: fail, the cause is upstream of the flag — do not rename or replace it.
@pytest.fixture(scope="session")
def cosign_image() -> str:
    """Pull the pinned cosign image once.

    Raises rather than skips, matching `sigstore_stack`: a skipped interop test
    is indistinguishable from a passing one, and this is the only evidence that
    ocx's bundles are readable by anything but ocx.
    """
    import subprocess

    pulled = subprocess.run(
        ["docker", "pull", "--quiet", cosign.COSIGN_IMAGE],
        capture_output=True,
        text=True, check=False,
    )
    assert pulled.returncode == 0, f"could not pull {cosign.COSIGN_IMAGE}:\n{pulled.stderr}"
    return cosign.COSIGN_IMAGE


def _subject(ocx: OcxRunner, pkg: PackageInfo) -> tuple[str, int, bytes]:
    """The signed platform manifest: its digest, its size, and its exact bytes.

    The bytes are what both tools sign over, so they must be the ones the
    registry served rather than a re-encoding — a re-serialised equal document
    hashes differently and would fail for a reason that has nothing to do with
    interop.
    """
    digest, size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    raw, _ = reg.fetch_manifest_raw(ocx.registry, pkg.repo, digest)
    return digest, size, raw


def test_cosign_verifies_a_bundle_ocx_produced(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """cosign 3.x accepts an ocx signature end to end.

    Identity and issuer are asserted through cosign's own flags rather than
    ocx's, so a pass also proves the certificate extensions ocx relies on are
    the ones upstream reads.
    """
    signed = ocx.run(
        "package", "sign", *sigstore_stack.sign_args(identity_token), published_package.short, check=False
    )
    assert signed.returncode == 0, f"sign failed\nstdout: {signed.stdout}\nstderr: {signed.stderr}"

    _digest, _size, subject_bytes = _subject(ocx, published_package)
    bundle = adversarial.signature_bundle(ocx.registry, published_package.repo, _digest)

    blob = cosign.stage(tmp_path, "subject.manifest", subject_bytes)
    bundle_file = cosign.stage(tmp_path, "bundle.sigstore.json", bundle)
    trusted_root = cosign.stage(
        tmp_path, "trusted_root.json", sigstore_stack.trusted_root_json.read_bytes()
    )

    verified = cosign.run(
        tmp_path,
        "verify-blob",
        # No `--new-bundle-format`: cosign 3 detects the new format from the
        # file's own contents, and the flag is not even registered on
        # `verify-blob`. What that flag's error message ("--trusted-root only
        # supported with --new-bundle-format") actually reports is a bundle
        # that failed v0.3 profile validation -- which is what this test
        # catches if ocx ever regresses the bundle shape.
        "--bundle", bundle_file,
        "--trusted-root", trusted_root,
        "--certificate-identity", sigstore_stack.identity,
        "--certificate-oidc-issuer", sigstore_stack.issuer,
        blob,
    )
    assert verified.returncode == 0, (
        f"cosign rejected an ocx-produced bundle\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )


def test_ocx_refuses_a_cosign_blob_signature_bundle(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """`ocx package verify` refuses a `cosign sign-blob` bundle — decision D2.

    `sign-blob` emits a `messageSignature` bundle: a raw signature over the
    blob's digest, carrying no statement about *what* was signed. An ocx image
    signature is a DSSE in-toto Statement whose predicateType is
    `https://sigstore.dev/cosign/sign/v1` and whose subject binds the manifest
    digest — the shape `cosign sign` writes against a registry. Decision D2 of
    `design_spec_cosign_parity.md` deleted `messageSignature` from the read path
    as well as the write path, so a bundle that carries one is no longer a
    usable candidate and the ANY-of scan ends with nothing to report.

    This test still runs cosign, still pushes the bundle as a real
    `SIGSTORE_BUNDLE_V03` referrer and still makes ocx read it back out of the
    registry — only the expected verdict changed, from acceptance to a refusal
    naming its cause.

    The meta-plan's criterion for this file — "the existing 5 blob-level tests
    stay and keep passing" — is met in letter: this test stays, and it passes.
    Its spirit, that ocx verifies cosign *blob* signatures, was retired by D2,
    which deleted `messageSignature` from the read path along with the write
    path. That retirement is correct, and this test's own inversion is the
    evidence for it: the criterion's premise was that registry-level discovery
    was impossible, so handing a bundle over as a file was the only testable
    contract — probe P3 falsified that premise (`cosign verify <ref>` reads the
    Referrers API, the OCI fallback tag, and the `.sig` sidecar), and a
    criterion satisfied by a test that asserts the opposite of what it meant is
    not a criterion. `test_cosign_matrix_*.py` carries the replacement: 14 of
    the 16 image-level cells and all 4 attest cells assert parity, and each
    demonstrated its own refusal on a corrupted signature it proved landed.

    The other two are the reason the replacement is a criterion and not a
    slogan. M-13 and M-14 — a keyless ocx sidecar that ocx accepts and cosign
    refuses — are **inverted**, named for what they measure rather than for
    what was wanted. They pass by asserting the break, and they red on the day
    it is fixed, at which point they become the parity cells they were meant to
    be. M-03 and M-04 (an ocx key-mode bundle read by cosign) were a third such
    pair until the write side stopped emitting
    `dsseEnvelope.signatures[0].keyid` — cosign omits that member in key mode
    as in keyless, and its DSSE verifier matches candidates on it, so every ocx
    key-mode signature was filtered out before any cryptography ran. Those two
    are parity cells again.

    M-13/M-14 carried the `divergence` marker, with the two downgrade cells,
    while ocx accepted a keyless sidecar cosign refuses for want of a
    transparency-log entry. All four are parity cells now and no test in the
    matrix carries the marker; it stays registered because the honest way to
    cite this matrix as compatibility evidence is a recipe that can still
    exclude a disclosure, and the next one should be marked rather than
    counted.

    This test and its four siblings stay as the payload-agreement layer
    underneath that matrix, not as its discovery evidence.
    """
    digest, size, subject_bytes = _subject(ocx, published_package)

    blob = cosign.stage(tmp_path, "subject.manifest", subject_bytes)
    token = cosign.stage(tmp_path, "identity-token", identity_token.read_bytes())
    # cosign verifies the log entry it just obtained before it writes the bundle,
    # so signing needs the trust root as much as verifying does. Without it the
    # sign fails against the local Rekor, not against ocx.
    trusted_root = cosign.stage(
        tmp_path, "trusted_root.json", sigstore_stack.trusted_root_json.read_bytes()
    )
    config = cosign.signing_config(
        tmp_path,
        fulcio_url=sigstore_stack.fulcio_url,
        rekor_url=sigstore_stack.rekor_url,
        oidc_url=sigstore_stack.issuer,
    )

    signed = cosign.run(
        tmp_path,
        "sign-blob",
        "--signing-config", config,
        "--trusted-root", trusted_root,
        "--identity-token", token,
        "--bundle", "cosign-bundle.json",
        "--yes",
        blob,
    )
    assert signed.returncode == 0, (
        f"cosign could not sign against the local stack\n"
        f"stdout: {signed.stdout}\nstderr: {signed.stderr}"
    )

    bundle = (tmp_path / "cosign-bundle.json").read_bytes()
    reg.push_referrer(
        ocx.registry,
        published_package.repo,
        digest,
        size,
        artifact_type=adversarial.SIGSTORE_BUNDLE_V03,
        payload=bundle,
    )

    assert "messageSignature" in json.loads(bundle), (
        "cosign wrote something other than a blob-signature bundle; the refusal "
        "below would then be about a different shape"
    )

    verified = ocx.run(
        "package", "verify", *sigstore_stack.verify_args(), published_package.short, check=False
    )
    assert verified.returncode == 79, (
        f"expected exit 79 (NotFound), got {verified.returncode}\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )
    # 79 alone cannot tell "the referrer carried no usable bundle" from "the
    # push never landed": both are NotFound. The slug is what pins the cause.
    envelope = json.loads(verified.stdout)
    assert envelope["error"]["detail"] == "no_signatures_found", (
        f"the blob bundle must be skipped as unusable, not fail for another "
        f"reason; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Attestations (S-011) — the milestone's interop criterion
# ──────────────────────────────────────────────────────────────────────────────


def test_cosign_verifies_an_attestation_ocx_produced(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """S-011: `cosign verify-blob-attestation --type cyclonedx` accepts ocx's work.

    The signature tests above prove the two implementations agree on a
    *message-signature* bundle. This proves it for the DSSE half, which is a
    strictly larger contract: on top of the certificate chain and the log entry,
    cosign must also read the in-toto Statement ocx wrote, resolve the same
    `cyclonedx` alias to the same predicateType URI, and — with `--check-claims`
    — find the subject manifest's own digest in the Statement's subject array.

    `--check-claims` is left at its default rather than disabled: without it
    cosign verifies the envelope and never looks at what it is an attestation
    *of*, which would leave the subject binding — the whole reason the
    attestation is attached to this manifest and not another — unasserted.

    `verify-blob-attestation` rather than `verify-attestation` for the same
    reason the signature tests use the blob commands: this test asserts payload
    agreement, handing cosign the bundle as a file rather than asking it to
    find it on a registry. The image-level counterpart — `cosign
    verify-attestation` resolving the referrer itself, both directions, both
    registries — is `test_cosign_matrix_attest.py`'s A-01..A-04.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())

    attested = ocx.run(
        "package", "attest",
        *sigstore_stack.sign_args(identity_token),
        "--predicate", str(predicate),
        "--type", "cyclonedx",
        published_package.short,
        check=False,
    )
    assert attested.returncode == 0, (
        f"attest failed\nstdout: {attested.stdout}\nstderr: {attested.stderr}"
    )

    digest, _size, subject_bytes = _subject(ocx, published_package)
    bundle = attestations.attestation_bundle(ocx.registry, published_package.repo, digest)

    blob = cosign.stage(tmp_path, "subject.manifest", subject_bytes)
    bundle_file = cosign.stage(tmp_path, "attestation.sigstore.json", bundle)
    trusted_root = cosign.stage(
        tmp_path, "trusted_root.json", sigstore_stack.trusted_root_json.read_bytes()
    )

    verified = cosign.run(
        tmp_path,
        "verify-blob-attestation",
        "--bundle", bundle_file,
        "--trusted-root", trusted_root,
        "--type", "cyclonedx",
        "--certificate-identity", sigstore_stack.identity,
        "--certificate-oidc-issuer", sigstore_stack.issuer,
        blob,
    )
    assert verified.returncode == 0, (
        f"cosign rejected an ocx-produced attestation\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )


def test_cosign_rejects_an_ocx_attestation_narrowed_to_the_wrong_type(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """The negative control for the interop test above.

    Without it, `--type cyclonedx` passing proves only that cosign accepted the
    bundle — not that it read the predicateType ocx wrote. If cosign returned 0
    for `--type spdxjson` as well, the type assertion in the sibling test would
    be decorative, and a regression that published the wrong resolved URI would
    sail through both.
    """
    predicate = tmp_path / "sbom.cdx.json"
    predicate.write_bytes(attestations.PRETTY_CYCLONEDX_PATH.read_bytes())

    attested = ocx.run(
        "package", "attest",
        *sigstore_stack.sign_args(identity_token),
        "--predicate", str(predicate),
        "--type", "cyclonedx",
        published_package.short,
        check=False,
    )
    assert attested.returncode == 0, f"attest failed: {attested.stderr}"

    digest, _size, subject_bytes = _subject(ocx, published_package)
    bundle = attestations.attestation_bundle(ocx.registry, published_package.repo, digest)

    blob = cosign.stage(tmp_path, "subject.manifest", subject_bytes)
    bundle_file = cosign.stage(tmp_path, "attestation.sigstore.json", bundle)
    trusted_root = cosign.stage(
        tmp_path, "trusted_root.json", sigstore_stack.trusted_root_json.read_bytes()
    )

    mismatched = cosign.run(
        tmp_path,
        "verify-blob-attestation",
        "--bundle", bundle_file,
        "--trusted-root", trusted_root,
        "--type", "spdxjson",
        "--certificate-identity", sigstore_stack.identity,
        "--certificate-oidc-issuer", sigstore_stack.issuer,
        blob,
    )
    assert mismatched.returncode != 0, (
        "cosign accepted a CycloneDX attestation under --type spdxjson, so the "
        "sibling test's --type assertion proves nothing about the predicateType "
        "ocx published\n"
        f"stdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
    )
    assert "invalid predicate type" in mismatched.stderr, (
        "cosign refused for some reason other than the predicate type, so this "
        "controls for the wrong thing\n"
        f"stdout: {mismatched.stdout}\nstderr: {mismatched.stderr}"
    )


def test_ocx_verifies_an_attestation_cosign_produced(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """S-011 direction (b): `ocx package verify --attestation` accepts cosign's work.

    Also the wire-truth row for the in-toto statement version. cosign writes the
    v0.1 `_type` spelling and ocx writes v1, so ocx's read side has to accept
    both. The v0.1 spelling is asserted on the payload *before* ocx is asked to
    verify it -- without that, a pass would prove only that some cosign bundle
    verified, and would keep passing on the day cosign starts emitting v1.
    """
    digest, size, subject_bytes = _subject(ocx, published_package)

    blob = cosign.stage(tmp_path, "subject.manifest", subject_bytes)
    predicate = cosign.stage(
        tmp_path, "sbom.cdx.json", attestations.PRETTY_CYCLONEDX_PATH.read_bytes()
    )
    token = cosign.stage(tmp_path, "identity-token", identity_token.read_bytes())
    trusted_root = cosign.stage(
        tmp_path, "trusted_root.json", sigstore_stack.trusted_root_json.read_bytes()
    )
    config = cosign.signing_config(
        tmp_path,
        fulcio_url=sigstore_stack.fulcio_url,
        rekor_url=sigstore_stack.rekor_url,
        oidc_url=sigstore_stack.issuer,
    )

    attested = cosign.run(
        tmp_path,
        "attest-blob",
        "--signing-config", config,
        "--trusted-root", trusted_root,
        "--identity-token", token,
        "--predicate", predicate,
        "--type", "cyclonedx",
        "--bundle", "cosign-attestation.json",
        "--yes",
        blob,
    )
    assert attested.returncode == 0, (
        f"cosign could not attest against the local stack\n"
        f"stdout: {attested.stdout}\nstderr: {attested.stderr}"
    )

    bundle = (tmp_path / "cosign-attestation.json").read_bytes()
    payload = json.loads(bundle)["dsseEnvelope"]["payload"]
    statement = json.loads(base64.b64decode(payload))
    assert statement["_type"] == "https://in-toto.io/Statement/v0.1", (
        "this row exists to prove ocx accepts the older statement spelling; "
        f"cosign emitted {statement['_type']!r} instead, so it no longer covers that"
    )

    reg.push_referrer(
        ocx.registry,
        published_package.repo,
        digest,
        size,
        artifact_type=adversarial.SIGSTORE_BUNDLE_V03,
        payload=bundle,
    )

    verified = ocx.run(
        "package", "verify", "--attestation", *sigstore_stack.verify_args(),
        published_package.short, check=False,
    )
    assert verified.returncode == 0, (
        f"ocx rejected a cosign-produced attestation\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )
