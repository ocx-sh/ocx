"""Mutate a real Sigstore bundle in the registry, one field at a time.

The fake Sigstore stack produced hostile artifacts by flipping a server-side
switch before signing. Against a real Fulcio and Rekor there is no such switch:
the log signs what it signs. So a hostile artifact is made the way an attacker
would make one — take a genuine signed bundle off the registry, change exactly
one field, and put it back.

That is strictly better evidence. The fake's tampered SET was a signature over
a payload its own verifier could not reconstruct; these mutations are applied to
a bundle a real Rekor signed, so a test that passes proves ocx rejected material
that is authentic everywhere except the one field under test.

Each helper replaces the signature referrer in place (push the mutated manifest,
delete the original) so the subsequent verify sees exactly one candidate and
cannot pass by finding the untouched original.
"""

from __future__ import annotations

import base64
import copy
import json
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from src import registry as reg

SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"

#: `aud` Fulcio requires — the dex static client in `sigstore/fulcio-config.json`.
FULCIO_CLIENT_ID = "ocx-test"


def subject_of(registry: str, repo: str, tag: str, platform: str = "linux/amd64") -> tuple[str, int]:
    """The (digest, size) of the platform manifest a signature is attached to.

    Size comes from the bytes the registry served, not a re-encoding — the
    referrer's ``subject`` descriptor must match what the registry stored.
    """
    digest = reg.fetch_platform_manifest_digest(registry, repo, tag, platform=platform)
    raw, _ = reg.fetch_manifest_raw(registry, repo, digest)
    return digest, len(raw)


def signature_referrer(registry: str, repo: str, subject_digest: str) -> dict[str, Any]:
    """The one bundle referrer attached to ``subject_digest``.

    Raises rather than returning ``None`` on an unexpected count: a tamper test
    that silently found no referrer would assert against nothing.
    """
    status, index = reg.list_referrers(registry, repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03)
    if status != 200 or index is None:
        raise RuntimeError(f"referrers list failed ({status}) for {repo}@{subject_digest}")
    manifests = index.get("manifests") or []
    if len(manifests) != 1:
        raise RuntimeError(f"expected exactly 1 bundle referrer, found {len(manifests)}: {manifests!r}")
    return manifests[0]


def _bundle_of(registry: str, repo: str, subject_digest: str) -> tuple[dict[str, Any], dict[str, Any]]:
    """The one signature referrer's manifest descriptor and its decoded bundle."""
    referrer = signature_referrer(registry, repo, subject_digest)
    manifest = reg.get_manifest(registry, repo, referrer["digest"])
    bundle = json.loads(reg.get_blob(registry, repo, manifest["layers"][0]["digest"]))
    return referrer, bundle


def signature_bundle(registry: str, repo: str, subject_digest: str) -> dict[str, Any]:
    """The decoded Sigstore bundle of the one signature referrer on ``subject_digest``.

    The interop tests read the bundle without mutating it, but the "exactly one
    referrer" check is the same one every tamper helper needs — a test that
    silently picked one of several candidates would not be testing what it says.
    """
    _, bundle = _bundle_of(registry, repo, subject_digest)
    return bundle


def _replace_bundle(
    registry: str,
    repo: str,
    subject_digest: str,
    subject_size: int,
    mutate: Callable[[dict[str, Any]], None],
) -> None:
    """Apply ``mutate`` to the bundle JSON and swap the referrer for the result."""
    referrer, bundle = _bundle_of(registry, repo, subject_digest)

    mutated = copy.deepcopy(bundle)
    mutate(mutated)
    if mutated == bundle:
        raise RuntimeError("mutation was a no-op — the tamper test would assert against a valid bundle")

    reg.push_referrer(
        registry,
        repo,
        subject_digest,
        subject_size,
        artifact_type=SIGSTORE_BUNDLE_V03,
        payload=json.dumps(mutated, separators=(",", ":")).encode(),
    )
    reg.delete_manifest(registry, repo, referrer["digest"])


def _set_leaf_certificate(bundle: dict[str, Any], leaf_der: bytes) -> None:
    """Put ``leaf_der`` in the bundle's leaf-certificate slot, whichever it uses.

    Bundle v0.3 carries a single ``certificate``; v0.2 carried an
    ``x509CertificateChain``. Both are written here so the helper keeps working
    against an older bundle, and the other slot is removed so the spliced leaf
    is the only certificate present -- leaving both would let a verifier that
    reads the one we did not replace pass the test for the wrong reason.
    """
    material = bundle["verificationMaterial"]
    material.pop("x509CertificateChain", None)
    material["certificate"] = {"rawBytes": base64.b64encode(leaf_der).decode()}


