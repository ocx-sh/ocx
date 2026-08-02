# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for policy-gated auto-verify on install/pull (#99).

When an operator `[[trust.policy]]` (in `$OCX_HOME/config.toml`) covers a
package, `ocx package install` / `ocx package pull` verify its Sigstore
signature automatically at the metadata-first seam — after the manifest digest
resolves, before any layer download. A covered-but-invalid signature aborts the
install fail-closed (no package-store or symlink state). No policy → no verify
(trust is opt-in). `--no-verify` / `OCX_NO_VERIFY` opt out with a single WARN
(the flag wins over the env).

Because install/pull carry no `--rekor-url` flag, auto-verify uses the DEFAULT
public Rekor endpoint — so these tests pin the Rekor key via
`OCX_SIGSTORE_TUF_ROOT` (a trusted-root JSON from the fake stack), the same
air-gapped seam `test_offline_verify.py` exercises. Signing uses the standalone
`fake_fulcio` / `fake_rekor` / `fake_oidc_token` fixtures, which derive from the
same `fake_sigstore_stack`, so the pinned key matches the signature.

The signing identity is `test-signer@example.com` / issuer `https://fake-oidc.test`.
Sign + install target `linux/amd64` (the CI/dev host arch) so the signed
platform digest is the one auto-verify resolves.
"""
from __future__ import annotations

import subprocess

from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import FakeFulcio, FakeRekor, FakeSigstoreStack, HttpStatus

GOOD_IDENTITY = "test-signer@example.com"
BAD_IDENTITY = "someone-else@example.com"
ISSUER = "https://fake-oidc.test"


def _policy_scope(ocx: OcxRunner, pkg: PackageInfo) -> str:
    """The canonical `registry/repository` the auto-verify gate matches on."""
    return f"{ocx.registry}/{pkg.repo}"


def _write_operator_policy(ocx: OcxRunner, scope: str, identity: str) -> None:
    """Write a `[[trust.policy]]` into `$OCX_HOME/config.toml` (operator tier)."""
    config = ocx.ocx_home / "config.toml"
    config.write_text(
        f'[[trust.policy]]\nscope = "{scope}"\nidentity = "{identity}"\noidc_issuer = "{ISSUER}"\n'
    )


def _sign(ocx: OcxRunner, pkg: PackageInfo, fulcio: FakeFulcio, rekor: FakeRekor, token: str) -> None:
    """Sign `pkg` online with the fake stack; publishes the signature referrer."""
    result = subprocess.run(
        [
            str(ocx.binary), "package", "sign",
            "--fulcio-url", fulcio.url,
            "--rekor-url", rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True, text=True, env={**ocx.env, "OCX_IDENTITY_TOKEN": token},
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"


def _run(ocx: OcxRunner, verb: str, *packages: str, flags: tuple[str, ...] = (), extra_env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    """Run `ocx package <verb> -p linux/amd64 [flags] <packages...>`."""
    return subprocess.run(
        [str(ocx.binary), "package", verb, "-p", "linux/amd64", *flags, *packages],
        capture_output=True, text=True, env={**ocx.env, **(extra_env or {})},
    )


def _assert_no_partial_state(ocx: OcxRunner, pkg: PackageInfo) -> None:
    """A fail-closed abort must leave no assembled package and no candidate symlink."""
    packages_dir = ocx.ocx_home / "packages"
    assert not packages_dir.exists() or not list(packages_dir.rglob("metadata.json")), (
        "fail-closed install must not assemble a package (aborts before download)"
    )
    which = _run(ocx, "which", pkg.short, flags=("--candidate",))
    assert which.returncode != 0, "fail-closed install must not create a candidate symlink"


def _tuf(stack: FakeSigstoreStack) -> dict[str, str]:
    """Env pinning the Rekor key so auto-verify needs no default-Rekor fetch."""
    return {"OCX_SIGSTORE_TUF_ROOT": str(stack.trusted_root_json_path())}


# ──────────────────────────────────────────────────────────────────────────────
# Policy-covered + valid signature → install auto-verifies and succeeds
# ──────────────────────────────────────────────────────────────────────────────


def test_policy_covered_valid_signature_installs(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """A matching policy + valid signature → install verifies and exits 0."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)

    result = _run(ocx, "install", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert result.returncode == 0, (
        f"policy-covered valid signature must install, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Policy-covered + wrong identity → abort fail-closed before download (exit 77)
# ──────────────────────────────────────────────────────────────────────────────


def test_policy_covered_identity_mismatch_aborts_fail_closed(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """Signature valid but signer ≠ policy identity → exit 77, no partial state."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), BAD_IDENTITY)

    result = _run(ocx, "install", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert result.returncode == 77, (
        f"identity mismatch must abort with exit 77, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    _assert_no_partial_state(ocx, pkg)


# ──────────────────────────────────────────────────────────────────────────────
# Policy-covered + no signature → abort fail-closed (exit 79)
# ──────────────────────────────────────────────────────────────────────────────


def test_policy_covered_unsigned_aborts_fail_closed(
    ocx: OcxRunner, published_package: PackageInfo, fake_sigstore_stack: FakeSigstoreStack,
) -> None:
    """A policy-covered package with no signature → exit 79, no partial state."""
    pkg = published_package  # never signed
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)

    result = _run(ocx, "install", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert result.returncode == 79, (
        f"unsigned policy-covered package must abort with exit 79, got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )
    _assert_no_partial_state(ocx, pkg)


# ──────────────────────────────────────────────────────────────────────────────
# No policy → install proceeds without verification (opt-in trust)
# ──────────────────────────────────────────────────────────────────────────────


def test_no_policy_installs_without_verification(
    ocx: OcxRunner, published_package: PackageInfo,
) -> None:
    """No `[[trust.policy]]` at all → the (unsigned) package installs, exit 0."""
    pkg = published_package  # unsigned, and no config.toml written
    result = _run(ocx, "install", pkg.short)
    assert result.returncode == 0, (
        f"a package no policy covers must install without verification, got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# --no-verify opts out with a single WARN across the batch
# ──────────────────────────────────────────────────────────────────────────────


def test_no_verify_skips_with_single_warn(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo],
) -> None:
    """`--no-verify` skips two covered packages with exactly one WARN (once/invocation)."""
    v1, v2 = published_two_versions  # same repo → one scope covers both; neither signed
    _write_operator_policy(ocx, _policy_scope(ocx, v1), GOOD_IDENTITY)

    result = _run(ocx, "install", v1.short, v2.short, flags=("--no-verify",))
    assert result.returncode == 0, (
        f"--no-verify must skip verification and install, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    warns = result.stderr.lower().count("verification skipped")
    assert warns == 1, f"expected exactly one WARN per invocation, got {warns}\nstderr: {result.stderr.strip()}"


# ──────────────────────────────────────────────────────────────────────────────
# Flag wins over env: OCX_NO_VERIFY skips, --verify overrides it back on
# ──────────────────────────────────────────────────────────────────────────────


def test_verify_flag_overrides_env_opt_out(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """`OCX_NO_VERIFY=1` skips (exit 0); adding `--verify` forces verify → exit 77."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), BAD_IDENTITY)  # would fail if verified

    skipped = _run(ocx, "install", pkg.short, extra_env={"OCX_NO_VERIFY": "1"})
    assert skipped.returncode == 0, (
        f"OCX_NO_VERIFY=1 must skip verification, got {skipped.returncode}\nstderr: {skipped.stderr.strip()}"
    )

    # Fresh env-only opt-out, but --verify on the command line wins over it.
    forced = _run(ocx, "install", pkg.short, flags=("--verify",), extra_env={"OCX_NO_VERIFY": "1", **_tuf(fake_sigstore_stack)})
    assert forced.returncode == 77, (
        f"--verify must override OCX_NO_VERIFY and re-verify (exit 77 on the bad policy), "
        f"got {forced.returncode}\nstderr: {forced.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# `ocx package pull` is gated too
# ──────────────────────────────────────────────────────────────────────────────


def test_pull_is_policy_gated(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """`ocx package pull` aborts fail-closed on a covered-but-mismatched signature."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), BAD_IDENTITY)

    result = _run(ocx, "pull", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert result.returncode == 77, (
        f"pull must be policy-gated and abort with exit 77, got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    packages_dir = ocx.ocx_home / "packages"
    assert not packages_dir.exists() or not list(packages_dir.rglob("metadata.json")), (
        "fail-closed pull must not materialise the package"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Offline + pinned trust material → auto-verify works with no Sigstore network
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_auto_verify_with_pinned_material(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """Online install caches content; an OFFLINE re-install verifies from the pinned
    Rekor key with Rekor forced to 503 — proving no Sigstore-services fetch."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)

    online = _run(ocx, "install", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert online.returncode == 0, f"online install (cache warm) failed: {online.stderr.strip()}"

    # Kill Rekor: a later verify that still succeeds cannot have fetched its key.
    fake_rekor.set_failure_mode(HttpStatus(503))

    offline = _run(ocx, "install", pkg.short, extra_env={"OCX_OFFLINE": "1", **_tuf(fake_sigstore_stack)})
    assert offline.returncode == 0, (
        f"offline install with pinned material must auto-verify and succeed, got "
        f"{offline.returncode}\nstderr: {offline.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# The bypass is closed: every auto-install surface is gated, not just install/pull
# ──────────────────────────────────────────────────────────────────────────────


def test_package_exec_is_policy_gated(
    ocx: OcxRunner, published_package: PackageInfo, fake_sigstore_stack: FakeSigstoreStack,
) -> None:
    """`ocx package exec` auto-installs — a policy-covered unsigned package must
    abort (exit 79), not silently install and run the binary (the closed bypass)."""
    pkg = published_package  # never signed
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", "-p", "linux/amd64", pkg.short, "--", "hello"],
        capture_output=True, text=True, env={**ocx.env, **_tuf(fake_sigstore_stack)},
    )
    assert result.returncode == 79, (
        f"exec on a policy-covered unsigned package must abort (79), not silently install, "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    _assert_no_partial_state(ocx, pkg)


def test_package_env_is_policy_gated(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """`ocx package env` auto-installs — a covered-but-mismatched signature must
    abort (exit 77), not silently install (the closed bypass)."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), BAD_IDENTITY)

    result = subprocess.run(
        [str(ocx.binary), "package", "env", pkg.short],
        capture_output=True, text=True, env={**ocx.env, **_tuf(fake_sigstore_stack)},
    )
    assert result.returncode == 77, (
        f"env on a covered-but-mismatched package must abort (77), not silently install, "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    _assert_no_partial_state(ocx, pkg)


# ──────────────────────────────────────────────────────────────────────────────
# Offline auto-verify still ENFORCES the policy — never silently skips
# ──────────────────────────────────────────────────────────────────────────────


def test_offline_auto_verify_bad_policy_still_enforced(
    ocx: OcxRunner, published_package: PackageInfo,
    fake_sigstore_stack: FakeSigstoreStack, fake_fulcio: FakeFulcio, fake_rekor: FakeRekor, fake_oidc_token: str,
) -> None:
    """An OFFLINE re-install under a BAD_IDENTITY policy must still abort (exit 77).

    Complements ``test_offline_auto_verify_with_pinned_material`` (the GOOD-policy
    pass): here we warm the index+content online under a GOOD policy, flip the
    policy to a wrong identity, force Rekor to 503, then re-install OFFLINE. The
    signature verifies cryptographically from the pinned key (no fetch), but the
    signer is not the policy identity — so auto-verify must fail (77). Exit 0
    would mean offline auto-verify silently skipped the identity check.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)
    online = _run(ocx, "install", pkg.short, extra_env=_tuf(fake_sigstore_stack))
    assert online.returncode == 0, f"online install (index+content warm) failed: {online.stderr.strip()}"

    # Flip the policy to a wrong signer and kill Rekor: a later pass can only come
    # from the pinned key + cache, and the identity check must now reject.
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), BAD_IDENTITY)
    fake_rekor.set_failure_mode(HttpStatus(503))

    offline = _run(ocx, "install", pkg.short, extra_env={"OCX_OFFLINE": "1", **_tuf(fake_sigstore_stack)})
    assert offline.returncode == 77, (
        f"offline auto-verify under a bad policy must still enforce identity (exit 77), "
        f"not silently skip — got {offline.returncode}\nstderr: {offline.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# `ocx run` (toolchain-tier) is gated too — the 6th auto-verify surface
# ──────────────────────────────────────────────────────────────────────────────


def test_run_is_policy_gated(
    ocx: OcxRunner, published_package: PackageInfo, fake_sigstore_stack: FakeSigstoreStack, tmp_path,
) -> None:
    """`ocx run` auto-installs the toolchain — a policy-covered unsigned tool must
    abort (exit 79), not silently install and run the binary.

    `ocx run` is the toolchain-tier auto-install surface (the sixth, alongside
    install/pull/exec/env/`package env`). It resolves an `ocx.toml` binding to a
    package and installs on demand; the auto-verify gate must fire there too. We
    lock with `--no-pull` so the store stays empty and `run` is the surface that
    triggers the install + gate.
    """
    pkg = published_package  # never signed
    _write_operator_policy(ocx, _policy_scope(ocx, pkg), GOOD_IDENTITY)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n{pkg.repo} = "{ocx.registry}/{pkg.repo}:{pkg.tag}"\n'
    )

    tuf_env = _tuf(fake_sigstore_stack)
    lock = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project, capture_output=True, text=True, env={**ocx.env, **tuf_env},
    )
    assert lock.returncode == 0, f"lock setup failed: rc={lock.returncode}\nstderr: {lock.stderr.strip()}"

    result = subprocess.run(
        [str(ocx.binary), "run", "--", "hello"],
        cwd=project, capture_output=True, text=True, env={**ocx.env, **tuf_env},
    )
    assert result.returncode == 79, (
        f"ocx run on a policy-covered unsigned tool must abort (79), not silently install "
        f"and run — got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    _assert_no_partial_state(ocx, pkg)
