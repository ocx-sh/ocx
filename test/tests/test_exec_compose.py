# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx exec`` Phase 4 unified composition model.

Covers the 14 acceptance-test bullets in plan §4 (lines 681–696):
groups via ``-g``, positional packages with optional ``[name=]identifier``,
overrides, duplicate-across-groups detection, lock-staleness gate, and
the parity guarantee that positional-only invocations stay hermetic.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Exit codes — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_DATA = 65
EXIT_CONFIG = 78


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_exec(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx exec`` with a real ``cwd`` so the project walk fires."""
    cmd = [str(ocx.binary), "exec", *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _run_lock(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), "lock", *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _project_with_default(
    ocx: OcxRunner, project_dir: Path, tools: list[tuple[str, str]]
) -> None:
    """Write an ``ocx.toml`` with a ``[tools]`` block from ``(name, value)`` pairs."""
    body = "[tools]\n" + "\n".join(f'{n} = "{v}"' for n, v in tools) + "\n"
    (project_dir / "ocx.toml").write_text(body)


def _project_with_groups(
    ocx: OcxRunner,
    project_dir: Path,
    tools: list[tuple[str, str]] | None,
    groups: dict[str, list[tuple[str, str]]],
) -> None:
    """Write an ``ocx.toml`` with a top-level ``[tools]`` block and named groups."""
    parts: list[str] = []
    if tools:
        parts.append("[tools]")
        parts.extend(f'{n} = "{v}"' for n, v in tools)
        parts.append("")
    for gname, entries in groups.items():
        parts.append(f"[group.{gname}]")
        parts.extend(f'{n} = "{v}"' for n, v in entries)
        parts.append("")
    (project_dir / "ocx.toml").write_text("\n".join(parts) + "\n")


# ---------------------------------------------------------------------------
# Validation rules — no network required
# ---------------------------------------------------------------------------


def test_exec_no_packages_no_groups_errors_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx exec -- echo hi`` with no identifier source → exit 64."""
    result = _run_exec(ocx, tmp_path, "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "no packages or groups" in result.stderr.lower()


def test_exec_empty_group_segment_errors_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx exec -g ci,,lint -- ...`` → exit 64 (empty segment)."""
    result = _run_exec(ocx, tmp_path, "-g", "ci,,lint", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "empty group segment" in result.stderr.lower()


def test_exec_unknown_group_errors_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``-g nonexistent`` against a project with no such group → exit 64."""
    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [])
    result = _run_exec(ocx, project, "-g", "nonexistent", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "not found in ocx.toml" in result.stderr.lower()


def test_exec_group_without_project_errors_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``-g default`` outside any project → exit 64."""
    project = tmp_path / "no_project"
    project.mkdir()
    # Suppress home-tier fallback so the env is truly project-less.
    env = dict(ocx.env)
    env["OCX_NO_PROJECT"] = "1"
    cmd = [str(ocx.binary), "exec", "-g", "default", "--", "echo", "hi"]
    result = subprocess.run(cmd, cwd=project, capture_output=True, text=True, env=env)
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "requires an ocx.toml" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Lock-presence + staleness gate
# ---------------------------------------------------------------------------


def test_exec_default_group_lock_missing_errors_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``-g default`` with no ``ocx.lock`` → exit 78 (ConfigError)."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_lock_missing"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("hello", f"{ocx.registry}/{repo}:1.0.0")])

    result = _run_exec(ocx, project, "-g", "default", "--", "hello")
    assert result.returncode == EXIT_CONFIG, result.stderr
    assert "ocx.lock not found" in result.stderr.lower()


def test_exec_default_group_stale_lock_errors_65(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Modify ``ocx.toml`` after locking → exit 65 (DataError) on next exec."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_stale_a"
    repo_b = f"t_{short}_stale_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("alpha", f"{ocx.registry}/{repo_a}:1.0.0")])

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, lock.stderr

    # Mutate ocx.toml — declaration hash now differs from the locked value.
    _project_with_default(
        ocx,
        project,
        [
            ("alpha", f"{ocx.registry}/{repo_a}:1.0.0"),
            ("beta", f"{ocx.registry}/{repo_b}:1.0.0"),
        ],
    )

    result = _run_exec(ocx, project, "-g", "default", "--", "alpha")
    assert result.returncode == EXIT_DATA, result.stderr
    assert "stale" in result.stderr.lower() and "ocx lock" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Happy paths — groups
# ---------------------------------------------------------------------------


def test_exec_default_group_runs_locked_tool(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock && ocx exec -g default -- hello`` → tool runs, prints marker."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_default_ok"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("hello", f"{ocx.registry}/{repo}:1.0.0")])

    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    result = _run_exec(ocx, project, "-g", "default", "--", "hello")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg.marker in result.stdout


def test_exec_unions_multiple_groups_comma(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``-g default,ci`` runs the union of both groups."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_uc_a"
    repo_b = f"t_{short}_uc_b"
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False, bins=["alpha"])
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False, bins=["beta"])

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_groups(
        ocx,
        project,
        tools=[("alpha", f"{ocx.registry}/{repo_a}:1.0.0")],
        groups={"ci": [("beta", f"{ocx.registry}/{repo_b}:1.0.0")]},
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run_exec(ocx, project, "-g", "default,ci", "--", "alpha")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg_a.marker in result.stdout


def test_exec_repeated_g_flags_unions(ocx: OcxRunner, tmp_path: Path) -> None:
    """``-g default -g ci -g lint`` → union of all three groups."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_rep_a"
    repo_b = f"t_{short}_rep_b"
    repo_c = f"t_{short}_rep_c"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False, bins=["alpha"])
    pkg_b = make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False, bins=["beta"])
    make_package(ocx, repo_c, "1.0.0", tmp_path, new=True, cascade=False, bins=["gamma"])

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_groups(
        ocx,
        project,
        tools=[("alpha", f"{ocx.registry}/{repo_a}:1.0.0")],
        groups={
            "ci": [("beta", f"{ocx.registry}/{repo_b}:1.0.0")],
            "lint": [("gamma", f"{ocx.registry}/{repo_c}:1.0.0")],
        },
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run_exec(
        ocx, project, "-g", "default", "-g", "ci", "-g", "lint", "--", "beta"
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg_b.marker in result.stdout


def test_exec_duplicate_binding_across_groups_errors_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Two selected groups define the same binding at different digests → exit 64."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_dup"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_groups(
        ocx,
        project,
        tools=None,
        groups={
            "ci": [("hello", f"{ocx.registry}/{repo}:1.0.0")],
            "lint": [("hello", f"{ocx.registry}/{repo}:2.0.0")],
        },
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run_exec(ocx, project, "-g", "ci", "-g", "lint", "--", "hello")
    assert result.returncode == EXIT_USAGE, result.stderr
    # Library-tier message: "tool '<name>' defined in multiple selected groups".
    assert "hello" in result.stderr
    assert "multiple selected groups" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Positional overrides
# ---------------------------------------------------------------------------


def test_exec_positional_overrides_group_by_inferred_binding(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``-g default cmake:3.29`` overrides the locked ``cmake`` (inferred binding)."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_inf"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["hello"])
    pkg2 = make_package(
        ocx, repo, "2.0.0", tmp_path, new=False, cascade=False, bins=["hello"]
    )

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [(repo, f"{ocx.registry}/{repo}:1.0.0")])
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run_exec(
        ocx, project, "-g", "default", f"{ocx.registry}/{repo}:2.0.0", "--", "hello"
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg2.marker in result.stdout


def test_exec_positional_explicit_binding_wins_over_repo_basename(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``alias=ocx.sh/repo:2.0.0`` overrides the ``alias`` binding even when the repo basename differs."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_alias"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["hello"])
    pkg2 = make_package(
        ocx, repo, "2.0.0", tmp_path, new=False, cascade=False, bins=["hello"]
    )

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("alias", f"{ocx.registry}/{repo}:1.0.0")])
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run_exec(
        ocx,
        project,
        "-g",
        "default",
        f"alias={ocx.registry}/{repo}:2.0.0",
        "--",
        "hello",
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg2.marker in result.stdout


def test_exec_positional_with_different_binding_keeps_both(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A positional whose binding does not match any group entry is added fresh."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_keep_a"
    repo_b = f"t_{short}_keep_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False, bins=["alpha"])
    pkg_b = make_package(
        ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False, bins=["beta"]
    )

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("alpha", f"{ocx.registry}/{repo_a}:1.0.0")])
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    # Override the binding name explicitly so it doesn't collide with `alpha`.
    result = _run_exec(
        ocx,
        project,
        "-g",
        "default",
        f"beta={ocx.registry}/{repo_b}:1.0.0",
        "--",
        "beta",
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg_b.marker in result.stdout


# ---------------------------------------------------------------------------
# Hermetic positional-only parity (lock NOT consulted)
# ---------------------------------------------------------------------------


def test_exec_positional_only_in_project_dir_is_hermetic(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Positional-only invocation inside a project dir → lock is NOT consulted.

    The project has a stale lock (`ocx.lock` written, `ocx.toml` modified
    after). With ``-g default`` this would fail with exit 65 (see
    ``test_exec_default_group_stale_lock_errors_65``); with positionals only,
    the run succeeds — proving the staleness gate is gated on group selection.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_herm"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _project_with_default(ocx, project, [("placeholder", f"{ocx.registry}/{repo}:1.0.0")])
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    # Mutate ocx.toml — lock is now stale.
    _project_with_default(ocx, project, [])

    result = _run_exec(ocx, project, f"{ocx.registry}/{repo}:1.0.0", "--", "hello")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg.marker in result.stdout


def test_exec_positional_only_outside_project(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Positional-only invocation with no project in scope → runs as today."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_noproj"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    cwd = tmp_path / "no_project"
    cwd.mkdir()
    env = dict(ocx.env)
    env["OCX_NO_PROJECT"] = "1"
    cmd = [
        str(ocx.binary),
        "exec",
        f"{ocx.registry}/{repo}:1.0.0",
        "--",
        "hello",
    ]
    result = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert pkg.marker in result.stdout
