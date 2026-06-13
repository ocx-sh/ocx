# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for non-destructive package publish (M1 / INV-M1).

These tests exercise the cross-process side of INV-M1 (system_design_shared_store.md
§5 M1): a lock-free reader must never observe ``packages/{digest}/`` missing or
half-deleted while another process re-publishes (re-pulls) the same digest over a
broken install.

Determinism is achieved with the publish fault hook added in P1.5:

- ``__OCX_TESTING_PUBLISH_PAUSE=1`` — when set, ``finalize_package_dir``'s
  broken-install stash→swap blocks BEFORE any rename (the broken dir still
  occupies the canonical name) until a release file appears.
- ``__OCX_TESTING_PUBLISH_RELEASE_FILE=<path>`` — the swap resumes once this
  file exists.

The fault hook is gated behind the ``__OCX_TESTING_`` env prefix (testing-only
namespace convention) and is a zero-cost env probe in production.

Spec sources
------------
- system_design_shared_store.md §5 M1 (INV-M1, stash→swap), §5 M5 (fault hook)
- plan_shared_store.md P1.5
- mirrors the ThreadPoolExecutor / release-file pattern in test_project_concurrency.py
"""
from __future__ import annotations

import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, PackageInfo, registry_dir
from src.helpers import make_package

# Mirror crates/ocx_lib/src/cli/exit_code.rs.
EXIT_SUCCESS = 0

# Testing-only fault hook env vars (must match pull.rs::maybe_pause_publish).
PAUSE_ENV = "__OCX_TESTING_PUBLISH_PAUSE"
RELEASE_ENV = "__OCX_TESTING_PUBLISH_RELEASE_FILE"

_PLATFORM = "linux/amd64"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _spawn(ocx: OcxRunner, *args: str, extra_env: dict[str, str] | None = None) -> subprocess.Popen[str]:
    """Spawn ``ocx`` without blocking; caller drives wait()/communicate()."""
    cmd = [str(ocx.binary), "--format", "json", *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )


def _resolve_cas_package_dir(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Resolve the on-disk content-addressed package dir for ``pkg``.

    Reads the install candidate symlink and walks up from the resolved target
    to the package root (the dir holding ``install.json``).
    """
    which = ocx.json("package", "which", "--candidate", pkg.short)
    # `package which` returns a mapping short -> path (the candidate symlink).
    raw = which[pkg.short] if isinstance(which, dict) else which
    path = Path(raw["path"] if isinstance(raw, dict) else raw)
    target = path.resolve()
    # Walk up to the dir containing install.json.
    cursor = target
    for _ in range(6):
        if (cursor / "install.json").exists():
            return cursor
        if cursor.parent == cursor:
            break
        cursor = cursor.parent
    return target


def _wait_until(predicate, timeout: float = 30.0, interval: float = 0.05) -> bool:
    """Poll ``predicate`` until true or ``timeout`` elapses."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


# ---------------------------------------------------------------------------
# INV-M1: concurrent same-digest re-publish never exposes a missing dir
# ---------------------------------------------------------------------------


def test_concurrent_same_digest_publish_no_enoent(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A reader never observes the CAS package dir missing during a paused re-pull swap.

    Install + select a package, break its ``install.json`` so a re-install is
    forced down the broken-install stash→swap path, then run that re-install
    paused via ``__OCX_TESTING_PUBLISH_PAUSE``. While it is paused, spin a
    cross-process reader (``ocx package which``) and assert it always resolves
    the candidate symlink to an existing CAS dir — never ENOENT. Release the
    pause; the re-install must complete successfully.

    Requirement: system_design_shared_store.md §5 M1 INV-M1; plan P1.5
    ``test_concurrent_same_digest_publish_no_enoent``.
    """
    repo = f"t_{uuid4().hex[:8]}_inv_m1_enoent"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    # Install + select so a candidate symlink and a complete CAS dir exist.
    ocx.json("package", "install", "--select", pkg.short)

    cas_dir = _resolve_cas_package_dir(ocx, pkg)
    install_json = cas_dir / "install.json"
    assert install_json.exists(), f"install.json must exist before breaking: {cas_dir}"

    # Break the install so the re-install is forced down the swap path.
    install_json.unlink()

    release_file = tmp_path / "inv_m1_release"

    # Spawn the re-install paused inside the broken-install swap.
    repull = _spawn(
        ocx,
        "package",
        "install",
        pkg.short,
        extra_env={PAUSE_ENV: "1", RELEASE_ENV: str(release_file)},
    )

    try:
        # Wait until the re-install has reached the swap and is paused: the
        # debug log line is on stderr but we cannot read it without blocking,
        # so instead poll until the candidate dir is observably stable AND the
        # process is still running (it blocks in the swap, not exiting).
        time.sleep(0.5)
        assert repull.poll() is None, (
            "paused re-install must still be running (blocked in the swap); "
            f"it exited early rc={repull.returncode}"
        )

        # Cross-process reader loop: `ocx package which` must always resolve the
        # candidate symlink to an existing CAS package dir during the pause.
        observed = 0
        for _ in range(40):
            assert repull.poll() is None, "re-install must remain paused during the reader loop"
            which = ocx.run("package", "which", "--candidate", pkg.short, check=False)
            assert which.returncode == EXIT_SUCCESS, (
                "INV-M1: a lock-free reader (`package which`) must never fail during a "
                f"paused re-publish swap; rc={which.returncode} stderr={which.stderr!r}"
            )
            assert cas_dir.exists(), (
                "INV-M1: the canonical CAS package dir must never be missing during the "
                f"paused swap; checked: {cas_dir}"
            )
            observed += 1
            time.sleep(0.02)
        assert observed == 40, "reader loop must have run all iterations"
    finally:
        # Release the pause so the swap completes and the process exits.
        release_file.write_text("go")
        out, err = repull.communicate(timeout=60)

    assert repull.returncode == EXIT_SUCCESS, (
        f"paused re-install must succeed after release; rc={repull.returncode}\n"
        f"stdout={out!r}\nstderr={err!r}"
    )
    # After the swap the canonical dir is present with a healthy install.
    assert cas_dir.exists(), "canonical CAS package dir must remain present after the swap"
    assert install_json.exists(), "re-install must restore install.json after the swap"


