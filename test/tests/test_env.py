"""Tests for OCI-tier per-package env (``ocx package env``).

Rewritten Phase 5 (plan_toolchain_cli.md):
- ``ocx env <pkg>`` → ``ocx package env <pkg>`` (OCI-tier, C3 contract).
- ``ocx shell env <pkg>`` (deleted) → rewritten to assert exit 64.

Note (W5): ``ocx package env`` auto-installs missing packages via
``find_or_install_all`` (deliberate, handshake §2 accepted this cut).
Do NOT assert old 'shell env no-download' semantics against ``package env``.
"""
from __future__ import annotations

import subprocess

from src import OcxRunner, PackageInfo, registry_dir

# Exit code for deleted commands
EXIT_USAGE = 64


def test_env_path_contains_bin(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> → PATH includes /bin"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)
    path_entry = next(e for e in env_result["entries"] if e["key"] == "PATH")
    assert "/bin" in path_entry["value"] or "\\bin" in path_entry["value"]


def test_env_constant_contains_content_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> — constant var points to content dir"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert registry_dir(ocx.registry) in home_entry["value"]
    # CAS layout: packages/{registry}/sha256/{prefix}/{suffix}/content
    assert "packages" in home_entry["value"]


def test_env_candidate_uses_symlink_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env --candidate <pkg>"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", "--candidate", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert f"candidates/{pkg.tag}" in home_entry["value"] or f"candidates\\{pkg.tag}" in home_entry["value"]


def test_shell_env_removed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """``ocx shell env <pkg>`` → exit 64 (deleted command, plan C4).

    The ``ocx shell env`` command is removed. Per-package env is now
    ``ocx package env``; sourceable form uses ``--shell[=NAME]``.
    """
    pkg = published_package
    result = subprocess.run(
        [str(ocx.binary), "shell", "env", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell env must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
