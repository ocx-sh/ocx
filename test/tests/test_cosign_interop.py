"""Bidirectional bundle interop between ocx and cosign 3.x (issue #197).

What is under test is the Sigstore bundle, in both directions: cosign accepts
what ocx produced, and ocx accepts what cosign produced. Both run against the
same local Fulcio and Rekor, so a pass means the two implementations agree on
the certificate chain, the message signature, and the transparency-log entry —
not merely that both parse the same JSON.

Discovery is deliberately out of scope. ocx publishes its signature through the
OCI 1.1 Referrers API and has no `sha256-<hex>.sig` tag-schema fallback; cosign
3.x has only the tag schema. That divergence is a documented ocx decision, so
these tests hand each tool the bundle directly rather than asserting a
cross-tool registry lookup that cannot succeed by design.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src import registry as reg
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import adversarial, cosign
from tests.fixtures.sigstore_stack import SigstoreStack


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
        text=True,
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


def test_ocx_verifies_a_bundle_cosign_produced(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    cosign_image: str,
    tmp_path: Path,
) -> None:
    """`ocx package verify` accepts a signature cosign 3.x produced.

    cosign signs the subject manifest bytes as a blob and its bundle is attached
    as an ordinary referrer, which is the shape ocx discovers. Nothing about the
    bundle is rewritten on the way in.
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

    verified = ocx.run(
        "package", "verify", *sigstore_stack.verify_args(), published_package.short, check=False
    )
    assert verified.returncode == 0, (
        f"ocx rejected a cosign-produced bundle\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}\n"
        f"bundle: {json.loads(bundle).keys()}"
    )
