# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Fake Sigstore services for acceptance testing without live Fulcio/Rekor.

Per ADR D9 (``adr_oci_referrers_signing_v1.md``): CI must not depend on live
Sigstore. This module provides three HTTP servers that mimic the endpoints the
Rust sign/verify pipeline hits, plus a composite fixture that wires them
together.

Architecture
============

Each server runs in a daemon thread with a ``threading.Event`` for
readiness signalling. Servers bind on port 0 (OS assigns ephemeral port);
the bound address is read after ``server_bind()`` completes.

All servers speak plain HTTP — the Rust client will be pointed at
``http://127.0.0.1:<port>`` (C-S1-3 injection seam), avoiding TLS
certificate trust issues in test environments.

Fake services
=============

``FakeOidcIssuer``
    Mints ES256 JWTs with ``sub=test-signer@example.com``,
    ``iss=https://fake-oidc.test``, ``aud=sigstore``, 10-minute expiry.
    Exposes ``/.well-known/openid-configuration`` and
    ``/.well-known/jwks.json`` for discovery and key verification.

``FakeFulcio``
    Accepts ``POST /api/v2/signingCert``. Verifies the OIDC JWT (ES256,
    audience=sigstore), mints a P-256 leaf cert with SAN=email, Fulcio
    issuer OID ``1.3.6.1.4.1.57264.1.1``, 10-minute validity. Returns the
    Fulcio v2 JSON response shape with the self-signed cert chain.

``FakeRekor``
    Accepts ``POST /api/v1/log/entries`` with a ``hashedrekord:0.0.1``
    proposal. Assigns a sequential log index, sets ``integrated_time=now``,
    builds a canonical payload and signs it with an Ed25519 key to produce a
    Signed Entry Timestamp (SET). Returns the Rekor v1 entry JSON.
    Also serves ``GET /api/v1/log/publicKey`` for trust-root verification.

``FakeSigstoreStack``
    Composite that owns instances of all three fakes and exposes the
    convenience attributes used by test fixtures: ``fulcio_url``,
    ``rekor_url``, ``oidc_token()``, ``trust_root_pem_path()``.
