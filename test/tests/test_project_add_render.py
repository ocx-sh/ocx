# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance test for the toolchain home `ocx add` renders on a no-op batch.

`ocx add` promises an unconditional re-render of `<project>/.ocx/toolchain/`.
The whole-batch no-op — every binding already declared and already pinned —
never reaches the shared commit-and-render path, so it has to render on its
own; without that, a re-add after an earlier ``--no-pull`` leaves
``toolchain/active/bin`` without its trampolines until some later ``ocx pull``,
which is exactly the state ``activate = "bin"`` resolves tools through.

Its own module rather than a case in ``test_project_add.py``: the assertions
need ``src.toolchain_fixtures``, and the test-diff guard freezes the import
block of an existing acceptance module.
"""

from __future__ import annotations

from pathlib import Path
from uuid import uuid4

from src.helpers import make_package, write_ocx_toml
from src.runner import OcxRunner
from src.toolchain_fixtures import (
    DEFAULT_GROUP,
    entry_link,
    run_in,
    shell_bin,
    toolchain_home,
)

EXIT_SUCCESS = 0


def test_add_renders_the_toolchain_home_on_a_whole_batch_no_op(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A re-add of what is already declared and pinned renders the home.

    ``--no-pull`` stands in for a first add whose download failed: it leaves the
    binding declared and pinned with a cold object store, so the closure walk
    the render needs cannot complete and the tree is never written. The second
    ``ocx add`` changes neither ``ocx.toml`` nor ``ocx.lock`` — and must still
    leave the entry link and the trampoline behind.
    """
    short = uuid4().hex[:8]
    binary = f"t_{short}_tool"
    package = make_package(
        ocx,
        f"t_{short}_render",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[binary],
        bin_scripts={binary: f"#!/bin/sh\necho ran-{short}\n"},
    )

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    write_ocx_toml(project_dir, "[tools]\n")

    key = "rendered"
    cold = run_in(ocx, project_dir, "add", "--no-pull", f"{key}={package.fq}")
    assert cold.returncode == EXIT_SUCCESS, (
        f"cold add failed: rc={cold.returncode}, stderr={cold.stderr!r}"
    )

    home = toolchain_home(project_dir)
    link = entry_link(home, DEFAULT_GROUP, key)
    trampoline = shell_bin(home) / binary
    assert not link.exists(), (
        "precondition: a cold store cannot resolve the surface, so the render "
        f"writes nothing; found {link}"
    )
    assert not trampoline.exists(), (
        f"precondition: no trampoline before the store is warm; found {trampoline}"
    )

    lock_path = project_dir / "ocx.lock"
    manifest_before = (project_dir / "ocx.toml").read_bytes()
    lock_before = lock_path.read_bytes()
    lock_mtime_before = lock_path.stat().st_mtime_ns

    warm = run_in(ocx, project_dir, "add", f"{key}={package.fq}")
    assert warm.returncode == EXIT_SUCCESS, (
        f"the re-add must exit {EXIT_SUCCESS}; "
        f"rc={warm.returncode}, stderr={warm.stderr!r}"
    )

    assert link.is_symlink() or link.exists(), (
        f"the re-add must render the entry link {link}; stderr={warm.stderr!r}"
    )
    assert trampoline.exists(), (
        f"the re-add must render the trampoline {trampoline}; stderr={warm.stderr!r}"
    )

    assert (project_dir / "ocx.toml").read_bytes() == manifest_before, (
        "the no-op re-add must leave ocx.toml byte-identical"
    )
    assert lock_path.read_bytes() == lock_before, (
        "the no-op re-add must leave ocx.lock byte-identical"
    )
    assert lock_path.stat().st_mtime_ns == lock_mtime_before, (
        "the render happens directly, so the no-op re-add must not touch the "
        "ocx.lock mtime"
    )
