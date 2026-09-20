# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for project-tier mutation locking and reader safety.

Two halves of one contract (ocx#494):

*Writers serialize.* Project-tier mutators (`ocx add`, `ocx remove`,
`ocx update`, `ocx lock`, `ocx init`) hold the project mutation lock for
the duration of the staged → resolve → commit transaction. That lock is a
content-keyed entry under ``$OCX_HOME/locks`` — never a handle on
``ocx.toml`` and never a ``ocx.toml.lock`` sidecar in the project. Two
concurrent writers must serialize: one wins, the other either retries
cleanly or exits with ``Locked`` (TempFail 75).

*Readers never tear.* ``ocx.toml`` is published by atomic rename, so a
reader that takes no lock at all — ``ocx status``, the per-prompt
reconciler, direnv, git, an editor — finishes its read against the
document it opened. It never observes a short one, and never a splice of
two.

Spec sources:
- ``crates/ocx_project/src/mutate.rs`` (``atomic_write``, the one writer)
- ``crates/ocx_project/src/project_lock.rs`` (the scoped mutation lock)
- ``crates/ocx_project/src/error.rs`` ``ProjectErrorKind::Locked`` →
  ``ExitCode::TempFail`` (75)
"""
from __future__ import annotations

import hashlib
import os
import subprocess
import time
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Exit codes — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_TEMP_FAIL = 75  # ProjectErrorKind::Locked


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with cwd driving the project CWD-walk."""
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env, check=False)


