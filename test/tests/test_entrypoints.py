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
import threading
from pathlib import Path

import pytest

from src.helpers import make_package, make_package_with_entrypoints
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


def test_collision_at_select_time_fails_with_structured_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """select on a package whose entrypoint name collides → EntrypointNameCollision, exit 65."""
    repo_a = f"{unique_repo}-a"
    repo_b = f"{unique_repo}-b"

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
    result = ocx.run("install", "--select", pkg_b.short, check=False)

    assert result.returncode == 65, (
        f"colliding --select must exit 65 (DataError); got rc={result.returncode}, "
        f"stderr={result.stderr.strip()}"
    )
    assert "cmake" in result.stderr, (
        f"collision error must cite the colliding entrypoint name; stderr={result.stderr.strip()}"
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


# ---------------------------------------------------------------------------
# Cross-cutting regressions
# ---------------------------------------------------------------------------


def test_select_command_collision_rejects_with_structured_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx select pkg2` with a colliding entrypoint name must fail with DataError (65)."""
    repo_a = f"{unique_repo}-sa"
    repo_b = f"{unique_repo}-sb"

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
    ocx.plain("install", pkg_b.short)

    result = ocx.run("select", pkg_b.short, check=False)
    assert result.returncode == 65, (
        f"`ocx select` with colliding entrypoint name must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "cmake" in result.stderr, (
        f"collision error must cite the colliding name 'cmake'; stderr={result.stderr.strip()}"
    )


def test_concurrent_install_select_one_loses(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Two concurrent `install --select` of distinct repos with the same entry-point name
    must serialize so exactly one wins.
    """
    repo_a = f"{unique_repo}-pa"
    repo_b = f"{unique_repo}-pb"

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

    barrier = threading.Barrier(2)
    results: dict[str, subprocess.CompletedProcess[str]] = {}

    def install_select(label: str, short: str) -> None:
        barrier.wait(timeout=10)
        results[label] = ocx.run("install", "--select", short, check=False)

    t1 = threading.Thread(target=install_select, args=("a", pkg_a.short))
    t2 = threading.Thread(target=install_select, args=("b", pkg_b.short))
    t1.start()
    t2.start()
    t1.join()
    t2.join()

    successes = [label for label, r in results.items() if r.returncode == 0]
    assert len(successes) == 1, (
        f"exactly one concurrent --select must win; got successes={successes}, "
        f"results={ {k: (v.returncode, v.stderr.strip()) for k, v in results.items()} }"
    )

    losers = [r for label, r in results.items() if label not in successes]
    assert losers, "at least one --select must lose the race"
    loser = losers[0]
    assert loser.returncode == 65, (
        f"losing concurrent --select must exit 65 (DataError); "
        f"got rc={loser.returncode}, stderr={loser.stderr.strip()}"
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
