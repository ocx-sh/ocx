# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Fake Sigstore services for acceptance testing without live Fulcio/Rekor.

Per ADR D9 (`adr_oci_referrers_signing_v1.md`): CI must not depend on live
Sigstore. These fixtures stand up in-process HTTP servers that mimic the
endpoints the sign/verify pipeline hits.

Phase 1 status
==============

This module ships a skeletal fixture API. The real implementations require:

- A self-signed CA to mint short-lived ECDSA P-256 leaf certificates keyed to
  the OIDC token's ``email`` claim (Fulcio behavior).
- A fake Rekor endpoint that accepts hashedrekord v0.0.1 entries, returns a
  deterministic UUID + integrated-time, and produces a signed entry timestamp
  (SET) using a stable test Rekor public key.
- A fake OIDC issuer that mints JWTs signed by a known RSA key.

Phase 5 (implementation) wires these against ``cryptography`` / ``pyca`` or
ORAS signing helpers. For Phase 4, the fixtures raise
``pytest.xfail(reason=...)`` so tests that consume them are reported as
expected failures until real crypto lands. The xfail reason message is the
contract — when Phase 5 implements the fixture, the xfail turns into a pass
automatically via ``strict=True``.
"""
from __future__ import annotations

import dataclasses
from pathlib import Path

import pytest


@dataclasses.dataclass
class FakeFulcio:
    """Handle for a fake Fulcio server.

    ``url`` is injected into sign pipelines via ``--fulcio-url``. ``root_pem``
    is the self-signed CA PEM that must be loaded into the verify pipeline's
    :class:`TrustRoot` so the leaf cert is trusted.
    """

    url: str
    root_pem: Path


@dataclasses.dataclass
class FakeRekor:
    """Handle for a fake Rekor transparency log."""

    url: str
    public_key_pem: Path


@dataclasses.dataclass
class FakeOidcIssuer:
    """Handle for a fake OIDC issuer used to mint keyless tokens."""

    issuer: str
    subject: str
    audience: str


@pytest.fixture()
def fake_fulcio(tmp_path: Path) -> FakeFulcio:
    """Stand up a fake Fulcio server for the duration of a test.

    Phase 5 implementation will:
    - Generate a self-signed ECDSA root CA under ``tmp_path``.
    - Start an ``aiohttp`` server bound to an ephemeral port.
    - Handle ``POST /api/v2/signingCert`` by minting a leaf cert for the
      subject claim of the submitted OIDC token.
    - Return a handle with ``url`` and ``root_pem``.
    """
    pytest.xfail(
        reason="Phase 5: needs cryptography-based self-signed CA + aiohttp "
        "server minting ECDSA P-256 leaf certs keyed to OIDC claims"
    )


@pytest.fixture()
def fake_rekor(tmp_path: Path) -> FakeRekor:
    """Stand up a fake Rekor server emitting deterministic SETs.

    Phase 5 implementation will:
    - Generate a stable ed25519 test keypair.
    - Start a server accepting ``POST /api/v2/log/entries``.
    - Return UUID + integrated-time + SET signed over the canonicalized
      entry body.
    """
    pytest.xfail(
        reason="Phase 5: needs stable Rekor test key + aiohttp server emitting "
        "deterministic SET over hashedrekord v0.0.1 entries"
    )


@pytest.fixture()
def fake_oidc_token() -> str:
    """Mint a fake OIDC token acceptable to ``fake_fulcio``.

    Phase 5 implementation will issue a JWT signed by the fake OIDC issuer's
    RSA key, with ``iss``, ``sub``, ``aud``, and ``exp`` claims matching what
    the fake Fulcio server expects.
    """
    pytest.xfail(reason="Phase 5: needs RSA-signed JWT minted by the fake OIDC issuer")
