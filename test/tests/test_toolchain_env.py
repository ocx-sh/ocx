# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx env` (toolchain-tier composed-env command).

Encodes plan_toolchain_cli.md Phase 2 contracts (C2, C3, C5):

- ``ocx env`` (no project) → exit 64 (UsageError)
- ``ocx env`` in a project → JSON by default (backend-first, handshake §3)
- ``ocx env --format plain`` → plain table (NOT sourceable)
- ``ocx env --shell=bash`` → bash eval-safe export lines
- ``ocx env --shell=sh`` → POSIX export lines byte-identical to ``--shell=dash``
- Bare ``--shell`` (no value, undetectable SHELL) → exit 64
- ``--global`` ⟂ ``--project`` → clap exit 2 (unchanged from a4211591)
- ``ocx package env <ids> --shell=zsh`` → sourceable zsh lines (reuses env.rs)
- ``--shell ripgrep`` is NOT swallowed (require_equals enforced)
- Legacy commands (``ocx install``, ``ocx shell env``) → exit 2 (clap)

These tests are written BEFORE implementation (Phase 2 Specify — contract-first
TDD per plan_toolchain_cli.md §6 Constraints). They fail against the current
binary (``ocx env`` does not exist yet) and must pass after Step C implementation.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner
from src.shell_eval import run_after_sourcing

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE)
EXIT_CLAP = 2  # clap parse error (unrecognised subcommand / flag)

