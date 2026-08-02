# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `[trust.policy]`-driven ``ocx package verify`` (#98).

Contract source: ``.claude/artifacts/adr_trust_policy.md``. When both
``--certificate-identity`` / ``--certificate-oidc-issuer`` are omitted, verify
resolves ``[[trust.policy]]`` under cross-tier precedence: the operator
``config.toml`` tiers are authoritative — if any operator policy matches the
target, the project ``ocx.toml`` is ignored entirely for it; only when no
operator policy matches does the project tier apply. Within whichever tier
governs, resolution is most-specific-scope-wins (ANY-of among ties), checked
against the signing certificate's SAN + issuer. Sibling to ``test_verify.py``
(the flag-mode / exit-code contract), which this module reuses the fake
Fulcio/Rekor/OIDC stack from.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src.runner import OcxRunner, PackageInfo
from tests.fixtures.fake_sigstore import (
    FAKE_ISSUER_URL,
    FAKE_SUBJECT,
    FakeFulcio,
    FakeRekor,
)


# ──────────────────────────────────────────────────────────────────────────────
# Local scaffolding — sign once, then verify with a crafted policy
# ──────────────────────────────────────────────────────────────────────────────


def _sign(
    ocx: OcxRunner,
    pkg: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Sign ``pkg`` with the fake Sigstore stack; asserts the setup step succeeds."""
    env = {**ocx.env, "OCX_IDENTITY_TOKEN": fake_oidc_token}
    sign = subprocess.run(
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
    assert sign.returncode == 0, f"sign setup failed: {sign.stderr}"


def _verify(
    ocx: OcxRunner,
    pkg: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    extra_env: dict[str, str] | None = None,
    cert_flags: list[str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``package verify``. Omit ``cert_flags`` to exercise policy mode."""
    env = {
        **ocx.env,
        "OCX_SIGSTORE_TRUST_ROOT": str(fake_fulcio.root_pem),
        **(extra_env or {}),
    }
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            *(cert_flags or []),
            "--rekor-url", fake_rekor.url,
            "--platform", "linux/amd64",
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=env,
    )


def _policy_block(
    scope: str,
    oidc_issuer: str,
    *,
    identity: str | None = None,
    identity_regexp: str | None = None,
) -> str:
    """Render one ``[[trust.policy]]`` TOML block."""
    lines = ["[[trust.policy]]", f'scope = "{scope}"']
    if identity is not None:
        lines.append(f'identity = "{identity}"')
    if identity_regexp is not None:
        lines.append(f'identity_regexp = "{identity_regexp}"')
    lines.append(f'oidc_issuer = "{oidc_issuer}"')
    return "\n".join(lines) + "\n"


# Config-tier-only tests disable project discovery entirely so the CWD walk
# never picks up this repo's own dogfooding `ocx.toml` (which has no `[trust]`
# section today, but relying on that would make the test's isolation
# accidental rather than guaranteed).
_NO_PROJECT: dict[str, str] = {"OCX_NO_PROJECT": "1"}


# ──────────────────────────────────────────────────────────────────────────────
# Matching policy — exit 0
# ──────────────────────────────────────────────────────────────────────────────


def test_config_tier_exact_identity_match_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A `$OCX_HOME/config.toml` policy with the correct identity + issuer passes."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT)
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        f"expected exit 0 (policy match), got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


def test_project_tier_ocx_toml_policy_match_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
    tmp_path: Path,
) -> None:
    """A policy declared in the project `ocx.toml` (via `OCX_PROJECT`) passes."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT))

    verify = _verify(
        ocx, pkg, fake_fulcio, fake_rekor, extra_env={"OCX_PROJECT": str(project_toml)}
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (project-tier policy match), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_regexp_identity_match_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """`identity_regexp` (anchored full-match) accepts the signer's SAN."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, FAKE_ISSUER_URL, identity_regexp="^test-signer@.*$")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        f"expected exit 0 (regexp identity match), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Mismatch / absence — exit 77 / 64
# ──────────────────────────────────────────────────────────────────────────────