# ---------------------------------------------------------------------------
# dest_override publish does not destructively race the shared CAS dir
# ---------------------------------------------------------------------------


def test_dest_override_no_destructive_race(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A ``dest_override`` materialization never disturbs the shared CAS dir under read.

    ``ocx package test --output DIR`` materializes the root package to a
    caller-owned directory via the destructive ``move_dir`` path — but that path
    targets ``DIR``, never the shared content-addressed ``packages/{digest}/``.

    Two distinct packages are used so their per-digest temp locks differ (a
    single digest would serialize the override behind the paused CAS swap on the
    shared temp lock — a deliberate non-race). Package P's broken-install CAS
    re-install is paused via the fault hook; package Q's ``--output``
    materialization runs concurrently. P's shared CAS dir must never be observed
    missing while Q's override publish is in flight, and Q must materialize into
    its caller-owned dir (not under the CAS ``packages/`` tree).

    Requirement: system_design_shared_store.md §5 M1 ("dest_override dest → keep
    move_dir; not a shared CAS target"); plan P1.5
    ``test_dest_override_no_destructive_race``.
    """
    short = uuid4().hex[:8]
    # Package P — the one whose CAS swap is paused and read concurrently.
    repo_p = f"t_{short}_override_cas"
    pkg_p = make_package(ocx, repo_p, "1.0.0", tmp_path, new=True, cascade=False)
    # Package Q — distinct content (distinct digest → distinct temp lock) for the
    # dest_override materialization.
    repo_q = f"t_{short}_override_dest"
    pkg_q = make_package(ocx, repo_q, "1.0.0", tmp_path, new=True, cascade=False)
    bundle_q = tmp_path / f"bundle-{repo_q}-1.0.0.tar.xz"
    metadata_q = tmp_path / f"metadata-{repo_q}-1.0.0.json"
    assert bundle_q.exists(), f"make_package must produce a bundle: {bundle_q}"
    assert metadata_q.exists(), f"make_package must produce metadata: {metadata_q}"

    # Install + select P so a candidate symlink and a complete CAS dir exist.
    ocx.json("package", "install", "--select", pkg_p.short)
    cas_dir = _resolve_cas_package_dir(ocx, pkg_p)
    install_json = cas_dir / "install.json"
    assert install_json.exists()

    # Break P's CAS install so its re-install runs the paused swap.
    install_json.unlink()
    release_file = tmp_path / "override_release"

    repull = _spawn(
        ocx,
        "package",
        "install",
        pkg_p.short,
        extra_env={PAUSE_ENV: "1", RELEASE_ENV: str(release_file)},
    )

    override_dir = tmp_path / "override-dest"
    try:
        time.sleep(0.5)
        assert repull.poll() is None, "paused CAS re-install must still be running"

        # While P's CAS swap is paused, materialize Q via the dest_override path
        # (`package test --output`) — move_dir to the caller-owned dir, a
        # distinct digest so it does not serialize behind P's temp lock.
        def _override() -> subprocess.CompletedProcess[str]:
            return ocx.run(
                "package",
                "test",
                "-p",
                _PLATFORM,
                "-m",
                str(metadata_q),
                "-i",
                pkg_q.short,
                str(bundle_q),
                "-o",
                str(override_dir),
                "--",
                "true",
                check=False,
            )

        with ThreadPoolExecutor(max_workers=1) as pool:
            override_future = pool.submit(_override)
            # Reader loop against P's shared CAS dir while both the paused swap
            # and Q's override materialization are in flight.
            for _ in range(30):
                assert cas_dir.exists(), (
                    "INV-M1: the shared CAS package dir must never be removed by a "
                    f"concurrent dest_override publish; checked: {cas_dir}"
                )
                which = ocx.run("package", "which", "--candidate", pkg_p.short, check=False)
                assert which.returncode == EXIT_SUCCESS, (
                    "a CAS reader must never fail while a dest_override publish runs; "
                    f"rc={which.returncode} stderr={which.stderr!r}"
                )
                time.sleep(0.02)
            override_result = override_future.result(timeout=120)
    finally:
        release_file.write_text("go")
        out, err = repull.communicate(timeout=60)

    assert override_result.returncode == EXIT_SUCCESS, (
        "dest_override materialization (`package test --output`) must succeed independently "
        f"of the CAS swap; rc={override_result.returncode}\nstderr={override_result.stderr!r}"
    )
    # The override dir is a real, independent materialization (NOT under the
    # shared CAS tree).
    assert override_dir.exists(), f"override dir must be materialized: {override_dir}"
    assert (override_dir / "content").exists(), "override materialization must contain content/"
    cas_root = Path(ocx.env["OCX_HOME"]) / "packages"
    assert not str(override_dir.resolve()).startswith(str(cas_root.resolve())), (
        "the override dir must be a caller-owned path, not under the shared CAS packages/ tree"
    )

    # The paused CAS re-install must have completed cleanly after release.
    assert repull.returncode == EXIT_SUCCESS, (
        f"paused CAS re-install must succeed after release; rc={repull.returncode}\n"
        f"stdout={out!r}\nstderr={err!r}"
    )
    assert cas_dir.exists(), "CAS dir present after both publishes"
    assert install_json.exists(), "CAS re-install restored install.json"
