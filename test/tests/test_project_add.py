# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx add`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx add`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx add")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §2.
"""
from __future__ import annotations

import re as _re_add
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


EXIT_SUCCESS = 0
# BindingAlreadyExists → UsageError (64) per error.rs ClassifyExitCode
EXIT_USAGE_ERROR = 64
# StaleLockOnPartial → DataError (65) per error.rs ClassifyExitCode
EXIT_DATA_ERROR = 65
# LockUpgradeRequired → ConfigError (78) per error.rs ClassifyExitCode
EXIT_CONFIG = 78


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def _packages_present_count(ocx: OcxRunner) -> int:
    """Count distinct ``content/`` directories under
    ``$OCX_HOME/packages/{registry_dir}/`` — the object-store observable for
    eager-vs-lazy materialization. Toolchain mutators (`add`/`lock`/`update`)
    pull blobs and assemble package content but never create candidate or
    `current` symlinks under the new model (see
    ``project_context.rs::materialize_lock``), so package count is the only
    public signal that distinguishes ``--pull`` from ``--no-pull``.
    """
    base = Path(ocx.ocx_home) / "packages" / registry_dir(ocx.registry)
    if not base.exists():
        return 0
    return sum(1 for p in base.rglob("content") if p.is_dir())


def test_add_appends_to_tools_table(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add <pkg>`` appends to ``[tools]``, updates ``ocx.lock``, and
    pre-warms the object store. No candidate symlink under the new
    toolchain-mutator model — resolution goes through ``ocx.lock``.

    Spec: Unit 7 §2 bullet 1.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_tools"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo in toml_content, (
        f"ocx.toml must contain a binding for {repo!r} after ocx add; got:\n{toml_content}"
    )

    assert (project_dir / "ocx.lock").exists(), "ocx.lock must exist after ocx add"

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx add must materialize the package into the object store"
    )
    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_not_exists(candidate)


def test_add_to_named_group_via_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --group ci <pkg>`` (flag before positional per OCX convention)
    places the binding under ``[group.ci.tools]``-equivalent, updates lock,
    and installs.

    Spec: Unit 7 §2 bullet 2.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_ci"
    pkg = make_package(ocx, repo, "2.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--group", "ci", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --group ci failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    # The ci group must appear in the TOML.
    assert "ci" in toml_content, (
        f"ocx.toml must contain a [group.ci] section after --group ci; got:\n{toml_content}"
    )
    assert repo in toml_content, (
        f"ocx.toml must contain the binding for {repo!r}; got:\n{toml_content}"
    )

    assert (project_dir / "ocx.lock").exists(), "ocx.lock must exist after ocx add --group"

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx add --group must materialize the package into the object store"
    )
    candidate = _candidate_path(ocx, repo, "2.0.0")
    assert_not_exists(candidate)


def test_add_rejects_existing_binding(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add`` when the binding name already exists exits with UsageError
    (64) and leaves ``ocx.toml`` unchanged.

    Spec: Unit 7 §2 bullet 3. Error variant: ``BindingAlreadyExists`` →
    ``UsageError`` (64).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_dup"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n'
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/{repo}:2.0.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add duplicate should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )

    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when ocx add rejects a duplicate binding"
    )


def test_add_with_bare_identifier_defaults_to_latest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add <registry>/<repo>`` (no tag) writes a binding with ``:latest``
    tag (or equivalent default) into ``ocx.toml``.

    Spec: Unit 7 §2 bullet 4. Bare-identifier-default-to-latest semantics
    from Unit 3 commit 7b8d7f2a and ``config.rs::parse_tool_map``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_bare"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    # Bare identifier: no tag component.
    bare_id = f"{ocx.registry}/{repo}"
    result = _run_cmd(ocx, project_dir, "add", bare_id)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add bare identifier failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo in toml_content, (
        f"ocx.toml must contain a binding for {repo!r}; got:\n{toml_content}"
    )
    # The stored value should resolve to :latest.
    assert "latest" in toml_content, (
        f"bare identifier add must write ':latest' tag into ocx.toml; got:\n{toml_content}"
    )


def test_add_atomic_full_lockfile_rewrite(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx add new:1.0``, ``ocx.lock`` contains entries for all
    pre-existing tools plus the newly added tool — it is a full rewrite, not
    a partial patch.

    Spec: Unit 7 §2 bullet 5 (research §3 + §6.3 — atomic full rewrite).
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_atomic_a"
    repo_b = f"t_{short}_atomic_b"
    repo_new = f"t_{short}_atomic_new"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_new, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    # First lock: establish baseline with two tools.
    lock_result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"baseline ocx lock failed: {lock_result.stderr}"
    )

    # Now add a new tool.
    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/{repo_new}:1.0.0")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add new tool failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    lock_text = (project_dir / "ocx.lock").read_text()
    # All three tools must appear in the lock.
    for repo in (repo_a, repo_b, repo_new):
        assert repo in lock_text, (
            f"ocx.lock must contain entry for {repo!r} after atomic rewrite; "
            f"lock content:\n{lock_text}"
        )


def test_add_fails_without_ocx_toml(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add`` in a directory with no ``ocx.toml`` must exit with UsageError
    (64) and must NOT create an ``ocx.toml``.

    Spec: Unit 7 security fix — no-project guard. The command must surface a
    clear "no ocx.toml found" message and refuse to scaffold one implicitly.
    """
    project_dir = tmp_path / "empty_proj"
    project_dir.mkdir()

    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/foo:1.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add without ocx.toml should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert "ocx.toml" in result.stderr.lower() or "ocx.toml" in result.stdout.lower(), (
        "error output must mention 'ocx.toml' when no project file is found; "
        f"stderr={result.stderr!r}, stdout={result.stdout!r}"
    )
    assert_not_exists(project_dir / "ocx.toml")


