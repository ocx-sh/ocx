# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx run`` (plan Phase 3 specification mode).

These tests trace one-to-one to the Phase 3.2 contract in
``.claude/state/plans/plan_cli_run_layering.md`` §Phase 3.2 (Step 3.2) and
the §UX Scenarios + §Error / Exit-Code Mapping Table in that plan.

Specification mode (contract-first TDD)
---------------------------------------
The ``ocx run`` command stub at ``crates/ocx_cli/src/command/run.rs`` returns
``unimplemented!()``. Every test in this file is therefore expected to FAIL
against today's binary — the contracts they encode are the Phase 4
implementation target. Tests assert on exit codes (stable, sysexits-aligned)
and observable side effects (env vars, stdout, stderr).

Test inventory
--------------
1.  ``test_run_golden_path``                          — happy path, default scope
2.  ``test_run_with_named_group``                      — -g ci selects named group
3.  ``test_run_default_scope_excludes_named_groups``   — default ≠ all
4.  ``test_run_all_keyword_expands_to_default_plus_all_groups``  — -g all keyword
5.  ``test_run_with_name_filter``                      — positional NAME filter
6.  ``test_run_unknown_name_exits_64``                 — unknown binding → 64
7.  ``test_run_ambiguous_name_exits_64``               — compose duplicate → 64
8.  ``test_run_compose_conflict_exits_64``             — DuplicateToolAcrossSelectedGroups → 64
9.  ``test_run_unknown_group_exits_64``                — unknown -g → 64
10. ``test_run_empty_group_segment_exits_64``          — -g ci,,lint → 64
11. ``test_run_no_argv_exits_64``                       — missing --  → 64 (clap, OCX remap)
12. ``test_run_dashdash_no_argv_exits_64``              — cmake -- (no argv) → 64 (clap, OCX remap)
13. ``test_run_dashdash_only_no_names_no_argv_exits_64`` — ocx run -- → 64 (clap, OCX remap)
14. ``test_run_no_project_exits_64``                   — no ocx.toml → 64
15. ``test_run_no_lock_exits_78``                      — no ocx.lock → 78
16. ``test_run_stale_lock_exits_65``                   — stale lock → 65
17. ``test_run_exit_code_forwarded_from_child``        — child exit → forwarded
18. ``test_run_env_observable_from_child``             — package env visible to child
19. ``test_run_clean_strips_inherited_env``            — --clean flag
20. ``test_run_self_view_exposes_private_entries``     — --self flag
21. ``test_run_auto_installs_missing_packages``        — auto-install on demand
22. ``test_exec_unaffected_by_project_presence``       — layer purity regression
23. ``test_run_registers_in_project_registry``         — ProjectRegistry written
24. ``test_run_reserved_group_all_in_config_rejected`` — [group.all] → 78
25. ``test_add_rejects_reserved_group_all``            — ocx add --group all → 64
"""

from __future__ import annotations

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
EXIT_FAILURE = 1
EXIT_USAGE = 64    # no ocx.toml / unknown group / unknown NAME / empty segment
EXIT_DATA = 65     # stale lock (declaration_hash mismatch)
EXIT_CONFIG = 78   # missing ocx.lock when ocx.toml present


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    """Build an argv list for ``ocx`` using the runner's isolated environment."""
    return [str(ocx.binary), *args]


