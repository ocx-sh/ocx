# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx pull`` project-config path (plan Phase 6).

These tests trace one-to-one to the Phase 6 contract in
``.claude/state/plans/plan_project_toolchain.md`` §6 (lines 734–751) plus the
parity invariants borrowed from Phase 4's ``ocx exec`` and the project-config
``Validation Checklist`` (line 934 in particular: the project-config path must
use ``pull_all()`` only — never ``install_all`` / ``find_or_install_all`` and
no symlinks created).

Specification mode (contract-first TDD)
---------------------------------------
The Phase 6 stub at ``crates/ocx_cli/src/command/pull.rs`` returns
``unimplemented!()``. Every test in this file is therefore expected to FAIL
(panic at the unimplemented body) against today's binary — the contract they
encode is the Phase 6 implementation target. Tests assert on exit codes
(stable, sysexits-aligned) and observable side effects (object-store paths
present / symlink tree absent / lockfile bytes unchanged).

Test inventory
--------------
1. ``test_pull_no_args_pulls_all_groups``        — happy path, no-flag invocation
2. ``test_pull_group_filter_pulls_only_named_group``   — ``-g ci`` selects single group
3. ``test_pull_group_default_pulls_only_top_level_tools`` — ``-g default`` reserved name
4. ``test_pull_group_filter_repeated_flag_unions``  — ``-g ci -g lint`` unions groups
5. ``test_pull_no_ocx_toml_exits_64``            — missing config → UsageError
6. ``test_pull_missing_lock_exits_78``           — missing ocx.lock → ConfigError
7. ``test_pull_stale_lock_exits_65``             — declaration_hash mismatch → DataError
8. ``test_pull_unknown_group_exits_64``          — undeclared group name
9. ``test_pull_empty_group_segment_exits_64``    — ``-g ci,,lint`` empty segment
10. ``test_pull_does_not_create_symlinks``        — pull_all-only invariant
11. ``test_pull_idempotent_second_run_no_changes`` — re-run is no-op
12. ``test_pull_does_not_modify_ocx_lock``        — ocx pull re-saves the lock with the same bytes (content preserved, mtime advances)
13. ``test_pull_offline_succeeds_when_objects_already_present`` — offline cache hit
"""
from __future__ import annotations

import re as _re_pull
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src import registry_dir
from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64       # no ocx.toml / unknown group / empty segment
EXIT_DATA = 65        # stale lock (declaration_hash mismatch)
EXIT_CONFIG = 78      # missing ocx.lock when ocx.toml present
EXIT_NOT_FOUND = 79   # tag unresolvable


# ---------------------------------------------------------------------------
# Helpers
#
# Co-located with the tests (DAMP > DRY for acceptance tests, per
# ``quality-core.md``). Mirrors helper shapes in ``test_lock.py`` and
# ``test_exec_compose.py``: subprocess-based to exercise the CWD-walk.
# ---------------------------------------------------------------------------


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    """Build an argv list for ``ocx`` with the runner's isolated env."""
    return [str(ocx.binary), *args]


def _run_pull(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx pull`` with ``cwd`` driving the ``ocx.toml`` CWD-walk.

    ``OcxRunner.run`` does not expose ``cwd=``, so go straight to
    ``subprocess.run`` — same pattern as ``test_lock._run_lock``.
    """
    cmd = _ocx_cmd(ocx, "pull", *extra)
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _run_lock(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx lock --no-pull`` with ``cwd`` driving the ``ocx.toml`` CWD-walk.

    Pull-test setup needs an empty object store as baseline — the test
    delta then measures what ``ocx pull`` materialized. ``ocx lock``
    materializes by default (eager); pass ``--no-pull`` so setup stays
    lock-only. Callers that want the eager-default lock behavior can
    override by passing ``"--pull"`` in ``extra``.
    """
    cmd = _ocx_cmd(ocx, "lock", "--no-pull", *extra)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    """Write an ``ocx.toml`` into ``project_dir`` and return the path."""
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _published_tool(
    ocx: OcxRunner, tmp_path: Path, label: str
) -> tuple[str, str]:
    """Publish a single test package (one tag) and return ``(repo, tag)``.

    ``label`` is a short string (``a``, ``ci``, etc.) embedded in the repo
    name so failure messages map back to the test's role for the package.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_pull_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)
    return repo, tag


def _content_path(ocx_home: Path, registry: str) -> Path:
    """Filesystem prefix under which pulled packages for ``registry`` appear.

    The object store layout is
    ``packages/{registry_dir}/{algo}/{2hex}/{remaining_hex}/content/`` — keyed
    on digest, not repo name. We assert presence by walking the registry-
    scoped subtree for **any** ``content/`` directory: the digest is opaque
    to the test, so a precise content-path assertion would re-implement the
    object-store layout. The walk is sufficient because each test publishes
    its own UUID-prefixed repos, and each registry directory is exclusive to
    a single test's ``OCX_HOME``.
    """
    return ocx_home / "packages" / registry_dir(registry)


def _packages_present_count(ocx_home: Path, registry: str) -> int:
    """Count distinct packages present in the object store for ``registry``.

    Each pulled package contributes exactly one ``content/`` directory
    somewhere under ``packages/{registry_dir}/...``. Counting them lets a
    test assert "exactly N tools pulled" without parsing digests.
    """
    base = _content_path(ocx_home, registry)
    if not base.exists():
        return 0
    return sum(1 for _ in base.rglob("content") if _.is_dir())


def _symlinks_root(ocx_home: Path) -> Path:
    return ocx_home / "symlinks"


# ---------------------------------------------------------------------------
# 1. No-args invocation pulls every tool from every group
# ---------------------------------------------------------------------------


def test_pull_no_args_pulls_all_groups(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock && ocx pull`` (no flags) → every tool present in object store.

    Plan §6: "No args: load ``ocx.toml`` + ``ocx.lock``, collect all tools
    for selected groups, call ``pull_all()`` with digest-pinned identifiers."
    """
    repo_default, tag_default = _published_tool(ocx, tmp_path, "noargs_default")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "noargs_ci")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