"""
from __future__ import annotations

import base64
import dataclasses
import json
import logging
import socket
import struct
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from io import BytesIO
from pathlib import Path
from typing import Any

import pytest

# ---------------------------------------------------------------------------
# Crypto helpers (cryptography + PyJWT)
# ---------------------------------------------------------------------------

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519
from cryptography.hazmat.primitives.asymmetric.ec import (
    SECP256R1,
    EllipticCurvePublicKey,
)
from cryptography.x509.oid import NameOID, ExtendedKeyUsageOID
import datetime

import jwt as pyjwt

log = logging.getLogger(__name__)

# OID for Fulcio's OIDC issuer extension (1.3.6.1.4.1.57264.1.1)
_FULCIO_ISSUER_OID = x509.ObjectIdentifier("1.3.6.1.4.1.57264.1.1")

# Subject / issuer used by every fake token & cert in this module.
_FAKE_SUBJECT = "test-signer@example.com"
_FAKE_ISSUER_URL = "https://fake-oidc.test"
_FAKE_AUDIENCE = "sigstore"


# ---------------------------------------------------------------------------
# Public dataclasses (imported by test files — keep names stable)
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class FakeFulcio:
    """Handle for a running fake Fulcio server.

    ``url`` is the ``http://127.0.0.1:<port>`` base URL injected into sign
    pipelines via ``--fulcio-url``. ``root_pem`` is the self-signed CA PEM
    that must be loaded into the verify pipeline's ``TrustRoot`` so the
    leaf cert is trusted.
    """

    url: str
    root_pem: Path


@dataclasses.dataclass
class FakeRekor:
    """Handle for a running fake Rekor transparency log server.

    ``url`` is the ``http://127.0.0.1:<port>`` base URL injected via
    ``--rekor-url``. ``public_key_pem`` is the Ed25519 signing key's public
    PEM, used for SET verification.
    """

    url: str
    public_key_pem: Path


@dataclasses.dataclass
class FakeOidcIssuer:
    """Handle for a running fake OIDC issuer."""

    issuer: str
    subject: str
    audience: str


# ---------------------------------------------------------------------------
# OIDC key material (stable per process invocation, regenerated each run)
# ---------------------------------------------------------------------------


class _OidcKeyMaterial:
    """ES256 key pair for the fake OIDC issuer; stable within a test session."""

    def __init__(self) -> None:
        self._private = ec.generate_private_key(SECP256R1())
        self._public = self._private.public_key()
        # kid must be stable for JWKS lookup
        self._kid = uuid.uuid4().hex

    def sign_token(
        self,
        subject: str = _FAKE_SUBJECT,
        issuer: str = _FAKE_ISSUER_URL,
        audience: str = _FAKE_AUDIENCE,
    ) -> str:
        """Mint an ES256 JWT with 10-minute expiry."""
        now = int(time.time())
        payload = {
            "iss": issuer,
            "sub": subject,
            "aud": audience,
            "email": subject,
            "email_verified": True,
            "iat": now,
            "exp": now + 600,
            "nbf": now,
        }
        private_pem = self._private.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
        return pyjwt.encode(
            payload,
            private_pem,
            algorithm="ES256",
            headers={"kid": self._kid},
        )

    def jwks(self) -> dict[str, Any]:
        """Return a JWKS document containing the public key."""
        pub = self._public
        # Serialize the public key into raw x/y coordinates (uncompressed point).
        pub_numbers = pub.public_numbers()
        # P-256 uses 32-byte coordinates.
        def _int_to_b64(n: int, length: int = 32) -> str:
            raw = n.to_bytes(length, "big")
            return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()

        return {
            "keys": [
                {
                    "kty": "EC",
                    "crv": "P-256",
                    "use": "sig",
                    "alg": "ES256",
                    "kid": self._kid,
                    "x": _int_to_b64(pub_numbers.x),
                    "y": _int_to_b64(pub_numbers.y),
                }
            ]
        }

    def verify_token(self, token: str, audience: str = _FAKE_AUDIENCE) -> dict[str, Any]:
        """Verify and decode a JWT minted by this issuer.

        Raises ``pyjwt.exceptions.PyJWTError`` on invalid token.
        """
        public_pem = self._public.public_bytes(
            serialization.Encoding.PEM,
            serialization.PublicFormat.SubjectPublicKeyInfo,
        )
        return pyjwt.decode(token, public_pem, algorithms=["ES256"], audience=audience)


# ---------------------------------------------------------------------------
# Fulcio CA material
# ---------------------------------------------------------------------------


class _FulcioCaMaterial:
    """Self-signed P-256 CA for the fake Fulcio server."""

    def __init__(self) -> None:
        self._ca_key = ec.generate_private_key(SECP256R1())
        self._ca_cert = self._build_ca_cert()

    def _build_ca_cert(self) -> x509.Certificate:
        pub = self._ca_key.public_key()
        name = x509.Name(
            [
                x509.NameAttribute(NameOID.COMMON_NAME, "Fake Fulcio Test CA"),
                x509.NameAttribute(NameOID.ORGANIZATION_NAME, "OCX Tests"),
            ]
        )
        now = datetime.datetime.now(datetime.timezone.utc)
        return (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(pub)
            .serial_number(x509.random_serial_number())
            .not_valid_before(now)
            .not_valid_after(now + datetime.timedelta(hours=24))
            .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
            .add_extension(
                x509.SubjectKeyIdentifier.from_public_key(pub), critical=False
            )
            .sign(self._ca_key, hashes.SHA256())
        )

    def root_pem(self) -> bytes:
        """Return the CA certificate as PEM bytes."""
        return self._ca_cert.public_bytes(serialization.Encoding.PEM)

    def mint_leaf_cert(
        self,
        subject_email: str,
        subject_public_key: EllipticCurvePublicKey,
        oidc_issuer: str,
    ) -> bytes:
        """Mint a short-lived leaf certificate with SAN=email, Fulcio OID."""
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, subject_email)])
        issuer_name = self._ca_cert.subject
        now = datetime.datetime.now(datetime.timezone.utc)
        cert = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(issuer_name)
            .public_key(subject_public_key)
            .serial_number(x509.random_serial_number())
            .not_valid_before(now)
            .not_valid_after(now + datetime.timedelta(minutes=10))
            .add_extension(
                x509.SubjectAlternativeName(
                    [x509.RFC822Name(subject_email)]
                ),
                critical=False,
            )
            .add_extension(
                x509.ExtendedKeyUsage([ExtendedKeyUsageOID.CODE_SIGNING]),
                critical=False,
            )
            .add_extension(
                x509.UnrecognizedExtension(
                    _FULCIO_ISSUER_OID,
                    # DER-encode the issuer URL as a UTF8String.
                    _der_utf8string(oidc_issuer),
                ),
                critical=False,
            )
            .sign(self._ca_key, hashes.SHA256())
        )
        return cert.public_bytes(serialization.Encoding.PEM)


def _der_utf8string(s: str) -> bytes:
    """Encode ``s`` as a DER UTF8String (tag 0x0C)."""
    encoded = s.encode("utf-8")
    length = len(encoded)
    if length < 128:
        return bytes([0x0C, length]) + encoded
    # Multi-byte length (handles long URLs gracefully)
    length_bytes = []
    n = length
    while n:
        length_bytes.insert(0, n & 0xFF)
        n >>= 8
    return bytes([0x0C, 0x80 | len(length_bytes)] + length_bytes) + encoded


# ---------------------------------------------------------------------------
# Rekor key material
# ---------------------------------------------------------------------------


class _RekorKeyMaterial:
    """Ed25519 signing key for the fake Rekor SET."""

    def __init__(self) -> None:
        self._private = ed25519.Ed25519PrivateKey.generate()
        self._public = self._private.public_key()
        # Stable log_id = SHA-256 of the raw public key bytes (Rekor convention).
        raw_pub = self._public.public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw
        )
        import hashlib
        self.log_id = hashlib.sha256(raw_pub).hexdigest()

    def public_pem(self) -> bytes:
        """Return the public key as PEM bytes."""
        return self._public.public_bytes(
            serialization.Encoding.PEM,
            serialization.PublicFormat.SubjectPublicKeyInfo,
        )

    def sign(self, data: bytes) -> bytes:
        """Sign ``data`` and return raw signature bytes."""
        return self._private.sign(data)


# ---------------------------------------------------------------------------
# HTTP server helpers
# ---------------------------------------------------------------------------


def _find_free_port() -> int:
    """Bind to port 0 and return the assigned ephemeral port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class _SilentHandler(BaseHTTPRequestHandler):
    """Base handler that suppresses the default request log lines."""

    def log_message(self, _format: str, *_args: Any) -> None:
        pass  # silence stdlib's default stderr logging


