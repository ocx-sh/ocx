"""Acceptance tests — Group 3.9: Cross-platform launcher smoke tests.

Tests are written from the ADR and plan contract. Platform-conditional via
pytest.mark.skipif. Windows / MSYS2 tests are xfail-documented per plan §3.9.
"""

from __future__ import annotations

import os
import stat
import sys
from pathlib import Path

import pytest

from src.helpers import make_package_with_entry_points
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_pkg(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    entry_points: list[dict],
    bins: list[str] | None = None,
    tag: str = "1.0.0",
) -> PackageInfo:
    """Cross-platform-flavored wrapper that uses a distinct file prefix."""
    return make_package_with_entry_points(
        ocx, unique_repo, tmp_path, entry_points,
        bins=bins, tag=tag, file_prefix="cp",
    )


def get_entrypoints_dir(ocx: OcxRunner, pkg: PackageInfo) -> Path | None:
    """Return the launcher directory reached via the per-repo `current` anchor.

    Post-flatten layout: there is no separate `entrypoints-current` symlink;
    `current` targets the package root and `current/entrypoints` is the
    publish anchor for launchers.
    """
    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    if not (current.exists() or current.is_symlink()):
        return None
    resolved = Path(os.path.realpath(str(current)))
    ep = resolved / "entrypoints"
    return ep if ep.is_dir() else None


# ---------------------------------------------------------------------------
# 3.9 Linux: .sh launcher exists + mode 0o100755
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "linux", reason="Linux-specific launcher test")
def test_linux_unix_launcher_exists_and_is_executable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Linux: Unix launcher file exists and has mode 0755 after install --select."""
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail(
            "current/entrypoints/ not reachable after install --select; "
            "expected the package root to expose an entrypoints/ child."
        )

    launcher = ep_dir / "hello"
    assert launcher.exists(), f"Unix launcher must exist: {launcher}"

    mode = launcher.stat().st_mode
    assert mode & stat.S_IXUSR, f"launcher must be user-executable, mode={oct(mode)}"
    assert mode & stat.S_IXGRP, f"launcher must be group-executable, mode={oct(mode)}"
    assert mode & stat.S_IXOTH, f"launcher must be other-executable, mode={oct(mode)}"

    # Confirm it's a shell script with the correct shebang.
    content = launcher.read_text()
    assert content.startswith("#!/bin/sh"), f"launcher must have #!/bin/sh shebang: {content!r}"


@pytest.mark.skipif(sys.platform != "linux", reason="Linux-specific launcher test")
def test_linux_windows_cmd_launcher_also_exists(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Linux: .cmd launcher is generated alongside the Unix launcher (cross-platform installs).

    ADR §6: Both templates always produced on both platforms. Cross-platform installs
    (e.g., Windows targets mounting a Linux-installed package) are a documented use case.
    """
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/entrypoints/ not reachable after install --select")

    cmd_launcher = ep_dir / "hello.cmd"
    assert cmd_launcher.exists(), f"Windows .cmd launcher must also exist on Linux: {cmd_launcher}"

    content = cmd_launcher.read_text()
    assert "@ECHO off" in content or "@echo off" in content, (
        f".cmd launcher must contain @ECHO off: {content!r}"
    )


# ---------------------------------------------------------------------------
# 3.9 macOS: .sh launcher exists
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS-specific launcher test")
def test_macos_unix_launcher_exists(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """macOS: Unix launcher exists after install --select."""
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/entrypoints/ not reachable after install --select")

    launcher = ep_dir / "hello"
    assert launcher.exists(), f"macOS launcher must exist: {launcher}"


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS path-with-spaces test")
def test_macos_path_with_spaces_in_ocx_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """macOS: launcher works when OCX_HOME path contains spaces.

    ADR §5 notes baked absolute paths must handle spaces safely.
    """
    # Create a temp dir whose path contains a space.
    spaced_home = tmp_path / "ocx home with spaces"
    spaced_home.mkdir(parents=True)

    from src.runner import OcxRunner as _OcxRunner  # noqa: PLC0415
    spaced_ocx = _OcxRunner(ocx.binary, spaced_home, ocx.registry)

    pkg = _make_pkg(
        spaced_ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    # ADR §5: baked absolute paths must handle spaces safely (single-quote on Unix).
    result = spaced_ocx.run("install", "--select", pkg.short, check=False)
    assert result.returncode == 0, (
        f"install --select must succeed with spaces in OCX_HOME; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# 3.9 Windows: .cmd exists (xfail-documented — CI runs Linux)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only launcher test")
def test_windows_cmd_launcher_exists(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Windows: .cmd launcher exists after install --select.

    This test only runs on Windows CI runners. For Linux CI, see
    test_linux_windows_cmd_launcher_also_exists above (both launchers generated
    on all platforms per ADR §6).
    """
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    assert current.exists() or current.is_symlink(), (
        f"current symlink must exist on Windows: {current}"
    )
    cmd_launcher = Path(os.path.realpath(str(current))) / "entrypoints" / "hello.cmd"
    assert cmd_launcher.exists(), f".cmd launcher must exist on Windows: {cmd_launcher}"


# ---------------------------------------------------------------------------
# 3.9 MSYS2 / Git Bash (xfail-documented — requires special CI setup)
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    reason=(
        "MSYS2 Git Bash smoke test requires a dedicated Windows CI runner "
        "with Git Bash + MSYS2 installed. "
        "Per ADR §3: Git Bash invokes .cmd via cmd.exe binfmt bridge. "
        "This test documents the expected behavior but cannot run in standard CI."
    ),
    strict=False,
)
def test_msys2_git_bash_invokes_cmd_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """MSYS2 Git Bash: `hello` resolves to .cmd launcher via cmd.exe interop.

    ADR Tension 3 Option A: Git Bash users invoke .cmd through MSYS2's binfmt
    bridge; path translation converts the baked Windows path seamlessly.
    This test is xfail-documented until a MSYS2 CI job is set up.
    """
    import subprocess  # noqa: PLC0415
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entry_points=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    hello_launcher = current / "entrypoints" / "hello"

    # Invoke the launcher via Git Bash.
    result = subprocess.run(
        ["bash", "-c", str(hello_launcher)],
        capture_output=True, text=True, env=ocx.env,
    )
    assert result.returncode == 0, f"MSYS2 launcher invocation failed: {result.stderr}"
