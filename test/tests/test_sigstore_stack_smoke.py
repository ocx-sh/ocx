"""End-to-end proof that ocx signs and verifies against real Sigstore services.

One test, deliberately. It is the gate the rest of the signing suite is
retargeted behind: if this is red, nothing downstream of it means anything.
"""

from __future__ import annotations

import json

from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack


def test_sign_then_verify_against_real_sigstore(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    signed = ocx.run(
        "package", "sign",
        *sigstore_stack.sign_args(identity_token),
        published_package.short,
        check=False,
    )
    assert signed.returncode == 0, (
        f"sign failed against real Fulcio/Rekor\n"
        f"stdout: {signed.stdout}\nstderr: {signed.stderr}"
    )

    verified = ocx.run(
        "package", "verify",
        *sigstore_stack.verify_args(),
        published_package.short,
        check=False,
    )
    assert verified.returncode == 0, (
        f"verify failed against real Fulcio/Rekor\n"
        f"stdout: {verified.stdout}\nstderr: {verified.stderr}"
    )


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    signed = ocx.run("package", "sign", *stack.sign_args(token), pkg.short, check=False)
    assert signed.returncode == 0, f"sign failed\nstdout: {signed.stdout}\nstderr: {signed.stderr}"


def _assert_rejected(result: object, slug: str) -> None:
    """Assert the exact refusal, not merely a refusal.

    Exit 65 covers every data-integrity failure including `bundle_parse_failed`,
    so a tamper test asserting only the code would pass against a verifier that
    never ran the check under test and simply choked on the altered JSON. The
    slug is what distinguishes the two.
    """
    assert result.returncode == 65, f"expected exit 65, got {result.returncode}: {result.stdout}"
    detail = json.loads(result.stdout)["error"]["detail"]
    assert detail == slug, f"expected {slug!r}, got {detail!r}: {result.stdout}"


def test_tampered_signed_entry_timestamp_is_rejected(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A SET altered after the log signed it must fail closed at exit 65.

    Everything else in the bundle is authentic, so this isolates the SET check
    from the certificate and signature checks. Exit 65 not 83: a tampered
    bundle is a data-integrity failure and retrying cannot fix it.
    """
    _sign(ocx, sigstore_stack, identity_token, published_package)
    digest, size = adversarial.subject_of(ocx.registry, published_package.repo, published_package.tag)
    adversarial.tamper_signed_entry_timestamp(ocx.registry, published_package.repo, digest, size)

    verified = ocx.run("package", "verify", *sigstore_stack.verify_args(), published_package.short, check=False)
    _assert_rejected(verified, "rekor_set_invalid")


def test_tampered_inclusion_proof_is_rejected(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A corrupted Merkle root must fail closed even though the SET still verifies.

    The SET is the log's promise to include; the inclusion proof is evidence the
    entry IS in a tree whose root the log signed. They are independent, so a
    verifier that checked only the promise would pass this — which is exactly
    what makes it worth asserting.
    """
    _sign(ocx, sigstore_stack, identity_token, published_package)
    digest, size = adversarial.subject_of(ocx.registry, published_package.repo, published_package.tag)
    adversarial.tamper_inclusion_proof(ocx.registry, published_package.repo, digest, size)

    verified = ocx.run("package", "verify", *sigstore_stack.verify_args(), published_package.short, check=False)
    _assert_rejected(verified, "rekor_set_invalid")


def test_certificate_outside_the_trust_root_is_rejected(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A well-formed leaf with the right identity but no path to the CA fails.

    The spliced certificate carries the same identity and issuer extensions the
    real one does, so passing this proves the chain is verified rather than the
    identity merely being read off whatever cert is present.
    """
    _sign(ocx, sigstore_stack, identity_token, published_package)
    digest, size = adversarial.subject_of(ocx.registry, published_package.repo, published_package.tag)
    leaf = adversarial.throwaway_leaf_der(sigstore_stack.identity, sigstore_stack.issuer)
    adversarial.splice_foreign_certificate(ocx.registry, published_package.repo, digest, size, leaf)

    verified = ocx.run("package", "verify", *sigstore_stack.verify_args(), published_package.short, check=False)
    _assert_rejected(verified, "cert_chain_invalid")
