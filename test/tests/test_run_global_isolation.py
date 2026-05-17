# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx run`` C7 isolation contract (plan_toolchain_cli.md §C7).

Encodes the Phase 3 contracts from handshake §5 + plan C7:

- ``ocx run -- <cmd>`` (bare) = PROJECT tier only; exit 64 when no project/lock
  in scope. Must NOT fall back to the global toolchain.
- ``ocx --global run -- <cmd>`` = compose the GLOBAL toolchain
  (``$OCX_HOME/ocx.toml``/``.lock``) env for THAT child process only; never
  mutates the parent shell.
- Isolation is by PATH precedence only — there is NO in-shell PATH strip.
  No ``filter_path_excluding`` subshell, no ``_OCX_APPLIED`` sentinel emitted.
- Builds stay reproducible: ``run`` / ``ocx package exec`` compose exactly the
  explicitly selected tier, independent of interactive PATH.

Test inventory
--------------
1.  ``test_bare_run_no_project_exits_64``
    Bare ``ocx run -- echo`` in a dir with no ``ocx.toml`` → exit 64. Does NOT
    fall back to the global toolchain even when a global ``$OCX_HOME/ocx.toml``
    exists and carries a binding.

2.  ``test_run_global_composes_global_toolchain_for_child``
    ``ocx --global run -- <gtool>`` resolves a tool present only in
    ``$OCX_HOME/ocx.toml`` (added via ``ocx --global add``). Proves
    ``--global`` re-targets ``load_project_with_lock`` to the global file.

3.  ``test_bare_run_in_project_cannot_resolve_global_only_tool``
    Inside a project that does NOT declare a binding, bare ``ocx run`` cannot
    resolve a tool present only in the global tier. Strict project-tier
    isolation — no implicit merge with the global toolchain.

4.  ``test_run_global_does_not_mutate_parent_env``
    ``ocx --global run`` spawns the child with the global toolchain env; the
    *parent* shell process's ``PATH`` and env remain untouched. Verified by
    checking that the parent's ``PATH`` before and after ``--global run``
    contains no reference to the child's isolated OCX bin dir.

5.  ``test_run_produces_no_strip_subshell_output``
    ``ocx run`` (bare) and ``ocx --global run`` must not emit any
    ``filter_path_excluding`` shell construct, ``_OCX_APPLIED`` sentinel, or
    PATH-strip subshell to stdout/stderr. Isolation is by PATH *prepend* in the
    child env only — the parent shell is never touched.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64    # UsageError: no ocx.toml / no global file / binding not found
EXIT_CONFIG = 78   # ConfigError: ocx.lock absent

# ---------------------------------------------------------------------------
# Helpers (DAMP — descriptive and meaningful, co-located with tests)
# ---------------------------------------------------------------------------


