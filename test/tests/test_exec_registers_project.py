# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ocx-sh/ocx#759: ``ocx exec`` roots the project it fetches for.

A fresh clone with a committed ``ocx.lock`` that only ever runs ``ocx exec`` used to get its tools
fetched and then removed by the next ``ocx clean``: the loaders never wrote the ``projects/`` ledger
entry, which is the only thing keeping a project's pins. Registration now happens on ``ocx exec`` in
a project, once, and never for a package run outside one.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, rglob_no_links
from src.runner import OcxRunner, registry_dir

pytestmark = pytest.mark.command("exec")


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        check=False,
    )


def _package_count(ocx_home: Path, registry: str) -> int:
    base = ocx_home / "packages" / registry_dir(registry)
    if not base.exists():
        return 0
    return sum(1 for p in rglob_no_links(base, "content") if p.is_dir())


def _ledger_entries(ocx_home: Path) -> list[Path]:
    projects = ocx_home / "projects"
    if not projects.is_dir():
        return []
    return [p for p in projects.iterdir() if not p.name.startswith(".tmp-")]


def _locked_project(ocx: OcxRunner, tmp_path: Path, label: str) -> tuple[Path, str, str]:
    """Publish a package and write a project dir whose ``ocx.lock`` is committed (made by a seed run)."""
    repo = f"t_{uuid4().hex[:8]}_reg_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, cascade=False)
    seed = tmp_path / "seed"
    seed.mkdir()
    (seed / "ocx.toml").write_text(f'[tools]\n{repo} = "{ocx.registry}/{repo}:{tag}"\n')
    locked = _run(ocx, seed, "lock")
    assert locked.returncode == 0, locked.stderr
    return seed, repo, tag


def _fresh_clone(ocx: OcxRunner, seed: Path, tmp_path: Path) -> tuple[OcxRunner, Path]:
    """A new home plus a project dir holding only the committed ``ocx.toml`` and ``ocx.lock``."""
    clone = tmp_path / "clone"
    clone.mkdir()
    for name in ("ocx.toml", "ocx.lock"):
        shutil.copy(seed / name, clone / name)
    home = tmp_path / "fresh-home"
    home.mkdir()
    return OcxRunner(ocx.binary, home, ocx.registry), clone


def test_exec_on_fresh_clone_survives_clean(ocx: OcxRunner, tmp_path: Path) -> None:
    """Fresh home, committed lock, ``ocx exec`` only, then ``ocx clean``: the tools stay."""
    seed, _, _ = _locked_project(ocx, tmp_path, "fresh")
    fresh, clone = _fresh_clone(ocx, seed, tmp_path)

    ran = _run(fresh, clone, "exec", "--", "hello")
    assert ran.returncode == 0, ran.stderr
    before = _package_count(fresh.ocx_home, fresh.registry)
    assert before >= 1, "ocx exec must have fetched the project's tool"

    cleaned = _run(fresh, clone, "clean")
    assert cleaned.returncode == 0, cleaned.stderr
    after = _package_count(fresh.ocx_home, fresh.registry)
    assert after == before, f"ocx clean removed the project's tools: {before} before, {after} after"


@pytest.mark.skipif(os.name == "nt", reason="inode identity of the ledger link is a POSIX probe")
def test_exec_in_registered_project_writes_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """A second ``ocx exec`` in an already registered project leaves the ledger link untouched."""
    seed, _, _ = _locked_project(ocx, tmp_path, "quiet")
    fresh, clone = _fresh_clone(ocx, seed, tmp_path)

    first = _run(fresh, clone, "exec", "--", "hello")
    assert first.returncode == 0, first.stderr
    entries = _ledger_entries(fresh.ocx_home)
    assert len(entries) == 1, f"ocx exec must register the project once; ledger: {entries}"
    link = entries[0]
    stat_before = os.lstat(link)

    second = _run(fresh, clone, "exec", "--", "hello")
    assert second.returncode == 0, second.stderr
    stat_after = os.lstat(link)
    assert (stat_after.st_ino, stat_after.st_mtime_ns) == (stat_before.st_ino, stat_before.st_mtime_ns), (
        "a registered project's ledger link must not be rewritten by ocx exec"
    )
    assert _ledger_entries(fresh.ocx_home) == entries


def test_package_exec_outside_project_stays_unrooted(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx package exec`` has no project to root against: the ledger stays empty."""
    _, repo, tag = _locked_project(ocx, tmp_path, "unrooted")
    fresh_home = tmp_path / "fresh-home"
    fresh_home.mkdir()
    fresh = OcxRunner(ocx.binary, fresh_home, ocx.registry)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()

    ran = _run(fresh, elsewhere, "package", "exec", f"{ocx.registry}/{repo}:{tag}", "--", "hello")
    assert ran.returncode == 0, ran.stderr
    assert _ledger_entries(fresh.ocx_home) == [], "a package run outside a project must register nothing"