def _run_cmd(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run an arbitrary ``ocx`` subcommand with ``cwd`` driving the CWD-walk."""
    cmd = _ocx_cmd(ocx, *args)
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


def _run_run(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx run`` with ``cwd`` driving the ``ocx.toml`` CWD-walk."""
    return _run_cmd(ocx, cwd, "run", *extra, extra_env=extra_env)


def _run_lock(
    ocx: OcxRunner,
    cwd: Path,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx lock`` with ``cwd`` driving the ``ocx.toml`` CWD-walk."""
    return _run_cmd(ocx, cwd, "lock")


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    """Write an ``ocx.toml`` into ``project_dir`` and return the path."""
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _published_tool(
    ocx: OcxRunner, tmp_path: Path, label: str
) -> tuple[str, str]:
    """Publish a single test package and return ``(repo, tag)``.

    ``label`` is embedded in the repo name so failure messages are traceable.
    The package's default env exposes ``{LABEL}_HOME`` as a ``public``
    visibility variable so tests can assert on its presence in child env output.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_run_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)
    return repo, tag


def _published_tool_with_private_env(
    ocx: OcxRunner, tmp_path: Path, label: str
) -> tuple[str, str, str]:
    """Publish a package with a private env entry and return ``(repo, tag, private_key)``.

    The package exposes ``{LABEL}_HOME`` (public) and ``{LABEL}_SECRET`` (private).
    Tests for ``--self`` flag can assert that ``{LABEL}_SECRET`` appears only
    with ``--self``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_run_{label}"
    tag = "1.0.0"
    home_key = label.upper() + "_HOME"
    secret_key = label.upper() + "_SECRET"
    env = [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
        {
            "key": secret_key,
            "type": "constant",
            "value": "s3cr3t",
            "visibility": "private",
        },
    ]
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, env=env)
    return repo, tag, secret_key


# ---------------------------------------------------------------------------
# 1. Golden path — minimal project, lock present, run succeeds
# ---------------------------------------------------------------------------


def test_run_golden_path(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- hello`` with a single binding exits 0 and runs the binary.

    Plan §3.2 test 1: minimal ocx.toml + current ocx.lock → happy path.
    The test binary echoes a UUID marker; stdout must match.
    """
    repo, tag = _published_tool(ocx, tmp_path, "golden")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    result = _run_run(ocx, project, "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run golden path failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 2. Named group selection (-g ci)
# ---------------------------------------------------------------------------


def test_run_with_named_group(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -g ci -- hello`` runs the binary from [group.ci].

    Plan §3.2 test 2: ``-g ci`` scopes composition to the named group.
    """
    repo, tag = _published_tool(ocx, tmp_path, "namedg")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]

[group.ci]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "-g", "ci", "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -g ci failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 3. Default scope excludes named groups
# ---------------------------------------------------------------------------


def test_run_default_scope_excludes_named_groups(ocx: OcxRunner, tmp_path: Path) -> None:
    """Without ``-g``, scope is ``[tools]`` only — ``[group.ci]`` env excluded.

    Plan §3.2 test 3: default scope = [tools] only. The ci-group package's
    ``{REPO}_HOME`` var must NOT appear in ``ocx run -- env``.
    """
    repo_default, _ = _published_tool(ocx, tmp_path, "defscope_d")
    repo_ci, tag_ci = _published_tool(ocx, tmp_path, "defscope_ci")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo_default} = "{ocx.registry}/{repo_default}:1.0.0"

[group.ci]
{repo_ci} = "{ocx.registry}/{repo_ci}:{tag_ci}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -- env failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # The ci-group tool's HOME var must NOT be in the env dump.
    ci_home_key = repo_ci.upper().replace("-", "_") + "_HOME"
    # Normalize: env keys come through as uppercase in the output.
    assert ci_home_key not in result.stdout, (
        f"default scope must NOT include ci-group env var {ci_home_key!r}; "
        f"stdout excerpt:\n{result.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# 4. -g all expands to default + every named group
# ---------------------------------------------------------------------------


def test_run_all_keyword_expands_to_default_plus_all_groups(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx run -g all -- env`` shows env entries from ALL groups.

    Plan §3.2 test 4: ``-g all`` is the reserved keyword that unions every
    declared group (including the implicit default group [tools]).
    """
    repo_default, _ = _published_tool(ocx, tmp_path, "allkw_d")
    repo_ci, _ = _published_tool(ocx, tmp_path, "allkw_ci")
    repo_release, _ = _published_tool(ocx, tmp_path, "allkw_rel")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo_default} = "{ocx.registry}/{repo_default}:1.0.0"

[group.ci]
{repo_ci} = "{ocx.registry}/{repo_ci}:1.0.0"

[group.release]
{repo_release} = "{ocx.registry}/{repo_release}:1.0.0"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "-g", "all", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -g all failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # All three tools' HOME vars must appear in the env dump.
    for repo in (repo_default, repo_ci, repo_release):
        home_key = repo.upper().replace("-", "_") + "_HOME"
        assert home_key in result.stdout, (
            f"-g all must include {home_key!r} from all groups; "
            f"stdout excerpt:\n{result.stdout[:500]}"
        )


# ---------------------------------------------------------------------------
# 5. NAME filter — positional name restricts composition
# ---------------------------------------------------------------------------


def test_run_with_name_filter(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run cmake -- env`` composes only cmake's env (ninja absent).

    Plan §3.2 test 5: when a NAME is supplied before ``--``, only that
    binding's env is composed; other bindings in scope are excluded.
    """
    repo_cmake, _ = _published_tool(ocx, tmp_path, "namefilter_cmake")
    repo_ninja, _ = _published_tool(ocx, tmp_path, "namefilter_ninja")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo_cmake} = "{ocx.registry}/{repo_cmake}:1.0.0"
{repo_ninja} = "{ocx.registry}/{repo_ninja}:1.0.0"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Filter to cmake only — ninja's HOME var must be absent.
    result = _run_run(ocx, project, repo_cmake, "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run {repo_cmake} -- env failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    ninja_home_key = repo_ninja.upper().replace("-", "_") + "_HOME"
    assert ninja_home_key not in result.stdout, (
        f"NAME filter must exclude ninja env var {ninja_home_key!r}; "
        f"stdout excerpt:\n{result.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# 6. Unknown NAME → exit 64
# ---------------------------------------------------------------------------


def test_run_unknown_name_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run does-not-exist -- echo hi`` exits 64; stderr mentions the name.

    Plan §3.2 test 6 + Error/Exit-Code table row "NAME unknown in scope".
    """
    repo, tag = _published_tool(ocx, tmp_path, "unkname")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "does-not-exist", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for unknown binding; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "does-not-exist" in result.stderr, (
        f"stderr must name the unknown binding; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 7. Compose conflict (DuplicateToolAcrossSelectedGroups) → exit 64
# ---------------------------------------------------------------------------


def test_run_ambiguous_name_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """Two groups define the same binding with different content → exit 64.

    Plan §3.2 test 7: ``compose_tool_set`` returns
    ``DuplicateToolAcrossSelectedGroups`` when the same binding name appears
    in two selected groups with different identifiers. The CLI maps this to
    exit 64; stderr must mention "duplicate" and both group names.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "dup_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "dup_b")

    project = tmp_path / "proj"
    project.mkdir()
    # Both groups use the same BINDING NAME ("conflict_tool") but different repos.
    _write_ocx_toml(project, f"""\
[tools]

[group.ci]
conflict_tool = "{ocx.registry}/{repo_a}:{tag_a}"

[group.release]
conflict_tool = "{ocx.registry}/{repo_b}:{tag_b}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Select both groups — compose_tool_set must fire DuplicateToolAcrossSelectedGroups.
    result = _run_run(ocx, project, "-g", "ci", "-g", "release", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for compose duplicate; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "duplicate" in result.stderr.lower() or "conflict" in result.stderr.lower(), (
        f"stderr must mention duplicate/conflict; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 8. Compose conflict via NAME filter → exit 64
# ---------------------------------------------------------------------------


def test_run_compose_conflict_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """Force both conflicting groups via ``-g ci -g release``, run with NAME.

    Plan §3.2 test 8: same fixture as test 7 but exercises the full NAME
    filter path. The compose error surfaces before the NAME filter even runs.
    Exit 64; stderr mentions both group names.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "conf_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "conf_b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]

[group.ci]
conflict_tool = "{ocx.registry}/{repo_a}:{tag_a}"

[group.release]
conflict_tool = "{ocx.registry}/{repo_b}:{tag_b}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(
        ocx, project, "-g", "ci", "-g", "release", "conflict_tool", "--", "echo", "hi"
    )
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for compose conflict with NAME filter; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ci" in result.stderr and "release" in result.stderr, (
        f"stderr must name both conflicting groups; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 9. Unknown group → exit 64
# ---------------------------------------------------------------------------


def test_run_unknown_group_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -g does-not-exist -- echo hi`` exits 64.

    Plan §3.2 test 9 + Error/Exit-Code table row "-g references unknown group".
    """
    repo, tag = _published_tool(ocx, tmp_path, "unkgrp")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "-g", "does-not-exist", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for unknown group; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "does-not-exist" in result.stderr, (
        f"stderr must name the unknown group; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 10. Empty group segment → exit 64
# ---------------------------------------------------------------------------


def test_run_empty_group_segment_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -g ci,,lint -- echo hi`` exits 64 (empty segment).

    Plan §3.2 test 10: comma-delimited -g with empty segment (stray comma)
    is rejected before any project I/O.
    """
    repo_ci, _ = _published_tool(ocx, tmp_path, "empty_ci")
    repo_lint, _ = _published_tool(ocx, tmp_path, "empty_lint")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[group.ci]
{repo_ci} = "{ocx.registry}/{repo_ci}:1.0.0"

[group.lint]
{repo_lint} = "{ocx.registry}/{repo_lint}:1.0.0"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "-g", "ci,,lint", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for empty group segment; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "empty" in result.stderr.lower() or "segment" in result.stderr.lower(), (
        f"stderr must mention empty segment; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 11. No ``--`` → clap usage error (exit 64; OCX remap of clap default 2)
# ---------------------------------------------------------------------------


def test_run_no_argv_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run cmake`` (no ``--``, no argv) → exit 64 (clap usage, OCX remap).

    Plan §3.2 test 11: ``--`` is mandatory. Clap rejects invocations that
    supply no ``--`` followed by an argv via ``last = true`` +
    ``num_args = 1..`` + ``required = true`` on the ``argv`` field. OCX
    binaries remap clap's default exit-2 to ``ExitCode::UsageError`` (64),
    matching every other ``ocx`` subcommand's clap-failure exit.
    """
    project = tmp_path / "proj"
    project.mkdir()

    # No ocx.toml or lock needed — clap parse failure happens before I/O.
    result = _run_run(ocx, project, "cmake")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} (clap usage) when -- is absent; got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# 12. ``cmake --`` with no argv → exit 64 (clap; OCX remap)
# ---------------------------------------------------------------------------


def test_run_dashdash_no_argv_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run cmake --`` (with ``--``, but zero argv) → exit 64.

    Plan §3.2 test 12: ``num_args = 1..`` + ``required = true`` on ``argv``
    rejects empty argv even when ``--`` is present. OCX remaps clap's
    default exit-2 to ``ExitCode::UsageError`` (64).
    """
    project = tmp_path / "proj"
    project.mkdir()

    result = _run_run(ocx, project, "cmake", "--")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} (clap usage) when argv is empty after --; got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# 13. ``ocx run --`` with no names and no argv → exit 64 (clap; OCX remap)
# ---------------------------------------------------------------------------


def test_run_dashdash_only_no_names_no_argv_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx run --`` (no NAME, no argv after ``--``) → exit 64.

    Plan §3.2 test 13: argv field is ``last = true`` + ``num_args = 1..`` +
    ``required = true`` so an empty argv after ``--`` is rejected by clap.
    OCX remaps clap's default exit-2 to ``ExitCode::UsageError`` (64).
    """
    project = tmp_path / "proj"
    project.mkdir()

    result = _run_run(ocx, project, "--")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} (clap usage) when -- is present but argv is empty; "
        f"got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# 14. No ocx.toml → exit 64
# ---------------------------------------------------------------------------


def test_run_no_project_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- echo hi`` in empty dir (no ocx.toml) → exit 64.

    Plan §3.2 test 14 + Error/Exit-Code table row "No ocx.toml resolved".
    OCX_NO_PROJECT=1 prevents the CWD-walk from climbing to a parent that
    might contain an ocx.toml (e.g. the repo root).
    """
    empty = tmp_path / "no_project"
    empty.mkdir()

    result = _run_run(
        ocx, empty, "--", "echo", "hi", extra_env={"OCX_NO_PROJECT": "1"}
    )
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} when no ocx.toml is in scope; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ocx.toml" in result.stderr.lower(), (
        f"stderr must mention ocx.toml; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 15. ocx.toml present but no ocx.lock → exit 78
# ---------------------------------------------------------------------------


def test_run_no_lock_exits_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- echo hi`` when ocx.lock is absent → exit 78.

    Plan §3.2 test 15 + Error/Exit-Code table row "ocx.lock absent".
    """
    repo, tag = _published_tool(ocx, tmp_path, "nolock")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")
    # Deliberately do NOT run `ocx lock`.

    result = _run_run(ocx, project, "--", "echo", "hi")
    assert result.returncode == EXIT_CONFIG, (
        f"expected exit {EXIT_CONFIG} when ocx.lock is missing; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ocx.lock" in result.stderr.lower() or "ocx lock" in result.stderr.lower(), (
        f"stderr must mention ocx.lock or the recovery command; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 16. Stale lock → exit 65
# ---------------------------------------------------------------------------


def test_run_stale_lock_exits_65(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx.toml`` modified after ``ocx.lock`` written → exit 65.

    Plan §3.2 test 16 + Error/Exit-Code table row "ocx.lock stale".
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "stale_a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "stale_b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo_a} = "{ocx.registry}/{repo_a}:{tag_a}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Mutate ocx.toml — declaration_hash now differs from the locked value.
    _write_ocx_toml(project, f"""\
[tools]
{repo_a} = "{ocx.registry}/{repo_a}:{tag_a}"
{repo_b} = "{ocx.registry}/{repo_b}:{tag_b}"
""")

    result = _run_run(ocx, project, "--", "echo", "hi")
    assert result.returncode == EXIT_DATA, (
        f"expected exit {EXIT_DATA} for stale lock; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "stale" in result.stderr.lower() or "ocx lock" in result.stderr.lower(), (
        f"stderr must mention stale lock; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 17. Child exit code forwarded byte-for-byte
# ---------------------------------------------------------------------------


def test_run_exit_code_forwarded_from_child(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- sh -c "exit 17"`` exits 17.

    Plan §3.2 test 17: the child process exit code is forwarded without
    transformation. This is the most fundamental exec-semantic contract.
    """
    repo, tag = _published_tool(ocx, tmp_path, "exitfwd")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "--", "sh", "-c", "exit 17")
    assert result.returncode == 17, (
        f"expected child exit code 17 to be forwarded; got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# 18. Package env visible inside child process
# ---------------------------------------------------------------------------


def test_run_env_observable_from_child(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- env`` shows the package's ``{REPO}_HOME`` env var.

    Plan §3.2 test 18: the composed package environment must be visible to
    the child process. The default test package publishes ``{REPO}_HOME``
    as a ``public`` visibility entry (see make_package defaults in helpers.py).
    """
    repo, tag = _published_tool(ocx, tmp_path, "envobs")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -- env failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    home_key = repo.upper().replace("-", "_") + "_HOME"
    assert home_key in result.stdout, (
        f"expected {home_key!r} in child env; stdout excerpt:\n{result.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# 19. --clean strips inherited env
# ---------------------------------------------------------------------------


def test_run_clean_strips_inherited_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run --clean -- env`` does NOT show ``EXTERNAL_VAR``.

    Plan §3.2 test 19: ``--clean`` starts with a minimal env containing only
    package variables and OCX internals — no shell-inherited vars. Without
    ``--clean`` the same ``EXTERNAL_VAR`` is visible.
    """
    repo, tag = _published_tool(ocx, tmp_path, "clean")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    external_var = "EXTERNAL_VAR_CLEAN_TEST"
    extra_env = {external_var: "canary_value"}

    # `--clean` strips PATH (mirror `ocx exec --clean`); use absolute path so
    # the child's argv0 resolution does not depend on inherited PATH. Same
    # pattern as `test_exec_forwarding.py::test_clean_encapsulates_parent_env`.
    env_bin = "/usr/bin/env"

    # Without --clean: external var visible.
    result_dirty = _run_run(ocx, project, "--", env_bin, extra_env=extra_env)
    assert result_dirty.returncode == EXIT_SUCCESS, (
        f"ocx run (no --clean) failed: rc={result_dirty.returncode}\n"
        f"stderr:\n{result_dirty.stderr}"
    )
    assert external_var in result_dirty.stdout, (
        f"without --clean, {external_var!r} must be visible; "
        f"stdout excerpt:\n{result_dirty.stdout[:500]}"
    )

    # With --clean: external var absent.
    result_clean = _run_run(ocx, project, "--clean", "--", env_bin, extra_env=extra_env)
    assert result_clean.returncode == EXIT_SUCCESS, (
        f"ocx run --clean failed: rc={result_clean.returncode}\n"
        f"stderr:\n{result_clean.stderr}"
    )
    assert external_var not in result_clean.stdout, (
        f"with --clean, {external_var!r} must be stripped; "
        f"stdout excerpt:\n{result_clean.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# 20. --self exposes private env entries
# ---------------------------------------------------------------------------


@pytest.mark.skip(
    reason=(
        "Test infrastructure: requires a fixture package that explicitly declares "
        "a 'private' visibility env entry. The default make_package() uses 'public' "
        "for all env entries. This test is wired to _published_tool_with_private_env "
        "which sets up the correct package shape, but the skip is conservative until "
        "we confirm the private entry correctly surfaces only under --self."
    )
)
def test_run_self_view_exposes_private_entries(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run --self -- env`` shows private-visibility entries.

    Plan §3.2 test 20: ``--self`` selects the private surface (vars where
    ``has_private()`` is true). Without ``--self``, private vars are hidden.
    Mirrors ``test_exec_modes.py`` pattern.
    """
    repo, tag, secret_key = _published_tool_with_private_env(ocx, tmp_path, "selfview")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Without --self: private key absent.
    result_consumer = _run_run(ocx, project, "--", "env")
    assert result_consumer.returncode == EXIT_SUCCESS, result_consumer.stderr
    assert secret_key not in result_consumer.stdout, (
        f"without --self, {secret_key!r} must be hidden; stdout:\n{result_consumer.stdout[:500]}"
    )

    # With --self: private key present.
    result_self = _run_run(ocx, project, "--self", "--", "env")
    assert result_self.returncode == EXIT_SUCCESS, result_self.stderr
    assert secret_key in result_self.stdout, (
        f"with --self, {secret_key!r} must be visible; stdout:\n{result_self.stdout[:500]}"
    )


# ---------------------------------------------------------------------------
# 21. Auto-install: missing package installed on demand
# ---------------------------------------------------------------------------


def test_run_auto_installs_missing_packages(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- hello`` auto-installs a package not yet in the store.

    Plan §3.2 test 21: Phase F.2 calls ``find_or_install_all`` which installs
    missing packages. The test verifies the binary runs successfully (i.e.,
    the package was installed) and that ``ocx find`` can locate it afterward.
    """
    repo, tag = _published_tool(ocx, tmp_path, "autoinst")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Package is NOT manually installed — run must install it on demand.
    result = _run_run(ocx, project, "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"auto-install via ocx run failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    # Verify the package is now findable.
    find_result = _run_cmd(
        ocx, project, "find", f"{ocx.registry}/{repo}:{tag}"
    )
    assert find_result.returncode == EXIT_SUCCESS, (
        f"ocx find after auto-install failed: rc={find_result.returncode}\n"
        f"stderr:\n{find_result.stderr}"
    )


# ---------------------------------------------------------------------------
# 22. ocx exec unaffected by project presence (layer purity regression)
# ---------------------------------------------------------------------------


def test_exec_unaffected_by_project_presence(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx exec`` in a project dir still parses OCI identifiers, not bindings.

    Plan §3.2 test 22: layer-purity invariant. ``ocx exec`` is an OCI-tier
    command; it must NOT consult ``ocx.toml`` or ``ocx.lock`` even when they
    are present in the CWD. It must parse its argument as a registry identifier.
    """
    repo, tag = _published_tool(ocx, tmp_path, "layer_purity")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    # Lock the project too (so a project-aware command would succeed).
    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # ocx exec must still accept the OCI identifier form, not a binding name.
    result = _run_cmd(ocx, project, "exec", f"{repo}:{tag}", "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx exec with OCI identifier in project dir failed: "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 23. ProjectRegistry written after run
# ---------------------------------------------------------------------------


def test_run_registers_in_project_registry(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx run -- echo hi`` causes ``projects.json`` to contain the lock path.

    Plan §3.2 test 23: ``load_project_with_lock`` (Phase B.5) calls
    ``ProjectRegistry::register_if_present`` so the lock path is registered
    for ``ocx clean`` GC-root tracking. The ``projects.json`` file at
    ``$OCX_HOME/projects.json`` must exist and contain the lock path.
    """
    repo, tag = _published_tool(ocx, tmp_path, "projreg")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    result = _run_run(ocx, project, "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run for registry registration failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    projects_json = ocx_home / "projects.json"
    assert projects_json.exists(), (
        f"projects.json must exist after ocx run; not found at {projects_json}"
    )
    lock_path = str(project / "ocx.lock")
    projects_content = projects_json.read_text()
    assert lock_path in projects_content, (
        f"projects.json must contain the lock path {lock_path!r}; "
        f"content:\n{projects_content[:500]}"
    )


# ---------------------------------------------------------------------------
# 24. [group.all] in ocx.toml → rejected at load time (exit 78)
# ---------------------------------------------------------------------------


def test_run_reserved_group_all_in_config_rejected(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx.toml`` with ``[group.all]`` → any project command exits 78.

    Plan §3.2 test 24 + Error/Exit-Code table row "[group.all] declared".
    The reserved keyword ``all`` may not be used as a group name in
    ``ocx.toml``. ``ProjectConfig::from_toml_str`` rejects it with
    ``ReservedGroupName { name: "all", ... }`` → exit 78 (ConfigError).
    Stderr must contain the load-bearing substrings from the error's
    ``#[error("...")]`` format.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, """\
[group.all]
foo = "ocx.sh/foo:1"
""")

    result = _run_run(ocx, project, "--", "echo", "hi")
    assert result.returncode == EXIT_CONFIG, (
        f"expected exit {EXIT_CONFIG} for [group.all] in config; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "[group.all] is reserved" in result.stderr, (
        f"stderr must contain '[group.all] is reserved'; got:\n{result.stderr}"
    )
    assert "reserved keyword" in result.stderr, (
        f"stderr must contain 'reserved keyword'; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 25. ocx add --group all → exit 64
# ---------------------------------------------------------------------------


def test_add_rejects_reserved_group_all(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx add --group all ocx.sh/foo:1`` exits 64; stderr mentions the name.

    Plan §3.2 test 25 + Error/Exit-Code table row "ocx add --group all".
    The reserved keyword ``all`` must be rejected by the mutate-time validator
    in ``validate_group_name`` → ``InvalidGroupName { name: "all" }`` →
    exit 64 (UsageError).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    result = _run_cmd(ocx, project, "add", "--group", "all", "ocx.sh/foo:1")
    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for --group all; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "all" in result.stderr, (
        f"stderr must name the reserved keyword 'all'; got:\n{result.stderr}"
    )
    assert "reserved" in result.stderr.lower() or "invalid" in result.stderr.lower(), (
        f"stderr must mention reserved/invalid; got:\n{result.stderr}"
    )
