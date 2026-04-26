"""Acceptance tests — Entry points happy path + error cases.

Post-flatten layout: per-repo `current` symlink targets the package root, and
`<current>/entrypoints` exposes the generated launchers (no separate
`entrypoints-current` symlink).
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def ocx_home_symlinks(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Return the symlinks/{registry}/{repo}/ directory for the package."""
    from src.runner import registry_dir  # noqa: PLC0415
    reg = registry_dir(ocx.registry)
    return Path(str(ocx.ocx_home)) / "symlinks" / reg / pkg.repo


def current_entrypoints(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Path to the launcher directory reached via the per-repo `current` anchor."""
    return ocx_home_symlinks(ocx, pkg) / "current" / "entrypoints"


# ---------------------------------------------------------------------------
# Happy path: install + select + launcher files reachable via current
# ---------------------------------------------------------------------------


def test_entrypoint_launcher_files_created_after_select(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """install --select wires `current` so launchers are reachable at `current/entrypoints/`."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    entrypoints_dir = current_entrypoints(ocx, pkg)
    assert entrypoints_dir.is_dir(), (
        f"current/entrypoints/ must exist after install --select: {entrypoints_dir}"
    )
    assert (entrypoints_dir / "hello").exists(), (
        f"unix launcher must be reachable at current/entrypoints/hello: {entrypoints_dir / 'hello'}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher test")
def test_entrypoint_unix_launcher_is_executable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Unix launcher file must exist (via current/entrypoints) and be executable (+x)."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"unix launcher must be generated: {launcher}"
    mode = launcher.stat().st_mode
    assert mode & stat.S_IXUSR, f"unix launcher must be executable: {launcher}, mode={oct(mode)}"


def test_deselect_removes_current_symlink(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx deselect` removes the `current` symlink, severing PATH access to launchers."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)
    ocx.plain("deselect", pkg.short)

    current = ocx_home_symlinks(ocx, pkg) / "current"
    assert not current.exists() and not current.is_symlink(), (
        f"current must be removed after deselect: {current}"
    )


def test_install_without_entrypoints_leaves_current_entrypoints_absent(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A package without entrypoints must not produce a current/entrypoints dir."""
    pkg = published_package
    ocx.plain("install", "--select", pkg.short)
    entrypoints_dir = current_entrypoints(ocx, pkg)
    assert not entrypoints_dir.exists(), (
        f"current/entrypoints/ must not exist for pkg without entrypoints: {entrypoints_dir}"
    )


# ---------------------------------------------------------------------------
# Error cases
# ---------------------------------------------------------------------------


def test_invalid_entrypoint_name_rejected_at_validation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Metadata with invalid entrypoint name (uppercase) is rejected at validation time.

    The custom `Entrypoints` deserializer enforces the name regex at deserialize time,
    so `package create` fails as soon as it parses the metadata file — no push needed.
    """
    del unique_repo  # rejection is local; no registry interaction

    pkg_dir = tmp_path / "pkg-invalid-ep"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    script = bin_dir / "hello"
    script.write_text("#!/bin/sh\necho hello\n")
    if sys.platform != "win32":
        script.chmod(script.stat().st_mode | stat.S_IEXEC)

    metadata_path = tmp_path / "metadata-invalid-ep.json"
    metadata_obj = {
        "type": "bundle",
        "version": 1,
        "entrypoints": [{"name": "INVALID_UPPER", "target": "${installPath}/bin/hello"}],
    }
    metadata_path.write_text(json.dumps(metadata_obj))

    bundle = tmp_path / "bundle-invalid-ep.tar.xz"
    result = ocx.run(
        "package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir),
        check=False,
    )
    assert result.returncode != 0, (
        "package create with invalid entrypoint name must fail at metadata parse\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "INVALID_UPPER" in result.stderr, (
        f"error must cite the offending name; stderr was: {result.stderr}"
    )


def test_install_without_select_does_not_trigger_collision_check(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Install alone (no --select) must not trigger collision check even with duplicate names."""
    repo_a = f"{unique_repo}-ca"
    repo_b = f"{unique_repo}-cb"

    pkg_a = make_package_with_entrypoints(
        ocx, repo_a, tmp_path,
        entrypoints=[{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        bins=["cmake"],
        tag="1.0.0",
    )
    pkg_b = make_package_with_entrypoints(
        ocx, repo_b, tmp_path,
        entrypoints=[{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        bins=["cmake"],
        tag="1.0.0",
    )

    ocx.plain("install", "--select", pkg_a.short)
    result = ocx.run("install", pkg_b.short, check=False)
    assert result.returncode == 0, (
        f"install without --select must succeed even when entrypoint names collide; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )


def test_reselect_to_package_without_entrypoints_drops_entrypoints_dir(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Re-selecting to a package without entrypoints must leave `current/entrypoints` absent.

    The flat layout achieves this via a single `current` flip: pointing it at a
    package root that has no `entrypoints/` child is enough — the launchers
    from the previous package are no longer reachable through `current`.
    """
    pkg_with = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
        tag="1.0.0",
    )
    pkg_without = make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False)

    ocx.plain("install", "--select", pkg_with.short)
    entrypoints_dir = current_entrypoints(ocx, pkg_with)
    assert entrypoints_dir.is_dir(), (
        f"precondition: pkg_with must materialize current/entrypoints/: {entrypoints_dir}"
    )

    ocx.plain("install", "--select", pkg_without.short)
    assert not entrypoints_dir.exists(), (
        f"re-selecting to a package without entrypoints must leave current/entrypoints/ unreachable; "
        f"still present at {entrypoints_dir}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher invocation test")
def test_launcher_invocation_runs_target_and_forwards_args(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Invoking the generated launcher with extra args runs the resolved target and forwards args."""
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
        tag="1.0.0",
    )
    ocx.plain("install", "--select", pkg.short)

    launcher = current_entrypoints(ocx, pkg) / "hello"
    assert launcher.exists(), f"unix launcher must exist: {launcher}"

    extra = "extra-arg-from-test"
    launcher_env = dict(ocx.env)
    launcher_env["PATH"] = f"{ocx.binary.parent}{os.pathsep}{launcher_env.get('PATH', '')}"
    completed = subprocess.run(
        [str(launcher), extra],
        capture_output=True,
        text=True,
        env=launcher_env,
        timeout=30,
        check=False,
    )
    assert completed.returncode == 0, (
        f"launcher invocation must succeed; rc={completed.returncode} "
        f"stderr={completed.stderr.strip()!r}"
    )
    assert pkg.marker in completed.stdout, (
        f"launcher must invoke the resolved target — package marker missing; "
        f"stdout={completed.stdout!r}"
    )
    assert extra in completed.stdout, (
        f"launcher must forward CLI args verbatim; stdout={completed.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Synthetic PATH entry: entrypoints/ added to env
# ---------------------------------------------------------------------------


def test_root_package_entrypoints_appear_in_self_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A root package with entrypoints must have its own entrypoints/ dir in PATH via ocx env.

    The visible-package pipeline emits a synthetic `PATH ⊳ <pkg_root>/entrypoints`
    entry so that the installed launchers are reachable after `eval $(ocx shell env ...)`.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    env_result = ocx.json("env", pkg.short)
    path_entries = [e["value"] for e in env_result if e["key"] == "PATH"]

    # At least one PATH entry must contain the entrypoints/ subdirectory.
    assert any("entrypoints" in v for v in path_entries), (
        f"expected an entrypoints/ PATH entry in env output; PATH values: {path_entries}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix exec integration test")
def test_exec_dep_launcher_via_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """exec A -- cmake executes B's cmake binary when A declares B as a public dep.

    B's cmake entrypoint must be reachable through the synthetic PATH entry that
    the visible-package pipeline emits for B's entrypoints/ directory.  The bin/
    PATH entry has higher priority (it is added AFTER the synthetic entry, ending
    up first in the prepend chain), so exec finds `bin/cmake` rather than the
    launcher — which means the real binary runs and the marker appears in stdout.
    """
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    pkg_b = make_package_with_entrypoints(
        ocx,
        b_repo,
        tmp_path,
        entrypoints=[{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="b",
    )

    dep_digest = fetch_manifest_digest(ocx.registry, b_repo, "1.0.0")
    dep_entry = {
        "identifier": f"{pkg_b.fq}@{dep_digest}",
        "visibility": "public",
    }

    from src.helpers import make_package  # noqa: PLC0415
    pkg_a = make_package(
        ocx,
        a_repo,
        "1.0.0",
        tmp_path,
        dependencies=[dep_entry],
    )

    ocx.plain("install", "--select", pkg_a.short)

    result = ocx.plain("exec", pkg_a.short, "--", "cmake")
    assert result.returncode == 0, (
        f"exec dep launcher must succeed; rc={result.returncode} stderr={result.stderr.strip()!r}"
    )
    assert pkg_b.marker in result.stdout, (
        f"exec must run B's cmake binary — marker missing; stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Windows: PATHEXT auto-inject (ocx exec) and warning (ocx install)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows PATHEXT test")
def test_exec_auto_injects_cmd_into_pathext_on_windows(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ocx exec auto-injects .CMD into PATHEXT so .cmd launchers are found
    even when the host shell's PATHEXT does not include .cmd.

    Builds a package with an entrypoint, installs + selects it, then invokes
    `ocx exec` with a custom env that strips .CMD from PATHEXT. The command
    must still succeed — proving that exec's auto-inject bridged the gap.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", "--select", pkg.short)

    # Build an env that deliberately has no .CMD in PATHEXT.
    import copy
    stripped_env = copy.copy(ocx.env)
    stripped_env["PATHEXT"] = ".EXE;.BAT;.COM"  # .CMD intentionally absent

    cmd = [str(ocx.binary), "exec", pkg.short, "--", "hello"]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=stripped_env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, (
        "ocx exec must succeed even when host PATHEXT lacks .CMD "
        f"(rc={result.returncode}, stderr={result.stderr.strip()!r})"
    )
    assert pkg.marker in result.stdout, (
        f"exec with stripped PATHEXT must still run the entrypoint; stdout={result.stdout!r}"
    )


@pytest.mark.skipif(sys.platform != "win32", reason="Windows PATHEXT warning test")
def test_install_warns_when_pathext_missing_cmd_on_windows(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """ocx install emits a warning to stderr when PATHEXT lacks .CMD on Windows.

    The warning fires because install is a consumer-boundary command — it emits
    paths that include .cmd launchers in entrypoints/, and the external shell
    needs .CMD in PATHEXT to find them.
    """
    import copy
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )

    stripped_env = copy.copy(ocx.env)
    stripped_env["PATHEXT"] = ".EXE;.BAT;.COM"  # .CMD intentionally absent

    cmd = [str(ocx.binary), "install", "--select", pkg.short]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=stripped_env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, (
        f"install must succeed even when PATHEXT lacks .CMD "
        f"(rc={result.returncode}, stderr={result.stderr.strip()!r})"
    )
    assert "PATHEXT" in result.stderr, (
        f"install must warn about missing .CMD in PATHEXT; stderr={result.stderr.strip()!r}"
    )