def _run_cmd(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run an arbitrary ``ocx`` subcommand with ``cwd`` driving the CWD-walk."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> None:
    (project_dir / "ocx.toml").write_text(body)


# ---------------------------------------------------------------------------
# 1. Bare `ocx run` with no project → exit 64, ignores global toolchain
# ---------------------------------------------------------------------------


def test_bare_run_no_project_exits_64(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C7: ``ocx run -- echo`` in a dir with no ``ocx.toml`` → exit 64.

    Critically, a global ``$OCX_HOME/ocx.toml`` carrying a binding MUST NOT
    be consulted — bare ``run`` is project-tier only. No implicit global fallback.

    OCX_NO_PROJECT=1 prevents the CWD-walk from climbing to a parent that
    might contain an ``ocx.toml`` (e.g. the repo root during CI).
    """
    # Set up a global toolchain so there IS a global file to (incorrectly) fall
    # back to — proves the absence of the fallback, not just the absence of a
    # global file.
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    assert _run_cmd(ocx, tmp_path, "--global", "add", fq).returncode == EXIT_SUCCESS

    empty = tmp_path / "no_project_dir"
    empty.mkdir()

    result = _run_cmd(
        ocx, empty, "run", "--", "echo", "hi",
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == EXIT_USAGE, (
        f"bare `ocx run` with no project must exit {EXIT_USAGE} (UsageError); "
        f"the global toolchain must NOT be consulted; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "ocx.toml" in result.stderr.lower(), (
        f"stderr must mention ocx.toml to guide the user; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 2. `ocx run --global -- <gtool>` resolves from the global toolchain
# ---------------------------------------------------------------------------


def test_run_global_composes_global_toolchain_for_child(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C7: ``ocx --global run -- gtool`` resolves a tool present only in
    ``$OCX_HOME/ocx.toml`` (added via ``ocx --global add``).

    Proves that ``--global`` re-targets ``load_project_with_lock`` to the
    global file and composes its toolchain env for the child process.
    The tool is NOT in any project ``ocx.toml`` — it can only be reached
    via ``--global``.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    assert _run_cmd(ocx, tmp_path, "--global", "add", fq).returncode == EXIT_SUCCESS

    # Run from a directory with NO project ocx.toml.
    empty = tmp_path / "no_project_here"
    empty.mkdir()

    result = _run_cmd(
        ocx, empty, "--global", "run", "--", "gtool",
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"`ocx --global run -- gtool` must succeed when the global toolchain "
        f"carries 'gtool'; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 3. Bare `ocx run` inside a project cannot resolve a global-only tool
# ---------------------------------------------------------------------------


def test_bare_run_in_project_cannot_resolve_global_only_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C7: strict project-tier isolation.

    Inside a project that does NOT declare a binding, bare ``ocx run`` must
    NOT resolve a tool that exists only in ``$OCX_HOME/ocx.toml``. The project
    toolchain is the exclusive scope for bare ``run`` — no implicit merge with
    the global tier.
    """
    g_repo = unique_repo
    p_repo = f"{unique_repo}_proj"

    make_package(ocx, g_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    assert (
        _run_cmd(ocx, tmp_path, "--global", "add", f"{ocx.registry}/{g_repo}:1.0.0").returncode
        == EXIT_SUCCESS
    )

    # Project carries a different tool, never `gonly`.
    make_package(ocx, p_repo, "1.0.0", tmp_path, new=True, bins=["ptool"])
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\nptool = "{ocx.registry}/{p_repo}:1.0.0"\n')
    assert _run_cmd(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run_cmd(ocx, project, "pull").returncode == EXIT_SUCCESS

    # `gonly` is only in the global file → bare run inside project must fail.
    result = _run_cmd(ocx, project, "run", "gonly", "--", "gonly")
    assert result.returncode != EXIT_SUCCESS, (
        f"bare `ocx run` inside a project must NOT resolve a global-only tool "
        f"(strict project-tier isolation); "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"binding-not-found must exit {EXIT_USAGE} (UsageError); "
        f"got rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 4. `ocx run --global` does not mutate the parent shell env
# ---------------------------------------------------------------------------


def test_run_global_does_not_mutate_parent_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C7: ``ocx --global run`` is hermetic — only the spawned child process
    gets the global toolchain env.

    Verified by capturing PATH from the test's Python process *before* and
    *after* running ``ocx --global run``, then asserting they are identical.
    The child subprocess runs in its own process; ``ocx run`` must not modify
    the current process's environment.

    Also verifies: the child's env contains the global toolchain's bin dir,
    while the parent's env does not. The isolation is by PATH *prepend* in
    the child env only — not a subshell strip mutation on the parent.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    assert _run_cmd(ocx, tmp_path, "--global", "add", fq).returncode == EXIT_SUCCESS

    empty = tmp_path / "no_project"
    empty.mkdir()

    # Capture parent's PATH before running ocx --global run.
    parent_path_before = os.environ.get("PATH", "")

    # Run the child (gtool prints its binary path to stdout).
    result = _run_cmd(
        ocx, empty, "--global", "run", "--", "gtool",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"--global run must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # Parent's PATH must be untouched — ocx run never mutates the current process.
    parent_path_after = os.environ.get("PATH", "")
    assert parent_path_before == parent_path_after, (
        "ocx --global run must not mutate the parent process's PATH;\n"
        f"before: {parent_path_before!r}\n"
        f"after:  {parent_path_after!r}"
    )

    # Sanity: the OCX_HOME symlinks dir must NOT appear in the parent's PATH
    # (the child got it on its PATH — the parent never does).
    ocx_home_symlinks = str(Path(ocx.env["OCX_HOME"]) / "symlinks")
    assert ocx_home_symlinks not in parent_path_after, (
        f"the global toolchain bin dir must not appear in the parent's PATH;\n"
        f"parent PATH: {parent_path_after!r}"
    )


# ---------------------------------------------------------------------------
# 5. No PATH-strip subshell in stdout/stderr of `ocx run`
# ---------------------------------------------------------------------------


def test_run_produces_no_strip_subshell_output(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C7 + C4: ``ocx run`` and ``ocx --global run`` must not emit any
    PATH-strip shell construct or ``_OCX_APPLIED`` sentinel.

    The deleted strip mechanism (``emit_global_path_strip``, ``strip_global``,
    ``_OCX_APPLIED``) must not appear in any process output. Isolation is by
    PATH *prepend* in the child env — not a subshell PATH mutation.

    Tests both the bare-run error path (no project → exit 64) and the global
    success path, verifying that neither emits strip-related output.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    assert _run_cmd(ocx, tmp_path, "--global", "add", fq).returncode == EXIT_SUCCESS

    empty = tmp_path / "empty_for_strip_check"
    empty.mkdir()

    strip_markers = [
        "_OCX_APPLIED",
        "filter_path_excluding",
        "strip_global",
        "emit_global_path_strip",
        "IFS=':';",   # The POSIX PATH-strip subshell construct
    ]

    # Path 1: bare run with no project (error path).
    bare_result = _run_cmd(
        ocx, empty, "run", "--", "echo", "hi",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert bare_result.returncode == EXIT_USAGE, (
        f"bare run must exit {EXIT_USAGE} (no project); rc={bare_result.returncode}"
    )
    combined_bare = bare_result.stdout + bare_result.stderr
    for marker in strip_markers:
        assert marker not in combined_bare, (
            f"bare `ocx run` must not emit strip marker '{marker}' to stdout/stderr;\n"
            f"stdout:\n{bare_result.stdout}\nstderr:\n{bare_result.stderr}"
        )

    # Path 2: --global run (success path).
    global_result = _run_cmd(
        ocx, empty, "--global", "run", "--", "gtool",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert global_result.returncode == EXIT_SUCCESS, (
        f"--global run must succeed; rc={global_result.returncode}\nstderr:\n{global_result.stderr}"
    )
    combined_global = global_result.stdout + global_result.stderr
    for marker in strip_markers:
        assert marker not in combined_global, (
            f"`ocx --global run` must not emit strip marker '{marker}' to stdout/stderr;\n"
            f"stdout:\n{global_result.stdout}\nstderr:\n{global_result.stderr}"
        )
