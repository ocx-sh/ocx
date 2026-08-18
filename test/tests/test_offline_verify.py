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

Each test proves "no Sigstore-services network" by killing the Rekor endpoint
*after* the trust material is cached/supplied: a verify that still succeeds
cannot have fetched the Rekor key. The real Rekor cannot be stopped, so the
endpoint under test is either a :class:`TcpRelay` in front of it (where the
cache key must survive the kill) or a port nothing listens on.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from urllib.parse import urlsplit

from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack
from tests.fixtures.tcp_relay import TcpRelay


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    """Sign ``pkg`` online against the real stack; publishes the signature referrer."""
    result = subprocess.run(
        [str(ocx.binary), "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"


def _verify(
    ocx: OcxRunner,
    stack: SigstoreStack,
    pkg: PackageInfo,
    *,
    rekor_url: str | None = None,
    identity: str | None = None,
    extra_env: dict[str, str],
) -> subprocess.CompletedProcess[str]:
    """Run ``package verify``. The trust root comes from ``extra_env`` only.

    No ``--tuf-root`` flag: every test here is about what the *environment*
    supplies, which is what an air-gapped deployment actually configures.
    """
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", identity or stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
            "--rekor-url", rekor_url or stack.rekor_url,
            "--platform", current_platform(),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, **extra_env},
    )


def _rekor_relay(stack: SigstoreStack) -> TcpRelay:
    """A killable stand-in for Rekor at a fixed authority."""
    endpoint = urlsplit(stack.rekor_url)
    assert endpoint.hostname and endpoint.port, f"unexpected Rekor URL: {stack.rekor_url}"
    return TcpRelay(endpoint.hostname, endpoint.port)


# ──────────────────────────────────────────────────────────────────────────────
# Online verify populates the trust-root cache; a later OFFLINE verify reuses it
# ──────────────────────────────────────────────────────────────────────────────


def test_online_verify_populates_cache_then_offline_verify_succeeds(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Online verify caches trust material; OFFLINE verify reuses it, no Rekor fetch.

    Step 2 (online verify) TOFU-fetches the Rekor key and caches it with the
    Fulcio CA under ``$OCX_HOME/state/trust_root/``. The relay is then closed.
    Step 4 (``OCX_OFFLINE=1``, no ``OCX_SIGSTORE_TRUST_ROOT``) must succeed purely
    from the cache — if it fetched the Rekor key it would be refused and exit 83.
    Both runs address the same relay, so the cache entry is the same one.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    rekor = _rekor_relay(sigstore_stack)
    # Carries the CA and the CT log key but no Rekor key, so the online run must
    # fetch one. (A bare CA PEM would also lack the CT key, and without it the
    # certificate's SCT cannot be checked at all — refused before Rekor.)
    online = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=rekor.url,
        extra_env={"OCX_SIGSTORE_TUF_ROOT": str(sigstore_stack.trusted_root_without_rekor_key(tmp_path))},
    )
    assert online.returncode == 0, f"online verify (cache populate) failed: {online.stderr}"

    rekor.close()

    offline = _verify(ocx, sigstore_stack, pkg, rekor_url=rekor.url, extra_env={"OCX_OFFLINE": "1"})
    assert offline.returncode == 0, (
        f"offline verify from cache must succeed with no Rekor fetch, got "
        f"{offline.returncode}\nstderr: {offline.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# OFFLINE verify from a WARM cache still ENFORCES identity — never silently skips
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_verify_from_warm_cache_still_enforces_identity(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Warm the trust cache online, then OFFLINE verify with a WRONG identity → 77.

    The failure mode this guards against is offline verify degrading into a
    silent no-op (exit 0) once it cannot reach Sigstore. We prove it still runs
    the full crypto + identity check: after an online verify warms the cache and
    the Rekor relay is closed, an OFFLINE verify against a *different*
    ``--certificate-identity`` must fail with ``IdentityMismatch`` (exit 77) —
    the bundle verifies cryptographically from the cache, but the signer is not
    the expected one. Exit 0 here would mean verification was skipped.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    rekor = _rekor_relay(sigstore_stack)
    online = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=rekor.url,
        extra_env={"OCX_SIGSTORE_TUF_ROOT": str(sigstore_stack.trusted_root_without_rekor_key(tmp_path))},
    )
    assert online.returncode == 0, f"online verify (cache populate) failed: {online.stderr}"

    # Kill Rekor: a pass can now only come from the cache.
    rekor.close()

    offline = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=rekor.url,
        identity="someone-else@example.com",
        extra_env={"OCX_OFFLINE": "1"},
    )
    assert offline.returncode == 77, (
        f"offline verify from a warm cache must still enforce identity (exit 77 on a "
        f"wrong signer), not silently skip — got {offline.returncode}\n"
        f"stderr: {offline.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# OFFLINE + no cached/supplied trust root → actionable fail, never skip
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_verify_without_trust_material_fails_not_skips(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """OFFLINE verify with no cache and no override → exit 78, naming the remedy.

    The package is signed (so a signature exists), but no prior verify ran, so
    the trust-root cache is empty. Rekor is deliberately left *reachable* here:
    the refusal must come from the offline policy, not from connectivity.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    result = _verify(ocx, sigstore_stack, pkg, extra_env={"OCX_OFFLINE": "1"})
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A trusted-root JSON supplies the Rekor key, so verify never fetches it.

    Verify addresses a dead Rekor port. With ``OCX_SIGSTORE_TUF_ROOT`` pointing
    at a local trusted-root JSON (Fulcio CA + pinned Rekor key), it must still
    succeed — proving the key came from the file, not the endpoint, and that no
    TUF network fetch is required.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    result = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=adversarial.unreachable_rekor_url(),
        extra_env={"OCX_SIGSTORE_TUF_ROOT": str(sigstore_stack.trusted_root_json)},
    )
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """OCX_OFFLINE + OCX_SIGSTORE_TUF_ROOT verifies with zero Sigstore network.

    Install first (populates the local index so the tag resolves offline), sign,
    then verify against a dead Rekor port with ``OCX_OFFLINE=1`` + a trusted-root
    JSON. The pinned Rekor key means the SET verifies with no fetch.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    ocx.json("package", "install", "--select", pkg.short)  # populate local index

    result = _verify(
        ocx, sigstore_stack, pkg,
        rekor_url=adversarial.unreachable_rekor_url(),
        extra_env={"OCX_OFFLINE": "1", "OCX_SIGSTORE_TUF_ROOT": str(sigstore_stack.trusted_root_json)},
    )
    assert result.returncode == 0, (
        f"air-gapped verify (OCX_OFFLINE + TUF root) must succeed, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )
