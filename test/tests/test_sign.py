# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sign`` (Slice 1 — referrers signing).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md`` +
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

All tests that exercise real crypto (`sign_then_verify_happy_path`, token
precedence paths) depend on ``fake_fulcio`` / ``fake_rekor`` / ``fake_oidc_token``
fixtures, which ``pytest.xfail()`` until Phase 5 ships the real fake services.
The xfail markers are ``strict=True`` so they flip to pass automatically when
the fixtures come online.

Tests that don't need crypto (flag parsing, offline policy rejection) run today
and pin the CLI surface.
"""
from __future__ import annotations

import json
import subprocess

import pytest

from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor


# ──────────────────────────────────────────────────────────────────────────────
# Happy path — end-to-end sign + verify
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_then_verify_happy_path(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """`sign` produces a referrer; `verify` accepts it — round-trip contract.

    This is the canonical happy path per ADR §"Target architecture". It
    xfails today because the fake fixtures aren't wired; Phase 5 flips it.
    """
    pkg = published_package
    env = {
        **ocx.env,
        "OCX_IDENTITY_TOKEN": fake_oidc_token,
    }
    sign_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
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
    assert sign_result.returncode == 0, sign_result.stderr
    sign_envelope = json.loads(sign_result.stdout)
    assert sign_envelope["schema_version"] == 1
    assert sign_envelope["command"] == "package sign"
    assert sign_envelope["exit_code"] == 0
    data = sign_envelope["data"]
    assert data["subject_digest"].startswith("sha256:")
    assert data["bundle_digest"].startswith("sha256:")

    # Identity/issuer must match what fake_oidc_token carries — Phase 5 wires
    # these into the fixture return so the values flow here.
    verify_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "verify",
            "--certificate-identity", "test-signer@example.com",
            "--certificate-oidc-issuer", "https://fake-oidc.test",
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert verify_result.returncode == 0, verify_result.stderr
    verify_envelope = json.loads(verify_result.stdout)
    assert verify_envelope["schema_version"] == 1
    assert verify_envelope["command"] == "verify"
    assert verify_envelope["data"]["subject_digest"] == data["subject_digest"]


# ──────────────────────────────────────────────────────────────────────────────
# Flag parsing — `--identity-token <TOKEN>` must NOT exist (C-S1-4)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_rejects_identity_token_flag(ocx: OcxRunner) -> None:
    """Raw ``--identity-token`` must be rejected — only file / stdin / env exist.

    C-S1-4: accepting a bare ``--identity-token <TOKEN>`` would land tokens in
    shell history, process listings, and CI logs. The flag must not exist in
    clap's parser at all.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token", "eyJhbGciOi...",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    # clap prints "unexpected argument" / "unknown option" to stderr.
    assert result.returncode != 0, (
        f"--identity-token must be rejected, got rc=0\nstdout: {result.stdout}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "unexpected argument" in stderr_lower
        or "unrecognized" in stderr_lower
        or "unknown" in stderr_lower
        or "unexpected" in stderr_lower
    ), f"expected parser rejection, got stderr: {result.stderr}"


def test_sign_identity_token_file_and_stdin_are_mutually_exclusive(
    ocx: OcxRunner, tmp_path
) -> None:
    """``--identity-token-file`` and ``--identity-token-stdin`` must conflict.

    Per ADR §"Token precedence", exactly one override source may be specified.
    clap's ``conflicts_with`` produces a usage error.
    """
    token_file = tmp_path / "token"
    token_file.write_text("dummy-token")
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token-file", str(token_file),
            "--identity-token-stdin",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode != 0, (
        f"expected rejection for conflicting token sources, got rc=0\n"
        f"stdout: {result.stdout}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "cannot be used with" in stderr_lower
        or "conflicts with" in stderr_lower
        or "the argument" in stderr_lower  # clap's standard "cannot be used with" framing
    ), f"expected conflict error, got stderr: {result.stderr}"


# ──────────────────────────────────────────────────────────────────────────────
# Token precedence — env, stdin, file (Phase 5 wires these)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_reads_env_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """``OCX_IDENTITY_TOKEN`` env var supplies the OIDC token to the sign flow.

    Precedence (lowest to highest): ambient provider → env → stdin → file.
    env overrides ambient; this test confirms env is consumed when present.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
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
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["data"]["bundle_digest"].startswith("sha256:")


def test_sign_reads_stdin_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """``--identity-token-stdin`` reads the token from stdin without shell exposure."""
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-stdin",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        input=fake_oidc_token,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["data"]["bundle_digest"].startswith("sha256:")


# ──────────────────────────────────────────────────────────────────────────────
# Offline policy — exit 81 (sign refused offline)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_offline_refused(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``--offline`` with ``package sign`` is a policy rejection (exit 77).

    Per ADR Risks: offline signing is unsupported in v1 because Fulcio + Rekor
    are hard dependencies. The rejection is a deliberate policy, not a network
    failure — hence ``PermissionDenied`` (77) not ``OfflineBlocked`` (81).

    Phase 5a wired the ``OfflineSignRefused`` early-exit in ``package_sign.rs``;
    this test pins that contract and will fail if the offline check regresses.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--offline",
            "package", "sign",
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / OfflineSignRefused), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — referrers API unsupported → exit 83
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.xfail(
    strict=True,
    reason="Phase 5: probe of referrers endpoint returns 404 against registry:2 "
    "(no referrers API support); sign must exit 83 (ReferrersUnsupported)",
)
def test_sign_referrers_unsupported_exits_83(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Registry without referrers API → exit 83.

    The test registry (``registry:2``) does not implement ``/v2/<name>/referrers/``.
    Phase 5's capability probe must detect the 404 and exit 83 before any
    signing work; sign cannot land without a referrers index.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    result = subprocess.run(
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
    assert result.returncode == 83, (
        f"expected exit 83 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
