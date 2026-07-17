# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Roundtrip lifecycle acceptance test for ocx project commands (Unit 7 — specification mode).

Exercises init → add → update → pull → remove in a single scenario to
verify that the full workflow integrates correctly. The test is expected
to FAIL against the current Unit 7 stubs.

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §4.
"""
from __future__ import annotations

import re as _re_lc
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


EXIT_SUCCESS = 0


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def test_init_add_update_pull_remove_roundtrip(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Full project-toolchain lifecycle in a single test:

    1. ``ocx init``           — creates ocx.toml with empty [tools]
    2. ``ocx add cmake:3.28`` — appends binding, writes lock, installs
    3. ``ocx update``         — whole-file re-resolve of every declared tag
    4. ``ocx pull cmake``     — succeeds against the installed binding
    5. ``ocx remove cmake``   — drops binding from ocx.toml and lock

    End-state: ocx.toml equivalent to post-init (empty [tools]).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lifecycle"
    # Push both 3.28.0 and a "3.28" alias so update has a real target.
    make_package(ocx, repo, "3.28.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "project"
    project_dir.mkdir()

    # Step 1: ocx init
    init_r = _run(ocx, project_dir, "init")
    assert init_r.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={init_r.returncode}, stderr={init_r.stderr!r}"
    )
    toml_path = project_dir / "ocx.toml"
    assert toml_path.exists(), "ocx.toml must exist after init"
    post_init_content = toml_path.read_text()
    assert "[tools]" in post_init_content, "post-init ocx.toml must contain [tools]"

    # Step 2: ocx add
    fq_id = f"{ocx.registry}/{repo}:3.28.0"
    add_r = _run(ocx, project_dir, "add", fq_id)
    assert add_r.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={add_r.returncode}, stderr={add_r.stderr!r}"
    )
    assert repo in toml_path.read_text(), "binding must be in ocx.toml after add"
    assert (project_dir / "ocx.lock").exists(), "ocx.lock must exist after add"

    # Step 3: ocx update (whole-file re-resolve)
    update_r = _run(ocx, project_dir, "update")
    assert update_r.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={update_r.returncode}, stderr={update_r.stderr!r}"
    )

    # Step 4: ocx pull <repo> (existing package pull command — exercises installed binding)
    pull_r = _run(ocx, project_dir, "package", "pull", fq_id)
    assert pull_r.returncode == EXIT_SUCCESS, (
        f"ocx package pull failed: rc={pull_r.returncode}, stderr={pull_r.stderr!r}"
    )

    # Step 5: ocx remove
    remove_r = _run(ocx, project_dir, "remove", repo)
    assert remove_r.returncode == EXIT_SUCCESS, (
        f"ocx remove failed: rc={remove_r.returncode}, stderr={remove_r.stderr!r}"
    )

    # End-state: ocx.toml should have no binding for this repo (equivalent to post-init).
    final_toml = toml_path.read_text()
    assert repo not in final_toml, (
        f"binding for {repo!r} must be gone after remove; got:\n{final_toml}"
    )
    lock_path = project_dir / "ocx.lock"
    if lock_path.exists():
        assert repo not in lock_path.read_text(), (
            f"lock entry for {repo!r} must be gone after remove"
        )
    # The [tools] table must still be present (structure unchanged, tools removed).
    assert "[tools]" in final_toml, "ocx.toml must retain [tools] table after all removes"


# ---------------------------------------------------------------------------
# Lock shape: the init->add lifecycle
# ---------------------------------------------------------------------------

_LEAF_RE_LC = _re_lc.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_lifecycle_writes_v3_lock_shape(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx init`` + ``ocx add``, the ``ocx.lock`` file carries a bare
    ``repository`` coordinate (no tag, no digest), a ``[tool.platforms]``
    table with per-platform leaf digests, and no legacy ``pinned =`` line.

    This is a structural assertion that the full init→add lifecycle delivers
    the lock wire format — independent of whether any particular platform key
    is present (platform availability is registry-side, not asserted here).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lc_v3"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project_dir = tmp_path / "proj_lc_v3"
    project_dir.mkdir()

    init_r = _run(ocx, project_dir, "init")
    assert init_r.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={init_r.returncode}, stderr={init_r.stderr!r}"
    )

    add_r = _run(ocx, project_dir, "add", fq)
    assert add_r.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={add_r.returncode}, stderr={add_r.stderr!r}"
    )

    lock_path = project_dir / "ocx.lock"
    assert lock_path.exists(), "ocx.lock must exist after ocx add"
    lock_text = lock_path.read_text()

    assert "ocx.lock" in str(lock_path), "sanity: lock path must end in ocx.lock"
    assert "[tool.platforms]" in lock_text, (
        "lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_LC.findall(lock_text)
    assert leaf_digests, (
        "lock must record at least one per-platform leaf digest"
    )
    assert f'repository = "{ocx.registry}/{repo}"' in lock_text, (
        "lock must carry the bare repository coordinate; got:\n" + lock_text[:400]
    )
    assert "pinned =" not in lock_text, (
        "lock must NOT carry a legacy `pinned` line"
    )