default_tool = "{ocx.registry}/{repo_default}:{tag_default}"

[group.ci]
ci_tool = "{ocx.registry}/{repo_ci}:{tag_ci}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 2, (
        f"expected 2 distinct package content/ dirs after pull, got {count}; "
        f"object store root: {_content_path(ocx_home, ocx.registry)}"
    )


# ---------------------------------------------------------------------------
# 2. --group filter restricts to a single named group
# ---------------------------------------------------------------------------


def test_pull_group_filter_pulls_only_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull --group ci`` → only ci-group tools pulled.

    Plan §6: "``--group <name>``: pull only the named group's tools."
    Setup has three buckets (root [tools], [group.ci], [group.lint]); we
    assert that exactly one package — the ci one — lands in the store.
    """
    repo_default, tag_default = _published_tool(ocx, tmp_path, "filter_default")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "filter_ci")
    repo_lint, tag_lint = _published_tool(ocx, tmp_path, "filter_lint")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
default_tool = "{ocx.registry}/{repo_default}:{tag_default}"

[group.ci]
ci_tool = "{ocx.registry}/{repo_ci}:{tag_ci}"

[group.lint]
lint_tool = "{ocx.registry}/{repo_lint}:{tag_lint}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Establish the baseline: count packages in the store before the pull.
    # `ocx lock` only resolves digests — it does NOT pull blobs/layers — so
    # the baseline is zero. Asserting the delta is equal to the expected
    # group size sidesteps any future change that might pre-warm the store
    # (e.g., `ocx lock --pull` mode) without weakening the contract.
    ocx_home = Path(ocx.env["OCX_HOME"])
    baseline = _packages_present_count(ocx_home, ocx.registry)
    assert baseline == 0, (
        f"baseline assumption violated: ocx lock should not pull blobs, "
        f"but {baseline} package content/ dir(s) present pre-pull"
    )

    result = _run_pull(ocx, project, "--group", "ci")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull --group ci failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    after = _packages_present_count(ocx_home, ocx.registry)
    assert after == 1, (
        f"--group ci must pull exactly one tool; found {after} package "
        f"content/ dirs in the object store"
    )


# ---------------------------------------------------------------------------
# 3. -g default selects only top-level [tools]
# ---------------------------------------------------------------------------


def test_pull_group_default_pulls_only_top_level_tools(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull -g default`` → only root ``[tools]`` entries pulled.

    Mirrors the Phase 4 reserved-name semantics for ``ocx exec -g default``:
    the literal name ``default`` selects the unnamed root table only, not
    the named ``[group.*]`` blocks.
    """
    repo_default, tag_default = _published_tool(ocx, tmp_path, "only_default")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "only_ci")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
default_tool = "{ocx.registry}/{repo_default}:{tag_default}"

[group.ci]
ci_tool = "{ocx.registry}/{repo_ci}:{tag_ci}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "-g", "default")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull -g default failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 1, (
        f"-g default must pull exactly the root [tools] entry; found {count} "
        f"package content/ dirs"
    )


# ---------------------------------------------------------------------------
# 4. Repeated -g flags union the selected groups
# ---------------------------------------------------------------------------


def test_pull_group_filter_repeated_flag_unions(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull -g ci -g lint`` → both groups' tools pulled, root [tools] not.

    Mirrors Phase 4's ``test_exec_repeated_g_flags_unions`` /
    ``test_exec_unions_multiple_groups_comma`` — the same union semantics
    apply here.
    """
    repo_default, tag_default = _published_tool(ocx, tmp_path, "rep_default")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "rep_ci")
    repo_lint, tag_lint = _published_tool(ocx, tmp_path, "rep_lint")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
default_tool = "{ocx.registry}/{repo_default}:{tag_default}"

[group.ci]
ci_tool = "{ocx.registry}/{repo_ci}:{tag_ci}"

[group.lint]
lint_tool = "{ocx.registry}/{repo_lint}:{tag_lint}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "-g", "ci", "-g", "lint")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull -g ci -g lint failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 2, (
        f"-g ci -g lint must pull exactly two tools (ci + lint); found "
        f"{count} package content/ dirs"
    )


# ---------------------------------------------------------------------------
# 4b. `-g all` keyword expands to default + every declared group (#176)
# ---------------------------------------------------------------------------


def test_pull_all_keyword_pulls_every_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull -g all`` → default + every ``[group.*]`` warmed (#176).

    ``pull`` previously rejected ``-g all`` ("unknown group"); it now adopts the
    same ``all`` keyword as ``run``/``env``. Distinct from the no-flag default
    (which already warms every tool) — this proves the keyword *resolves*.
    """
    repo_default, tag_default = _published_tool(ocx, tmp_path, "allkw_default")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "allkw_ci")
    repo_lint, tag_lint = _published_tool(ocx, tmp_path, "allkw_lint")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