def _tlog_entry(bundle: dict[str, Any]) -> dict[str, Any]:
    return bundle["verificationMaterial"]["tlogEntries"][0]


def _flip_last_byte(b64: str) -> str:
    raw = bytearray(base64.b64decode(b64))
    raw[-1] ^= 0xFF
    return base64.b64encode(bytes(raw)).decode()


def tamper_bundle_signature(registry: str, repo: str, subject_digest: str, subject_size: int) -> None:
    """Flip a byte of the signature the bundle's DSSE envelope carries.

    An image signature is a DSSE envelope since WP3, so the corrupted field is
    ``dsseEnvelope.signatures[0].sig`` — the successor of the ``messageSignature``
    slot cosign's pre-DSSE bundles used and the read path no longer accepts.
    Everything else — certificate, payload, SET, Merkle proof — stays exactly as
    the real stack produced it, so a passing test isolates the signature check.
    Expect exit 65.

    Raises on an envelope carrying anything other than one signature: a bundle
    with none would make the flip a no-op, and one with several would leave a
    signature the verifier could pass on.
    """

    def mutate(bundle: dict[str, Any]) -> None:
        signatures = bundle["dsseEnvelope"]["signatures"]
        if len(signatures) != 1:
            raise RuntimeError(f"expected exactly 1 DSSE signature to corrupt, found {len(signatures)}")
        signatures[0]["sig"] = _flip_last_byte(signatures[0]["sig"])

    _replace_bundle(registry, repo, subject_digest, subject_size, mutate)


def tamper_signed_entry_timestamp(registry: str, repo: str, subject_digest: str, subject_size: int) -> None:
    """Corrupt the Rekor SET so it no longer verifies under the log's key.

    Everything else — certificate, message signature, log entry body — stays
    exactly as the real stack produced it, so a passing test isolates the SET
    check. Expect exit 65 / ``rekor_set_invalid``.
    """

    def mutate(bundle: dict[str, Any]) -> None:
        promise = _tlog_entry(bundle)["inclusionPromise"]
        promise["signedEntryTimestamp"] = _flip_last_byte(promise["signedEntryTimestamp"])

    _replace_bundle(registry, repo, subject_digest, subject_size, mutate)


def tamper_inclusion_proof(registry: str, repo: str, subject_digest: str, subject_size: int) -> None:
    """Corrupt the Merkle root hash the inclusion proof must chain to.

    The SET stays valid, so this isolates the proof check from the promise
    check — the two are independent evidence and a verifier that skipped the
    Merkle path would still pass. Expect exit 65.
    """

    def mutate(bundle: dict[str, Any]) -> None:
        proof = _tlog_entry(bundle).get("inclusionProof")
        if not proof:
            raise RuntimeError("bundle carries no inclusion proof — nothing to tamper with")
        proof["rootHash"] = _flip_last_byte(proof["rootHash"])

    _replace_bundle(registry, repo, subject_digest, subject_size, mutate)


def shift_integrated_time_outside_certificate_window(
    registry: str, repo: str, subject_digest: str, subject_size: int, *, seconds_past: int = 3600
) -> int:
    """Move the Rekor entry's ``integratedTime`` past the certificate's ``notAfter``.

    Row 13 (CVE-2024-55655): a signature is only trustworthy if the log says it
    was made while the signing certificate was valid. Everything else — the
    leaf, the message signature, the logged body, the Merkle proof — stays as
    the real stack produced it, so a refusal isolates the window check.

    ``integratedTime`` is deliberately the ONE field that is not covered by the
    logged body: the SET signs it, but the entry body does not contain it, so
    editing it alone leaves every body-consistency check intact and the run
    reaches a genuine window comparison rather than dying earlier on a
    reconstruction mismatch.

    Returns the value written, so the caller can name it in an assertion.
    """
    from cryptography import x509

    _referrer, bundle = _bundle_of(registry, repo, subject_digest)
    material = bundle["verificationMaterial"]
    leaf_b64 = (material.get("certificate") or material["x509CertificateChain"]["certificates"][0])["rawBytes"]
    not_after = x509.load_der_x509_certificate(base64.b64decode(leaf_b64)).not_valid_after_utc
    shifted = int(not_after.timestamp()) + seconds_past

    def mutate(target: dict[str, Any]) -> None:
        _tlog_entry(target)["integratedTime"] = str(shifted)

    _replace_bundle(registry, repo, subject_digest, subject_size, mutate)
    return shifted


