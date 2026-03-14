"""Tests for ``ocx ci export``."""
from __future__ import annotations

from src.runner import OcxRunner, PackageInfo


def test_ci_export_github_actions(ocx: OcxRunner, published_package: PackageInfo, tmp_path) -> None:
    """``ocx ci export --flavor github-actions`` writes to GITHUB_PATH and GITHUB_ENV files."""
    ocx.plain("install", published_package.short)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    ocx.env["GITHUB_ACTIONS"] = "true"
    ocx.env["GITHUB_PATH"] = str(github_path)
    ocx.env["GITHUB_ENV"] = str(github_env)

    ocx.plain("ci", "export", "--flavor", "github-actions", published_package.short)

    path_content = github_path.read_text()
    env_content = github_env.read_text()

    # The test package declares PATH (path type) and HELLO_HOME (constant type).
    # PATH should be appended to GITHUB_PATH file, HELLO_HOME to GITHUB_ENV file.
    assert "bin" in path_content, f"PATH export should contain 'bin': {path_content}"
    assert "HELLO_HOME=" in env_content, f"HELLO_HOME constant should be exported: {env_content}"


def test_ci_export_auto_detect(ocx: OcxRunner, published_package: PackageInfo, tmp_path) -> None:
    """Auto-detection works when GITHUB_ACTIONS=true is set."""
    ocx.plain("install", published_package.short)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    ocx.env["GITHUB_ACTIONS"] = "true"
    ocx.env["GITHUB_PATH"] = str(github_path)
    ocx.env["GITHUB_ENV"] = str(github_env)

    ocx.plain("ci", "export", published_package.short)

    # Should have written to at least one of the files
    path_content = github_path.read_text()
    env_content = github_env.read_text()
    assert path_content or env_content, "Expected output in GITHUB_PATH or GITHUB_ENV"


def test_ci_export_no_ci_env_fails(ocx: OcxRunner, published_package: PackageInfo) -> None:
    """Error when no --flavor and no CI environment detected."""
    ocx.plain("install", published_package.short)

    # Ensure no CI env vars are set
    ocx.env.pop("GITHUB_ACTIONS", None)

    result = ocx.plain("ci", "export", published_package.short, check=False)
    assert result.returncode != 0
    assert "Could not detect CI environment" in result.stderr


def test_ci_export_missing_github_path_fails(ocx: OcxRunner, published_package: PackageInfo, tmp_path) -> None:
    """Error when GITHUB_PATH env var is not set."""
    ocx.plain("install", published_package.short)

    github_env = tmp_path / "github_env"
    github_env.write_text("")

    ocx.env["GITHUB_ACTIONS"] = "true"
    ocx.env.pop("GITHUB_PATH", None)
    ocx.env["GITHUB_ENV"] = str(github_env)

    result = ocx.plain("ci", "export", "--flavor", "github-actions", published_package.short, check=False)
    assert result.returncode != 0
    assert "GITHUB_PATH" in result.stderr