default_tool = "{ocx.registry}/{repo_default}:{tag_default}"

[group.ci]
ci_tool = "{ocx.registry}/{repo_ci}:{tag_ci}"

[group.lint]
lint_tool = "{ocx.registry}/{repo_lint}:{tag_lint}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "-g", "all")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull -g all must succeed (all keyword adopted); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 3, (
        f"-g all must warm every group (default + ci + lint); found "
        f"{count} package content/ dirs"
    )


# ---------------------------------------------------------------------------
# 5. No ocx.toml → exit 64 (UsageError)
# ---------------------------------------------------------------------------


def test_pull_no_ocx_toml_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` outside any project → exit 64 (UsageError).

    Plan §6: "Error handling: same staleness/missing-lock checks as
    ``ocx exec`` (minus the actual execution step)." Mirrors
    ``test_exec_group_without_project_errors_64`` — sets ``OCX_NO_PROJECT=1``
    so the home-tier fallback (Phase 9) cannot mask a missing project file.
    """
    empty = tmp_path / "no_project"
    empty.mkdir()

    result = _run_pull(
        ocx,
        empty,
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} when no ocx.toml is in scope; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ocx.toml" in result.stderr.lower(), (
        f"stderr must mention ocx.toml; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 6. Missing ocx.lock when ocx.toml present → exit 78 (ConfigError)
# ---------------------------------------------------------------------------


def test_pull_missing_lock_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` with ``ocx.toml`` but no ``ocx.lock`` → exit 78.

    Plan §6 parity invariant with ``ocx exec`` (Phase 4): missing-lock when
    a project config is present is a ConfigError. Mirrors
    ``test_exec_default_group_lock_missing_errors_78``.
    """
    repo, tag = _published_tool(ocx, tmp_path, "missing_lock")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )
    # Deliberately do NOT run `ocx lock` — the lock file must be absent.

    result = _run_pull(ocx, project)

    assert result.returncode == EXIT_CONFIG, (
        f"expected exit {EXIT_CONFIG} when ocx.lock is missing; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    # The diagnostic should point the user at `ocx lock`. The wording can be
    # any of "ocx.lock not found", "run `ocx lock`", etc.; a substring match
    # on the recovery command is the most stable assertion.
    assert "ocx lock" in result.stderr.lower(), (
        f"stderr must reference `ocx lock` (the recovery command); got:\n"
        f"{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 7. Stale lock (declaration_hash mismatch) → exit 65 (DataError)
# ---------------------------------------------------------------------------


def test_pull_stale_lock_exits_65(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Modify ``ocx.toml`` after locking → ``ocx pull`` exits 65.

    Plan §6 parity invariant with ``ocx exec``. Mirrors
    ``test_exec_default_group_stale_lock_errors_65``. ``ocx pull`` is
    strictly read-only on ``ocx.lock``; this test additionally asserts the
    lock bytes are unchanged after the failed run (distinguishes pull from
    ``ocx update``, which would silently re-resolve).
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "stale_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "stale_b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    lock_bytes_before = (project / "ocx.lock").read_bytes()

    # Mutate ocx.toml — declaration_hash now differs from the locked value.
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    result = _run_pull(ocx, project)

    assert result.returncode == EXIT_DATA, (
        f"expected exit {EXIT_DATA} for stale lock; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ocx lock" in result.stderr.lower(), (
        f"stderr must reference `ocx lock` (the recovery command); got:\n"
        f"{result.stderr}"
    )
    # Read-only invariant: pull must not rewrite the lock on a stale-detect.
    lock_bytes_after = (project / "ocx.lock").read_bytes()
    assert lock_bytes_after == lock_bytes_before, (
        "ocx pull must not modify ocx.lock on a stale-lock failure; bytes "
        "differ before/after the failed run"
    )


# ---------------------------------------------------------------------------
# 8. Unknown group → exit 64 (UsageError)
# ---------------------------------------------------------------------------


def test_pull_unknown_group_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull --group does-not-exist`` → exit 64.

    Mirrors ``test_lock_unknown_group_exits_64`` and
    ``test_exec_unknown_group_errors_64`` — selecting a group not declared
    in ``ocx.toml`` is a usage error in every Phase 3+/4+/6+ command.
    """
    repo, tag = _published_tool(ocx, tmp_path, "unknown_group")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "--group", "does-not-exist")

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for unknown group; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "does-not-exist" in result.stderr, (
        f"stderr must name the unknown group; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 9. Empty group segment (-g ci,,lint) → exit 64
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "group_value",
    ["ci,,lint", ",ci", "lint,", ",,"],
    ids=["middle", "leading", "trailing", "degenerate"],
)
def test_pull_empty_group_segment_exits_64(
    ocx: OcxRunner, tmp_path: Path, group_value: str
) -> None:
    """``ocx pull -g <value>`` with any empty comma segment → exit 64.

    Mirrors ``test_lock_empty_group_segment_exits_64`` and
    ``test_exec_empty_group_segment_errors_64``: clap parses the empty
    string at the comma boundary; the runtime validator must reject it.
    Hardened beyond the single middle-segment case (``ci,,lint``) to also
    cover a leading comma (``,ci``), a trailing comma (``lint,``), and a
    comma-only value (``,,``).
    """
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "empty_seg_ci")
    repo_lint, tag_lint = _published_tool(ocx, tmp_path, "empty_seg_lint")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[group.ci]
