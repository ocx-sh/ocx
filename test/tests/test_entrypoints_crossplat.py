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

from src.helpers import make_package_with_entrypoints
from src.launcher_fixtures import only_generation
from src.runner import OcxRunner, PackageInfo

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_pkg(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    entrypoints: list[dict],
    bins: list[str] | None = None,
    tag: str = "1.0.0",
) -> PackageInfo:
    """Cross-platform-flavored wrapper that uses a distinct file prefix."""
    return make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints,
        bins=bins, tag=tag, file_prefix="cp",
    )


def get_entrypoints_dir(ocx: OcxRunner, pkg: PackageInfo) -> Path | None:
    """Return the single `<G>` launcher directory reached via the per-repo `current` anchor.

    `current` targets the package root and `current/launchers/<G>` holds the
    home launchers (`NAME[.exe]` + `NAME.launch`); `None` when `current` or
    `launchers/` is absent.
    """
    from src.runner import registry_dir
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    if not (current.exists() or current.is_symlink()):
        return None
    launchers = Path(os.path.realpath(str(current))) / "launchers"
    return only_generation(launchers) if launchers.is_dir() else None


# ---------------------------------------------------------------------------
# 3.9 Linux: native launcher exists + mode 0o100755
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "linux", reason="Linux-specific launcher test")
def test_linux_unix_launcher_exists_and_is_executable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-128: Linux home launcher exists, is mode 0755 and is a native program, not a script."""
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail(
            "current/launchers/<G> not reachable after install --select; "
            "expected the package root to expose a launchers/ child."
        )

    launcher = ep_dir / "hello"
    assert launcher.exists(), f"Unix launcher must exist: {launcher}"

    mode = launcher.stat().st_mode
    assert mode & stat.S_IXUSR, f"launcher must be user-executable, mode={oct(mode)}"
    assert mode & stat.S_IXGRP, f"launcher must be group-executable, mode={oct(mode)}"
    assert mode & stat.S_IXOTH, f"launcher must be other-executable, mode={oct(mode)}"

    head = launcher.read_bytes()[:4]
    assert head == b"\x7fELF", f"launcher must be a native ELF program, not a script: {head!r}"
    assert launcher.stat().st_nlink == 1, "a home launcher is a byte copy, link count 1"


@pytest.mark.skipif(sys.platform != "linux", reason="Linux-specific launcher test")
def test_linux_install_writes_only_launcher_and_record(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """V-L38 writer half: a launcher directory holds `NAME` + `NAME.launch`, no `.cmd`/`.exe`/`.shim`."""
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/launchers/<G> not reachable after install --select")

    assert sorted(p.name for p in ep_dir.iterdir()) == ["hello", "hello.launch"], (
        "only the launcher and its record: no `.cmd`, `.exe`, `.shim` or `.exec`"
    )


@pytest.mark.skipif(sys.platform != "linux", reason="Linux-specific launcher test")
def test_linux_every_launcher_of_a_package_is_the_same_bytes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-128: every launcher of a package is a byte copy of one embedded blob, link count 1."""
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello", "other"],
        bins=["hello", "other"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/launchers/<G> not reachable after install --select")

    first, second = (ep_dir / "hello"), (ep_dir / "other")
    assert first.read_bytes() == second.read_bytes(), "both launchers are copies of the one blob"
    assert first.stat().st_nlink == 1 and second.stat().st_nlink == 1, (
        "copies, not hardlinks: each has link count 1"
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
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/launchers/<G> not reachable after install --select")

    launcher = ep_dir / "hello"
    assert launcher.exists(), f"macOS launcher must exist: {launcher}"
    assert (ep_dir / "hello.launch").is_file(), "the launcher's record sits beside it"


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

    from src.runner import OcxRunner as _OcxRunner
    spaced_ocx = _OcxRunner(ocx.binary, spaced_home, ocx.registry)

    pkg = _make_pkg(
        spaced_ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    # ADR §5: baked absolute paths must handle spaces safely (single-quote on Unix).
    result = spaced_ocx.run("package", "install", "--select", pkg.short, check=False)
    assert result.returncode == 0, (
        f"install --select must succeed with spaces in OCX_HOME; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )


# ---------------------------------------------------------------------------
# 3.9 Windows: native `.exe` launcher + `.launch` record, nothing else
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only launcher test")
def test_windows_native_launcher_exists_no_script_no_cmd(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """V-L38, C-128: Windows holds `<name>.exe` + `<name>.launch` after install --select, nothing else.

    No extensionless script, `.cmd`, `.shim`, `.shimref` or `.exec`; the `.exe` is
    a copy of the embedded blob with link count 1. This test only runs on Windows
    CI runners; the Linux side is pinned above.
    """
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    assert ep_dir is not None, "current/launchers/<G> must exist on Windows"
    assert sorted(p.name for p in ep_dir.iterdir()) == ["hello.exe", "hello.launch"], (
        f"a Windows home holds NAME.exe and NAME.launch only: {sorted(p.name for p in ep_dir.iterdir())}"
    )
    assert (ep_dir / "hello.exe").stat().st_nlink == 1, "a home launcher is a byte copy, link count 1"
