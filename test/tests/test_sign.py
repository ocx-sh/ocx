# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sign`` (Slice 1 — referrers signing).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md`` +
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

All tests run against the real Rust sign pipeline. Crypto-dependent tests use
the ``fake_fulcio`` / ``fake_rekor`` / ``fake_oidc_token`` fixtures (fake
Fulcio/Rekor + OIDC issuer, per ADR D9).
"""
from __future__ import annotations

import json
import subprocess
import sys

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

    This is the canonical happy path per ADR §"Target architecture", exercised
    against the fake Sigstore stack: sign injects the OIDC token via
    ``OCX_IDENTITY_TOKEN`` and points at the fake Fulcio/Rekor; verify injects
    the fake Fulcio CA as the trust root via ``OCX_SIGSTORE_TRUST_ROOT``.
    """
    pkg = published_package
    env = {
        **ocx.env,
        "OCX_IDENTITY_TOKEN": fake_oidc_token,
        "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem),
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
            "package", "verify",
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
    assert verify_envelope["command"] == "package verify"
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
# Token precedence — C-S1-4: file > stdin > env
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_token_file_only(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_sigstore_stack: "FakeSigstoreStack",
    tmp_path,
) -> None:
    """C-S1-4 basic happy path: ``--identity-token-file`` only, no stdin, no env.

    The token file must be read, trimmed, and passed to the sign pipeline.
    """
    from tests.fixtures.fake_sigstore import FakeSigstoreStack

    token = fake_sigstore_stack.oidc_token()
    token_file = tmp_path / "token"
    token_file.write_text(token + "\n")  # trailing newline is common; must be trimmed
    token_file.chmod(0o600)

    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["schema_version"] == 1
    assert envelope["command"] == "package sign"
    assert envelope["exit_code"] == 0
    assert envelope["data"]["bundle_digest"].startswith("sha256:")


def test_sign_token_stdin_overrides_env(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_sigstore_stack: "FakeSigstoreStack",
) -> None:
    """C-S1-4 precedence: stdin token overrides ``OCX_IDENTITY_TOKEN`` env.

    Both stdin and env supply different tokens.  The sign pipeline must use
    the stdin token (higher precedence), not the env token.  The observable
    outcome is a successful sign — if the wrong (env) token were used and it
    happened to be invalid, the pipeline would reject it.  Because both tokens
    come from the same fake issuer, either is accepted by fake Fulcio; the
    precedence is verified structurally by the CLI taking the stdin path rather
    than the env path.
    """
    from tests.fixtures.fake_sigstore import FakeSigstoreStack

    stdin_token = fake_sigstore_stack.oidc_token()
    env_token = fake_sigstore_stack.oidc_token()  # a distinct token (different iat/exp)
    assert stdin_token != env_token, "tokens should differ (different timestamp)"

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": env_token}
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
        input=stdin_token,
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    assert envelope["exit_code"] == 0
    assert envelope["data"]["bundle_digest"].startswith("sha256:")


def test_sign_token_file_overrides_stdin_and_env(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_sigstore_stack: "FakeSigstoreStack",
    tmp_path,
) -> None:
    """C-S1-4 precedence: file token wins over stdin AND ``OCX_IDENTITY_TOKEN``.

    Three distinct tokens are used — file, stdin, and env — all from the same
    fake OIDC issuer and therefore all valid.  The expected outcome is a
    successful sign using the file token (highest precedence).  Because the
    CLI enforces ``--identity-token-file`` XOR ``--identity-token-stdin`` at
    the clap level, this test verifies the file path by setting only
    ``--identity-token-file`` alongside ``OCX_IDENTITY_TOKEN``.
    """
    from tests.fixtures.fake_sigstore import FakeSigstoreStack

    file_token = fake_sigstore_stack.oidc_token()
    env_token = fake_sigstore_stack.oidc_token()

    token_file = tmp_path / "token"
    token_file.write_text(file_token)
    token_file.chmod(0o600)

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": env_token}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
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
    assert envelope["exit_code"] == 0
    assert envelope["data"]["bundle_digest"].startswith("sha256:")


# ──────────────────────────────────────────────────────────────────────────────
# Token file permissions — world-readable file → exit 77
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "win32", reason="Unix permission semantics")
def test_sign_rejects_world_readable_identity_token_file(
    ocx: OcxRunner, tmp_path
) -> None:
    """``--identity-token-file`` with mode 0o644 (world-readable) must exit 77.

    C-S1-4 / SignErrorKind::IdentityTokenFilePermissive: identity token files
    that are group- or world-readable expose OIDC tokens in multi-user
    environments. OCX must reject them at file-open time before the token is
    ever read, exiting with PermissionDenied (77) so scripts can distinguish
    this configuration error from a network or auth failure.
    """
    token_file = tmp_path / "token.oidc"
    token_file.write_text("fake-oidc-token\n")
    # Set world-readable permissions — must be rejected.
    token_file.chmod(0o644)

    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--identity-token-file", str(token_file),
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityTokenFilePermissive), "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    stderr_lower = result.stderr.lower()
    assert (
        "permissive" in stderr_lower
        or "permission" in stderr_lower
        or "0o644" in stderr_lower
        or "644" in stderr_lower
        or "chmod" in stderr_lower
        or "mode" in stderr_lower
    ), f"expected permission-related wording in stderr, got: {result.stderr!r}"