def test_policy_identity_mismatch_exits_77(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A matched policy pinning the wrong identity fails with `IdentityMismatch`."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, FAKE_ISSUER_URL, identity="someone-else@example.com")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_no_policy_no_flags_exits_64(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """No matching `[[trust.policy]]` anywhere, no flags -> `NoIdentityProvided`."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 64, (
        f"expected exit 64 (UsageError / NoIdentityProvided), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Resolution semantics — specificity, ANY-of rotation, tier pooling
# ──────────────────────────────────────────────────────────────────────────────


def test_most_specific_scope_wins(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """The longest-literal-prefix scope wins, not "any match".

    A broad scope with the CORRECT identity and a narrower scope (covering
    the same package) with a WRONG identity: the narrower policy must be the
    ONLY one resolved, so its wrong identity fails the verify.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope_broad = f"{ocx.registry}/"
    scope_narrow = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope_broad, FAKE_ISSUER_URL, identity=FAKE_SUBJECT) + _policy_block(
        scope_narrow, FAKE_ISSUER_URL, identity="someone-else@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 77, (
        f"expected exit 77 (narrower scope's wrong identity must win), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_any_of_equal_scope_rotation_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """Two policies at the identical scope: ANY-of passes if either matches.

    Models key/workflow rotation — the old and new identity coexist during
    the overlap window and either one must pass.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, FAKE_ISSUER_URL, identity="old-identity@example.com") + _policy_block(
        scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        f"expected exit 0 (ANY-of rotation match), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_tier_append_merge_pools_config_and_project(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
    tmp_path: Path,
) -> None:
    """Tiers array-append (union), never replace: config + project both pool.

    `config.toml` carries a policy for an UNRELATED scope (so it cannot
    match); the project `ocx.toml` carries the matching correct policy. Only
    pooling across both tiers explains a pass here.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    unrelated = _policy_block(
        f"{ocx.registry}/totally-unrelated-prefix", FAKE_ISSUER_URL, identity="nobody@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(unrelated)

    scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT))

    verify = _verify(
        ocx, pkg, fake_fulcio, fake_rekor, extra_env={"OCX_PROJECT": str(project_toml)}
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (config + project tiers pooled), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_operator_config_authoritative_over_project(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
    tmp_path: Path,
) -> None:
    """The operator `config.toml` tier is authoritative over the project `ocx.toml`.

    An operator policy with a BROAD scope pins the WRONG identity; a project
    policy with a MORE-SPECIFIC scope (that would otherwise win on
    specificity) pins the CORRECT identity. Because an operator policy
    matches the target at all, the project tier is not consulted for it — a
    project config can never override or weaken an operator pin, even by
    being more specific (security ruling, `resolve_tiered`).
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    operator_scope = f"{ocx.registry}/{pkg.repo[:4]}*"
    operator_toml = _policy_block(operator_scope, FAKE_ISSUER_URL, identity="attacker@evil.test")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(operator_toml)

    project_scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(project_scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT))

    verify = _verify(
        ocx, pkg, fake_fulcio, fake_rekor, extra_env={"OCX_PROJECT": str(project_toml)}
    )
    assert verify.returncode == 77, (
        f"expected exit 77 (operator's broad-but-wrong policy governs; correct "
        f"but more-specific project policy must be ignored), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Malformed matched policy — exit 78
# ──────────────────────────────────────────────────────────────────────────────


def test_both_identity_forms_on_matched_policy_exits_78(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """A matched policy setting BOTH `identity` and `identity_regexp` is a config error.

    Only a policy that actually matches the target scope is validated
    (`TrustPolicy::compile`) — this policy's scope matches the package, so
    the conflict surfaces as `TrustPolicyInvalid` (exit 78), not silently
    ignored.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(
        scope, FAKE_ISSUER_URL, identity=FAKE_SUBJECT, identity_regexp="^test-signer@.*$"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, pkg, fake_fulcio, fake_rekor, extra_env=_NO_PROJECT)
    assert verify.returncode == 78, (
        f"expected exit 78 (ConfigError / TrustPolicyInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Flag mode — overrides policy, both-or-neither
# ──────────────────────────────────────────────────────────────────────────────


def test_flags_override_policy(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """`--certificate-identity` + `--certificate-oidc-issuer` override any policy.

    A config.toml policy for the package's scope pins the WRONG identity and
    issuer; passing the CORRECT pair as flags must still pass, proving flags
    take precedence over (and never consult) the policy pool.
    """
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    scope = f"{ocx.registry}/{pkg.repo}"
    wrong_policy = _policy_block(
        scope, "https://wrong-issuer.example", identity="someone-else@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(wrong_policy)

    verify = _verify(
        ocx,
        pkg,
        fake_fulcio,
        fake_rekor,
        extra_env=_NO_PROJECT,
        cert_flags=[
            "--certificate-identity", FAKE_SUBJECT,
            "--certificate-oidc-issuer", FAKE_ISSUER_URL,
        ],
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (flags override policy), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_single_flag_without_pair_exits_64(
    ocx: OcxRunner,
    published_package: PackageInfo,
    fake_fulcio: FakeFulcio,
    fake_rekor: FakeRekor,
    fake_oidc_token: str,
) -> None:
    """`--certificate-identity` alone (no issuer) is a clap usage error."""
    pkg = published_package
    _sign(ocx, pkg, fake_fulcio, fake_rekor, fake_oidc_token)

    verify = _verify(
        ocx,
        pkg,
        fake_fulcio,
        fake_rekor,
        cert_flags=["--certificate-identity", FAKE_SUBJECT],
    )
    assert verify.returncode == 64, (
        f"expected exit 64 (clap requires both-or-neither), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
