# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the referrers-API capability cache (#106).

The cache-then-probe wiring itself (``from_cache`` → ``probe`` →
``write_cache``, no tag fallback, exit 84 on an unsupported registry) is
already covered by ``test_sign.py`` / ``test_verify.py``
(``test_sign_referrers_unsupported_exits_84`` /
``test_verify_referrers_unsupported_exits_84``, both against the
``legacy_registry`` negative fixture). This file covers the remaining #106
acceptance criterion: a supported registry's capability probe result is
cached to disk and reused on a subsequent invocation within the 6h TTL.

Acceptance-level, this can observe the *cache artifact* (file exists,
correct shape, fresh expiry) and that the artifact is untouched by a second
invocation. Since ``write_cache`` in both ``SignPipeline`` and
``VerifyPipeline`` only runs on the cache-miss branch (see
``crates/ocx_lib/src/oci/{sign,verify}/pipeline.rs``), an unchanged
``probed_at`` after a second successful sign is direct proof that branch was
not re-entered — i.e. no second probe happened. This test cannot observe the
real registry's HTTP traffic directly; the transport-level proof (a stub
that errors if probed) lives in
``crates/ocx_lib/src/oci/referrer/capability.rs::fresh_cache_short_circuits_probe``.
"""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

from src.runner import OcxRunner, PackageInfo, registry_dir
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor


def _cache_path(ocx: OcxRunner) -> Path:
    return ocx.ocx_home / "state" / "referrers" / f"{registry_dir(ocx.registry)}.json"


def _sign(
    ocx: OcxRunner,
    pkg: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> subprocess.CompletedProcess[str]:
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )


def test_sign_writes_capability_cache_with_fresh_expiry(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A successful sign against the referrers-capable registry (zot) writes
    the capability cache with the shape the #106 acceptance criteria expect:
    correct registry, ``supported``, and a not-yet-expired TTL window."""
    result = _sign(ocx, published_package, fake_fulcio, fake_rekor, fake_oidc_token)
    assert result.returncode == 0, result.stderr

    cache_path = _cache_path(ocx)
    assert cache_path.exists(), f"capability cache not written at {cache_path}"
    cache = json.loads(cache_path.read_text())
    assert cache["registry"] == ocx.registry
    assert cache["supported"] == "supported"
    probed_at = cache["probed_at"]["secs_since_epoch"]
    ttl_seconds = cache["ttl_seconds"]
    assert ttl_seconds > 0
    # Fresh: the TTL window (probed_at + ttl_seconds) has not elapsed yet.
    assert probed_at + ttl_seconds > time.time(), "cache entry must not already be expired"


def test_second_sign_reuses_cached_capability_without_reprobing(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A second sign within the TTL must not re-probe: the cache file's
    ``probed_at`` timestamp is untouched, because ``write_cache`` only runs
    on the cache-miss branch of ``ensure_referrers_supported``. If the
    pipeline re-probed, ``probed_at`` would advance to the second sign's
    wall-clock time.
    """
    first = _sign(ocx, published_package, fake_fulcio, fake_rekor, fake_oidc_token)
    assert first.returncode == 0, first.stderr

    cache_path = _cache_path(ocx)
    first_probed_at = json.loads(cache_path.read_text())["probed_at"]

    second = _sign(ocx, published_package, fake_fulcio, fake_rekor, fake_oidc_token)
    assert second.returncode == 0, second.stderr

    second_probed_at = json.loads(cache_path.read_text())["probed_at"]
    assert second_probed_at == first_probed_at, (
        "probed_at changed after a second sign within the TTL — the "
        "capability cache was re-probed instead of reused"
    )