a = "{ocx.registry}/{repo_ci}:{tag_ci}"

[group.lint]
b = "{ocx.registry}/{repo_lint}:{tag_lint}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "-g", group_value)

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for empty group segment ({group_value!r}); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "empty" in result.stderr.lower() or "segment" in result.stderr.lower(), (
        f"stderr must mention the empty segment / stray comma; got:\n"
        f"{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 10. ocx pull does NOT create symlinks (pull_all-only invariant)
# ---------------------------------------------------------------------------


def test_pull_does_not_create_symlinks(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Critical Phase 6 invariant: ``ocx pull`` uses ``pull_all()``, never
    ``install_all`` / ``find_or_install_all``. No candidate or current
    symlinks may appear under ``OCX_HOME/symlinks/``.

    Plan §6: "**Does not create symlinks.** Uses ``pull_all()`` only."
    Validation Checklist line: "Project-config ``ocx exec`` path uses
    ``pull_all()`` only — no call to ``install_all`` or
    ``find_or_install_all``; no symlinks created" — Phase 6 carries the
    same invariant for ``ocx pull``.

    Compare against ``ocx package pull`` (already covered by
    ``test_package_pull_does_not_create_candidate_symlink``) — same
    parity, distinct command.
    """
    repo, tag = _published_tool(ocx, tmp_path, "no_symlinks")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])

    # Positive control: the package content/ tree exists.
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 1, (
        f"sanity: expected 1 package in object store, got {count}"
    )

    # Invariant: no symlinks/ namespace was created — or, if the directory
    # exists for some unrelated reason, it must contain no candidate /
    # current entries for our test repo.
    symlinks_root = _symlinks_root(ocx_home)
    if symlinks_root.exists():
        # `candidates/` and `current` are the two writable namespaces created
        # by `install_all`. Either entry under our registry/repo path is a
        # contract violation.
        repo_symlink_dir = symlinks_root / registry_dir(ocx.registry) / repo
        offenders: list[Path] = []
        if repo_symlink_dir.exists():
            offenders = [p for p in repo_symlink_dir.rglob("*")]
        assert not offenders, (
            f"ocx pull must not create symlinks for {repo}; found "
            f"{len(offenders)} entry/entries under {repo_symlink_dir}: "
            f"{[str(p) for p in offenders[:5]]}"
        )


# ---------------------------------------------------------------------------
# 11. Idempotent: second run is a no-op (or at least succeeds without error)
# ---------------------------------------------------------------------------


def test_pull_idempotent_second_run_no_changes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` twice on the same project → both succeed; the object
    store is unchanged after the second run.

    The strongest, most-portable observable invariant for "no-op on second
    run" is the package count under ``packages/{registry}/`` — once the
    object store has every locked tool, the second run has nothing to do.
    Timing- or stdout-based signals are flakier (parallel layer fetches,
    progress-bar output) and depend on internals not contractually fixed
    in the plan.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "idem_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "idem_b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:{tag_a}"
b = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    first = _run_pull(ocx, project)
    assert first.returncode == EXIT_SUCCESS, (
        f"first ocx pull failed: rc={first.returncode}\nstderr:\n{first.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count_after_first = _packages_present_count(ocx_home, ocx.registry)
    assert count_after_first == 2, (
        f"after first pull, expected 2 packages in store; got {count_after_first}"
    )

    second = _run_pull(ocx, project)
    assert second.returncode == EXIT_SUCCESS, (
        f"second ocx pull failed: rc={second.returncode}\nstderr:\n{second.stderr}"
    )

    count_after_second = _packages_present_count(ocx_home, ocx.registry)
    assert count_after_second == count_after_first, (
        f"second run must be idempotent on the object store; before={count_after_first} "
        f"after={count_after_second}"
    )


# ---------------------------------------------------------------------------
# 12. ocx pull does NOT modify ocx.lock (read-only on the lock)
# ---------------------------------------------------------------------------


def test_pull_does_not_modify_ocx_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` re-saves ``ocx.lock`` with byte-identical content so its
    mtime advances; the lock text itself is not modified.

    After a successful pull the lock is atomically re-written via
    ``ProjectLock::save`` with the prior lock passed as ``existing`` argument.
    The ``tools_content_equal`` guard inside ``save`` freezes ``generated_at``
    when the resolved content is unchanged, so the resulting bytes are
    identical to the input. The atomic rename still advances the file's mtime,
    which re-fires ``direnv watch_file ocx.lock`` for consumers that rely on
    the mtime change. Asserted here via byte-level equality before/after.
    """
    repo, tag = _published_tool(ocx, tmp_path, "readonly_lock")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    lock_bytes_before = (project / "ocx.lock").read_bytes()

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_bytes_after = (project / "ocx.lock").read_bytes()
    assert lock_bytes_after == lock_bytes_before, (
        "ocx pull must not modify ocx.lock; bytes differ before/after a "
        "successful run — that would make pull a write surface and break "
        "the lock vs update separation"
    )


# ---------------------------------------------------------------------------
# 13. Offline second run succeeds when objects already present
# ---------------------------------------------------------------------------


def test_pull_offline_succeeds_when_objects_already_present(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After populating the object store online, ``ocx --offline pull`` is
    a successful no-op.

    Validates that ``pull_all`` short-circuits via ``find_plain`` when
    every locked digest is already in the content-addressed store — the
    network is not contacted. The ``--offline`` flag turns any latent
    network attempt into a hard error (``OfflineBlocked``, exit 81), so
    a clean exit 0 here proves the cache hit.
    """
    repo, tag = _published_tool(ocx, tmp_path, "offline_cached")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Online warm-up populates the object store.
    online = _run_pull(ocx, project)
    assert online.returncode == EXIT_SUCCESS, (
        f"online warm-up pull failed: rc={online.returncode}\n"
        f"stderr:\n{online.stderr}"
    )

    # Offline second run: every digest is cached, pull_all returns immediately.
    cmd = [str(ocx.binary), "--offline", "pull"]
    offline = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert offline.returncode == EXIT_SUCCESS, (
        f"offline pull on a populated store must succeed; "
        f"rc={offline.returncode}\nstderr:\n{offline.stderr}"
    )


# ---------------------------------------------------------------------------
# 14. --quiet / OCX_QUIET suppress stdout report; default keeps it
# ---------------------------------------------------------------------------


def test_pull_quiet_flag_produces_empty_stdout(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull --quiet`` writes nothing to stdout while still succeeding.

    Quiet is opt-in; pre-existing default-stdout coverage in this file
    already locks in the table format. Stderr is intentionally not asserted
    on — progress and errors continue to surface there.
    """
    repo, tag = _published_tool(ocx, tmp_path, "quiet_flag")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # ``--quiet`` is a global flag; it must precede the subcommand.
    cmd = [str(ocx.binary), "--quiet", "pull"]
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --quiet pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stdout == "", (
        f"ocx --quiet pull must produce empty stdout; got:\n{result.stdout!r}"
    )


def test_pull_quiet_env_var_produces_empty_stdout(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_QUIET=1 ocx pull`` is equivalent to ``ocx pull --quiet``.

    Locks in the env-var fallback path — same suppression, no flag.
    """
    repo, tag = _published_tool(ocx, tmp_path, "quiet_env")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, extra_env={"OCX_QUIET": "1"})
    assert result.returncode == EXIT_SUCCESS, (
        f"OCX_QUIET=1 ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stdout == "", (
        f"OCX_QUIET=1 ocx pull must produce empty stdout; got:\n{result.stdout!r}"
    )


def test_pull_default_stdout_not_empty(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Without ``--quiet`` / ``OCX_QUIET``, ``ocx pull`` still emits the
    standard report (regression guard against accidental quiet default).
    """
    repo, tag = _published_tool(ocx, tmp_path, "loud_default")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert result.stdout != "", (
        "ocx pull without --quiet must emit a report on stdout"
    )


# ---------------------------------------------------------------------------
# 15. --jobs / OCX_JOBS — concurrency cap on the outer pull dispatch
# ---------------------------------------------------------------------------


def test_pull_jobs_one_serial_completes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_JOBS=1 ocx pull`` against a multi-tool fixture exits 0.

    Behavior smoke: the semaphore-gated outer dispatch must not deadlock
    when serialised, and packages must still all land in the store.
    Stronger timing-based parallelism assertions belong in a perf suite —
    this test guards the deadlock-safety invariant only.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "jobs1_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "jobs1_b")
    repo_c, tag_c = _published_tool(ocx, tmp_path, "jobs1_c")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:{tag_a}"
b = "{ocx.registry}/{repo_b}:{tag_b}"
c = "{ocx.registry}/{repo_c}:{tag_c}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, extra_env={"OCX_JOBS": "1"})
    assert result.returncode == EXIT_SUCCESS, (
        f"OCX_JOBS=1 ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 3, (
        f"expected 3 packages after serialised pull; got {count}"
    )


def test_pull_jobs_zero_resolves_to_cores(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --jobs 0 pull`` resolves to ``num_cpus`` (not an error).

    Diverges from Cargo's ``--jobs 0`` (which errors). OCX follows GNU
    Parallel convention so CI matrices can request "saturate this runner"
    without computing the core count themselves.
    """
    repo, tag = _published_tool(ocx, tmp_path, "jobs0")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    cmd = [str(ocx.binary), "--jobs", "0", "pull"]
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --jobs 0 pull must succeed (cores convention); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


def test_pull_jobs_negative_rejected_at_parse(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--jobs -1`` is rejected by clap (usize parser); exit code != 0."""
    cmd = [str(ocx.binary), "--jobs", "-1", "pull"]
    result = subprocess.run(
        cmd,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode != EXIT_SUCCESS, (
        "ocx --jobs -1 must fail at clap parse time"
    )


def test_pull_dry_run_lists_all_locked_tools_no_writes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull --dry-run`` previews the lock without touching the store.

    Asserts: exit 0, structured JSON enumerating every locked tool with a
    `would-fetch` status (since the store is empty), and no `content/`
    directory created under ``packages/``.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "dryrun_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "dryrun_b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:{tag_a}"
b = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    cmd = [str(ocx.binary), "--format", "json", "pull", "--dry-run"]
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull --dry-run failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    import json as _json

    payload = _json.loads(result.stdout)
    assert isinstance(payload, list), f"expected JSON array, got {type(payload).__name__}"
    assert len(payload) == 2, f"expected 2 entries, got {len(payload)}"
    statuses = {entry["status"] for entry in payload}
    assert statuses == {"would-fetch"}, (
        f"all entries should be would-fetch on a cold store; got {statuses}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count == 0, (
        f"--dry-run must not write to the object store; found {count} packages"
    )


def test_pull_dry_run_after_warm_pull_reports_cached(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After a real pull populates the store, ``--dry-run`` reports `cached`.

    Locks in dry-run ↔ pull_all agreement: both use ``find_plain`` as their
    first decision, so a cached entry on dry-run means the real pull would
    short-circuit for that package.
    """
    repo, tag = _published_tool(ocx, tmp_path, "dryrun_warm")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    warm = _run_pull(ocx, project)
    assert warm.returncode == EXIT_SUCCESS, warm.stderr

    cmd = [str(ocx.binary), "--format", "json", "pull", "--dry-run"]
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    import json as _json

    payload = _json.loads(result.stdout)
    assert len(payload) == 1, f"expected 1 entry, got {len(payload)}"
    assert payload[0]["status"] == "cached", (
        f"warmed entry must report cached; got {payload[0]}\n"
        f"stderr:\n{result.stderr}\nstdout:\n{result.stdout}"
    )
    assert payload[0]["path"] is not None, (
        "cached entry must include a path"
    )


def test_pull_emits_package_root_not_content_dir(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` and ``ocx pull --dry-run`` both emit package-root paths.

    Locks the contract documented on ``api/data/paths.rs::PathEntry`` and
    ``api/data/pull_dry_run.rs::DryRunEntry``: the ``Path`` column / JSON
    ``path`` field is the package root (parent of ``content/`` and
    ``entrypoints/``), never the ``content/`` subdirectory. Consumers (CI
    scripts, project tooling) traverse into ``<root>/content/`` themselves
    or, preferably, consume ``ocx env``.

    Asserts on both code paths:

    1. ``ocx pull --format json`` (real pull, ``command/pull.rs`` line ~140)
    2. ``ocx pull --dry-run --format json`` cached entry (line ~175)

    A regression in either site would surface a trailing ``/content`` and
    a missing ``<path>/content`` subdir on disk.
    """
    repo, tag = _published_tool(ocx, tmp_path, "pull_root")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # ── Phase 1: real pull emits a root path ─────────────────────────────
    pull = subprocess.run(
        _ocx_cmd(ocx, "--format", "json", "pull"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert pull.returncode == EXIT_SUCCESS, (
        f"ocx pull failed: rc={pull.returncode}\nstderr:\n{pull.stderr}"
    )

    import json as _json

    pull_payload = _json.loads(pull.stdout)
    # ``Paths`` serializes as an object keyed by the input identifier; one entry here.
    assert len(pull_payload) == 1, f"expected 1 pull entry, got {pull_payload}"
    pull_path_str = next(iter(pull_payload.values()))
    pull_path = Path(pull_path_str)
    assert pull_path.name != "content", (
        f"ocx pull emitted content/ instead of package root: {pull_path_str}"
    )
    assert (pull_path / "content").is_dir(), (
        f"package root {pull_path_str} must contain a content/ subdir"
    )

    # ── Phase 2: dry-run cached entry emits a root path ──────────────────
    dry = subprocess.run(
        _ocx_cmd(ocx, "--format", "json", "pull", "--dry-run"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert dry.returncode == EXIT_SUCCESS, (
        f"ocx pull --dry-run failed: rc={dry.returncode}\nstderr:\n{dry.stderr}"
    )

    dry_payload = _json.loads(dry.stdout)
    assert len(dry_payload) == 1, f"expected 1 dry-run entry, got {dry_payload}"
    cached = dry_payload[0]
    assert cached["status"] == "cached", cached
    dry_path = Path(cached["path"])
    assert dry_path.name != "content", (
        f"dry-run emitted content/ instead of package root: {cached['path']}"
    )
    assert (dry_path / "content").is_dir(), (
        f"dry-run package root {cached['path']} must contain a content/ subdir"
    )
    # Pull and dry-run must agree on the root for the same identifier.
    assert dry_path == pull_path, (
        f"pull root ({pull_path}) and dry-run root ({dry_path}) must agree"
    )


def test_pull_dry_run_does_not_modify_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--dry-run`` is read-only on ``ocx.lock`` — bytes AND mtime unchanged.

    Extended in Phase-5 to also assert mtime is preserved, locking down the
    no-touch contract: ``--dry-run`` must never advance the lock's mtime even
    when the real pull would.
    """
    repo, tag = _published_tool(ocx, tmp_path, "dryrun_lock")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    lock_path = project / "ocx.lock"
    lock_bytes_before = lock_path.read_bytes()
    mtime_before = lock_path.stat().st_mtime_ns

    result = _run_pull(ocx, project, "--dry-run")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    lock_bytes_after = lock_path.read_bytes()
    mtime_after = lock_path.stat().st_mtime_ns

    assert lock_bytes_before == lock_bytes_after, (
        "ocx pull --dry-run must not modify ocx.lock bytes"
    )
    assert mtime_after == mtime_before, (
        "ocx pull --dry-run must not advance ocx.lock mtime"
    )


def test_pull_dry_run_stale_lock_exits_65_before_preview(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The staleness gate fires ahead of the dry-run branch.

    A stale lock must exit 65 even with ``--dry-run`` — the preview is not
    a way to bypass declaration_hash validation.
    """
    repo, tag = _published_tool(ocx, tmp_path, "dryrun_stale")

    project = tmp_path / "proj"
    project.mkdir()
    config_path = _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Mutate ocx.toml so declaration_hash no longer matches the lock.
    config_path.write_text(
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"

[group.extra]
ignored = "{ocx.registry}/{repo}:{tag}"
"""
    )

    result = _run_pull(ocx, project, "--dry-run")
    assert result.returncode == EXIT_DATA, (
        f"stale lock + --dry-run must exit {EXIT_DATA}; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


def test_pull_jobs_cli_overrides_env(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Explicit ``--jobs`` wins over ``OCX_JOBS``.

    Locks in the precedence chain: the CLI flag is the last word. We can't
    cheaply observe the active permit count from outside, so the assertion
    is liveness only — both env and flag set, exit 0, store populated.
    """
    repo, tag = _published_tool(ocx, tmp_path, "jobs_override")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    cmd = [str(ocx.binary), "--jobs", "4", "pull"]
    env = dict(ocx.env)
    env["OCX_JOBS"] = "1"
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"OCX_JOBS=1 + --jobs 4 must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Phase-5 contract: ``ocx pull`` touches ``ocx.lock`` mtime on success
# ---------------------------------------------------------------------------


def test_pull_advances_lock_mtime_on_success(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` re-saves ``ocx.lock`` (same bytes, new mtime) so that
    direnv's ``watch_file`` directive re-fires after a successful pull.

    We sleep 1 second before pulling to ensure the filesystem mtime granularity
    on coarse-grained filesystems (1-second resolution) produces a distinct
    mtime_ns value. The assertion is post_mtime_ns > pre_mtime_ns.

    Plan Phase-5 Step 3.5 contract. This test will FAIL against the stub
    because ``pull.rs`` has a ``TODO(phase-5)`` in place of the lock-touch.
    """
    import time

    repo, tag = _published_tool(ocx, tmp_path, "mtime_advance")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    lock_path = project / "ocx.lock"
    lock_bytes_before = lock_path.read_bytes()

    # Sleep to ensure a distinct mtime on coarse (1-second) filesystems.
    time.sleep(1.1)

    mtime_ns_before = lock_path.stat().st_mtime_ns

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    mtime_ns_after = lock_path.stat().st_mtime_ns
    lock_bytes_after = lock_path.read_bytes()

    assert lock_bytes_after == lock_bytes_before, (
        "ocx pull must not change the content of ocx.lock"
    )
    assert mtime_ns_after > mtime_ns_before, (
        "ocx pull must advance ocx.lock mtime so direnv watch_file re-fires; "
        f"mtime before={mtime_ns_before} after={mtime_ns_after}"
    )


# ---------------------------------------------------------------------------
# Lock: ``ocx pull`` resolves the host-leaf digest from [tool.platforms]
# ---------------------------------------------------------------------------

_LEAF_RE_PULL = _re_pull.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_pull_resolves_host_leaf_from_platforms_table(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull`` resolves each tool's host-platform leaf digest from
    ``[tool.platforms]``.

    Scenario: publish, ``ocx lock`` (``--no-pull`` to start cold), then
    ``ocx pull``. Assert:
    1. The lock carries ``[tool.platforms]``, no ``pinned =`` line, at least
       one leaf digest recorded.
    2. ``ocx pull`` exits 0 and the package appears in the object store.
    """
    repo, tag = _published_tool(ocx, tmp_path, "v3pull")

    project = tmp_path / "proj_v3pull"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
the_tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)  # --no-pull is the default helper behaviour
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    lock_text = (project / "ocx.lock").read_text()
    assert "[tool.platforms]" in lock_text, (
        "lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_PULL.findall(lock_text)
    assert leaf_digests, "lock must record at least one leaf digest"
    assert "pinned =" not in lock_text, (
        "lock must not carry a legacy `pinned` line"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    count_before = _packages_present_count(ocx_home, ocx.registry)
    assert count_before == 0, (
        "object store must be cold before pull (--no-pull lock setup)"
    )

    result = _run_pull(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull must succeed (resolves host-leaf from [tool.platforms]); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    count_after = _packages_present_count(ocx_home, ocx.registry)
    assert count_after >= 1, (
        f"at least one package must be present after ocx pull; got {count_after}"
    )


def test_pull_repeated_platform_flag_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx pull --platform=A --platform=B`` always exits 64 before any
    network I/O — `--platform` takes at most one value for every resolution
    command (D4 of `adr_platform_model_unification.md`); a second occurrence
    is a clap usage error regardless of the lock's format version.
    """
    repo, tag = _published_tool(ocx, tmp_path, "multiplatform")

    project = tmp_path / "proj_multiplatform"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
the_tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_pull(ocx, project, "--platform=linux/amd64", "--platform=linux/arm64")

    assert result.returncode == EXIT_USAGE, (
        f"repeated --platform must exit {EXIT_USAGE}; got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "cannot be used multiple times" in result.stderr, (
        f"stderr must name the repeated-flag usage error; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Gap 3 — host-platform absent from lock → clean pre-network error (exit 78)
# ---------------------------------------------------------------------------


def test_pull_host_platform_absent_exits_78_before_network(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A lock whose ``[tool.platforms]`` carries only a non-host key causes
    ``ocx pull`` to exit **78** (``ConfigError``) with an error mentioning
    ``ocx update``, and the failure fires **before** any network I/O (proven by
    identical behaviour under ``--offline``).

    Setup:
    1. Push a normal package and write a lock with ``ocx lock --no-pull`` so
       we have a real declaration_hash and a syntactically valid repository.
    2. Rewrite the lock's ``[tool.platforms]`` to contain only a dummy
       platform key that is *not* the host platform — simulating "publisher
       ships only for an OS the host runner doesn't run."
    3. Run ``ocx pull`` → expect exit 78.
    4. Run ``ocx --offline pull`` → expect the same exit 78 (same error,
       proving the check is pre-network and not triggered by a registry 404).
    """
    repo, tag = _published_tool(ocx, tmp_path, "absent_host")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    # Populate a lock without warming the object store.
    lock_result = _run_lock(ocx, project)
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"setup lock failed: rc={lock_result.returncode}\nstderr:\n{lock_result.stderr}"
    )
    lock_text = (project / "ocx.lock").read_text()

    # Extract fields needed to reconstruct a syntactically-valid lock.
    decl_match = _re_pull.search(
        r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', lock_text
    )
    repo_match = _re_pull.search(r'repository\s*=\s*"([^"]+)"', lock_text)
    assert decl_match and repo_match, (
        f"lock must carry declaration_hash and repository; got:\n{lock_text[:400]}"
    )
    decl_hash = decl_match.group(1)
    bare_repo = repo_match.group(1)

    # Craft a fake leaf digest for a non-host platform.  We use "darwin/arm64"
    # as a portable non-host key on the Linux CI runner (and on darwin/amd64
    # runners).  We pick the lexicographically smallest key that is NOT the
    # host platform's canonical key to ensure the host-key lookup fails.
    fake_leaf = "b" * 64
    non_host_platform = "darwin/arm64"

    (project / "ocx.lock").write_text(
        f"""\
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "tool"
group = "default"
repository = "{bare_repo}"

[tool.platforms]
"{non_host_platform}" = "sha256:{fake_leaf}"
"""
    )

    # Online run — expect exit 78, error mentions ocx update.
    result_online = _run_pull(ocx, project)
    assert result_online.returncode == EXIT_CONFIG, (
        f"ocx pull with host platform absent must exit 78 (ConfigError); "
        f"got {result_online.returncode}\nstderr:\n{result_online.stderr}"
    )
    assert "ocx update" in result_online.stderr, (
        f"stderr must mention 'ocx update'; got:\n{result_online.stderr}"
    )

    # Offline run — same exit code and same error text, proving pre-network.
    # ``--offline`` is a root flag (must appear before the subcommand).
    cmd_offline = [str(ocx.binary), "--offline", "pull"]
    result_offline = subprocess.run(
        cmd_offline,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result_offline.returncode == EXIT_CONFIG, (
        f"ocx --offline pull with host platform absent must also exit 78 "
        f"(pre-network check); got {result_offline.returncode}\n"
        f"stderr:\n{result_offline.stderr}"
    )
    assert "ocx update" in result_offline.stderr, (
        f"offline stderr must still mention 'ocx update'; "
        f"got:\n{result_offline.stderr}"
    )
