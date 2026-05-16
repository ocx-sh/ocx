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
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

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
def test_linux_install_emits_no_cmd_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Linux: only the Unix `<name>` launcher is generated — no `.cmd`.

    Post-cutover (`adr_windows_exe_shim.md` Axis C → C2) no `.cmd` is emitted
    on any platform; the Windows `.exe`/`.shim` pair is cfg-gated to Windows
    targets only. A Linux install therefore yields exactly `<name>` and
    nothing else in `entrypoints/`.
    """
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/entrypoints/ not reachable after install --select")

    assert (ep_dir / "hello").exists(), "Unix launcher must exist"
    assert not (ep_dir / "hello.cmd").exists(), (
        "no `.cmd` launcher may be generated (cutover to `.exe`-only)"
    )
    assert not (ep_dir / "hello.exe").exists(), (
        "no `.exe` shim off Windows (emission is cfg-gated to Windows targets)"
    )
<<<<<<< HEAD
    assert not (ep_dir / "hello.shim").exists(), (
        "no `.shim` sidecar off Windows (emission is cfg-gated to Windows targets)"
=======
    ocx.plain("package", "install", "--select", pkg.short)

    ep_dir = get_entrypoints_dir(ocx, pkg)
    if ep_dir is None:
        pytest.fail("current/entrypoints/ not reachable after install --select")

    cmd_launcher = ep_dir / "hello.cmd"
    assert cmd_launcher.exists(), f".cmd launcher must exist: {cmd_launcher}"
    content = cmd_launcher.read_text()
    assert "EXIT /B %ERRORLEVEL%" in content, (
        f".cmd launcher must terminate with `EXIT /B %ERRORLEVEL%` to propagate "
        f"inner-ocx exit code (no cmd.exe exec equivalent); got:\n{content!r}"
    )
    last_nonempty = next(
        (line for line in reversed(content.splitlines()) if line.strip()),
        "",
    )
    assert last_nonempty.strip() == "EXIT /B %ERRORLEVEL%", (
        f".cmd launcher's final non-empty line must be `EXIT /B %ERRORLEVEL%` "
        f"(nothing must run after it that could overwrite ERRORLEVEL); "
        f"last non-empty line was {last_nonempty!r}"
>>>>>>> 9b296687 (feat(cli)!: toolchain CLI taxonomy + global activation via env exporter)
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
# 3.9 Windows: native `.exe` shim + `.shim` sidecar, no `.cmd`
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only launcher test")
def test_windows_native_shim_launcher_exists_no_cmd(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Windows: `<name>.exe` + `<name>.shim` exist after install --select; no `.cmd`.

    Post-cutover (`adr_windows_exe_shim.md` Axis C → C2) the Windows launcher
    is the native `.exe` shim plus its `.shim` sidecar — no `.cmd` is emitted.
    This test only runs on Windows CI runners; the Linux side is pinned by
    test_linux_install_emits_no_cmd_launcher above.
    """
    pkg = _make_pkg(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    assert current.exists() or current.is_symlink(), (
        f"current symlink must exist on Windows: {current}"
    )
    ep_dir = Path(os.path.realpath(str(current))) / "entrypoints"
    assert (ep_dir / "hello.exe").is_file(), (
        f"native `.exe` shim must exist on Windows: {ep_dir / 'hello.exe'}"
    )
<<<<<<< HEAD
    assert (ep_dir / "hello.shim").is_file(), (
        f"`.shim` sidecar must exist on Windows: {ep_dir / 'hello.shim'}"
    )
    assert not (ep_dir / "hello.cmd").exists(), (
        f"no `.cmd` launcher may exist (cutover to `.exe`-only): {ep_dir / 'hello.cmd'}"
=======
    ocx.plain("package", "install", "--select", pkg.short)

    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    current = Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo / "current"
    hello_launcher = current / "entrypoints" / "hello"

    # Invoke the launcher via Git Bash.
    result = subprocess.run(
        ["bash", "-c", str(hello_launcher)],
        capture_output=True, text=True, env=ocx.env,
>>>>>>> 9b296687 (feat(cli)!: toolchain CLI taxonomy + global activation via env exporter)
    )