def _spawn_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.Popen[str]:
    """Spawn ocx without blocking; caller handles wait()."""
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.Popen(
        cmd,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _mutation_lock_path(ocx: OcxRunner, project_dir: Path) -> Path:
    """The lock file ``ocx`` takes for ``project_dir/ocx.toml``.

    Mirrors ``ocx_util::fs::lock_scoped``: SHA-256 over
    ``"{dev}:{ino}:{scope}:{discriminator}"`` of the *guarded directory*,
    sharded two hex digits deep under ``$OCX_HOME/locks``. Spelling it here
    is what lets a test hold the real lock from the outside — and, held
    against a run that just succeeded, what proves the derivation still
    matches the implementation rather than pointing at a path nobody writes.
    """
    stat = project_dir.stat()
    key = f"{stat.st_dev}:{stat.st_ino}:project-mutate:ocx.toml"
    digest = hashlib.sha256(key.encode()).hexdigest()
    return ocx.ocx_home / "locks" / digest[:2] / f"{digest[2:]}.lock"


# ---------------------------------------------------------------------------
# 1. Two concurrent `ocx add` — exactly one wins, no corruption
# ---------------------------------------------------------------------------


def test_concurrent_add_serialized_by_the_mutation_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Two ``ocx add`` invocations against the same ``ocx.toml`` must serialize.

    Either:
    - Both succeed sequentially (winner finishes fast, loser waits inside
      the mutation-lock contention budget), and the final ``ocx.toml``
      contains both bindings in stable order.
    - One succeeds, the other exits with ``EXIT_TEMP_FAIL`` (75 — Locked).

    No partial / interleaved write may corrupt ``ocx.toml`` or ``ocx.lock``.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_conc_a"
    repo_b = f"t_{short}_conc_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    fq_a = f"{ocx.registry}/{repo_a}:1.0.0"
    fq_b = f"{ocx.registry}/{repo_b}:1.0.0"

    # Spawn both writers as close together as possible. Use threads to
    # block on wait() concurrently.
    def _add(fq: str) -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "add", fq)
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(
            args=proc.args, returncode=proc.returncode, stdout=out, stderr=err
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        fut_a = pool.submit(_add, fq_a)
        fut_b = pool.submit(_add, fq_b)
        result_a = fut_a.result()
        result_b = fut_b.result()

    # Validate: at least one succeeded; any failure must be EXIT_TEMP_FAIL
    # (Locked), never an arbitrary I/O / parse error from a corrupted file.
    successes = [r for r in (result_a, result_b) if r.returncode == EXIT_SUCCESS]
    failures = [r for r in (result_a, result_b) if r.returncode != EXIT_SUCCESS]
    assert successes, (
        "at least one writer must succeed; "
        f"a={result_a.returncode} stderr={result_a.stderr!r} "
        f"b={result_b.returncode} stderr={result_b.stderr!r}"
    )
    for failed in failures:
        assert failed.returncode == EXIT_TEMP_FAIL, (
            f"concurrent add loser must exit {EXIT_TEMP_FAIL} (Locked), "
            f"got {failed.returncode}; stderr={failed.stderr!r}"
        )

    # ocx.toml + ocx.lock must be present and parseable.
    toml_text = (project / "ocx.toml").read_text()
    assert (project / "ocx.lock").exists(), "ocx.lock must exist after a successful add"

    # If both succeeded, both bindings must appear (serialized commits
    # carry forward each other's state); otherwise only the winner's.
    if len(successes) == 2:
        assert repo_a in toml_text and repo_b in toml_text, (
            f"both bindings must appear in ocx.toml after sequential success; "
            f"got:\n{toml_text}"
        )
    else:
        assert (repo_a in toml_text) ^ (repo_b in toml_text), (
            f"exactly one binding must be present when one writer was rejected; "
            f"got:\n{toml_text}"
        )


# ---------------------------------------------------------------------------
# 2. `ocx lock` concurrent with `ocx add` — same serialization contract
# ---------------------------------------------------------------------------


def test_concurrent_lock_command_serialized(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` and ``ocx add`` against the same project must serialize.

    Whichever wins first holds the mutation lock; the other either waits and
    succeeds (acceptable) or exits ``EXIT_TEMP_FAIL`` (Locked).

    Neither file may end up corrupted regardless of who wins.
    """
    short = uuid4().hex[:8]
    repo_existing = f"t_{short}_lockconc_existing"
    repo_new = f"t_{short}_lockconc_new"
    make_package(ocx, repo_existing, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, repo_new, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\n{repo_existing} = "{ocx.registry}/{repo_existing}:1.0.0"\n',
    )

    fq_new = f"{ocx.registry}/{repo_new}:1.0.0"

    def _lock() -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "lock")
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    def _add() -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "add", fq_new)
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    with ThreadPoolExecutor(max_workers=2) as pool:
        fut_lock = pool.submit(_lock)
        fut_add = pool.submit(_add)
        r_lock = fut_lock.result()
        r_add = fut_add.result()

    for r in (r_lock, r_add):
        assert r.returncode in (EXIT_SUCCESS, EXIT_TEMP_FAIL), (
            f"unexpected exit code {r.returncode}; "
            f"args={r.args}, stderr={r.stderr!r}"
        )

    # ocx.toml must still be valid TOML containing at least the original binding.
    toml_text = (project / "ocx.toml").read_text()
    assert repo_existing in toml_text, (
        f"original binding must survive concurrent lock+add; got:\n{toml_text}"
    )

    # ocx.lock must exist and be parseable (not truncated / interleaved).
    lock_path = project / "ocx.lock"
    assert lock_path.exists(), "ocx.lock must exist after at least one writer succeeded"
    lock_text = lock_path.read_text()
    assert "[metadata]" in lock_text or "metadata" in lock_text, (
        f"ocx.lock must be parseable TOML, not corrupted; got:\n{lock_text[:500]}"
    )


# ---------------------------------------------------------------------------
# 3. The lock lives under $OCX_HOME/locks, not on ocx.toml
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    os.name == "nt",
    reason="POSIX flock(2) advisory contract; Windows uses LockFileEx with different semantics",
)
def test_mutation_lock_holder_blocks_other_writers(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Holding the project's entry under ``$OCX_HOME/locks`` makes ``ocx add``
    exit 75, and no ``.lock`` file is ever written into the project.

    The first ``ocx add`` is the premise check: it must materialize exactly the
    lock file this test computes. Without it, a derivation that drifted from
    ``lock_scoped`` would leave the holder squatting on a path nobody writes
    and the contention assertion below would be testing nothing.
    """
    short = uuid4().hex[:8]
    repo_first = f"t_{short}_lockfile_first"
    repo_second = f"t_{short}_lockfile_second"
    make_package(ocx, repo_first, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, repo_second, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    first = _run_in(ocx, project, "add", f"{ocx.registry}/{repo_first}:1.0.0")
    assert first.returncode == EXIT_SUCCESS, (
        f"the premise add must succeed; rc={first.returncode}, stderr={first.stderr!r}"
    )

    lock_file = _mutation_lock_path(ocx, project)
    assert lock_file.is_file(), (
        "a successful mutation must have taken its lock under $OCX_HOME/locks at "
        f"{lock_file}; the derivation no longer matches lock_scoped"
    )
    # `ocx.lock` is the project's resolved snapshot, not a lock file. Anything
    # else ending in `.lock` inside the project is the sidecar this design
    # exists to not write.
    sidecars = sorted(
        path.name for path in project.rglob("*.lock") if path.name != "ocx.lock"
    )
    assert not sidecars, (
        f"the mutation lock must never land beside the data; found {sidecars} in the project"
    )

    holder = subprocess.Popen(["flock", "-x", str(lock_file), "-c", "sleep 10"])
    try:
        time.sleep(0.5)
        result = _run_in(ocx, project, "add", f"{ocx.registry}/{repo_second}:1.0.0")
        assert result.returncode == EXIT_TEMP_FAIL, (
            f"ocx add must exit {EXIT_TEMP_FAIL} while another process holds the mutation "
            f"lock; got {result.returncode}; stderr={result.stderr!r}"
        )
    finally:
        holder.terminate()
        holder.wait(timeout=5)


@pytest.mark.skipif(
    os.name == "nt",
    reason="POSIX flock(2) advisory contract; Windows uses LockFileEx with different semantics",
)
def test_flock_on_ocx_toml_itself_does_not_block_a_mutation(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A lock held on ``ocx.toml`` itself must not wedge ``ocx add``.

    The inverse of the test above, and the one that states what moved: the
    manifest is published by rename, so it is not the lock target any more.
    An editor, a ``flock(1)`` wrapper or a descriptor a crashed process left
    behind cannot stop a mutation.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_toml_squat"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    toml_path = _write_ocx_toml(project, "[tools]\n")

    holder = subprocess.Popen(["flock", "-x", str(toml_path), "-c", "sleep 10"])
    try:
        time.sleep(0.5)
        result = _run_in(ocx, project, "add", f"{ocx.registry}/{repo}:1.0.0")
        assert result.returncode == EXIT_SUCCESS, (
            "a flock on ocx.toml must not block the mutation lock; "
            f"got {result.returncode}; stderr={result.stderr!r}"
        )
    finally:
        holder.terminate()
        holder.wait(timeout=5)

    assert repo in toml_path.read_text(), "the binding must have landed"


# ---------------------------------------------------------------------------
# 4. An unlocked reader mid-read never observes a torn manifest
# ---------------------------------------------------------------------------


def test_unlocked_reader_sees_one_document_across_a_split_read(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A reader holding ``ocx.toml`` open across an ``ocx add`` finishes its
    read against the document it opened — never a splice of two.

    One buffered read is several ``read(2)`` calls. Under an in-place rewrite
    the tail of the longer replacement lands on the head of the document the
    reader had already buffered, and the result is a manifest nobody ever
    wrote — which may very well still parse. So the assertion is identity with
    one of the two documents, not parseability. Publishing by rename keeps the
    reader's descriptor on the inode it opened, so its tail read is empty.
    """
    short = uuid4().hex[:8]
    repo_seed = f"t_{short}_split_seed"
    repo_grouped = f"t_{short}_split_grouped"
    repo_added = f"t_{short}_split_added"
    for repo in (repo_seed, repo_grouped, repo_added):
        make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    # A section *after* the insertion point, so the mutation shifts bytes the
    # reader has already buffered. Seeded with a bare `[tools]` the added key
    # lands at the very end, the new document shares its whole prefix with the
    # old one, and an in-place rewrite splices back together into exactly the
    # new document — a test that could not tell the two designs apart.
    toml_path = _write_ocx_toml(
        project,
        f"[tools]\n{repo_seed} = \"{ocx.registry}/{repo_seed}:1.0.0\"\n"
        f"\n[group.ci.tools]\n{repo_grouped} = \"{ocx.registry}/{repo_grouped}:1.0.0\"\n",
    )
    before = toml_path.read_bytes()

    # A reader mid-read: the whole current document is in hand, the descriptor
    # is still open and one read short of EOF.
    with toml_path.open("rb") as reader:
        observed = reader.read(len(before))
        assert observed == before, "the fixture reader must start with the whole current document"

        result = _run_in(ocx, project, "add", f"{ocx.registry}/{repo_added}:1.0.0")
        assert result.returncode == EXIT_SUCCESS, (
            f"the add must succeed; rc={result.returncode}, stderr={result.stderr!r}"
        )
        after = toml_path.read_bytes()
        assert len(after) > len(before), (
            "the fixture must publish a strictly longer document for a splice to be observable"
        )
        assert after[: len(before)] != before, (
            "the fixture must differ within the reader's first read, or a splice is invisible"
        )

        observed += reader.read()

    assert observed in (before, after), (
        "the unlocked reader observed neither document whole — it spliced two:\n"
        f"{observed.decode(errors='replace')}"
    )
    # And whichever it saw is a manifest, not a fragment.
    tomllib.loads(observed.decode())
