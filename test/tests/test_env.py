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
from pathlib import Path

from src import OcxRunner, PackageInfo, registry_dir
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest

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


# ---------------------------------------------------------------------------
# adr_declared_binaries_metadata.md §4 — `binaries`/`entrypoints` JSON arrays
# ---------------------------------------------------------------------------


def test_package_env_json_binaries_array_has_package_attribution(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package env`'s JSON `binaries` array names the admitted package
    that declared each claim (`package` = the admitted `PinnedIdentifier`'s
    string form, ADR §4 Decision A)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    assert env_result["binaries"] == [{"name": "hello", "package": f"{pkg.fq}@{digest}"}], (
        env_result["binaries"]
    )
    assert env_result["entrypoints"] == [], env_result["entrypoints"]


def test_package_env_json_binaries_entrypoints_present_but_empty_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`binaries`/`entrypoints` are always present as arrays — possibly
    empty — even for a package that declares neither (ADR §4).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/` — Auto mode (the default) would
    otherwise fill it, which is exactly the behavior under test elsewhere.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    assert env_result["binaries"] == []
    assert env_result["entrypoints"] == []


def test_package_env_shell_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--shell` stays the eval-safe channel only — a declared `binaries`
    claim never leaks into it (ADR §4: both sinks return before `EnvVars`
    is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short, "--shell=bash")

    assert result.returncode == 0, result.stderr
    assert "export" in result.stdout, result.stdout
    assert "binaries" not in result.stdout.lower(), result.stdout
    assert "hello" not in result.stdout, result.stdout


def test_package_env_ci_github_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--ci=github`` stays a CI persistence sink only — a declared `binaries`
    claim never leaks into the ``$GITHUB_ENV`` / ``$GITHUB_PATH`` sink files
    (ADR §4: both sinks return before `EnvVars` is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    result = ocx.plain(
        "package",
        "env",
        pkg.short,
        "--ci=github",
        env_overrides={
            "GITHUB_ACTIONS": "true",
            "GITHUB_PATH": str(github_path),
            "GITHUB_ENV": str(github_env),
        },
    )

    assert result.returncode == 0, result.stderr
    sink_text = github_path.read_text() + github_env.read_text()
    assert "binaries" not in sink_text.lower(), sink_text
    assert "hello" not in sink_text, sink_text


def test_package_env_plain_shows_hint_when_binaries_present(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Decision C: the plain `entries` table stays byte-stable; a hint line
    below it announces binary/entrypoint availability when the admitted set
    carries any claims."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "hello" in result.stdout, result.stdout
    assert "available" in result.stdout.lower(), result.stdout


def test_package_env_plain_omits_hint_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No hint line renders when the admitted set carries no binaries or
    entrypoints (the common case, unaffected by this feature).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "available" not in result.stdout.lower(), result.stdout
