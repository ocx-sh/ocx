"""Tests for the removed ``ocx ci`` command (handshake §6 / plan C4).

``ocx ci`` is REMOVED. CI export is a deferred extension point, not a command.
All tests assert that ``ocx ci ...`` exits non-zero with a clap
unrecognised-subcommand error (exit 64, UsageError).

The old behavioural tests (GitHub Actions file writes, auto-detect, path
accumulation) are REWRITTEN here to assert the new surface contract:
the ``ci`` subcommand does not exist.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src.runner import OcxRunner, PackageInfo

# Exit code: ocx maps clap parse errors (unrecognised subcommand) to UsageError
EXIT_USAGE = 64


def _run_ci(ocx: OcxRunner, tmp_path: Path, *extra_args: str) -> subprocess.CompletedProcess[str]:
    """Run ``ocx ci <extra_args>`` and return the result without checking exit code."""
    cmd = [str(ocx.binary), "ci", *extra_args]
    return subprocess.run(
        cmd,
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def test_ci_command_does_not_exist(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx ci`` → clap unrecognised-subcommand error, exit 64.

    The ``ci`` subcommand was removed per handshake §6.
    CI export is a deferred extension point, not a live command.
    """
    result = _run_ci(ocx, tmp_path, "export", "--flavor", "github-actions", published_package.short)
    assert result.returncode != 0, (
        f"ocx ci must fail (removed); got exit 0\nstdout:\n{result.stdout}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci must exit {EXIT_USAGE} (UsageError / clap unrecognised subcommand); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "ci" in result.stderr.lower(), (
        f"expected clap unrecognized-subcommand stderr; got:\n{result.stderr}"
    )


def test_ci_export_github_actions_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx ci export --flavor github-actions`` → exit 64 (removed).

    Previously tested GitHub Actions file writes. Now asserts the surface
    is gone per handshake §6 deferred scope.
    """
    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"
    env["GITHUB_PATH"] = str(github_path)
    env["GITHUB_ENV"] = str(github_env)

    result = subprocess.run(
        [str(ocx.binary), "ci", "export", "--flavor", "github-actions", published_package.short],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci export must exit {EXIT_USAGE} (removed); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_ci_export_auto_detect_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx ci export`` (auto-detect) → exit 64 (removed)."""
    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"

    result = subprocess.run(
        [str(ocx.binary), "ci", "export", published_package.short],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci export must exit {EXIT_USAGE} (removed); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_ci_export_no_ci_env_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx ci export`` with no CI env → exit 64 (removed).

    Previously tested that this would fail with 'could not detect CI environment'.
    Now both the no-env case and all other ci invocations fail with exit 64
    because the ``ci`` subcommand does not exist.
    """
    env = dict(ocx.env)
    env.pop("GITHUB_ACTIONS", None)

    result = subprocess.run(
        [str(ocx.binary), "ci", "export", published_package.short],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, (
        "ocx ci must fail with non-zero exit (removed)"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci must exit {EXIT_USAGE} (removed); got {result.returncode}"
    )


def test_ci_export_path_accumulation_removed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx ci export`` (multi-package) → exit 64 (removed).

    Previously tested that two packages with overlapping LD_LIBRARY_PATH
    produced a single accumulated line. Now asserts the surface is gone.
    """
    result = subprocess.run(
        [str(ocx.binary), "ci", "export", "--flavor", "github-actions",
         f"{unique_repo}a:1.0.0", f"{unique_repo}b:1.0.0"],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci export must exit {EXIT_USAGE} (removed); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_ci_export_missing_github_path_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``ocx ci export --flavor github-actions`` without GITHUB_PATH → exit 64 (removed).

    Previously tested that missing GITHUB_PATH caused a specific error.
    Now the entire subcommand is gone.
    """
    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"
    env.pop("GITHUB_PATH", None)
    env["GITHUB_ENV"] = str(tmp_path / "github_env")

    result = subprocess.run(
        [str(ocx.binary), "ci", "export", "--flavor", "github-actions", published_package.short],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci export must exit {EXIT_USAGE} (removed); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