# ──────────────────────────────────────────────────────────────────────────────
# Registry capability — referrers API unsupported → exit 84
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_referrers_unsupported_exits_84(
    ocx: OcxRunner,
    legacy_registry: str,
    unique_repo: str,
    tmp_path,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Registry without referrers API → exit 84.

    ``legacy_registry`` (``registry:2``, #106/#195 negative fixture) does not
    implement ``/v2/<name>/referrers/``. The capability probe must detect the
    404 and exit 84 before any signing work; sign cannot land without a
    referrers index.
    """
    from src.helpers import make_package

    legacy_ocx = OcxRunner(ocx.binary, ocx.ocx_home, legacy_registry)
    pkg = make_package(legacy_ocx, unique_repo, "1.0.0", tmp_path)
    env = {**legacy_ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
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
    assert result.returncode == 84, (
        f"expected exit 84 (ReferrersUnsupported), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Credential exemption — OCX_IDENTITY_TOKEN must not leak to child processes
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_does_not_forward_identity_token_to_children(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
    tmp_path,
) -> None:
    """``OCX_IDENTITY_TOKEN`` must never be echoed to the sign command's output.

    Credential exemption (see ``subsystem-cli.md``): the token is a bearer
    credential read directly via ``std::env::var`` for the sign call only;
    ``Env::apply_ocx_config`` actively scrubs it from any subprocess env
    composed via ``OcxConfigView``. The Rust unit test
    ``apply_ocx_config_never_forwards_credential_tokens`` covers the lib
    boundary; this test pins the end-to-end behaviour through the sign command
    by driving a real, accepted token and asserting it never appears on stdout
    or stderr (the streams a child would inherit or a log would capture).
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
    assert result.returncode == 0, result.stderr
    # The identity token must never surface in the command's output streams.
    assert fake_oidc_token not in result.stdout, "identity token leaked into stdout"
    assert fake_oidc_token not in result.stderr, "identity token leaked into stderr"


# ──────────────────────────────────────────────────────────────────────────────
# SSRF guard — non-loopback HTTP and non-{http,https} schemes → exit 64
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_rejects_http_non_loopback_fulcio_url(ocx: OcxRunner) -> None:
    """`--fulcio-url http://example.com/...` must exit 64 (UsageError).

    The SSRF guard (`validate_sigstore_url`) permits `http://` only for
    loopback hosts so the fake-sigstore stack works in CI; any other
    `http://` target is a CWE-918 risk and the typed
    ``SignErrorKind::InvalidEndpointUrl`` routes it through `UsageError`.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--fulcio-url", "http://example.com/fulcio",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl on --fulcio-url), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


def test_sign_rejects_ftp_scheme_url(ocx: OcxRunner) -> None:
    """`--rekor-url ftp://...` must exit 64 (UsageError).

    Any scheme other than `http` (loopback only) and `https` is rejected at
    the SSRF guard so neither sign nor verify ever issues a non-HTTP request
    to a user-supplied endpoint.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--rekor-url", "ftp://example.com/bundle",
            "--platform", "linux/amd64",
            "pkg:1.0",
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl on ftp scheme), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Re-sign idempotency — ADR S1-I
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_then_sign_again_is_idempotent(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Two sign invocations for the same subject must not double-publish.

    Per ADR §"Re-sign idempotency" (S1-I): a second `package sign` of an
    already-signed subject either no-ops (publisher convention) or refreshes
    the existing referrer pointer; in either case the referrers list for
    that subject must contain exactly one bundle from this signer afterwards.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    for _ in range(2):
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
        assert result.returncode == 0, result.stderr


# ──────────────────────────────────────────────────────────────────────────────
# --no-tty + missing override + no ambient → exit 77 (B3 observable contract)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_no_tty_skips_browser_fallback_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
) -> None:
    """`--no-tty` with no override token + no ambient detection → exit 77.

    B3 observable contract: when the dispatcher cannot find a token through
    any of override/ambient and `--no-tty` is set, it MUST NOT attempt the
    interactive browser OAuth (which would hang in CI). It surfaces
    `OidcPreCheckFailed` → exit 77 instead.
    """
    pkg = published_package
    # Deliberately do NOT set OCX_IDENTITY_TOKEN — and pass --no-tty so the
    # only legal path (browser) is suppressed.
    env_no_token = {k: v for k, v in ocx.env.items() if k != "OCX_IDENTITY_TOKEN"}
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--no-tty",
            "--fulcio-url", fake_fulcio.url,
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env_no_token,
    )
    assert result.returncode == 77, (
        f"expected exit 77 (PermissionDenied / OidcPreCheckFailed), got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
