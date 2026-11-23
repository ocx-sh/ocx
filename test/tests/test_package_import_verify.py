# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for auto-verify during ``ocx package import``.

A signed package is exported with its referrers and imported into an empty home
whose ``[[trust.policy]]`` covers it, with every registry request refused.
Verification is satisfied from the exported referrers; an export made with
``--no-referrers`` carries none, so import fails. The home pins the stack's
Rekor key through ``OCX_SIGSTORE_TRUSTED_ROOT``: offline verify cannot fetch one.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from src.export_dir import read_marker
from src.registry_guard import RegistryGuard
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack

pytestmark = pytest.mark.command("package_export", "package_import")

BAD_IDENTITY = "someone-else@example.com"


def _run(
    runner: OcxRunner, *args: str, extra_env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(runner.binary), "--format", "json", *args],
        capture_output=True,
        text=True,
        env={**runner.env, **(extra_env or {})},
        check=False,
    )


def _signed_export(
    ocx: OcxRunner,
    stack: SigstoreStack,
    token: Path,
    pkg: PackageInfo,
    out: Path,
    *flags: str,
) -> None:
    signed = subprocess.run(
        [str(ocx.binary), "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert signed.returncode == 0, f"sign setup failed: {signed.stderr}"
    exported = _run(
        ocx,
        "package",
        "export",
        "-o",
        str(out),
        "-p",
        current_platform(),
        *flags,
        pkg.short,
    )
    assert exported.returncode == 0, f"export setup failed: {exported.stderr}"
    # The directory must really hold the package, or a failing import below proves nothing about referrers.
    assert read_marker(out), "export wrote no marker"


def _write_policy(home: OcxRunner, scope: str, identity: str, issuer: str) -> None:
    (home.ocx_home / "config.toml").write_text(
        f'[[trust.policy]]\nscope = "{scope}"\n'
        f'signers = [{{ kind = "keyless", identity = "{identity}", oidc_issuer = "{issuer}" }}]\n',
        encoding="utf-8",
    )


def _import(
    home: OcxRunner, stack: SigstoreStack, directory: Path
) -> subprocess.CompletedProcess[str]:
    return _run(
        home,
        "package",
        "import",
        "-p",
        current_platform(),
        str(directory),
        extra_env={"OCX_SIGSTORE_TRUSTED_ROOT": str(stack.trusted_root_json)},
    )


def test_import_verifies_from_carried_referrers(
    ocx: OcxRunner,
    second_home: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    registry_guard: RegistryGuard,
    tmp_path: Path,
) -> None:
    """Referrers in the export satisfy the policy offline; without them import exits 79."""
    pkg = published_package
    scope = f"{ocx.registry}/{pkg.repo}"
    with_referrers = tmp_path / "with-referrers"
    without_referrers = tmp_path / "no-referrers"
    _signed_export(ocx, sigstore_stack, identity_token, pkg, with_referrers)
    exported = _run(
        ocx,
        "package",
        "export",
        "-o",
        str(without_referrers),
        "-p",
        current_platform(),
        "--no-referrers",
        pkg.short,
    )
    assert exported.returncode == 0, (
        f"--no-referrers export setup failed: {exported.stderr}"
    )

    _write_policy(second_home, scope, sigstore_stack.identity, sigstore_stack.issuer)
    registry_guard.deny(second_home)

    ok = _import(second_home, sigstore_stack, with_referrers)
    assert ok.returncode == 0, (
        f"import must verify from carried referrers, got {ok.returncode}\nstderr: {ok.stderr.strip()}"
    )
    assert registry_guard.count == 0, (
        f"import dialled the registry: {registry_guard.requests}"
    )

    # Fresh empty home so the second import cannot lean on the first one's state.
    bare = tmp_path / "bare-home"
    bare.mkdir()
    refused_home = OcxRunner(ocx.binary, bare, ocx.registry)
    _write_policy(refused_home, scope, sigstore_stack.identity, sigstore_stack.issuer)
    registry_guard.deny(refused_home)
    registry_guard.reset()

    refused = _import(refused_home, sigstore_stack, without_referrers)
    assert refused.returncode == 79, (
        f"got {refused.returncode}\nstdout: {refused.stdout}\nstderr: {refused.stderr}"
    )
    assert json.loads(refused.stdout)["error"]["detail"] == "no_signatures_found", (
        refused.stdout
    )
    assert "not exported" in refused.stdout + refused.stderr, (
        "error must say the referrers were not exported"
    )
    assert registry_guard.count == 0, (
        f"refused import dialled the registry: {registry_guard.requests}"
    )


def test_import_enforces_policy_identity_from_carried_referrers(
    ocx: OcxRunner,
    second_home: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    registry_guard: RegistryGuard,
    tmp_path: Path,
) -> None:
    """Control: the carried signature is checked, not skipped; a policy naming another signer exits 77."""
    pkg = published_package
    out = tmp_path / "export"
    _signed_export(ocx, sigstore_stack, identity_token, pkg, out)
    _write_policy(
        second_home, f"{ocx.registry}/{pkg.repo}", BAD_IDENTITY, sigstore_stack.issuer
    )
    registry_guard.deny(second_home)

    result = _import(second_home, sigstore_stack, out)
    assert result.returncode == 77, (
        f"got {result.returncode}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert registry_guard.count == 0, (
        f"import dialled the registry: {registry_guard.requests}"
    )
