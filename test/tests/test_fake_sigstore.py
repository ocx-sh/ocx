# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Smoke tests for the fake Sigstore stack.

These tests verify that ``FakeSigstoreStack`` can be instantiated, spawns
working HTTP servers, and that the key material flows correctly through the
OIDC → Fulcio → Rekor pipeline.

They do NOT exercise the Rust sign/verify pipeline (that is Phase 5c). These
are pure-Python fixture validation tests so regressions in the test
infrastructure are caught before Phase 5c integration begins.
"""
from __future__ import annotations

import base64
import json
import urllib.request
from pathlib import Path

import pytest

from tests.fixtures.fake_sigstore import (
    FakeFulcio,
    FakeRekor,
    FakeSigstoreStack,
    FAKE_AUDIENCE,
    FAKE_ISSUER_URL,
    FAKE_SUBJECT,
)


# ---------------------------------------------------------------------------
# Stack lifecycle
# ---------------------------------------------------------------------------


def test_stack_starts_and_shuts_down(tmp_path: Path) -> None:
    """FakeSigstoreStack starts three servers and shuts them down cleanly."""
    stack = FakeSigstoreStack(tmp_path)
    assert stack.fulcio_url.startswith("http://127.0.0.1:")
    assert stack.rekor_url.startswith("http://127.0.0.1:")
    assert stack.oidc_url.startswith("http://127.0.0.1:")
    stack.shutdown()


def test_stack_context_manager(tmp_path: Path) -> None:
    """FakeSigstoreStack works as a context manager."""
    with FakeSigstoreStack(tmp_path) as stack:
        assert stack.trust_root_pem_path().exists()
        assert stack.rekor_public_key_pem_path().exists()


# ---------------------------------------------------------------------------
# Trust root files
# ---------------------------------------------------------------------------


def test_trust_root_pem_is_valid_certificate(tmp_path: Path) -> None:
    """The Fulcio root PEM is a valid X.509 certificate."""
    from cryptography import x509

    with FakeSigstoreStack(tmp_path) as stack:
        pem_bytes = stack.trust_root_pem_path().read_bytes()
        cert = x509.load_pem_x509_certificate(pem_bytes)
        assert cert.subject.get_attributes_for_oid(
            x509.NameOID.COMMON_NAME
        )[0].value == "Fake Fulcio Test CA"


def test_rekor_public_key_pem_is_valid_ed25519(tmp_path: Path) -> None:
    """The Rekor public key PEM is a valid Ed25519 public key."""
    from cryptography.hazmat.primitives.serialization import load_pem_public_key
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    with FakeSigstoreStack(tmp_path) as stack:
        pem_bytes = stack.rekor_public_key_pem_path().read_bytes()
        key = load_pem_public_key(pem_bytes)
        assert isinstance(key, Ed25519PublicKey)


# ---------------------------------------------------------------------------
# OIDC issuer endpoints
# ---------------------------------------------------------------------------


def test_oidc_discovery_endpoint(tmp_path: Path) -> None:
    """/.well-known/openid-configuration returns the discovery document."""
    with FakeSigstoreStack(tmp_path) as stack:
        resp = urllib.request.urlopen(f"{stack.oidc_url}/.well-known/openid-configuration")
        doc = json.loads(resp.read())
    assert doc["issuer"] == FAKE_ISSUER_URL
    assert "jwks_uri" in doc
    assert "ES256" in doc["id_token_signing_alg_values_supported"]


def test_oidc_jwks_endpoint(tmp_path: Path) -> None:
    """/.well-known/jwks.json returns a JWKS with one ES256 P-256 key."""
    with FakeSigstoreStack(tmp_path) as stack:
        resp = urllib.request.urlopen(f"{stack.oidc_url}/.well-known/jwks.json")
        doc = json.loads(resp.read())
    keys = doc["keys"]
    assert len(keys) == 1
    key = keys[0]
    assert key["kty"] == "EC"
    assert key["crv"] == "P-256"
    assert key["alg"] == "ES256"
    assert "x" in key and "y" in key


# ---------------------------------------------------------------------------
# OIDC token minting
# ---------------------------------------------------------------------------


def test_oidc_token_contains_expected_claims(tmp_path: Path) -> None:
    """oidc_token() mints an ES256 JWT with the expected claims (verify-only, no expiry check)."""
    import jwt as pyjwt

    with FakeSigstoreStack(tmp_path) as stack:
        token = stack.oidc_token()
        # Decode without verification to inspect claims
        claims = pyjwt.decode(token, options={"verify_signature": False})

    assert claims["iss"] == FAKE_ISSUER_URL
    assert claims["sub"] == FAKE_SUBJECT
    assert claims["aud"] == FAKE_AUDIENCE
    assert claims["email"] == FAKE_SUBJECT
    assert claims["exp"] > claims["iat"]


def test_oidc_token_verifies_against_jwks(tmp_path: Path) -> None:
    """The minted token verifies correctly against the JWKS public key."""
    import jwt as pyjwt

    with FakeSigstoreStack(tmp_path) as stack:
        token = stack.oidc_token()
        # Fetch the public key from JWKS
        resp = urllib.request.urlopen(f"{stack.oidc_url}/.well-known/jwks.json")
        jwks = json.loads(resp.read())
        key = pyjwt.algorithms.ECAlgorithm.from_jwk(json.dumps(jwks["keys"][0]))
        claims = pyjwt.decode(token, key, algorithms=["ES256"], audience=FAKE_AUDIENCE)

    assert claims["sub"] == FAKE_SUBJECT


# ---------------------------------------------------------------------------
# Fake Fulcio endpoint
# ---------------------------------------------------------------------------


def _mint_client_keypair():
    """Generate a P-256 ephemeral key pair for test use."""
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

    priv = ec.generate_private_key(ec.SECP256R1())
    pub_pem = priv.public_key().public_bytes(Encoding.PEM, PublicFormat.SubjectPublicKeyInfo)
    return priv, pub_pem


def test_fulcio_issues_cert_for_valid_token(tmp_path: Path) -> None:
    """Fulcio returns a signed cert chain for a valid OIDC token + CSR."""
    _, pub_pem = _mint_client_keypair()
    pub_b64 = base64.b64encode(pub_pem).decode()

    with FakeSigstoreStack(tmp_path) as stack:
        token = stack.oidc_token()
        req_body = json.dumps(
            {
                "credentials": {"oidcIdentityToken": token},
                "publicKeyRequest": {
                    "publicKey": {"algorithm": "ECDSA", "content": pub_b64},
                    "proofOfPossession": "",
                },
            }
        ).encode()
        req = urllib.request.Request(
            f"{stack.fulcio_url}/api/v2/signingCert",
            data=req_body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        resp = urllib.request.urlopen(req)
        resp_body = json.loads(resp.read())

    certs = resp_body["signedCertificateEmbeddedSct"]["chain"]["certificates"]
    assert len(certs) >= 1
    # Leaf cert must contain the subject email as SAN
    from cryptography import x509

    leaf = x509.load_pem_x509_certificate(certs[0].encode())
    sans = leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName)
    emails = sans.value.get_values_for_type(x509.RFC822Name)
    assert FAKE_SUBJECT in emails


def test_fulcio_rejects_invalid_token(tmp_path: Path) -> None:
    """Fulcio returns 403 for a token with a bad signature."""
    _, pub_pem = _mint_client_keypair()
    pub_b64 = base64.b64encode(pub_pem).decode()

    with FakeSigstoreStack(tmp_path) as stack:
        req_body = json.dumps(
            {
                "credentials": {"oidcIdentityToken": "not.a.valid.jwt"},
                "publicKeyRequest": {
                    "publicKey": {"algorithm": "ECDSA", "content": pub_b64},
                    "proofOfPossession": "",
                },
            }
        ).encode()
        req = urllib.request.Request(
            f"{stack.fulcio_url}/api/v2/signingCert",
            data=req_body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            urllib.request.urlopen(req)
            pytest.fail("expected HTTP error from Fulcio for invalid token")
        except urllib.error.HTTPError as exc:
            assert exc.code == 403


# ---------------------------------------------------------------------------
# Fake Rekor endpoint
# ---------------------------------------------------------------------------


def _make_hashedrekord_body(pub_b64: str) -> bytes:
    return json.dumps(
        {
            "kind": "hashedrekord",
            "apiVersion": "0.0.1",
            "spec": {
                "signature": {
                    "content": base64.b64encode(b"fake-sig").decode(),
                    "publicKey": {"content": pub_b64},
                },
                "data": {
                    "hash": {
                        "algorithm": "sha256",
                        "value": "a" * 64,
                    }
                },
            },
        }
    ).encode()


def test_rekor_accepts_hashedrekord_entry(tmp_path: Path) -> None:
    """Rekor returns a log entry with a valid SET on POST /api/v1/log/entries."""
    _, pub_pem = _mint_client_keypair()
    pub_b64 = base64.b64encode(pub_pem).decode()

    with FakeSigstoreStack(tmp_path) as stack:
        req = urllib.request.Request(
            f"{stack.rekor_url}/api/v1/log/entries",
            data=_make_hashedrekord_body(pub_b64),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        resp = urllib.request.urlopen(req)
        assert resp.status == 201
        body = json.loads(resp.read())

    assert len(body) == 1
    _uuid, entry = next(iter(body.items()))
    assert "logIndex" in entry
    assert "integratedTime" in entry
    assert "logID" in entry
    assert entry["verification"]["signedEntryTimestamp"]


def test_rekor_set_verifies_with_public_key(tmp_path: Path) -> None:
    """The Signed Entry Timestamp in a Rekor entry verifies with the published public key."""
    from cryptography.hazmat.primitives.serialization import load_pem_public_key
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    _, pub_pem = _mint_client_keypair()
    pub_b64 = base64.b64encode(pub_pem).decode()

    with FakeSigstoreStack(tmp_path) as stack:
        req_body_bytes = _make_hashedrekord_body(pub_b64)
        req = urllib.request.Request(
            f"{stack.rekor_url}/api/v1/log/entries",
            data=req_body_bytes,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        resp = urllib.request.urlopen(req)
        body = json.loads(resp.read())
        _uuid, entry = next(iter(body.items()))

        # Reconstruct the canonical payload that was signed.
        canonical = json.dumps(
            {
                "body": base64.b64encode(req_body_bytes).decode(),
                "integratedTime": entry["integratedTime"],
                "logID": entry["logID"],
                "logIndex": entry["logIndex"],
            },
            sort_keys=True,
        ).encode()

        set_bytes = base64.b64decode(entry["verification"]["signedEntryTimestamp"])
        pub_key_pem = stack.rekor_public_key_pem_path().read_bytes()

    pub_key = load_pem_public_key(pub_key_pem)
    assert isinstance(pub_key, Ed25519PublicKey)
    # verify() raises InvalidSignature on failure
    pub_key.verify(set_bytes, canonical)


def test_rekor_log_index_increments(tmp_path: Path) -> None:
    """Sequential entries get increasing log indices."""
    _, pub_pem = _mint_client_keypair()
    pub_b64 = base64.b64encode(pub_pem).decode()

    with FakeSigstoreStack(tmp_path) as stack:
        indices = []
        for _ in range(3):
            req = urllib.request.Request(
                f"{stack.rekor_url}/api/v1/log/entries",
                data=_make_hashedrekord_body(pub_b64),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            resp = urllib.request.urlopen(req)
            body = json.loads(resp.read())
            _uuid, entry = next(iter(body.items()))
            indices.append(entry["logIndex"])

    assert indices == sorted(indices), f"log indices not monotonically increasing: {indices}"


def test_rekor_public_key_endpoint(tmp_path: Path) -> None:
    """GET /api/v1/log/publicKey returns the Ed25519 public key PEM."""
    with FakeSigstoreStack(tmp_path) as stack:
        resp = urllib.request.urlopen(f"{stack.rekor_url}/api/v1/log/publicKey")
        pem = resp.read()

    assert pem.startswith(b"-----BEGIN PUBLIC KEY-----")


# ---------------------------------------------------------------------------
# Pytest fixture integration smoke test
# ---------------------------------------------------------------------------


def test_fixtures_are_wired_in_conftest(
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """conftest.py re-exports the three fixtures and they return correct types."""
    assert isinstance(fake_fulcio, FakeFulcio)
    assert fake_fulcio.url.startswith("http://127.0.0.1:")
    assert fake_fulcio.root_pem.exists()

    assert isinstance(fake_rekor, FakeRekor)
    assert fake_rekor.url.startswith("http://127.0.0.1:")
    assert fake_rekor.public_key_pem.exists()

    assert isinstance(fake_oidc_token, str)
    assert fake_oidc_token.count(".") == 2, "expected JWT with 3 parts"
