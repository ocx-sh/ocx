"""Fixtures for the real Sigstore stack (`sigstore` compose profile).

Replaces the in-repo Sigstore fake. The difference that matters is not fidelity
in the abstract: the fake DER-wrapped Fulcio's deprecated `.1.1` issuer
extension, which real Fulcio writes raw, so identity pinning could never have
matched a real CA and the whole suite stayed green over it for the life of the
branch.

Everything here is session-scoped. Bringing the stack up costs ~13s once, and
a Fulcio certificate is good for ten minutes, so signing per-test would spend
the budget on nothing.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

import pytest

from src.helpers import (
    SIGSTORE_DIR,
    SIGSTORE_IDENTITY,
    SIGSTORE_ISSUER,
    mint_identity_token,
    start_sigstore_stack,
)
from src.runner import current_platform


def _port(var: str, default: str) -> str:
    return os.environ.get(var, default)


@dataclass(frozen=True, slots=True)
class SigstoreStack:
    """Everything a test needs to address the local stack."""

    fulcio_url: str
    rekor_url: str
    ct_url: str
    #: The SAN the local dex mints. Matches `--certificate-identity`.
    identity: str
    #: The `iss` claim, and so `--certificate-oidc-issuer`. This is the
    #: IN-NETWORK url: the host fetches the token over localhost, but the claim
    #: inside it names the address Fulcio resolves. ocx never dials the issuer,
    #: it only compares the string, so the two never have to agree.
    issuer: str
    #: Directory holding `trusted_root.json`; what `--tuf-root` takes.
    trust_root: Path

    @property
    def fulcio_ca_pem(self) -> Path:
        """The Fulcio CA alone — what `--trust-root`/`OCX_SIGSTORE_TRUST_ROOT` takes.

        It carries no Rekor public key, so verify must fetch that key and cache
        it. That is the only route to the trust-cache population path.
        """
        return self.trust_root / "keys" / "fulcio-ca.crt.pem"

    @property
    def trusted_root_json(self) -> Path:
        """Fulcio CA *and* pinned Rekor key; what the air-gapped paths take."""
        return self.trust_root / "trusted_root.json"

    def trusted_root_without_rekor_key(self, tmp_path: Path) -> Path:
        """A trusted root carrying the CA and the CT log key but no Rekor key.

        The only way to reach the online Rekor-key lookup now that verification
        is delegated: a bare CA PEM has no CT log key either, and without one
        nothing can check the certificate's embedded SCT, so the run is refused
        before it ever asks Rekor for anything.

        Written next to the test rather than into the stack directory — the
        latter is shared, session-scoped state.
        """
        import json

        document = json.loads(self.trusted_root_json.read_text())
        document["tlogs"] = []
        target = tmp_path / "trusted_root.json"
        target.write_text(json.dumps(document))
        return target

    def sign_args(self, token_file: Path, platform: str | None = None) -> list[str]:
        return [
            "--platform", platform or current_platform(),
            "--fulcio-url", self.fulcio_url,
            "--rekor-url", self.rekor_url,
            "--identity-token-file", str(token_file),
        ]

    def verify_args(self, platform: str | None = None) -> list[str]:
        return [
            "--platform", platform or current_platform(),
            "--rekor-url", self.rekor_url,
            "--tuf-root", str(self.trust_root),
            "--certificate-identity", self.identity,
            "--certificate-oidc-issuer", self.issuer,
        ]


@pytest.fixture(scope="session")
def sigstore_stack() -> SigstoreStack:
    """Bring the stack up and describe it.

    Deliberately raises rather than skipping. A skip here is indistinguishable
    from a pass, and these are the tests that carry the supply-chain contract.
    """
    start_sigstore_stack()
    return SigstoreStack(
        fulcio_url=f"http://localhost:{_port('OCX_TEST_FULCIO_PORT', '5555')}",
        rekor_url=f"http://localhost:{_port('OCX_TEST_REKOR_PORT', '3000')}",
        ct_url=f"http://localhost:{_port('OCX_TEST_CT_PORT', '6962')}/ocx-test",
        identity=SIGSTORE_IDENTITY,
        issuer=SIGSTORE_ISSUER,
        trust_root=SIGSTORE_DIR,
    )


@pytest.fixture(scope="session")
def identity_token(sigstore_stack: SigstoreStack, tmp_path_factory) -> Path:
    """An OIDC identity token from the local dex, in a file `sign` can read.

    A file rather than an env var or an argument: `ocx package sign` has no
    `--identity-token <VALUE>` flag by design, because a token in argv is
    world-readable in /proc.
    """
    return mint_identity_token(tmp_path_factory.mktemp("sigstore") / "identity-token")