def splice_foreign_certificate(
    registry: str, repo: str, subject_digest: str, subject_size: int, leaf_der: bytes
) -> None:
    """Swap in a leaf certificate that does not chain to the trusted Fulcio CA.

    ``leaf_der`` comes from :func:`throwaway_leaf_der` — a self-signed cert from
    a CA the trust root has never heard of. Expect exit 65 (chain invalid).
    """

    def mutate(bundle: dict[str, Any]) -> None:
        _set_leaf_certificate(bundle, leaf_der)

    _replace_bundle(registry, repo, subject_digest, subject_size, mutate)


def add_rival_bundle_with_foreign_certificate(
    registry: str, repo: str, subject_digest: str, subject_size: int, leaf_der: bytes
) -> None:
    """Attach a SECOND bundle referrer, authentic except for an untrusted leaf.

    Unlike :func:`splice_foreign_certificate` this adds rather than replaces, so
    the subject ends up with two candidates: one that must fail and the genuine
    one. The local dex mints exactly one identity, so a rival candidate signed by
    a *different but trusted* signer is not obtainable here — an untrusted leaf
    is the available way to make one candidate fail while the other is real.
    """
    _, bundle = _bundle_of(registry, repo, subject_digest)
    rival = copy.deepcopy(bundle)
    _set_leaf_certificate(rival, leaf_der)
    reg.push_referrer(
        registry,
        repo,
        subject_digest,
        subject_size,
        artifact_type=SIGSTORE_BUNDLE_V03,
        payload=json.dumps(rival, separators=(",", ":")).encode(),
    )


def unreachable_rekor_url() -> str:
    """A loopback URL nothing is listening on.

    The fake stack made Rekor fail by serving a 503 on demand; the real one has
    no such switch, so unavailability is produced by pointing ocx somewhere dead.
    Connection-refused and 503 both classify as ``TransparencyLogUnavailable``, so the exit
    code under test is unchanged.

    Binding then closing is what makes the port free *and* known — an arbitrary
    high port could belong to something else on a developer's machine.
    """
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    return f"http://127.0.0.1:{port}"


def foreign_identity_token(issuer: str, identity: str, tmp_path: Path) -> Path:
    """An ES256 JWT with the right claims and a key the issuer never published.

    Every claim Fulcio reads — ``iss``, ``aud``, ``sub``, ``email`` — is correct,
    so a rejection can only come from the signature check against dex's JWKS.
    Written 0600 because `--identity-token-file` refuses a permissive file.
    """
    import jwt
    from cryptography.hazmat.primitives.asymmetric import ec

    now = int(time.time())
    token = jwt.encode(
        {
            "iss": issuer,
            "aud": FULCIO_CLIENT_ID,
            "sub": identity,
            "email": identity,
            "email_verified": True,
            "iat": now,
            "exp": now + 600,
        },
        ec.generate_private_key(ec.SECP256R1()),
        algorithm="ES256",
    )

    path = tmp_path / "foreign-identity-token"
    path.write_text(token)
    path.chmod(0o600)
    return path


def throwaway_leaf_der(identity: str, issuer: str) -> bytes:
    """A self-signed P-256 leaf carrying the right identity and the wrong CA.

    Deliberately well-formed and correctly identified: the only thing wrong with
    it is that nothing in the trust root signed it. A test using this proves the
    chain check runs, not that the parser rejects garbage.
    """
    import datetime

    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "ocx untrusted leaf")])
    now = datetime.datetime.now(datetime.UTC)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(hours=1))
        .add_extension(
            x509.SubjectAlternativeName([x509.RFC822Name(identity)]),
            critical=True,
        )
        # The next three are what make this a *leaf* rather than merely a
        # certificate. A verifier shaped like Fulcio's refuses anything that is
        # not one before it ever builds a chain, so a cert missing them would
        # fail as a malformed bundle and the test would stop proving that the
        # chain is checked at all.
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=False,
                crl_sign=False,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.CODE_SIGNING]),
            critical=False,
        )
        # 1.3.6.1.4.1.57264.1.8 — Fulcio's issuer (v2), a DER UTF8String.
        .add_extension(
            x509.UnrecognizedExtension(
                x509.ObjectIdentifier("1.3.6.1.4.1.57264.1.8"),
                b"\x0c" + bytes([len(issuer)]) + issuer.encode(),
            ),
            critical=False,
        )
        .sign(key, hashes.SHA256())
    )
    return cert.public_bytes(serialization.Encoding.DER)
