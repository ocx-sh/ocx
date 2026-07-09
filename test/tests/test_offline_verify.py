# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for offline / air-gapped ``ocx package verify`` (#196).

Contract source: ``.claude/artifacts/adr_offline_verify_trust_cache.md``.

For verify, ``OCX_OFFLINE`` scopes to the Sigstore trust services (the Rekor
public-key fetch and TUF) — NOT the artifact registry, which verify still reads
the signature referrer + bundle from (a local mirror, in air-gapped setups). So
offline verify:

- reuses cached or supplied trust material (which must carry a **pinned** Rekor
  key) and contacts no Sigstore service;
- fails with an actionable exit-78 error when no such material exists — it never
  silently skips verification.

Each test proves "no Sigstore-services network" by driving the fake Rekor into a
503 failure mode *after* the trust material is cached/supplied: a subsequent
verify that still succeeds cannot have fetched the Rekor key.
"""
from __future__ import annotations

import subprocess

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor, FakeSigstoreStack, HttpStatus


def _sign(ocx: OcxRunner, pkg: PackageInfo, fulcio: FakeFulcio, rekor: FakeRekor, token: str) -> None:
    """Sign ``pkg`` online with the fake stack; publishes the signature referrer."""
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": token}
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fulcio.url,
            "--rekor-url", rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"


def _verify(ocx: OcxRunner, pkg: PackageInfo, rekor: FakeRekor, *, extra_env: dict[str, str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, **extra_env},
    )


# ──────────────────────────────────────────────────────────────────────────────
# Online verify populates the trust-root cache; a later OFFLINE verify reuses it
# ──────────────────────────────────────────────────────────────────────────────


def test_online_verify_populates_cache_then_offline_verify_succeeds(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Online verify caches trust material; OFFLINE verify reuses it, no Rekor fetch.

    Step 2 (online verify) TOFU-fetches the Rekor key and caches it with the
    Fulcio CA under ``$OCX_HOME/state/trust_root/``. Rekor is then forced to 503.
    Step 4 (``OCX_OFFLINE=1``, no ``OCX_SIGSTORE_TRUST_ROOT``) must succeed purely
    from the cache — if it fetched the Rekor key it would hit the 503 and exit 83.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    online = _verify(ocx, pkg, fake_rekor, extra_env={"OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem)})
    assert online.returncode == 0, f"online verify (cache populate) failed: {online.stderr}"

    # Kill the Rekor endpoint: any later key fetch now 503s.
    fake_rekor.set_failure_mode(HttpStatus(503))

    offline = _verify(ocx, pkg, fake_rekor, extra_env={"OCX_OFFLINE": "1"})
    assert offline.returncode == 0, (
        f"offline verify from cache must succeed with no Rekor fetch, got "
        f"{offline.returncode}\nstderr: {offline.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# OFFLINE + no cached/supplied trust root → actionable fail, never skip
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_verify_without_trust_material_fails_not_skips(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """OFFLINE verify with no cache and no override → exit 78, naming the remedy.

    The package is signed (so a signature exists), but no prior verify ran, so
    the trust-root cache is empty. Offline cannot fetch the Rekor key or fall
    back to the embedded root, so it must fail with an actionable error — never
    exit 0 / silently skip verification.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    result = _verify(ocx, pkg, fake_rekor, extra_env={"OCX_OFFLINE": "1"})
    assert result.returncode == 78, (
        f"offline verify without trust material must fail with exit 78 (never skip), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert "--tuf-root" in result.stderr or "online verify" in result.stderr, (
        f"error must name the remedy, got: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# OCX_SIGSTORE_TUF_ROOT override pins the Rekor key — no fetch (online)
# ──────────────────────────────────────────────────────────────────────────────


def test_tuf_root_override_pins_rekor_key_no_fetch(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A trusted-root JSON supplies the Rekor key, so verify never fetches it.

    Rekor is forced to 503 before verify. With ``OCX_SIGSTORE_TUF_ROOT`` pointing
    at a local trusted-root JSON (Fulcio CA + pinned Rekor key), verify must
    still succeed — proving the key came from the file, not the (503) endpoint,
    and that no TUF network fetch is required.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    fake_rekor.set_failure_mode(HttpStatus(503))

    tuf_root = fake_sigstore_stack.trusted_root_json_path()
    result = _verify(ocx, pkg, fake_rekor, extra_env={"OCX_SIGSTORE_TUF_ROOT": str(tuf_root)})
    assert result.returncode == 0, (
        f"verify with OCX_SIGSTORE_TUF_ROOT must succeed without a Rekor fetch, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Fully air-gapped: OCX_OFFLINE + OCX_SIGSTORE_TUF_ROOT, no Sigstore network
# ──────────────────────────────────────────────────────────────────────────────


def test_tuf_root_offline_air_gapped_verify(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """OCX_OFFLINE + OCX_SIGSTORE_TUF_ROOT verifies with zero Sigstore network.

    Install first (populates the local index so the tag resolves offline), sign,
    then force Rekor to 503 and verify with ``OCX_OFFLINE=1`` + a trusted-root
    JSON. The pinned Rekor key means the SET verifies with no fetch.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    ocx.json("package", "install", "--select", pkg.short)  # populate local index
    fake_rekor.set_failure_mode(HttpStatus(503))

    tuf_root = fake_sigstore_stack.trusted_root_json_path()
    result = _verify(
        ocx,
        pkg,
        fake_rekor,
        extra_env={"OCX_OFFLINE": "1", "OCX_SIGSTORE_TUF_ROOT": str(tuf_root)},
    )
    assert result.returncode == 0, (
        f"air-gapped verify (OCX_OFFLINE + TUF root) must succeed, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