def test_add_rejects_path_traversal_group_name(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --group '../../etc' <pkg>`` must exit with UsageError (64) and
    leave ``ocx.toml`` unchanged.

    Spec: Unit 7 security fix #2 — group name validation. Smoke-test that the
    ``InvalidGroupName`` variant is wired and that path-traversal attempts are
    rejected before any filesystem mutation.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "add", "--group", "../../etc", f"{ocx.registry}/foo:1.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add with path-traversal group should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when add rejects an invalid group name"
    )


# ---------------------------------------------------------------------------
# Phase-5 contracts: --pull / --no-pull flag pair on ``ocx add``
# ---------------------------------------------------------------------------


def test_add_eager_default_warms_object_store_without_symlinks(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """REGRESSION GUARD: ``ocx add tool:tag`` (no flags) pre-warms the object
    store but creates **no** candidate or `current` symlink.

    Locks in the new toolchain-mutator invariant:
    ``project_context.rs::materialize_lock`` calls ``pull_all`` (not
    ``install_all``). Project-tier resolution flows through ``ocx.lock``,
    not symlinks, so candidate creation here would only produce a second,
    redundant GC root — see commit fix(cli) after 066b50b9.

    Two assertions wear two hats:

    - Object-store content present → the eager default still materialized.
    - Candidate symlink absent → the OCI-tier ``install`` shape is NOT
      smuggled in via the toolchain-tier mutator.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_eager_guard"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx add must materialize the package into the object store"
    )
    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_not_exists(candidate)


def test_add_no_pull_skips_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --no-pull tool:tag`` must write the binding + lock and leave
    the object store cold. No candidate symlink (eager doesn't create one
    either under the new model).

    Plan Phase-5 Step 3.4 contract.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_nopull"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --no-pull failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    # Binding must be in the toml.
    assert repo in (project_dir / "ocx.toml").read_text(), (
        "ocx.toml must contain the binding after ocx add --no-pull"
    )
    # Lock must be written.
    assert (project_dir / "ocx.lock").exists(), (
        "ocx.lock must exist after ocx add --no-pull"
    )
    # Object store must stay cold — the only observable that distinguishes
    # --no-pull from eager under the no-symlink mutator model.
    assert _packages_present_count(ocx) == 0, (
        "ocx add --no-pull must not materialize the package into the object store"
    )
    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_not_exists(candidate)


def test_add_pull_then_no_pull_last_wins_no_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --pull --no-pull tool:tag`` → ``--no-pull`` wins (POSIX
    last-wins); candidate symlink must NOT exist.

    Plan Phase-5 Step 3.4 last-wins contract.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_p_np"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--pull", "--no-pull", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --pull --no-pull failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    assert _packages_present_count(ocx) == 0, (
        "--no-pull must win: object store stays cold"
    )
    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_not_exists(candidate)


def test_add_no_pull_then_pull_last_wins_installs(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --no-pull --pull tool:tag`` → ``--pull`` wins (POSIX
    last-wins); object store warms, candidate symlink remains absent.

    Plan Phase-5 Step 3.4 last-wins contract.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_np_p"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", "--pull", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --no-pull --pull failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    assert _packages_present_count(ocx) >= 1, (
        "--pull must win: object store warms"
    )
    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_not_exists(candidate)


# ---------------------------------------------------------------------------
# Lock shape: ``ocx add`` writes bare ``repository`` + ``[tool.platforms]``;
# untouched carried-forward entries pass through byte-identical.
# ---------------------------------------------------------------------------

_LEAF_RE_ADD = _re_add.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_add_writes_v3_lock_shape(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add <pkg>`` writes the ``ocx.lock`` shape: bare ``repository``
    (no tag, no digest) + ``[tool.platforms]`` table with per-platform leaf
    digests. No ``pinned =`` line may appear in the written lock.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_v3shape"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj_v3shape"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    lock_path = project_dir / "ocx.lock"
    assert lock_path.exists(), "ocx.lock must be written by ocx add"
    lock_text = lock_path.read_text()

    assert "[tool.platforms]" in lock_text, (
        "ocx add lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_ADD.findall(lock_text)
    assert leaf_digests, (
        "ocx add lock must record at least one per-platform leaf digest"
    )
    # The bare repository coordinate must be present (no tag, no digest suffix).
    assert f'repository = "{ocx.registry}/{repo}"' in lock_text, (
        f"ocx add lock must carry a bare repository coordinate for {repo!r}; "
        f"got:\n" + lock_text[:400]
    )
    assert f'repository = "{ocx.registry}/{repo}@' not in lock_text, (
        "repository must NOT carry a digest suffix"
    )
    assert f'repository = "{ocx.registry}/{repo}:' not in lock_text, (
        "repository must NOT carry a tag suffix"
    )
    # Legacy `pinned` line must be absent.
    assert "pinned =" not in lock_text, (
        "lock written by ocx add must NOT carry a legacy `pinned` line"
    )


# ---------------------------------------------------------------------------
# Whole-file model: pin preservation + freshness gate + bootstrap (spec §8.2)
# ---------------------------------------------------------------------------


def _leaves_for_add(lock_text: str, name: str) -> list[str]:
    """Return the sorted per-platform leaf digests for the ``[[tool]]`` entry
    whose ``name`` field equals ``name``; ``[]`` when no entry matches."""
    marker = f'name = "{name}"'
    if marker not in lock_text:
        return []
    start = lock_text.index(marker)
    rest = lock_text[start:]
    next_tool = rest.find("[[tool]]", len("[[tool]]"))
    slice_text = rest if next_tool == -1 else rest[:next_tool]
    return sorted(_LEAF_RE_ADD.findall(slice_text))


def test_add_preserves_untouched_pin_when_upstream_tag_moved(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Core regression: ``ocx add B`` must NOT re-resolve an untouched binding
    A even when A's upstream moving tag advanced. A's leaf digests stay
    byte-identical — only B is resolved.

    Whole-file model §2: ``add`` re-resolves only the new binding; every
    pre-existing binding is carried forward verbatim.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_pres_a"
    repo_b = f"t_{short}_pres_b"

    # A on a moving ``:latest`` tag; B as the tool to add later.
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=True)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\na = "{ocx.registry}/{repo_a}:latest"\n',
    )

    lock_r = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"
    initial_a = _leaves_for_add((project_dir / "ocx.lock").read_text(), "a")
    assert initial_a, "tool 'a' must record leaf digests"

    # Move A's upstream ``:latest`` to a new digest, refresh the local index.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    refresh = subprocess.run(
        [str(ocx.binary), "index", "update", f"{ocx.registry}/{repo_a}"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    # Add B. A is untouched → its leaves must NOT change.
    result = _run_cmd(ocx, project_dir, "add", "--no-pull", f"{ocx.registry}/{repo_b}:1.0.0")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    after_text = (project_dir / "ocx.lock").read_text()
    after_a = _leaves_for_add(after_text, "a")
    after_b = _leaves_for_add(after_text, repo_b)
    assert after_b, "the newly added tool B must record leaf digests"
    assert after_a == initial_a, (
        "untouched tool 'a' must keep its old pin even though its upstream tag moved; "
        f"before={initial_a}, after={after_a}"
    )


def test_add_fails_when_toml_handedited_since_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add`` fails with exit 65 when ``ocx.toml`` drifted from
    ``ocx.lock`` BEFORE this add (a hand-edit since the last lock). The message
    names ``ocx lock`` as the remedy. The whole-file freshness gate refuses to
    carry untouched bindings forward against a stale lock.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_he_a"
    repo_b = f"t_{short}_he_b"
    repo_new = f"t_{short}_he_new"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=False, cascade=False)
    pkg_new = make_package(ocx, repo_new, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'a = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'b = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    lock_r = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"

    # Hand-edit an UNRELATED binding's tag since the lock — declaration_hash
    # now drifts from the lock's stored hash.
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'a = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'b = "{ocx.registry}/{repo_b}:2.0.0"\n',
    )

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg_new.fq)
    assert result.returncode == EXIT_DATA_ERROR, (
        f"ocx add on a hand-edited ocx.toml must exit {EXIT_DATA_ERROR}; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    combined = (result.stderr + result.stdout).lower()
    assert "ocx lock" in combined or "`ocx lock`" in combined, (
        "error must direct the user to run `ocx lock` to reconcile; "
        f"stderr:\n{result.stderr}\nstdout:\n{result.stdout}"
    )


def test_add_with_no_lock_succeeds(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bootstrap: ``ocx add`` against a project with an ``ocx.toml`` but NO
    ``ocx.lock`` succeeds (direct resolve, never fails closed)."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_nolock"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")
    assert_not_exists(project_dir / "ocx.lock")

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"bootstrap add (no lock) must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project_dir / "ocx.lock").exists(), "ocx.lock must be created by bootstrap add"


def test_add_with_empty_tools_lock_succeeds(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bootstrap: ``ocx add`` against an empty-tools lock (``tools = []``)
    succeeds — the empty carry set resolves only the new binding."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_emptylock"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    # An empty-tools lock current with the empty config (lock first so the
    # declaration_hash matches the empty [tools] table exactly).
    lock_r = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline empty lock failed: {lock_r.stderr}"

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"add against an empty-tools lock must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    lock_text = (project_dir / "ocx.lock").read_text()
    assert _leaves_for_add(lock_text, repo), "the added tool must record leaf digests"


# ---------------------------------------------------------------------------
# Multi-identifier `ocx add a b c` (single flock, single commit, single lock)
# ---------------------------------------------------------------------------


def test_add_multiple_identifiers_in_one_call(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add A B`` adds both bindings to ``ocx.toml`` and both entries to
    ``ocx.lock`` in a single invocation."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_multi_a"
    repo_b = f"t_{short}_multi_b"
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    pkg_b = make_package(ocx, repo_b, "2.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg_a.fq, pkg_b.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add A B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    lock_text = (project_dir / "ocx.lock").read_text()
    for repo in (repo_a, repo_b):
        assert repo in toml_content, f"{repo!r} must be in ocx.toml; got:\n{toml_content}"
        assert repo in lock_text, f"{repo!r} must be in ocx.lock; got:\n{lock_text}"


def test_add_batch_atomic_on_duplicate_in_batch(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add A:1 A`` (two identifiers deriving the same binding key) fails
    fast with UsageError (64) and leaves ``ocx.toml`` byte-unchanged — the
    in-memory stage catches the duplicate before any disk write."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_multi_dup"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project_dir, original_toml)

    # Both identifiers derive binding key = repo basename → duplicate in batch.
    bare = f"{ocx.registry}/{repo}"
    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg.fq, bare)
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"duplicate-in-batch add should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be byte-unchanged when a batch add rejects a duplicate"
    )
    assert_not_exists(project_dir / "ocx.lock")


def test_add_multiple_group_applies_to_all(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --group ci A B`` places both bindings under the ci group."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_grp_a"
    repo_b = f"t_{short}_grp_b"
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    pkg_b = make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--group", "ci", "--no-pull", pkg_a.fq, pkg_b.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --group ci A B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert "ci" in toml_content, f"[group.ci] must appear; got:\n{toml_content}"
    for repo in (repo_a, repo_b):
        assert repo in toml_content, f"{repo!r} must be under the ci group; got:\n{toml_content}"


def test_add_no_pull_batch_stays_cold(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --no-pull A B`` writes both bindings + lock but leaves the
    object store cold for the whole batch."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_np_a"
    repo_b = f"t_{short}_np_b"
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    pkg_b = make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--no-pull", pkg_a.fq, pkg_b.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --no-pull A B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert _packages_present_count(ocx) == 0, (
        "batch --no-pull must not materialize any package into the object store"
    )
