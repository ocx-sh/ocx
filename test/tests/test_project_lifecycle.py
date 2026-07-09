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
# V2 lock shape: lifecycle writes V2; V1 lock installs without forced upgrade
# ---------------------------------------------------------------------------

_LEAF_RE_LC = _re_lc.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_lifecycle_writes_v2_lock_shape(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx init`` + ``ocx add``, the ``ocx.lock`` file is written in V2
    shape: ``lock_version = 2``, bare ``repository`` coordinate (no tag, no
    digest), ``[tool.platforms]`` table with per-platform leaf digests, and no
    legacy ``pinned =`` line.

    ADR: "Write only V2.  No code path emits V1."

    This is a structural assertion that the full init→add lifecycle delivers
    the V2 wire format — independent of whether any particular platform key is
    present (platform availability is registry-side, not asserted here).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lc_v2"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project_dir = tmp_path / "proj_lc_v2"
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
    assert "lock_version = 2" in lock_text, (
        "lifecycle must produce a V2 lock (lock_version = 2); got:\n" + lock_text[:400]
    )
    assert "[tool.platforms]" in lock_text, (
        "V2 lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_LC.findall(lock_text)
    assert leaf_digests, (
        "V2 lock must record at least one per-platform leaf digest"
    )
    assert f'repository = "{ocx.registry}/{repo}"' in lock_text, (
        "V2 lock must carry the bare repository coordinate; got:\n" + lock_text[:400]
    )
    assert "pinned =" not in lock_text, (
        "V2 lock must NOT carry a legacy `pinned` line"
    )


def test_lifecycle_v1_lock_pull_installs_without_forced_upgrade(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A V1 ``ocx.lock`` committed before the V2 migration allows ``ocx pull``
    to install the tool via the legacy index-digest path — no forced upgrade,
    no read-path mutation.

    ADR: "A committed V1 lock keeps installing/running offline with no forced
    upgrade and no read-path mutation."

    Flow:
    1. Publish the package; run ``ocx lock`` (V2) to get real coordinates.
    2. Overwrite the lock with a hand-authored V1 form (real pinned identifier
       built from the V2 bare-repo + one leaf).
    3. ``ocx pull`` must succeed (registry still has the index manifest).
    4. The ``ocx.lock`` file must NOT have been mutated by ``ocx pull``
       (legacy read path, no forced write).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lc_v1"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project_dir = tmp_path / "proj_lc_v1"
    project_dir.mkdir()

    # Step 1: init + lock to get a real V2 lock with live coordinates.
    init_r = _run(ocx, project_dir, "init")
    assert init_r.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={init_r.returncode}"
    )
    add_r = _run(ocx, project_dir, "add", "--no-pull", fq)
    # ``add --no-pull`` writes the lock without downloading; gives us V2 coords.
    assert add_r.returncode == EXIT_SUCCESS, (
        f"ocx add --no-pull failed: rc={add_r.returncode}"
    )

    lock_path = project_dir / "ocx.lock"
    v2_text = lock_path.read_text()

    repo_match = _re_lc.search(r'repository\s*=\s*"([^"]+)"', v2_text)
    leaf_match = _LEAF_RE_LC.search(v2_text)
    decl_match = _re_lc.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_text)
    assert repo_match and leaf_match and decl_match, (
        "V2 lock must carry repository + leaf + declaration_hash; got:\n" + v2_text[:400]
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)
    decl_hash = decl_match.group(1)

    # Step 2: overwrite with a V1 lock.
    v1_content = f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "{repo}"
group = "default"
pinned = "{bare_repo}@sha256:{leaf_hex}"
"""
    lock_path.write_text(v1_content)

    # Step 3: ``ocx pull`` on the V1 lock must succeed.
    pull_r = _run(ocx, project_dir, "pull")
    assert pull_r.returncode == EXIT_SUCCESS, (
        f"ocx pull on V1 lock must succeed via legacy index-digest path; "
        f"rc={pull_r.returncode}\nstderr:\n{pull_r.stderr}"
    )

    # Step 4: the V1 lock must NOT have been mutated by ``ocx pull``.
    after_text = lock_path.read_text()
    assert after_text == v1_content, (
        "ocx pull must NOT mutate a V1 ocx.lock (no forced upgrade on read); "
        "lock content changed unexpectedly"
    )
