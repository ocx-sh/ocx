# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package sign`` (Slice 1 — referrers signing).

Contract source: ``.claude/artifacts/adr_oci_referrers_signing_v1.md`` +
``.claude/state/plans/plan_slice1_sign_and_verify.md``.

All tests run against the real Rust sign pipeline. Crypto-dependent tests drive
the real Sigstore stack (`sigstore` compose profile) through the
``sigstore_stack`` / ``identity_token`` fixtures.
"""
from __future__ import annotations

import base64
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

from src.registry import get_blob, get_manifest, list_referrers
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack

# Sigstore bundle v0.3 artifact type — mirrors the Rust constant
# `oci::referrer::media_types::SIGSTORE_BUNDLE_V03`.
SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"

# A full, un-shortened digest: `sha256:` + 64 hex chars. `.startswith("sha256:")`
# alone is also satisfied by the 12-hex short form the CLI's plain-mode output
# uses (`api/data/signature.rs::plain_fields`), so it cannot tell "JSON was
# shortened" from "JSON stayed full" — a regression that shortened the JSON
# digest would still pass a bare `startswith` check.
FULL_SHA256_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")


def _endpoint_args(stack: SigstoreStack) -> list[str]:
    """`sign_args` minus `--identity-token-file`, for the token-source tests.

    Those tests are about where the token comes from, so the flag that supplies
    one has to be absent — otherwise they would all pass through the same path.
    """
    return [
        "--platform", current_platform(),
        "--fulcio-url", stack.fulcio_url,
        "--rekor-url", stack.rekor_url,
    ]


# ──────────────────────────────────────────────────────────────────────────────
# Happy path — end-to-end sign + verify
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_then_verify_happy_path(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`sign` produces a referrer; `verify` accepts it — round-trip contract.

    The canonical happy path per ADR §"Target architecture", with the JSON
    envelope pinned on both halves — that is what this adds over the smoke test,
    which only pins the exit codes.
    """
    pkg = published_package
    sign_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            *sigstore_stack.sign_args(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert sign_result.returncode == 0, sign_result.stderr
    sign_envelope = json.loads(sign_result.stdout)
    assert sign_envelope["schema_version"] == 1
    assert sign_envelope["command"] == "package sign"
    assert sign_envelope["exit_code"] == 0
    data = sign_envelope["data"]
    assert data["subject_digest"].startswith("sha256:")
    assert data["bundle_digest"].startswith("sha256:")
    # The identity sign reports is the SAN of the certificate it just had
    # issued, not the token's `sub`. dex mints a `sub` that is an opaque
    # base64 provider id while Fulcio puts the *email* in the SAN, so
    # reporting the claim gave a user an identity that no
    # `--certificate-identity` and no `[[trust.policy]]` would ever match.
    assert data["certificate_identity"] == sigstore_stack.identity
    assert data["certificate_oidc_issuer"] == sigstore_stack.issuer

    verify_result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "verify",
            *sigstore_stack.verify_args(),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert verify_result.returncode == 0, verify_result.stderr
    verify_envelope = json.loads(verify_result.stdout)
    assert verify_envelope["schema_version"] == 1
    assert verify_envelope["command"] == "package verify"
    assert verify_envelope["data"]["subject_digest"] == data["subject_digest"]
    # Both commands read one certificate; they must name one identity.
    assert verify_envelope["data"]["certificate_identity"] == data["certificate_identity"]
    assert verify_envelope["data"]["certificate_oidc_issuer"] == data["certificate_oidc_issuer"]


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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``OCX_IDENTITY_TOKEN`` env var supplies the OIDC token to the sign flow.

    Precedence (lowest to highest): ambient provider → env → stdin → file.
    env overrides ambient; this test confirms env is consumed when present.
    """
    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": identity_token.read_text().strip()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    bundle_digest = envelope["data"]["bundle_digest"]
    assert FULL_SHA256_DIGEST_RE.fullmatch(bundle_digest), (
        f"JSON bundle_digest must stay the full sha256:<64hex> form, not the "
        f"12-hex short form plain-mode uses, got: {bundle_digest!r}"
    )


def test_sign_reads_stdin_token(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``--identity-token-stdin`` reads the token from stdin without shell exposure."""
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-stdin",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        input=identity_token.read_text().strip(),
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, result.stderr
    envelope = json.loads(result.stdout)
    bundle_digest = envelope["data"]["bundle_digest"]
    assert FULL_SHA256_DIGEST_RE.fullmatch(bundle_digest), (
        f"JSON bundle_digest must stay the full sha256:<64hex> form, not the "
        f"12-hex short form plain-mode uses, got: {bundle_digest!r}"
    )


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
            "--format", "json",
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
    # 77 alone is also IdentityTokenFilePermissive and OidcPreCheckFailed (and
    # any bare filesystem EPERM) — assert the frozen slug so this test cannot
    # pass for a cause other than the offline policy refusal it names.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "offline_sign_refused", (
        f"exit 77 must be the offline-policy refusal, not a different "
        f"PermissionDenied cause; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Token precedence — C-S1-4: file > stdin > env
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_token_file_only(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 basic happy path: ``--identity-token-file`` only, no stdin, no env.

    The token file must be read, trimmed, and passed to the sign pipeline.
    """
    token_file = tmp_path / "token"
    # Trailing newline is common; must be trimmed before the token reaches Fulcio.
    token_file.write_text(identity_token.read_text().strip() + "\n")
    token_file.chmod(0o600)

    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
            *_endpoint_args(sigstore_stack),
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 precedence: stdin token overrides ``OCX_IDENTITY_TOKEN`` env.

    The env token is one real Fulcio rejects (right claims, foreign key), so a
    successful sign can only mean stdin won. The fake stack could not do this —
    both its tokens were acceptable, so precedence was asserted structurally and
    a regression that read env instead would have passed.
    """
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": foreign.read_text()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-stdin",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        input=identity_token.read_text().strip(),
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path,
) -> None:
    """C-S1-4 precedence: file token wins over ``OCX_IDENTITY_TOKEN``.

    The env token is one Fulcio rejects, so exit 0 proves the file token was the
    one exchanged. clap enforces ``--identity-token-file`` XOR
    ``--identity-token-stdin``, so stdin cannot be in the picture as well.
    """
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )

    token_file = tmp_path / "token"
    token_file.write_text(identity_token.read_text().strip())
    token_file.chmod(0o600)

    pkg = published_package
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": foreign.read_text()}
    result = subprocess.run(
        [
            str(ocx.binary),
            "--format", "json",
            "package", "sign",
            "--identity-token-file", str(token_file),
            *_endpoint_args(sigstore_stack),
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            *sigstore_stack.sign_args(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=legacy_ocx.env,
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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
    token = identity_token.read_text().strip()
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            *_endpoint_args(sigstore_stack),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_IDENTITY_TOKEN": token},
    )
    assert result.returncode == 0, result.stderr
    # The identity token must never surface in the command's output streams.
    assert token not in result.stdout, "identity token leaked into stdout"
    assert token not in result.stderr, "identity token leaked into stderr"


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


@pytest.mark.xfail(
    strict=True,
    reason="ADR S1-I (idempotent re-sign) not yet implemented: sign/pipeline.rs "
    "unconditionally pushes a new bundle+referrer, so a re-sign leaves two. "
    "Deferred (not a review-fix-pass regression); tracked as production follow-up. "
    "ANY-of verify tolerates the duplicate, so this is a publisher-hygiene gap, "
    "not a verification hole. Remove this marker when S1-I dedup lands.",
)
def test_sign_then_sign_again_is_idempotent(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Two sign invocations for the same subject must not double-publish.

    Per ADR §"Re-sign idempotency" (S1-I): a second `package sign` of an
    already-signed subject either no-ops (publisher convention) or refreshes
    the existing referrer pointer; in either case the referrers list for
    that subject must contain exactly one bundle from this signer afterwards.

    Effect assertion (not just rc==0): after two signs we count the Sigstore
    bundle referrers on the *subject digest* via the Referrers API. Now that
    ANY-of verification ships, a re-sign that appended a duplicate referrer
    would silently pass every verify — so the idempotency contract has to be
    pinned on the referrer count, not the exit code.
    """
    pkg = published_package
    subject_digest: str | None = None
    for _ in range(2):
        result = subprocess.run(
            [
                str(ocx.binary),
                "--format", "json",
                "package", "sign",
                *sigstore_stack.sign_args(identity_token),
                pkg.short,
            ],
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert result.returncode == 0, result.stderr
        subject_digest = json.loads(result.stdout)["data"]["subject_digest"]

    assert subject_digest is not None
    status, index = list_referrers(
        ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03
    )
    assert status == 200, f"referrers listing failed with status {status}"
    bundles = [
        m for m in (index or {}).get("manifests", [])
        if m.get("artifactType") == SIGSTORE_BUNDLE_V03
    ]
    assert len(bundles) == 1, (
        f"re-sign must be idempotent: expected exactly one bundle referrer on "
        f"{subject_digest}, found {len(bundles)}: {bundles}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# --no-tty + missing override + no ambient → exit 77 (B3 observable contract)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_no_tty_skips_browser_fallback_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
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
            "--format", "json",
            "package", "sign",
            "--no-tty",
            *_endpoint_args(sigstore_stack),
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
    # 77 alone is also OfflineSignRefused and IdentityTokenFilePermissive —
    # assert the frozen slug so this test cannot pass for a cause other than
    # the no-tty/no-ambient-token precheck it names.
    envelope = json.loads(result.stdout)
    assert envelope["error"]["detail"] == "oidc_pre_check_failed", (
        f"exit 77 must be the OIDC pre-check refusal, not a different "
        f"PermissionDenied cause; got {envelope['error']}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rekor unavailable DURING SIGN — exit 83 (TransparencyLogUnavailable)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_transparency_log_unavailable_exits_83(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Rekor unreachable during the sign-time log upload → exit 83.

    Signing requires a Rekor transparency-log entry; when the log is down the
    sign cannot complete. This is a service-availability failure (retry may
    help), so it maps to ``TransparencyLogUnavailable`` (83), distinct from a data
    failure. Fulcio stays real, so the failure is isolated to the log step.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--platform", current_platform(),
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", adversarial.unreachable_rekor_url(),
            "--identity-token-file", str(identity_token),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 83, (
        f"expected exit 83 (TransparencyLogUnavailable) when Rekor 503s during sign, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Wrong-key OIDC token — Fulcio rejects → exit 80 (AuthError / OidcTokenRejected)
# ──────────────────────────────────────────────────────────────────────────────


def test_sign_wrong_key_oidc_token_exits_80(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path,
) -> None:
    """A JWT with valid claims but signed by an untrusted key → exit 80.

    The token's iss/aud/sub look correct, but its signature does not verify
    against the JWKS Fulcio fetches from dex (it was signed by a foreign key).
    Fulcio must reject the CSR and the sign pipeline surfaces
    ``OidcTokenRejected`` → ``AuthError`` (80), never a network/usage code.
    """
    pkg = published_package
    foreign = adversarial.foreign_identity_token(
        sigstore_stack.issuer, sigstore_stack.identity, tmp_path
    )
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            *sigstore_stack.sign_args(foreign),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 80, (
        f"expected exit 80 (AuthError / OidcTokenRejected) for a wrong-key token, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


def test_sign_refuses_a_rekor_url_that_resolves_into_a_forbidden_range(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A trust-service endpoint is judged by where it resolves, not how it is spelled.

    `--rekor-url https://169.254.169.254/` passes the string-level check at the
    CLI boundary -- it is HTTPS -- so without the dial-time guard ocx would
    fetch the cloud metadata endpoint and surface the response in an error
    message (CWE-918). The realistic carrier is a hostile `ocx.toml` or managed
    config tier rather than a typed flag; the flag is how the test reaches the
    same code path.

    Asserted on the exit code AND on the refusal naming the offending flag, so
    a run that merely fails to reach the metadata endpoint for some unrelated
    reason cannot be mistaken for the guard firing.
    """
    result = subprocess.run(
        [
            str(ocx.binary),
            "package", "sign",
            "--platform", current_platform(),
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", "https://169.254.169.254/",
            "--identity-token-file", str(identity_token),
            published_package.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 64, (
        f"expected exit 64 (UsageError / InvalidEndpointUrl) for a link-local "
        f"rekor URL, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert "--rekor-url" in result.stderr, (
        f"the refusal must name the offending flag\nstderr: {result.stderr.strip()}"
    )
    assert "169.254.169.254" in result.stderr, (
        f"the refusal must name the forbidden target\nstderr: {result.stderr.strip()}"
    )


def test_sign_still_accepts_the_explicitly_named_local_stack(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The loopback carve-out survives the dial-time guard.

    The positive half of the check above: every other signing test in this file
    points at `http://localhost:...`, which resolves into a forbidden range and
    is admitted only because the URL literally says loopback. Without this, a
    guard that refused everything would look identical to a guard that worked.
    """
    envelope = ocx.json("package", "sign", *sigstore_stack.sign_args(identity_token), published_package.short)
    assert envelope["exit_code"] == 0, envelope
    assert envelope["data"]["bundle_digest"].startswith("sha256:"), envelope


# ──────────────────────────────────────────────────────────────────────────────
# SOURCE_DATE_EPOCH — one instant, taken once, reaching every place it belongs
# ──────────────────────────────────────────────────────────────────────────────

#: An arbitrary fixed instant, and its RFC 3339 rendering with an explicit `Z`
#: (Go's `time.RFC3339`, second precision — the form cosign writes). Spelled out
#: rather than computed so the test asserts against a literal a human checked,
#: not against a second copy of the formatter under test.
SOURCE_DATE_EPOCH = "1740000000"
SOURCE_DATE_EPOCH_RFC3339 = "2025-02-19T21:20:00Z"

#: Where a bundle push records its instant. Mirrors `ANNOTATION_CREATED`.
ANNOTATION_CREATED = "org.opencontainers.image.created"


def _bundle_referrer_manifest(ocx: OcxRunner, pkg: PackageInfo) -> dict:
    """The one Sigstore-bundle referrer manifest attached to ``pkg``'s platform manifest."""
    subject_digest, _size = adversarial.subject_of(ocx.registry, pkg.repo, pkg.tag)
    status, index = list_referrers(
        ocx.registry, pkg.repo, subject_digest, artifact_type=SIGSTORE_BUNDLE_V03
    )
    assert status == 200 and index is not None, f"referrers list failed ({status})"
    manifests = index.get("manifests") or []
    assert len(manifests) == 1, f"expected exactly 1 bundle referrer, found {len(manifests)}"
    return get_manifest(ocx.registry, pkg.repo, manifests[0]["digest"])


def test_sign_source_date_epoch_pins_the_created_annotation(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`SOURCE_DATE_EPOCH` in the child's environment fixes the `created` annotation.

    Black-box on purpose. `bundle_now()` reads the variable straight off the
    process environment (`std::env::var_os`), which no unit test may set —
    edition 2024 makes `env::set_var` unsafe precisely because it races every
    other thread — so a child process with the variable in its environment is
    the only honest way to exercise the read. That leaves the routing itself
    (does the value actually reach the annotation?) covered nowhere but here.
    """
    pkg = published_package
    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "sign",
            *sigstore_stack.sign_args(identity_token), pkg.short,
        ],
        capture_output=True, text=True,
        env={**ocx.env, "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH},
    )
    assert result.returncode == 0, f"sign failed: {result.stderr}"

    annotations = _bundle_referrer_manifest(ocx, pkg).get("annotations") or {}
    assert annotations.get(ANNOTATION_CREATED) == SOURCE_DATE_EPOCH_RFC3339, (
        f"SOURCE_DATE_EPOCH={SOURCE_DATE_EPOCH} must stamp "
        f"{SOURCE_DATE_EPOCH_RFC3339}; got {annotations.get(ANNOTATION_CREATED)!r}. "
        f"A wall-clock value here means the variable is being ignored."
    )


def test_attest_source_date_epoch_stamps_one_instant_in_both_places(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """The attest path stamps the same instant in the annotation and inside the signature.

    Two renderings of one `bundle_now()` reading: the unsigned
    `org.opencontainers.image.created` annotation on the referrer manifest, and
    the `Timestamp` field of the cosign predicate wrapper — which is *inside*
    the DSSE payload and therefore signed. Two independent clock reads would
    let one of them stop honouring `SOURCE_DATE_EPOCH` with nothing noticing,
    so the assertion that matters is that the two agree, not merely that each
    is right.

    `--type custom` because the wrapper exists only for custom predicates; the
    other nine types pass the predicate through untouched and carry no
    `Timestamp` to compare against.
    """
    pkg = published_package
    predicate = tmp_path / "custom.json"
    predicate.write_text(json.dumps({"hello": "world"}))

    result = subprocess.run(
        [
            str(ocx.binary), "--format", "json", "package", "attest",
            "--platform", current_platform(),
            "--predicate", str(predicate), "--type", "custom",
            "--fulcio-url", sigstore_stack.fulcio_url,
            "--rekor-url", sigstore_stack.rekor_url,
            "--identity-token-file", str(identity_token),
            pkg.short,
        ],
        capture_output=True, text=True,
        env={**ocx.env, "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH},
    )
    assert result.returncode == 0, f"attest failed: {result.stderr}"

    manifest = _bundle_referrer_manifest(ocx, pkg)
    annotations = manifest.get("annotations") or {}
    assert annotations.get(ANNOTATION_CREATED) == SOURCE_DATE_EPOCH_RFC3339, (
        f"the attest path must honour SOURCE_DATE_EPOCH too; got "
        f"{annotations.get(ANNOTATION_CREATED)!r}"
    )

    bundle = json.loads(get_blob(ocx.registry, pkg.repo, manifest["layers"][0]["digest"]))
    statement = json.loads(base64.b64decode(bundle["dsseEnvelope"]["payload"]))
    signed_timestamp = statement["predicate"]["Timestamp"]
    assert signed_timestamp == SOURCE_DATE_EPOCH_RFC3339, (
        f"the signed cosign wrapper carries a different instant than the "
        f"annotation: {signed_timestamp!r} vs {SOURCE_DATE_EPOCH_RFC3339}. One "
        f"clock read serves both, so a difference means a second one crept in."
    )