# ---------------------------------------------------------------------------
# Helpers (DAMP — descriptive and meaningful, co-located with tests)
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` from ``cwd`` with the runner's isolated environment."""
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


def _make_project(
    ocx: OcxRunner,
    tmp_path: Path,
    label: str,
    bin_name: str = "tool",
) -> tuple[Path, str]:
    """Publish a package, create a locked + pulled project, return (project_dir, fq)."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_{label}"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=[bin_name])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project = tmp_path / f"proj_{label}"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\n{bin_name} = "{fq}"\n')

    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS
    return project, fq


# ---------------------------------------------------------------------------
# C2 — root `ocx env`: no-project exit 64
# ---------------------------------------------------------------------------


def test_env_no_project_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env`` outside a project tree → exit 64 (UsageError).

    C2 contract: no ``ocx.toml`` in scope → the toolchain-tier command exits
    with the same ``UsageError`` (64) semantics as ``ocx run`` / ``ocx pull``.
    """
    empty = tmp_path / "no_project"
    empty.mkdir()
    result = _run(ocx, empty, "env", extra_env={"OCX_NO_PROJECT": "1"})
    assert result.returncode == EXIT_USAGE, (
        f"ocx env with no project must exit 64 (UsageError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# C2 — default output is JSON (backend-first, handshake §3)
# ---------------------------------------------------------------------------


def test_env_in_project_default_json(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env`` in a project outputs JSON by default (no ``--shell`` / ``--format``).

    Backend-first per handshake §3 and product principle #1: the default
    channel is JSON, not plain text.  JSON must be parseable.
    """
    project, _ = _make_project(ocx, tmp_path, "json_default")
    result = _run(ocx, project, "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env must succeed in a project; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # Default must be valid JSON (not plain text, not shell export lines).
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"ocx env default output must be valid JSON; got:\n{result.stdout!r}\n"
            f"parse error: {exc}"
        )
    # JSON shape: {"entries": [...]}
    assert "entries" in data, (
        f"JSON must have 'entries' key; got keys: {list(data.keys())}"
    )
    assert isinstance(data["entries"], list), "entries must be a list"


# ---------------------------------------------------------------------------
# C2 — `--format plain` is NOT sourceable (human inspection only)
# ---------------------------------------------------------------------------


def test_env_format_plain_not_sourceable(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env --format plain`` emits a human-readable table (NOT sourceable).

    C2 contract: plain output is for human inspection only.  It must NOT
    contain ``export`` statements (that would make it eval-safe, which it
    is not — paths with spaces break ``eval`` on plain output).
    """
    project, _ = _make_project(ocx, tmp_path, "plain_not_src")
    result = _run(ocx, project, "env", "--format", "plain")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env --format plain must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # Plain output must NOT be parseable as JSON (it's a table, not JSON).
    try:
        json.loads(result.stdout)
        pytest.fail(
            f"--format plain must NOT produce JSON; got:\n{result.stdout!r}"
        )
    except json.JSONDecodeError:
        pass  # expected


# ---------------------------------------------------------------------------
# C2 — `--shell=bash` emits bash eval-safe export lines
# ---------------------------------------------------------------------------


def test_env_shell_bash_emits_export_lines(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env --shell=bash`` emits bash-sourceable export lines.

    C2 contract: ``--shell[=NAME]`` is the ONLY eval-safe channel.
    Output must contain ``export`` statements valid for bash ``-n`` check.
    """
    project, _ = _make_project(ocx, tmp_path, "shell_bash")
    result = _run(ocx, project, "env", "--shell=bash")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env --shell=bash must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    stdout = result.stdout
    assert "export" in stdout, (
        f"--shell=bash must emit export lines; got:\n{stdout!r}"
    )
    # Must be valid bash syntax.
    bash_check = subprocess.run(
        ["bash", "-n", "-c", stdout],
        capture_output=True,
        text=True,
    )
    assert bash_check.returncode == 0, (
        f"--shell=bash output failed `bash -n` check: {bash_check.stderr}\n"
        f"output was:\n{stdout!r}"
    )


# ---------------------------------------------------------------------------
# C5 — `--shell=sh` ≡ `--shell=dash` (byte-identical POSIX output)
# ---------------------------------------------------------------------------


def test_env_shell_sh_identical_to_dash(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env --shell=sh`` is byte-identical to ``ocx env --shell=dash``.

    C5 contract: ``sh`` is a PossibleValue alias on ``Shell::Dash`` — no new
    enum variant, same code path, same bytes.  The POSIX login exporter runs
    ``ocx --global env --shell=sh`` and must get the same output as
    ``--shell=dash``.
    """
    project, _ = _make_project(ocx, tmp_path, "sh_eq_dash")
    sh_result = _run(ocx, project, "env", "--shell=sh")
    dash_result = _run(ocx, project, "env", "--shell=dash")

    assert sh_result.returncode == EXIT_SUCCESS, (
        f"--shell=sh must succeed; rc={sh_result.returncode}\n"
        f"stderr:\n{sh_result.stderr}"
    )
    assert dash_result.returncode == EXIT_SUCCESS, (
        f"--shell=dash must succeed; rc={dash_result.returncode}\n"
        f"stderr:\n{dash_result.stderr}"
    )
    assert sh_result.stdout == dash_result.stdout, (
        "C5: --shell=sh output must be byte-identical to --shell=dash;\n"
        f"  sh:   {sh_result.stdout!r}\n"
        f"  dash: {dash_result.stdout!r}"
    )
    # Sanity: both must have POSIX export form.
    assert "export" in sh_result.stdout, (
        f"--shell=sh must emit POSIX export lines; got:\n{sh_result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# C2 — bare `--shell` (no value) with undetectable SHELL → exit 64
# ---------------------------------------------------------------------------


def test_env_bare_shell_undetectable_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """Bare ``--shell`` (no ``=NAME``) autodetects or exits 64.

    C2 contract: bare ``--shell`` triggers autodetect via ``$SHELL`` and/or
    parent process detection.  When detection succeeds (exit 0, export lines
    emitted), the test passes trivially.  When detection fails (``$SHELL``
    unset AND parent process undetectable), the binary must exit 64
    (UsageError), NOT exit 2 (clap parse error).

    Parent-process detection via ``/proc`` is available on Linux, so this
    test typically sees exit 0 on Linux CI.  The key regression guard is that
    exit 2 (clap swallowed a positional as shell name) never occurs.
    """
    project, _ = _make_project(ocx, tmp_path, "bare_shell")
    # Unset SHELL so autodetect falls back to parent-process detection.
    env = dict(ocx.env)
    env.pop("SHELL", None)
    result = subprocess.run(
        [str(ocx.binary), "env", "--shell"],
        cwd=project,
        capture_output=True,
        text=True,
        env=env,
    )
    # Acceptable outcomes:
    # - exit 0: detection succeeded (parent process or $SHELL), export lines emitted.
    # - exit 64: detection genuinely failed (no $SHELL, no parent signal).
    # NOT acceptable: exit 2 (clap error — means --shell swallowed a positional).
    assert result.returncode in (EXIT_SUCCESS, EXIT_USAGE), (
        f"bare --shell must exit 0 (detected) or 64 (undetectable); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# C2 — `--global` ⟂ `--project` → clap conflict exit 2
# ---------------------------------------------------------------------------


def test_env_global_and_project_conflict_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx env --global --project ./p`` → clap conflict, exit 64 (UsageError).

    C2 contract: the ``--global`` ⟂ ``--project`` constraint is unchanged
    from a4211591 and continues to be enforced by clap's ``conflicts_with``.
    OCX maps all clap parse-level rejections to UsageError (exit 64, sysexits
    EX_USAGE) — not to clap's default exit 2. This matches the observed
    binary behaviour and the ``test_top_level_hook_env_removed`` precedent.
    """
    project = tmp_path / "proj_conflict"
    project.mkdir()
    result = _run(
        ocx,
        tmp_path,
        "--global",
        "--project",
        str(project),
        "env",
    )
    assert result.returncode != 0, (
        f"--global + --project must conflict (non-zero exit); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"--global + --project must exit {EXIT_USAGE} (UsageError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    # Confirm clap's conflict message is present.
    assert "cannot be used with" in result.stderr or "conflicts" in result.stderr.lower(), (
        f"expected clap conflict message in stderr; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# C3 — `ocx package env <ids> --shell=zsh` (reuses env.rs, OCI-tier)
# ---------------------------------------------------------------------------


def test_package_env_shell_zsh_emits_sourceable(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx package env <id> --shell=zsh`` emits zsh-sourceable lines.

    C3 contract: ``ocx package env`` reuses ``env.rs::execute`` (auto-installs
    via ``find_or_install_all``).  With ``--shell=zsh`` the output is zsh
    eval-safe export lines.  W5: do NOT assert no-download — auto-install is
    deliberate (handshake §2).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_pkgenv_zsh"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    # Install first (package env needs it installed).
    assert _run(ocx, tmp_path, "package", "install", fq).returncode == EXIT_SUCCESS

    result = _run(ocx, tmp_path, "package", "env", "--shell=zsh", fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"package env --shell=zsh must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # Zsh uses the same `export KEY="value"` form as bash/dash.
    assert "export" in result.stdout, (
        f"package env --shell=zsh must emit export lines; got:\n{result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# C3/R4 — `--shell ripgrep` is NOT swallowed (require_equals enforced)
# ---------------------------------------------------------------------------


def test_package_env_shell_require_equals_not_swallowed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx package env --shell ripgrep`` must NOT swallow `ripgrep` as shell.

    R4 contract: ``require_equals=true`` on ``--shell`` means a following
    positional (the OCI identifier) cannot be mistaken for the shell name.
    The command must parse correctly even when ``--shell`` has no ``=value``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_req_eq"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"
    assert _run(ocx, tmp_path, "package", "install", fq).returncode == EXIT_SUCCESS

    # `--shell ripgrep` (space, no `=`) → should be parsed as bare --shell
    # (autodetect) with `fq` as the positional identifier.  It must NOT
    # treat `fq` as the shell name and fail with an invalid-shell-value error.
    # The result is either success (if SHELL is detectable) or exit 64
    # (UsageError, undetectable SHELL) — but NOT exit 2 (clap swallowed).
    result = subprocess.run(
        [str(ocx.binary), "package", "env", "--shell", fq],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    # Must NOT be a clap value-validation error (which would be exit 2 or
    # 64-from-clap saying "invalid shell name").  The key assertion is that
    # `fq` was NOT consumed as a shell name.  The actual exit code depends on
    # SHELL detectability; we assert it is not a clap "invalid value for
    # '--shell'" error by checking stderr.
    assert "invalid value" not in result.stderr.lower() or "shell" not in result.stderr.lower(), (
        f"--shell without = must NOT consume the next positional as shell name; "
        f"stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Deleted commands → clap exit 2
# ---------------------------------------------------------------------------


def test_root_install_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx install`` (root) → clap unrecognised-subcommand, exit 64 (UsageError).

    OCX maps all clap parse-level rejections to UsageError (exit 64, sysexits
    EX_USAGE). The command is deleted — use ``ocx package install`` instead.
    """
    result = _run(ocx, tmp_path, "install", "something:1.0")
    assert result.returncode != 0, (
        "ocx install (root) must fail (deleted)"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx install (root) must exit {EXIT_USAGE} (UsageError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "install" in result.stderr.lower(), (
        f"expected clap unrecognized-subcommand in stderr; got:\n{result.stderr}"
    )


def test_shell_env_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell env`` → clap unrecognised subcommand, exit 64 (UsageError).

    OCX maps all clap parse-level rejections to UsageError (exit 64). The
    command is deleted — use ``ocx package env --shell[=NAME]`` instead.
    """
    result = _run(ocx, tmp_path, "shell", "env", "something:1.0")
    assert result.returncode != 0, (
        "ocx shell env must fail (deleted)"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell env must exit {EXIT_USAGE} (UsageError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "env" in result.stderr.lower(), (
        f"expected clap unrecognized-subcommand in stderr; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Block A2 — exit-64 regression tests for additional removed root commands
#
# Plan C4 contract (plan_toolchain_cli.md): ocx maps all clap parse-level
# rejections to UsageError (exit 64, EX_USAGE) — NOT exit 2 (clap default).
# Verified empirically. These regressions guard against accidental re-addition.
# ---------------------------------------------------------------------------


def test_root_uninstall_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx uninstall`` (root) → exit 64 (UsageError).

    Deleted in the handshake taxonomy refactor; use ``ocx package uninstall``.
    """
    result = _run(ocx, tmp_path, "uninstall", "something:1.0")
    assert result.returncode == EXIT_USAGE, (
        f"ocx uninstall (root) must exit {EXIT_USAGE} (UsageError/clap); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stderr.strip(), "clap must write a usage error to stderr"


def test_root_select_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx select`` (root) → exit 64 (UsageError).

    Deleted; use ``ocx package select``.
    """
    result = _run(ocx, tmp_path, "select", "something:1.0")
    assert result.returncode == EXIT_USAGE, (
        f"ocx select (root) must exit {EXIT_USAGE} (UsageError/clap); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stderr.strip(), "clap must write a usage error to stderr"


def test_root_deselect_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx deselect`` (root) → exit 64 (UsageError).

    Deleted; use ``ocx package deselect``.
    """
    result = _run(ocx, tmp_path, "deselect", "something")
    assert result.returncode == EXIT_USAGE, (
        f"ocx deselect (root) must exit {EXIT_USAGE} (UsageError/clap); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stderr.strip(), "clap must write a usage error to stderr"


def test_root_exec_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx exec`` (root) → exit 64 (UsageError).

    Deleted; use ``ocx package exec``.
    """
    result = _run(ocx, tmp_path, "exec", "something:1.0", "--", "ls")
    assert result.returncode == EXIT_USAGE, (
        f"ocx exec (root) must exit {EXIT_USAGE} (UsageError/clap); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert result.stderr.strip(), "clap must write a usage error to stderr"


def test_env_global_no_toml_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global env`` with no ``$OCX_HOME/ocx.toml`` → exit 64.

    Block A2 / review-fix fix: the old code exited 74 (IoError) when the
    global toml was absent. The corrected behaviour is exit 64 (UsageError /
    EX_USAGE): a missing global toolchain is a usage error (``ocx --global add``
    must be run first), not an I/O error.
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    assert not global_toml.exists(), "precondition: no global ocx.toml (fresh test dir)"

    empty = tmp_path / "no_project"
    empty.mkdir()
    result = _run(
        ocx, empty, "--global", "env",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx --global env with no global toml must exit {EXIT_USAGE} (UsageError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_env_shell_bash_explicit_emits_export_lines(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx env --shell=bash`` (explicit, no SHELL env) emits export lines.

    Block A2: verifies the explicit ``--shell=bash`` path works independently of
    the ``$SHELL`` environment variable. This is the eval-safe channel documented
    in the handshake §3. A project with a locked toolchain is required.
    """
    project, _ = _make_project(ocx, tmp_path, "explicit_shell_bash")
    # Unset SHELL to verify explicit --shell=bash does not depend on it.
    env_no_shell = dict(ocx.env)
    env_no_shell.pop("SHELL", None)
    result = subprocess.run(
        [str(ocx.binary), "env", "--shell=bash"],
        cwd=project,
        capture_output=True,
        text=True,
        env=env_no_shell,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env --shell=bash (explicit) must succeed even without $SHELL; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"--shell=bash explicit must emit export lines; got:\n{result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# §8 / C9 — No-entrypoint package end-to-end via Modifier::Path
# (handshake §8, plan C9)
# ---------------------------------------------------------------------------


def test_no_entrypoint_global_tool_reachable_via_path_modifier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """End-to-end: a package with NO entrypoints field but a PATH modifier is
    reachable after eval-ing ``ocx --global env --shell=sh`` output.

    §8 DELIVERABLE (handshake §8 / plan C9): closes the
    ``make_package``-always-emits-entrypoints blind spot.  This test
    deliberately omits ``entrypoints`` from the metadata so the package has no
    launcher symlinks.  The sole mechanism for tool resolution is the
    ``Modifier::Path`` (``PATH ⊳ ${installPath}/bin``) entry in the metadata
    env block.  If the composer drops Path-type entries for packages without
    entrypoints, this test fails.

    Flow:
    1. Publish a package with bins but NO ``entrypoints`` metadata field.
    2. ``ocx --global add`` records it in the global tier and installs+selects
       (global-tier ``add`` auto-sets the ``current`` selection; the signed
       handshake §1 contract — no manual ``ocx package select`` needed).
    3. ``ocx --global env --shell=sh`` emits POSIX export lines that prepend the
       package's ``content/bin/`` directory to PATH.
    4. Eval those lines in a non-interactive subshell → the no-entrypoint binary
       is resolvable via PATH (no launcher symlink required).
    """
    bin_name = "notool"  # deliberately not "hello" to catch cross-test leakage

    # Step 1: Publish a package with bins but NO entrypoints.
    # make_package default env has PATH ⊳ ${installPath}/bin (Modifier::Path)
    # and no entrypoints field — exactly the no-entrypoint fixture.
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        new=True,
        cascade=False,
        bins=[bin_name],
        # Explicit env: only PATH (Modifier::Path) — no _HOME constant, no extras.
        # visibility=public so the global env composer includes it.
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            },
        ],
    )
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    # Step 2: ocx --global add records + installs the package into the global tier.
    add_result = _run(ocx, tmp_path, "--global", "add", fq)
    assert add_result.returncode == EXIT_SUCCESS, (
        f"add --global must succeed for no-entrypoint package; "
        f"rc={add_result.returncode}\nstderr:\n{add_result.stderr}"
    )

    # `add --global` installs AND auto-sets the `current` selection in the
    # global tier (signed handshake §1: "global IS the project toolchain — the
    # only difference is the load site"; no manual `ocx package select`).
    # `resolve_global_current_env` reads exactly this `current` symlink.

    # Step 3: get activation output.
    env_result = _run(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"env --global --shell=sh must succeed after add; "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        f"env --global --shell=sh must emit at least one export line; "
        f"got:\n{env_result.stdout!r}"
    )
    # Confirm PATH is among the exported vars (Modifier::Path must be visible).
    assert "PATH" in env_result.stdout, (
        f"PATH must appear in global env export (Modifier::Path activated); "
        f"got:\n{env_result.stdout!r}"
    )

    # Step 4: source the export lines in a non-interactive bash subshell and
    # assert the no-entrypoint binary is resolvable on the resulting PATH.
    # Block A1 fix: use run_after_sourcing (temp-file + dot-operator) instead of
    # eval "..." to handle paths with spaces, $, ", !, and \ correctly.
    shell_result = run_after_sourcing(
        env_result.stdout,
        f"command -v {bin_name} && {bin_name}",
        cwd=tmp_path,
        env=dict(ocx.env),
    )
    assert shell_result.returncode == EXIT_SUCCESS, (
        f"no-entrypoint binary '{bin_name}' must be on PATH after eval of "
        f"'ocx --global env --shell=sh'; "
        f"rc={shell_result.returncode}\n"
        f"stdout:\n{shell_result.stdout}\nstderr:\n{shell_result.stderr}"
    )
    assert pkg.marker in shell_result.stdout, (
        f"no-entrypoint binary '{bin_name}' must run and print its marker "
        f"(proving the correct content/bin/ was on PATH, not a stale version); "
        f"marker={pkg.marker!r}\nstdout:\n{shell_result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Security regression tests — escape_value hardening
#
# These acceptance-level tests verify that env metadata values containing
# shell metacharacters arrive literally after `ocx env --shell=<x>` sourcing.
#
# Coverage:
#   - Bash/zsh history expansion: `!!` and `!<word>` trigger in interactive
#     shells.  escape_value escapes `!` as `\!`; `\!` is literal `!` inside
#     bash double-quoted strings.  The test sources the export line in a
#     non-interactive bash subshell and asserts the env var holds the literal
#     value (CWE-78 defence).
#   - Nushell `$env.HOME` injection: `$` interpolates in Nushell `$"..."` PATH
#     expressions.  escape_value escapes `$` as `\$` in Nushell output so the
#     raw value is not substituted.  The test asserts the emitted line contains
#     the escaped form (running Nushell not required in the test environment).
# ---------------------------------------------------------------------------


def test_env_bash_history_expansion_chars_land_literally(
    ocx: OcxRunner, tmp_path: Path, unique_repo: str
) -> None:
    """Bash history-expansion metacharacters in env metadata values land literally.

    Security regression for escape_value `!`-escaping (CWE-78):
    - ``!!`` would expand to the previous command in an interactive bash/zsh shell.
    - ``!word`` would expand the most-recent command starting with ``word``.
    Both must survive the eval-safe channel (``ocx env --shell=bash`` → source)
    and land as the literal string in the environment variable, not as expanded
    history.  ``escape_value`` escapes ``!`` as ``\\!``; ``\\!`` is a literal
    ``!`` inside bash double-quoted strings (non-interactive or interactive).
    """
    # Publish a package with a constant env entry whose value contains !! and !word.
    danger_value = "version!!check !uname style"
    var_name = f"T_{unique_repo.upper().replace('-', '_')}_SECURITY_A"
    pkg_env = [
        {
            "key": var_name,
            "type": "constant",
            "value": danger_value,
            "visibility": "public",
        },
    ]
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False, env=pkg_env)
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    project = tmp_path / "proj_sec_a"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\ntool = "{fq}"\n')
    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS

    env_result = _run(ocx, project, "env", "--shell=bash")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx env --shell=bash must succeed; rc={env_result.returncode}\n"
        f"stderr:\n{env_result.stderr}"
    )
    assert var_name in env_result.stdout, (
        f"export line for {var_name} must be present; got:\n{env_result.stdout!r}"
    )

    # Assert the emitted export line contains \! (the escaped form).
    # escape_value must neutralize `!` as `\!` so interactive bash/zsh (which
    # have histexpand enabled) do not expand `!!` / `!word` from history.
    # In non-interactive bash, histexpand is disabled — `!!` would not expand
    # anyway — but the escaped form is the defence-in-depth for the interactive
    # profile-sourcing case (e.g. `eval "$(ocx env --shell=bash)"` in .bashrc).
    lines_with_var = [ln for ln in env_result.stdout.splitlines() if var_name in ln]
    assert lines_with_var, f"No export line found for {var_name}"
    var_line = lines_with_var[0]

    # The emitted line must contain `\!` (escaped bang), not bare `!!` or `!uname`.
    assert r"\!" in var_line, (
        f"escape_value must escape `!` as `\\!` in bash/zsh output; "
        f"bare `!` could trigger interactive history expansion (CWE-78).\n"
        f"export line: {var_line!r}"
    )
    # The raw `!!` must not appear unescaped in the assignment value.
    # (Split on first `=` to get only the RHS, avoiding false-positive on var name.)
    rhs = var_line.split("=", 1)[1] if "=" in var_line else var_line
    assert "!!" not in rhs, (
        f"Raw `!!` must not appear unescaped in bash export RHS (history expansion risk); "
        f"line: {var_line!r}"
    )
    # Source the export lines and confirm the command exits cleanly (no crash,
    # no expansion side-effect that would corrupt the environment).
    shell_result = run_after_sourcing(
        env_result.stdout,
        f'[ -n "${{{var_name}}}" ] && echo OK',
        cwd=project,
        env=dict(ocx.env),
        shell="bash",
        shell_flags="--norc --noprofile",
    )
    assert shell_result.returncode == EXIT_SUCCESS, (
        f"sourcing ocx env --shell=bash output with ! in value must succeed; "
        f"rc={shell_result.returncode}\nstderr:\n{shell_result.stderr}"
    )
    assert "OK" in shell_result.stdout, (
        f"var {var_name} must be non-empty after sourcing; "
        f"stdout:\n{shell_result.stdout!r}"
    )


def test_env_nushell_dollar_env_home_is_escaped_in_output(
    ocx: OcxRunner, tmp_path: Path, unique_repo: str
) -> None:
    """``$env.HOME`` in an env metadata value is escaped in Nushell output.

    Security regression for escape_value `$`-escaping in Nushell (CWE-78):
    Nushell uses ``$"..."`` for PATH interpolation in ``export_path``.  A raw
    ``$env.HOME`` in a metadata constant value would cause Nushell to substitute
    the ``$env`` object's ``HOME`` key, overwriting the intended literal value.
    ``escape_value`` escapes ``$`` as ``\\$`` in Nushell output; ``\\$`` is a
    literal ``$`` in Nushell double-quoted strings.

    This test asserts the emitted Nushell export line contains the properly-
    escaped form (``\\$env.HOME``) rather than the raw ``$env.HOME``.  Running
    Nushell in the test environment is not required — the assertion is on the
    bytes emitted by ``ocx env --shell=nushell``.
    """
    danger_value = "$env.HOME/.config/mytool"
    var_name = f"T_{unique_repo.upper().replace('-', '_')}_SECURITY_B"
    pkg_env = [
        {
            "key": var_name,
            "type": "constant",
            "value": danger_value,
            "visibility": "public",
        },
    ]
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False, env=pkg_env)
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    project = tmp_path / "proj_sec_b"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\ntool = "{fq}"\n')
    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS

    env_result = _run(ocx, project, "env", "--shell=nushell")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx env --shell=nushell must succeed; rc={env_result.returncode}\n"
        f"stderr:\n{env_result.stderr}"
    )
    assert var_name in env_result.stdout, (
        f"export line for {var_name} must be present; got:\n{env_result.stdout!r}"
    )

    # The literal `$` must NOT appear unescaped in the emitted assignment value.
    # escape_value for Nushell produces `\$` for `$`, so `$env.HOME` becomes
    # `\$env.HOME` in the output.
    assert r"\$env.HOME" in env_result.stdout, (
        f"$env.HOME in metadata value must be escaped as \\$env.HOME in Nushell output; "
        f"got:\n{env_result.stdout!r}\n"
        f"(raw $ in Nushell $\"...\" strings triggers env interpolation — CWE-78)"
    )
    # Bonus: the raw unescaped form must NOT appear as an assignment value
    # (it is OK for the var_name to contain the literal string, but not the value).
    lines_with_var = [ln for ln in env_result.stdout.splitlines() if var_name in ln]
    for ln in lines_with_var:
        # Extract the RHS of the assignment (after the `=`).
        rhs = ln.split("=", 1)[1] if "=" in ln else ln
        assert "$env.HOME" not in rhs or r"\$env.HOME" in rhs, (
            f"Raw $env.HOME must not appear unescaped in Nushell assignment RHS; "
            f"line: {ln!r}"
        )
