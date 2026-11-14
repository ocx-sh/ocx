"""Acceptance tests: switching the selected ocx to a release candidate and back.

A package published as ``<repo>:0.9.0`` and ``<repo>:0.9.0-rc1`` stands in for
``ocx.sh/ocx/cli``. The launcher reached through the per-repo ``current``
symlink must run whichever version was selected last.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner, PackageInfo, registry_dir

EXIT_NOT_FOUND = 79  # ocx_exit::ExitCode::NotFound

pytestmark = [
    pytest.mark.command("select", "install"),
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX launcher invocation"),
]


def _launcher_output(ocx: OcxRunner, pkg: PackageInfo) -> str:
    """Run the entrypoint launcher reached through ``current`` and return stdout."""
    launcher = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
        / "entrypoints"
        / "hello"
    )
    env = dict(ocx.env)
    env["PATH"] = f"{ocx.binary.parent}{os.pathsep}{env.get('PATH', '')}"
    result = subprocess.run(
        [str(launcher)], capture_output=True, text=True, env=env, timeout=30, check=True
    )
    return result.stdout


def _publish_pair(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    stable = make_package_with_entrypoints(
        ocx, repo, tmp_path, entrypoints=["hello"], tag="0.9.0", file_prefix="stable"
    )
    rc = make_package_with_entrypoints(
        ocx, repo, tmp_path, entrypoints=["hello"], tag="0.9.0-rc1", file_prefix="rc"
    )
    return stable, rc


def test_select_switches_the_launcher_to_the_candidate_and_back(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    stable, rc = _publish_pair(ocx, unique_repo, tmp_path)
    assert stable.marker != rc.marker

    ocx.plain("package", "install", "--select", stable.short)
    assert stable.marker in _launcher_output(ocx, stable)

    # Installing the candidate alone must not move the selected version.
    ocx.plain("package", "install", rc.short)
    before = _launcher_output(ocx, stable)
    assert stable.marker in before and rc.marker not in before

    ocx.plain("package", "select", rc.short)
    during = _launcher_output(ocx, stable)
    assert rc.marker in during and stable.marker not in during

    ocx.plain("package", "select", stable.short)
    after = _launcher_output(ocx, stable)
    assert stable.marker in after and rc.marker not in after


def test_select_of_an_uninstalled_version_fails_and_keeps_the_selection(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    stable, rc = _publish_pair(ocx, unique_repo, tmp_path)
    ocx.plain("package", "install", "--select", stable.short)

    result = ocx.run("package", "select", rc.short, check=False)

    assert result.returncode == EXIT_NOT_FOUND, result.stderr
    assert stable.marker in _launcher_output(ocx, stable)
