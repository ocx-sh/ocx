# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""TLS-wrapped ``StaticIndexServer`` fixture for the extra-CA-roots acceptance
tests (ocx#448).

Mirrors the Rust-side fixture at ``crates/ocx_lib/src/tls.rs``'s
``test_pki`` module: a minted P-256 self-signed CA root, a leaf certificate
for ``127.0.0.1`` signed by it, and an HTTPS wrapper around
``src.static_index.StaticIndexServer`` that presents the leaf — the one
cross-language proof that a seeded root is consulted at TLS handshake time
(S-002 / S-007), not merely stored.
"""

from __future__ import annotations

import contextlib
import datetime
import ipaddress
import secrets
import socket
import ssl
import tempfile
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from typing import NamedTuple

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

from src.static_index import StaticIndexServer


class MintedPki(NamedTuple):
    """A minted CA root plus a leaf certificate it signed, and the leaf's
    private key — everything an HTTPS fixture needs to present an identity,
    and the root PEM an ``OCX_EXTRA_CA_CERTS`` value is built from.
    """

    ca_cert_pem: bytes
    leaf_cert_pem: bytes
    leaf_key_pem: bytes


def _name(common_name: str) -> x509.Name:
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, common_name)])


def mint_ca_and_leaf(
    san_ip: str = "127.0.0.1", san_dns: list[str] | None = None
) -> MintedPki:
    """Mint a self-signed CA root and a leaf certificate for ``san_ip``
    (SAN ``IP:<san_ip>``, plus ``DNS:<name>`` for every ``san_dns`` entry),
    signed by that root, using ``cryptography`` (already a test dependency).

    The root carries ``basicConstraints CA:TRUE`` and ``keyCertSign`` — what
    makes it acceptable as a trust anchor; the leaf carries the IP SAN and
    ``serverAuth`` — what makes rustls accept it for a dial to ``san_ip``.
    Every call mints fresh keys AND a fresh CA subject name, so two fixtures
    never share a root and a test asserting on rejection can never
    accidentally trust a sibling's — and the rejection is ``UnknownIssuer``
    (no anchor by that name), not ``BadSignature`` (an anchor by that name
    whose key does not verify), which is the verdict the tests assert.
    """
    now = datetime.datetime.now(datetime.UTC)
    not_before = now - datetime.timedelta(minutes=5)
    not_after = now + datetime.timedelta(hours=1)

    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = _name(f"ocx test corp CA {secrets.token_hex(4)}")
    ca_cert = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=True,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(ca_key.public_key()),
            critical=False,
        )
        .sign(ca_key, hashes.SHA256())
    )

    leaf_key = ec.generate_private_key(ec.SECP256R1())
    leaf_cert = (
        x509.CertificateBuilder()
        .subject_name(_name("ocx test index"))
        .issuer_name(ca_name)
        .public_key(leaf_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .add_extension(
            x509.SubjectAlternativeName(
                [
                    x509.IPAddress(ipaddress.ip_address(san_ip)),
                    *(x509.DNSName(name) for name in san_dns or ()),
                ]
            ),
            critical=False,
        )
        .add_extension(
            x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False
        )
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()),
            critical=False,
        )
        .sign(ca_key, hashes.SHA256())
    )

    return MintedPki(
        ca_cert_pem=ca_cert.public_bytes(serialization.Encoding.PEM),
        leaf_cert_pem=leaf_cert.public_bytes(serialization.Encoding.PEM),
        leaf_key_pem=leaf_key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        ),
    )


class TlsStaticIndexServer(StaticIndexServer):
    """A :class:`StaticIndexServer` whose listening socket is wrapped in
    :meth:`ssl.SSLContext.wrap_socket`, presenting ``pki``'s minted leaf
    certificate — the HTTPS-index fixture behind S-002 / S-005 / S-007.

    ``base_url`` overrides the parent's ``http://`` form with ``https://``;
    every other behaviour (request recording, refusal injection, slow-body
    pacing) is inherited unchanged. A client that refuses the leaf (no
    trusted root) fails inside ``accept()``'s handshake with an
    ``ssl.SSLError`` — an ``OSError`` — which ``socketserver`` swallows, so
    the red half of a red/green pair never wedges the serve loop.
    """

    def __init__(self, root: Path, pki: MintedPki) -> None:
        super().__init__(root)
        # `load_cert_chain` reads files, never bytes: stage the leaf beside
        # (never inside) the served root, torn down in `server_close`.
        self._tls_dir = tempfile.TemporaryDirectory(prefix="ocx-tls-index-")
        cert_path = Path(self._tls_dir.name) / "leaf.pem"
        key_path = Path(self._tls_dir.name) / "leaf.key"
        cert_path.write_bytes(pki.leaf_cert_pem)
        key_path.write_bytes(pki.leaf_key_pem)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(cert_path), str(key_path))
        self.socket = context.wrap_socket(self.socket, server_side=True)

    def server_close(self) -> None:
        super().server_close()
        self._tls_dir.cleanup()

    @property
    def base_url(self) -> str:
        """``https://<host>:<port>`` — the TLS counterpart of
        :attr:`StaticIndexServer.base_url`.
        """
        host, port = self.server_address[:2]
        return f"https://{host}:{port}"


@contextlib.contextmanager
def running_tls(root: Path, pki: MintedPki) -> Iterator[TlsStaticIndexServer]:
    """The ``running(root)`` (``src.static_index``) counterpart for
    :class:`TlsStaticIndexServer` — starts the TLS-wrapped server on a
    background thread, waits for it to accept connections, and tears it
    down on exit.

    The readiness probe is a plain TCP connect that closes immediately; the
    server's handshake on it fails and is swallowed, which is exactly the
    signal that the listener is up.
    """
    server = TlsStaticIndexServer(root, pki)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                with socket.create_connection(server.server_address, timeout=0.2):
                    break
            except OSError:
                time.sleep(0.05)
        else:
            raise RuntimeError(
                "TLS static index fixture server did not become reachable"
            )
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