def _start_server(
    handler_class: type[BaseHTTPRequestHandler],
) -> tuple[HTTPServer, int]:
    """Start an HTTPServer on a free port in a daemon thread.

    Returns ``(server, port)`` after the server is ready to accept connections.
    """
    port = _find_free_port()
    server = HTTPServer(("127.0.0.1", port), handler_class)
    ready = threading.Event()

    def _serve() -> None:
        ready.set()
        server.serve_forever()

    t = threading.Thread(target=_serve, daemon=True)
    t.start()
    ready.wait(timeout=5.0)
    return server, port


# ---------------------------------------------------------------------------
# OIDC server
# ---------------------------------------------------------------------------


def _make_oidc_handler(key_material: _OidcKeyMaterial, server_url: str) -> type:
    """Return a handler class closed over ``key_material`` and ``server_url``."""

    class OidcHandler(_SilentHandler):
        def do_GET(self) -> None:  # noqa: N802 (stdlib convention)
            if self.path == "/.well-known/openid-configuration":
                body = json.dumps(
                    {
                        "issuer": _FAKE_ISSUER_URL,
                        "jwks_uri": f"{server_url}/.well-known/jwks.json",
                        "id_token_signing_alg_values_supported": ["ES256"],
                        "subject_types_supported": ["public"],
                    }
                ).encode()
                self._respond(200, "application/json", body)
            elif self.path == "/.well-known/jwks.json":
                body = json.dumps(key_material.jwks()).encode()
                self._respond(200, "application/json", body)
            else:
                self._respond(404, "text/plain", b"not found")

        def _respond(self, code: int, content_type: str, body: bytes) -> None:
            self.send_response(code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    return OidcHandler


# ---------------------------------------------------------------------------
# Fulcio server
# ---------------------------------------------------------------------------


def _make_fulcio_handler(
    oidc_key: _OidcKeyMaterial, ca: _FulcioCaMaterial
) -> type:
    """Return a Fulcio handler class."""

    class FulcioHandler(_SilentHandler):
        def do_POST(self) -> None:  # noqa: N802
            if self.path not in ("/api/v2/signingCert", "/api/v2/signingCert/"):
                self._respond(404, b"not found")
                return

            length = int(self.headers.get("Content-Length", 0))
            body_raw = self.rfile.read(length)
            try:
                body = json.loads(body_raw)
            except Exception:
                self._respond(400, b"bad json")
                return

            # Extract OIDC token from credentials.
            credentials = body.get("credentials", {})
            oidc_token = credentials.get("oidcIdentityToken", "")
            if not oidc_token:
                self._respond(401, b"missing oidcIdentityToken")
                return

            try:
                claims = oidc_key.verify_token(oidc_token, audience=_FAKE_AUDIENCE)
            except Exception as exc:
                log.debug("fake Fulcio: token rejected: %s", exc)
                self._respond(403, b"token rejected")
                return

            subject_email = claims.get("email") or claims.get("sub", "unknown@test")

            # Extract public key from publicKeyRequest.
            pub_req = body.get("publicKeyRequest", {})
            pub_key_info = pub_req.get("publicKey", {})
            pub_key_content = pub_key_info.get("content", "")
            if not pub_key_content:
                self._respond(400, b"missing publicKey.content")
                return

            try:
                pub_key_pem = base64.b64decode(pub_key_content + "==")
                from cryptography.hazmat.primitives.serialization import load_pem_public_key
                subject_pub_key = load_pem_public_key(pub_key_pem)
                if not isinstance(subject_pub_key, EllipticCurvePublicKey):
                    raise ValueError("expected EC key")
            except Exception as exc:
                log.debug("fake Fulcio: bad public key: %s", exc)
                self._respond(400, b"bad public key")
                return

            oidc_issuer = claims.get("iss", _FAKE_ISSUER_URL)
            leaf_pem = ca.mint_leaf_cert(subject_email, subject_pub_key, oidc_issuer)
            root_pem = ca.root_pem()

            # Fulcio v2 response shape: signedCertificateEmbeddedSct.chain.certificates
            # Each cert is PEM-encoded. Leaf first, then root.
            response = {
                "signedCertificateEmbeddedSct": {
                    "chain": {
                        "certificates": [
                            leaf_pem.decode(),
                            root_pem.decode(),
                        ]
                    }
                }
            }
            resp_bytes = json.dumps(response).encode()
            self._respond(200, resp_bytes, content_type="application/json")

        def _respond(
            self, code: int, body: bytes, content_type: str = "text/plain"
        ) -> None:
            self.send_response(code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    return FulcioHandler


# ---------------------------------------------------------------------------
# Rekor server
# ---------------------------------------------------------------------------


def _make_rekor_handler(rekor_key: _RekorKeyMaterial) -> type:
    """Return a Rekor handler class."""
    _log_index_counter = [0]
    _lock = threading.Lock()

    class RekorHandler(_SilentHandler):
        def do_GET(self) -> None:  # noqa: N802
            if self.path in ("/api/v1/log/publicKey", "/api/v1/log/publicKey/"):
                pub_pem = rekor_key.public_pem()
                self._respond(200, pub_pem, content_type="application/x-pem-file")
            else:
                self._respond(404, b"not found")

        def do_POST(self) -> None:  # noqa: N802
            if self.path not in ("/api/v1/log/entries", "/api/v1/log/entries/"):
                self._respond(404, b"not found")
                return

            length = int(self.headers.get("Content-Length", 0))
            body_raw = self.rfile.read(length)
            try:
                body = json.loads(body_raw)
            except Exception:
                self._respond(400, b"bad json")
                return

            # Validate hashedrekord proposal (we accept without deep verification).
            kind = body.get("kind", "")
            api_version = body.get("apiVersion", "")
            if kind != "hashedrekord" or api_version != "0.0.1":
                self._respond(400, f"unsupported kind={kind!r} version={api_version!r}".encode())
                return

            with _lock:
                log_index = _log_index_counter[0]
                _log_index_counter[0] += 1

            entry_uuid = uuid.uuid4().hex
            integrated_time = int(time.time())

            # TODO(phase-5c-verify): this SET payload is simplified for slice-1 scaffolding.
            # Rekor v1 signs a canonicalized JSON payload describing the log entry (not a
            # protobuf — sigstore_rekor.proto describes the entry schema, not the signing
            # payload). Phase 5c must either (a) align this payload with the real Rekor v1
            # signing format, or (b) route the Rust verify pipeline through a fake-aware
            # SET verifier shim.
            #
            # SOTA NOTE (2026-04-21): Rekor v2 went GA on 2025-10-10, dropping SET entirely
            # in favour of RFC 3161 Timestamp Authority responses. The VerifyErrorKind
            # `RekorSetAbsentTsaPresent` safety valve already accommodates that transition.
            # Phase 5c must handle both v1 (SET) and v2 (TSA) verification paths.
            # Refs: https://blog.sigstore.dev/rekor-v2-ga/
            #       https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_rekor.proto
            #
            # Build a stable deterministic payload so the verify path can reproduce it.
            canonical = json.dumps(
                {
                    "body": base64.b64encode(body_raw).decode(),
                    "integratedTime": integrated_time,
                    "logID": rekor_key.log_id,
                    "logIndex": log_index,
                },
                sort_keys=True,
            ).encode()

            set_bytes = rekor_key.sign(canonical)
            set_b64 = base64.b64encode(set_bytes).decode()

            # Rekor v1 response: dict keyed by UUID containing the LogEntry.
            entry_body = {
                "body": base64.b64encode(body_raw).decode(),
                "integratedTime": integrated_time,
                "logID": rekor_key.log_id,
                "logIndex": log_index,
                "verification": {
                    "inclusionProof": None,
                    "signedEntryTimestamp": set_b64,
                },
            }
            response = {entry_uuid: entry_body}
            resp_bytes = json.dumps(response).encode()
            self.send_response(201)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp_bytes)))
            self.end_headers()
            self.wfile.write(resp_bytes)

        def _respond(
            self, code: int, body: bytes, content_type: str = "text/plain"
        ) -> None:
            self.send_response(code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    return RekorHandler


# ---------------------------------------------------------------------------
# FakeSigstoreStack — composite
# ---------------------------------------------------------------------------


class FakeSigstoreStack:
    """Composite fixture that owns all three fake servers.

    Instantiate with ``FakeSigstoreStack(tmp_path)`` inside a pytest fixture;
    call ``shutdown()`` in a ``finally`` block (or use the context manager).
    The ``tmp_path`` directory is used to write key material files that the
    Rust pipeline can load as trust roots.
    """

    def __init__(self, tmp_path: Path) -> None:
        self._tmp = tmp_path
        self._oidc_key = _OidcKeyMaterial()
        self._ca = _FulcioCaMaterial()
        self._rekor_key = _RekorKeyMaterial()

        # Start OIDC server first so we can include its URL in the discovery doc.
        oidc_port = _find_free_port()
        self._oidc_url = f"http://127.0.0.1:{oidc_port}"
        oidc_handler = _make_oidc_handler(self._oidc_key, self._oidc_url)
        self._oidc_server = HTTPServer(("127.0.0.1", oidc_port), oidc_handler)
        self._start_thread(self._oidc_server)

        # Fulcio server
        fulcio_port = _find_free_port()
        self._fulcio_url = f"http://127.0.0.1:{fulcio_port}"
        fulcio_handler = _make_fulcio_handler(self._oidc_key, self._ca)
        self._fulcio_server = HTTPServer(("127.0.0.1", fulcio_port), fulcio_handler)
        self._start_thread(self._fulcio_server)

        # Rekor server
        rekor_port = _find_free_port()
        self._rekor_url = f"http://127.0.0.1:{rekor_port}"
        rekor_handler = _make_rekor_handler(self._rekor_key)
        self._rekor_server = HTTPServer(("127.0.0.1", rekor_port), rekor_handler)
        self._start_thread(self._rekor_server)

        # Write trust-root files to tmp_path
        self._root_pem_path = tmp_path / "fulcio-root.pem"
        self._root_pem_path.write_bytes(self._ca.root_pem())

        self._rekor_pub_path = tmp_path / "rekor-public-key.pem"
        self._rekor_pub_path.write_bytes(self._rekor_key.public_pem())

    @staticmethod
    def _start_thread(server: HTTPServer) -> None:
        t = threading.Thread(target=server.serve_forever, daemon=True)
        t.start()

    @property
    def fulcio_url(self) -> str:
        """Base URL for the fake Fulcio server."""
        return self._fulcio_url

    @property
    def rekor_url(self) -> str:
        """Base URL for the fake Rekor server."""
        return self._rekor_url

    @property
    def oidc_url(self) -> str:
        """Base URL for the fake OIDC issuer server."""
        return self._oidc_url

    def oidc_token(
        self,
        subject: str = _FAKE_SUBJECT,
        issuer: str = _FAKE_ISSUER_URL,
        audience: str = _FAKE_AUDIENCE,
    ) -> str:
        """Mint a fresh ES256 JWT valid for 10 minutes."""
        return self._oidc_key.sign_token(subject=subject, issuer=issuer, audience=audience)

    def trust_root_pem_path(self) -> Path:
        """Path to the fake Fulcio CA PEM (for TrustRoot::load_from_pem)."""
        return self._root_pem_path

    def rekor_public_key_pem_path(self) -> Path:
        """Path to the fake Rekor public key PEM."""
        return self._rekor_pub_path

    def shutdown(self) -> None:
        """Shut down all servers. Safe to call multiple times."""
        for server in (self._oidc_server, self._fulcio_server, self._rekor_server):
            try:
                server.shutdown()
            except Exception:
                pass

    def __enter__(self) -> "FakeSigstoreStack":
        return self

    def __exit__(self, *_: object) -> None:
        self.shutdown()


# ---------------------------------------------------------------------------
# Pytest fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def fake_sigstore_stack(tmp_path: Path) -> "FakeSigstoreStack":
    """Spawn all three fake Sigstore services for the duration of a test.

    Provides ``FakeSigstoreStack`` with ``fulcio_url``, ``rekor_url``,
    ``oidc_token()``, and ``trust_root_pem_path()``.
    Servers are shut down after the test via ``yield + finally``.
    """
    stack = FakeSigstoreStack(tmp_path)
    try:
        yield stack
    finally:
        stack.shutdown()


@pytest.fixture()
def fake_fulcio(tmp_path: Path, fake_sigstore_stack: "FakeSigstoreStack") -> FakeFulcio:
    """Stand up a fake Fulcio server for the duration of a test.

    Backed by ``fake_sigstore_stack`` — requesting this fixture also starts
    the OIDC issuer and Rekor servers. The ``root_pem`` path can be loaded
    as a trust anchor by the Rust verify pipeline.
    """
    return FakeFulcio(
        url=fake_sigstore_stack.fulcio_url,
        root_pem=fake_sigstore_stack.trust_root_pem_path(),
    )


@pytest.fixture()
def fake_rekor(tmp_path: Path, fake_sigstore_stack: "FakeSigstoreStack") -> FakeRekor:
    """Stand up a fake Rekor server for the duration of a test.

    Backed by ``fake_sigstore_stack``. The ``public_key_pem`` path holds the
    Ed25519 public key used to verify Signed Entry Timestamps.
    """
    return FakeRekor(
        url=fake_sigstore_stack.rekor_url,
        public_key_pem=fake_sigstore_stack.rekor_public_key_pem_path(),
    )


@pytest.fixture()
def fake_oidc_token(fake_sigstore_stack: "FakeSigstoreStack") -> str:
    """Mint a fresh ES256 OIDC token from the fake OIDC issuer.

    The token has ``sub=test-signer@example.com``, ``iss=https://fake-oidc.test``,
    ``aud=sigstore``, and a 10-minute expiry. The fake Fulcio server trusts
    this token's signing key so sign pipelines can use it directly.
    """
    return fake_sigstore_stack.oidc_token()
