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
(the flag-mode / exit-code contract), which this module shares the real
Sigstore stack with.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack


# ──────────────────────────────────────────────────────────────────────────────
# Local scaffolding — sign once, then verify with a crafted policy
# ──────────────────────────────────────────────────────────────────────────────


def _sign(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
) -> None:
    """Sign ``pkg`` against the real stack; asserts the setup step succeeds."""
    sign = subprocess.run(
        [str(ocx.binary), "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert sign.returncode == 0, f"sign setup failed: {sign.stderr}"


def _attest(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    predicate: Path,
    predicate_type: str,
) -> None:
    """Attach a signed attestation to ``pkg``; asserts the setup step succeeds."""
    attest = subprocess.run(
        [
            str(ocx.binary),
            "package", "attest",
            *stack.sign_args(token),
            "--predicate", str(predicate),
            "--type", predicate_type,
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert attest.returncode == 0, f"attest setup failed: {attest.stderr}"


def _verify(
    ocx: OcxRunner,
    stack: SigstoreStack,
    pkg: PackageInfo,
    extra_env: dict[str, str] | None = None,
    cert_flags: list[str] | None = None,
    *,
    attestation: bool = False,
    predicate_type: str | None = None,
    json_format: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run ``package verify``. Omit ``cert_flags`` to exercise policy mode.

    ``attestation``/``predicate_type`` mirror ``test_verify.py``'s ``_verify``
    so a builder-pin check (#103, S-013) can exercise attestation mode without
    ``--certificate-identity``/``--certificate-oidc-issuer`` -- flag-mode verify
    always sets ``CompiledPolicy::exact(..)``, whose ``builder`` field is
    unconditionally ``None`` (see ``trust.rs``'s
    ``the_flag_override_policy_carries_no_builder_pin``), so only policy mode
    can reach the code path this section pins.
    """
    env = {**ocx.env, **(extra_env or {})}
    return subprocess.run(
        [
            str(ocx.binary),
            *(["--format", "json"] if json_format else []),
            "package", "verify",
            *(cert_flags or []),
            *(["--attestation"] if attestation else []),
            *(["--type", predicate_type] if predicate_type else []),
            "--platform", current_platform(),
            "--rekor-url", stack.rekor_url,
            "--sigstore-trusted-root", str(stack.trust_root),
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
    builder: str | None = None,
) -> str:
    """Render one ``[[trust.policy]]`` TOML block with a single keyless signer.

    ``builder`` is a top-level sibling of ``scope`` -- NOT a member of the
    signer entry -- per ``trust.rs``'s
    ``builder_is_a_backend_independent_sibling_of_scope`` fixture (#103).
    """
    lines = ["[[trust.policy]]", f'scope = "{scope}"']
    if builder is not None:
        lines.append(f'builder = "{builder}"')
    entry = ['kind = "keyless"']
    if identity is not None:
        entry.append(f'identity = "{identity}"')
    if identity_regexp is not None:
        entry.append(f'identity_regexp = "{identity_regexp}"')
    entry.append(f'oidc_issuer = "{oidc_issuer}"')
    lines.append("signers = [{ " + ", ".join(entry) + " }]")
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A `$OCX_HOME/config.toml` policy with the correct identity + issuer passes."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, sigstore_stack.issuer, identity=sigstore_stack.identity)
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        f"expected exit 0 (policy match), got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


def test_project_tier_ocx_toml_policy_match_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A policy declared in the project `ocx.toml` (via `OCX_PROJECT`) passes."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(scope, sigstore_stack.issuer, identity=sigstore_stack.identity))

    verify = _verify(
        ocx, sigstore_stack, pkg, extra_env={"OCX_PROJECT": str(project_toml)}
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (project-tier policy match), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_regexp_identity_match_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`identity_regexp` (anchored full-match) accepts the signer's SAN."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, sigstore_stack.issuer, identity_regexp="^ocx-test@.*$")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A matched policy pinning the wrong identity fails with `IdentityMismatch`."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, sigstore_stack.issuer, identity="someone-else@example.com")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 77, (
        f"expected exit 77 (PermissionDenied / IdentityMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_no_policy_no_flags_exits_64(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """No matching `[[trust.policy]]` anywhere, no flags -> `NoIdentityProvided`."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """The longest-literal-prefix scope wins, not "any match".

    A broad scope with the CORRECT identity and a narrower scope (covering
    the same package) with a WRONG identity: the narrower policy must be the
    ONLY one resolved, so its wrong identity fails the verify.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope_broad = f"{ocx.registry}/"
    scope_narrow = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope_broad, sigstore_stack.issuer, identity=sigstore_stack.identity) + _policy_block(
        scope_narrow, sigstore_stack.issuer, identity="someone-else@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 77, (
        f"expected exit 77 (narrower scope's wrong identity must win), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_any_of_equal_scope_rotation_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Two policies at the identical scope: ANY-of passes if either matches.

    Models key/workflow rotation — the old and new identity coexist during
    the overlap window and either one must pass.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(scope, sigstore_stack.issuer, identity="old-identity@example.com") + _policy_block(
        scope, sigstore_stack.issuer, identity=sigstore_stack.identity
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        f"expected exit 0 (ANY-of rotation match), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_tier_append_merge_pools_config_and_project(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Tiers array-append (union), never replace: config + project both pool.

    `config.toml` carries a policy for an UNRELATED scope (so it cannot
    match); the project `ocx.toml` carries the matching correct policy. Only
    pooling across both tiers explains a pass here.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    unrelated = _policy_block(
        f"{ocx.registry}/totally-unrelated-prefix", sigstore_stack.issuer, identity="nobody@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(unrelated)

    scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(scope, sigstore_stack.issuer, identity=sigstore_stack.identity))

    verify = _verify(
        ocx, sigstore_stack, pkg, extra_env={"OCX_PROJECT": str(project_toml)}
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (config + project tiers pooled), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_operator_config_authoritative_over_project(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
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
    _sign(ocx, sigstore_stack, identity_token, pkg)

    operator_scope = f"{ocx.registry}/{pkg.repo[:4]}*"
    operator_toml = _policy_block(operator_scope, sigstore_stack.issuer, identity="attacker@evil.test")
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(operator_toml)

    project_scope = f"{ocx.registry}/{pkg.repo}"
    project_toml = tmp_path / "ocx.toml"
    project_toml.write_text(_policy_block(project_scope, sigstore_stack.issuer, identity=sigstore_stack.identity))

    verify = _verify(
        ocx, sigstore_stack, pkg, extra_env={"OCX_PROJECT": str(project_toml)}
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A matched policy setting BOTH `identity` and `identity_regexp` is a config error.

    Only a policy that actually matches the target scope is validated
    (`TrustPolicy::compile`) — this policy's scope matches the package, so
    the conflict surfaces as `TrustPolicyInvalid` (exit 78), not silently
    ignored.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(
        scope, sigstore_stack.issuer, identity=sigstore_stack.identity, identity_regexp="^ocx-test@.*$"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
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
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`--certificate-identity` + `--certificate-oidc-issuer` override any policy.

    A config.toml policy for the package's scope pins the WRONG identity and
    issuer; passing the CORRECT pair as flags must still pass, proving flags
    take precedence over (and never consult) the policy pool.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    wrong_policy = _policy_block(
        scope, "https://wrong-issuer.example", identity="someone-else@example.com"
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(wrong_policy)

    verify = _verify(
        ocx,
        sigstore_stack,
        pkg,
        extra_env=_NO_PROJECT,
        cert_flags=[
            "--certificate-identity", sigstore_stack.identity,
            "--certificate-oidc-issuer", sigstore_stack.issuer,
        ],
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (flags override policy), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_single_flag_without_pair_exits_64(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """`--certificate-identity` alone (no issuer) is a clap usage error."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    verify = _verify(
        ocx,
        sigstore_stack,
        pkg,
        cert_flags=["--certificate-identity", sigstore_stack.identity],
    )
    assert verify.returncode == 64, (
        f"expected exit 64 (clap requires both-or-neither), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# ocx.toml preservation — the in-place editor must not touch [[trust.policy]]
# ──────────────────────────────────────────────────────────────────────────────


def test_add_then_remove_preserves_trust_policy_section(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """`ocx add` / `ocx remove` preserve a hand-written `[[trust.policy]]`
    section byte-for-byte, comment included.

    `render_preserving` (commit eff2205a) rewrote `ocx.toml` mutation from a
    whole-struct re-serialize to an in-place edit of only the mutated table;
    this is the sibling regression net to
    ``test_project_toml_preservation.py`` for the one table that test module
    never touches.
    """
    scope = f"{ocx.registry}/{published_package.repo}"
    # Literals, not the stack's values: nothing here signs or verifies, so the
    # test must not depend on Docker being up to prove a TOML editor preserves
    # a table it does not own.
    trust_section = (
        "[[trust.policy]]\n"
        f'scope = "{scope}"\n'
        "# pinned deliberately, do not relax\n"
        "signers = [\n"
        '  { kind = "keyless", identity = "release-bot@example.com", '
        'oidc_issuer = "https://accounts.example.com" },\n'
        "]\n"
    )
    body = f"[tools]\n\n{trust_section}"
    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    (project_dir / "ocx.toml").write_text(body)

    added = make_package(
        ocx, f"t_{uuid4().hex[:8]}_trustadd", "1.0.0", tmp_path, cascade=False
    )

    add_result = subprocess.run(
        [str(ocx.binary), "add", added.fq],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert add_result.returncode == 0, (
        f"ocx add failed: rc={add_result.returncode}, stderr={add_result.stderr!r}"
    )
    after_add = (project_dir / "ocx.toml").read_text()
    assert trust_section in after_add, (
        f"[[trust.policy]] must survive ocx add byte-for-byte; got:\n{after_add}"
    )

    remove_result = subprocess.run(
        [str(ocx.binary), "remove", added.repo],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert remove_result.returncode == 0, (
        f"ocx remove failed: rc={remove_result.returncode}, stderr={remove_result.stderr!r}"
    )
    after_remove = (project_dir / "ocx.toml").read_text()
    assert trust_section in after_remove, (
        f"[[trust.policy]] must survive ocx remove byte-for-byte; got:\n{after_remove}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Builder pin (#103) — end-to-end through a policy-driven verify --attestation
# ──────────────────────────────────────────────────────────────────────────────

#: A minimal SLSA v1 provenance predicate carrying a builder id. Predicates
#: are read as raw, well-formed JSON with no further schema enforcement, so
#: this is sufficient to exercise `dsse.rs`'s `enforce_builder_pin`.
_V1_BUILDER_PROVENANCE = (
    '{"runDetails":{"builder":{"id":"https://ci.example/builder@v1"}}}'
)


def test_builder_pin_matching_builder_passes(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A `[[trust.policy]]` `builder` pin matching the attested provenance's
    builder id lets `verify --attestation` pass (exit 0).

    Mirrors the Rust unit `builder_pin_accepts_the_builder_it_pins`
    (`dsse.rs`) end-to-end through the real config-driven `[[trust.policy]]`
    resolution path this file otherwise pins -- policy mode only, per
    `_verify`'s docstring above.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    predicate = tmp_path / "provenance.json"
    predicate.write_text(_V1_BUILDER_PROVENANCE)
    _attest(ocx, sigstore_stack, identity_token, pkg, predicate, "slsaprovenance1")

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(
        scope, sigstore_stack.issuer,
        identity=sigstore_stack.identity,
        builder="https://ci.example/builder@v1",
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(
        ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT,
        attestation=True, predicate_type="slsaprovenance1",
    )
    assert verify.returncode == 0, (
        f"expected exit 0 (builder pin matches), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )


def test_builder_pin_different_builder_refuses(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """A `builder` pin naming a DIFFERENT builder than the attested provenance
    refuses with `error.detail == "builder_mismatch"`, not just a nonzero exit.

    Mirrors the Rust unit `builder_pin_refuses_a_different_builder`
    (`dsse.rs`). The slug is asserted because 65 (DataError) is shared by
    many `VerifyErrorKind` variants -- the whole point of the pin is that the
    refusal names the RIGHT reason.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)
    predicate = tmp_path / "provenance.json"
    predicate.write_text(_V1_BUILDER_PROVENANCE)
    _attest(ocx, sigstore_stack, identity_token, pkg, predicate, "slsaprovenance1")

    scope = f"{ocx.registry}/{pkg.repo}"
    policy_toml = _policy_block(
        scope, sigstore_stack.issuer,
        identity=sigstore_stack.identity,
        builder="https://evil.example/other-builder",
    )
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(policy_toml)

    verify = _verify(
        ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT,
        attestation=True, predicate_type="slsaprovenance1", json_format=True,
    )
    assert verify.returncode == 65, (
        f"expected exit 65 (DataError / BuilderMismatch), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    envelope = json.loads(verify.stdout)
    assert envelope["error"]["detail"] == "builder_mismatch", (
        f"a builder-mismatch refusal must name its own slug: {envelope['error']}"
    )
