# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx env` (toolchain-tier composed-env command).

Encodes plan_toolchain_cli.md Phase 2 contracts (C2, C3, C5) — updated to the
context-format model (format reversal, no longer "backend-first JSON default"):

- ``ocx env`` (no project) → exit 64 (UsageError)
- ``ocx env`` in a project → plain table by default (context-level format, same
  as every other command; NOT JSON, NOT sourceable)
- ``ocx --format json env`` → JSON (root flag, NOT a subcommand flag)
- ``ocx env --format plain`` → clap usage error (exit 64, flag no longer exists)
- ``ocx env --shell=bash`` → bash eval-safe export lines (``--shell`` unchanged)
- ``ocx env --shell=sh`` → POSIX export lines byte-identical to ``--shell=dash``
- Bare ``--shell`` (no value, undetectable SHELL) → exit 64
- ``--global`` ⟂ ``--project`` → clap exit 64 (UsageError)
- ``ocx package env <ids> --shell=zsh`` → sourceable zsh lines (reuses env.rs)
- ``--shell ripgrep`` is NOT swallowed (require_equals enforced)
- Legacy commands (``ocx install``, ``ocx shell env``) → exit 64 (clap)
"""
from __future__ import annotations

import json
import re as _re_te
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
# C2 — default output is plain table (context-level format, NOT JSON)
# ---------------------------------------------------------------------------


def test_env_in_project_default_plain(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env`` in a project outputs a plain table by default (no flags).

    Context-format model: output format is the root ``--format`` concern (same
    as every other command).  The default is plain.  The subcommand ``--format``
    flag was deleted from ``ToolchainEnv`` — format is no longer env-specific.
    JSON requires the ROOT flag: ``ocx --format json env``.
    """
    project, _ = _make_project(ocx, tmp_path, "plain_default")
    result = _run(ocx, project, "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env must succeed in a project; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # Default must be a plain table, NOT JSON.
    try:
        json.loads(result.stdout)
        pytest.fail(
            f"ocx env default output must NOT be JSON; got:\n{result.stdout!r}\n"
            f"(subcommand --format deleted; plain is the context default)"
        )
    except json.JSONDecodeError:
        pass  # expected: plain table is not valid JSON
    # Plain output must not contain export statements (not sourceable).
    assert "export " not in result.stdout, (
        f"plain table output must NOT contain 'export' lines; got:\n{result.stdout!r}"
    )
    # Must contain at least one non-empty line (actual table content).
    non_empty = [ln for ln in result.stdout.splitlines() if ln.strip()]
    assert non_empty, (
        f"plain output must contain at least one data row; got:\n{result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# C2 — subcommand `--format` was deleted; JSON requires root `--format json`
# ---------------------------------------------------------------------------


def test_env_subcommand_format_flag_rejected(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env --format plain`` → exit 64 (flag no longer exists on subcommand).

    The subcommand ``--format`` flag was deleted from ``ToolchainEnv``.  Format
    is now a ROOT flag concern (``ocx --format plain env``).  Passing
    ``--format`` after the subcommand is a clap unrecognised-argument error →
    exit 64 (OCX maps all clap usage errors to EX_USAGE 64).
    """
    project, _ = _make_project(ocx, tmp_path, "no_sub_format")
    result = _run(ocx, project, "env", "--format", "plain")
    assert result.returncode == EXIT_USAGE, (
        f"ocx env --format plain must exit {EXIT_USAGE} (subcommand --format deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unexpected argument" in result.stderr.lower() or "format" in result.stderr.lower(), (
        f"expected clap unrecognised-argument message in stderr; got:\n{result.stderr}"
    )


def test_env_root_format_json_emits_json(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --format json env`` emits JSON (context-level root flag).

    JSON output requires the ROOT flag (before the subcommand).  This is the
    machine-readable form for backend tools.  The JSON shape must be parseable
    and contain an ``entries`` key.
    """
    project, _ = _make_project(ocx, tmp_path, "root_fmt_json")
    # Root --format flag must come BEFORE the subcommand name.
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "env"],
        cwd=project,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --format json env must succeed in a project; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"ocx --format json env output must be valid JSON; got:\n{result.stdout!r}\n"
            f"parse error: {exc}"
        )
    assert "entries" in data, (
        f"JSON must have 'entries' key; got keys: {list(data.keys())}"
    )
    assert isinstance(data["entries"], list), "entries must be a list"


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


def test_env_global_no_toml_is_empty_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global env`` with no ``$OCX_HOME/ocx.toml`` → exit 0 (empty env).

    A global toolchain is OPTIONAL — "not configured yet" is a valid empty
    state, not a usage error (EX_USAGE). Querying an unconfigured global tier
    returns the empty set on the report path AND the eval-safe path, with no
    asymmetry between them. (History: this previously exited 64; the report-path
    error was reconsidered — an unconfigured global tier is not a misuse of the
    command, only a real failure like a corrupt lock is.)
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
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global env with no global toolchain must exit {EXIT_SUCCESS} (empty env, "
        f"not a usage error); got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_env_global_no_toolchain_shell_is_silent_noop(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global env --shell=NAME`` with no toolchain → exit 0, empty stdout.

    Regression for the released Windows activation bug: the eval-safe ``--shell``
    channel is the login exporter that runs on EVERY shell start (``env.sh`` /
    ``env.ps1``). With no global toolchain configured it must stay silent — emit
    nothing and exit 0 — so a fresh install does not print a scary ERROR line on
    POSIX or feed ``$null`` into ``Invoke-Expression`` on PowerShell. The report
    path (no ``--shell``) keeps exit 64; see ``test_env_global_no_toml_exits_64``.
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    assert not (ocx_home / "ocx.toml").exists(), "precondition: no global ocx.toml"

    empty = tmp_path / "no_project"
    empty.mkdir()
    for shell in ("sh", "bash", "pwsh", "powershell"):
        result = _run(
            ocx, empty, "--global", "env", f"--shell={shell}",
            extra_env={"OCX_NO_PROJECT": "1"},
        )
        assert result.returncode == EXIT_SUCCESS, (
            f"ocx --global env --shell={shell} with no toolchain must exit 0 "
            f"(silent no-op); got {result.returncode}\nstderr:\n{result.stderr}"
        )
        assert result.stdout == "", (
            f"ocx --global env --shell={shell} with no toolchain must emit nothing "
            f"on stdout; got:\n{result.stdout!r}"
        )


def test_env_global_corrupt_lock_is_empty_env_both_paths(ocx: OcxRunner, tmp_path: Path) -> None:
    """A corrupt ``$OCX_HOME/ocx.lock`` degrades to an empty env on BOTH the
    eval-safe ``--shell`` path and the report path (Gap A — lenient global tier).

    The §4 login exporter ``eval "$(ocx --global env --shell=sh)"`` runs on EVERY
    shell start; a corrupt/stale global lock must NOT break the shell. The global
    tier is lenient and consistent: "no usable global toolchain" is an empty env
    (exit 0) regardless of ``--shell`` — there is no shell/no-shell asymmetry. A
    corrupt global lock instead surfaces via the lock-rewriting commands
    (``ocx --global lock``/``add``/``upgrade``), not this read-only exporter.
    (Before Gap A, only the clean "no lock" case was silenced — a parse failure
    propagated via ``?`` and broke the eval line on every shell start.)
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    # The global lock sits next to $OCX_HOME/ocx.toml — i.e. $OCX_HOME/ocx.lock.
    (ocx_home / "ocx.lock").write_text("@@@ this is not valid toml @@@\n")

    empty = tmp_path / "no_project"
    empty.mkdir()

    # Eval-safe path: empty env, exit 0, empty stdout.
    shell_result = _run(
        ocx, empty, "--global", "env", "--shell=sh",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert shell_result.returncode == EXIT_SUCCESS, (
        f"corrupt global lock on --shell path must degrade to exit 0; "
        f"got {shell_result.returncode}\nstderr:\n{shell_result.stderr}"
    )
    assert shell_result.stdout == "", (
        f"corrupt global lock on --shell path must emit nothing on stdout; "
        f"got:\n{shell_result.stdout!r}"
    )

    # Report path: same lenient outcome — exit 0 (not a usage/data error).
    report_result = _run(
        ocx, empty, "--global", "env",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert report_result.returncode == EXIT_SUCCESS, (
        f"corrupt global lock on the report path must also degrade to exit 0 "
        f"(lenient global tier — no shell/no-shell asymmetry); "
        f"got {report_result.returncode}\nstdout:\n{report_result.stdout}\nstderr:\n{report_result.stderr}"
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


# ---------------------------------------------------------------------------
# V2 lock shape: ``ocx env`` (global path) resolves per-platform leaf digest
# ---------------------------------------------------------------------------

_LEAF_RE_TE = _re_te.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_env_global_v2_lock_resolves_host_leaf_digest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --global env`` on a V2 ``$OCX_HOME/ocx.lock`` resolves the
    host-platform leaf digest from ``[tool.platforms]`` — not a legacy
    ``pinned`` index digest.

    ADR §toolchain_env.rs: "V2: host-platform leaf; V1: legacy index-digest path."

    Scenario: ``ocx --global add`` writes a V2 global lock and installs the
    package blobs.  Assert:
    1. The global lock is V2 (``lock_version = 2``, ``[tool.platforms]``
       present, no ``pinned =`` line, at least one leaf digest recorded).
    2. ``ocx --global env --shell=sh`` exits 0 and emits ``export`` lines,
       proving the V2 host-leaf was resolved successfully.

    This is a distinct concern from the ``corrupt-lock`` lenient-path tests
    that exercise the empty-env degradation path; this test verifies the
    *happy* path through the V2 code branch.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_te_v2leaf"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["genv"])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    empty = tmp_path / "no_proj_te"
    empty.mkdir()

    # ``ocx --global add`` installs the tool and writes the global lock + toml.
    add = subprocess.run(
        [str(ocx.binary), "--global", "add", fq],
        cwd=empty,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Verify the global lock is V2.
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_lock = ocx_home / "ocx.lock"
    assert global_lock.is_file(), "$OCX_HOME/ocx.lock must exist after add --global"
    lock_text = global_lock.read_text()

    assert "lock_version = 2" in lock_text, (
        "global lock must be V2 (lock_version = 2) after add --global; "
        "got:\n" + lock_text[:400]
    )
    assert "[tool.platforms]" in lock_text, (
        "global V2 lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_TE.findall(lock_text)
    assert leaf_digests, (
        "global V2 lock must record at least one per-platform leaf digest"
    )
    assert "pinned =" not in lock_text, (
        "global V2 lock must NOT carry a legacy `pinned` line"
    )

    # ``ocx --global env --shell=sh`` must resolve the V2 host-leaf and emit export lines.
    env_result = subprocess.run(
        [str(ocx.binary), "--global", "env", "--shell=sh"],
        cwd=empty,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert env_result.returncode == EXIT_SUCCESS, (
        "ocx --global env --shell=sh on a V2 global lock must exit 0 "
        "(resolves host-leaf from [tool.platforms]); "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        "V2 --global env --shell=sh must emit export lines; "
        f"got:\n{env_result.stdout!r}"
    )


def test_env_global_v1_lock_still_resolves_via_legacy_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A V1 ``$OCX_HOME/ocx.lock`` (``lock_version = 1``, ``pinned`` field)
    still lets ``ocx --global env`` succeed via the legacy index-digest path —
    no forced upgrade required.

    ADR: "A committed V1 lock keeps installing/running offline with no forced
    upgrade and no read-path mutation."

    This test is the global-lock counterpart to the lenient corrupt-lock test
    above: the corrupt-lock test exercises a *bad* lock → empty env degradation;
    this test exercises a *syntactically-valid* V1 lock → legacy path success.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_te_v1leg"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["genv"])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    empty = tmp_path / "no_proj_te2"
    empty.mkdir()

    # Step 1: run add --global to warm blobs and get a real V2 lock.
    add = subprocess.run(
        [str(ocx.binary), "--global", "add", fq],
        cwd=empty,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Step 2: read the real V2 lock and synthesise a V1 replacement.
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_lock = ocx_home / "ocx.lock"
    v2_text = global_lock.read_text()

    repo_match = _re_te.search(r'repository\s*=\s*"([^"]+)"', v2_text)
    leaf_match = _LEAF_RE_TE.search(v2_text)
    decl_match = _re_te.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_text)
    assert repo_match and leaf_match and decl_match, (
        "V2 global lock must carry repository + leaf + declaration_hash; "
        "got:\n" + v2_text[:400]
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)
    decl_hash = decl_match.group(1)

    # Overwrite the global lock with a V1 form using the real pinned identifier.
    global_lock.write_text(
        f"""\
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
    )

    # Step 3: ``ocx --global env --shell=sh`` must still resolve (blobs cached).
    env_result = subprocess.run(
        [str(ocx.binary), "--global", "env", "--shell=sh"],
        cwd=empty,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert env_result.returncode == EXIT_SUCCESS, (
        "V1 global lock must allow ocx --global env --shell=sh to succeed via the "
        "legacy index-digest path (blobs cached from add --global); "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        "V1 global lock: --global env --shell=sh must emit export lines; "
        f"got:\n{env_result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# #176 — `-g/--group` group-scoped env composition (mirror `run`/`pull`)
# ---------------------------------------------------------------------------


def _make_multigroup_project(
    ocx: OcxRunner,
    tmp_path: Path,
    label: str,
) -> tuple[Path, dict[str, str]]:
    """Publish a default/lint/ci tool trio, lock + pull, return (project, home_keys).

    Each group owns one distinct package whose default env exposes
    ``{REPO}_HOME`` (public). The returned ``home_keys`` maps group name →
    that env key so tests can assert membership in the composed env.
    """
    repos: dict[str, str] = {}
    for group in ("default", "lint", "ci"):
        short = uuid4().hex[:8]
        repo = f"t_{short}_{label}_{group}"
        make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["tool"])
        repos[group] = repo

    project = tmp_path / f"proj_{label}"
    project.mkdir()
    reg = ocx.registry
    _write_ocx_toml(
        project,
        f'[tools]\n{repos["default"]} = "{reg}/{repos["default"]}:1.0.0"\n\n'
        f'[group.lint]\n{repos["lint"]} = "{reg}/{repos["lint"]}:1.0.0"\n\n'
        f'[group.ci]\n{repos["ci"]} = "{reg}/{repos["ci"]}:1.0.0"\n',
    )
    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS

    home_keys = {g: r.upper().replace("-", "_") + "_HOME" for g, r in repos.items()}
    return project, home_keys


def _env_json_keys(ocx: OcxRunner, project: Path, *group_args: str) -> list[str]:
    """Run ``ocx --format json env [group_args]`` and return the entry keys, in order."""
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "env", *group_args],
        cwd=project,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --format json env {' '.join(group_args)} must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    data = json.loads(result.stdout)
    assert "entries" in data and isinstance(data["entries"], list), (
        f"JSON must carry an 'entries' list; got:\n{result.stdout!r}"
    )
    for entry in data["entries"]:
        assert set(entry) == {"key", "value", "type"}, (
            f"each entry must be {{key,value,type}}; got: {entry!r}"
        )
    return [entry["key"] for entry in data["entries"]]


def _global_env_json_keys(ocx: OcxRunner, cwd: Path, *group_args: str) -> list[str]:
    """Run ``ocx --global --format json env [group_args]`` and return entry keys, in order.

    ``--global`` mirrors ``_env_json_keys`` but must precede the subcommand
    (root flag), so it cannot reuse that helper's fixed argv shape.
    """
    result = subprocess.run(
        [str(ocx.binary), "--global", "--format", "json", "env", *group_args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global --format json env {' '.join(group_args)} must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    data = json.loads(result.stdout)
    assert "entries" in data and isinstance(data["entries"], list), (
        f"JSON must carry an 'entries' list; got:\n{result.stdout!r}"
    )
    return [entry["key"] for entry in data["entries"]]


def test_env_single_group_scopes_to_that_group(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env -g lint`` composes lint only — the default group is NOT auto-included.

    #176 option-B contract (identical to ``ocx run -g lint``): a single named
    group means only that group. The default group is opt-in via ``-g default``.
    """
    project, home = _make_multigroup_project(ocx, tmp_path, "single")
    keys = _env_json_keys(ocx, project, "-g", "lint")

    assert home["lint"] in keys, f"lint group's env var must be present; keys={keys}"
    assert home["default"] not in keys, (
        f"default group must NOT be auto-included with -g lint; keys={keys}"
    )
    assert home["ci"] not in keys, f"unrelated ci group must be absent; keys={keys}"


def test_env_default_plus_named_group_composes_both(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env -g default -g lint`` composes the default group AND lint.

    #176: the issue's "default + requested" case is served by naming both.
    """
    project, home = _make_multigroup_project(ocx, tmp_path, "twoflag")
    keys = _env_json_keys(ocx, project, "-g", "default", "-g", "lint")

    assert home["default"] in keys, f"default group must be present; keys={keys}"
    assert home["lint"] in keys, f"lint group must be present; keys={keys}"
    assert home["ci"] not in keys, f"unrequested ci group must be absent; keys={keys}"
    # Exact-order pin (#176): groups compose in the order requested on the
    # command line (`compose_tool_set` preserves user-specified group order),
    # and within each single-tool group the fixture's own metadata order is
    # PATH then `{REPO}_HOME` (`make_package`'s default env list, no
    # entrypoints synth-PATH since these fixtures declare none).
    assert keys == ["PATH", home["default"], "PATH", home["lint"]], (
        f"default+lint composition order must be [PATH, default HOME, PATH, "
        f"lint HOME]; got {keys}"
    )


def test_env_all_keyword_composes_every_group(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env -g all`` composes default + every declared ``[group.*]``."""
    project, home = _make_multigroup_project(ocx, tmp_path, "allkw")
    keys = _env_json_keys(ocx, project, "-g", "all")

    for group, key in home.items():
        assert key in keys, f"-g all must include {group} group's {key!r}; keys={keys}"
    # Exact-order pin (#176): `expand_all_keyword` expands `all` to `default`
    # followed by every named `[group.*]` in alphabetical order (`ci` before
    # `lint`), and `compose_tool_set` preserves that group order. Within each
    # single-tool group, the fixture contributes PATH then `{REPO}_HOME`.
    assert keys == [
        "PATH",
        home["default"],
        "PATH",
        home["ci"],
        "PATH",
        home["lint"],
    ], (
        f"-g all composition order must be default, then alphabetical named "
        f"groups (ci, lint); got {keys}"
    )


def test_env_no_group_is_default_only(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env`` (no ``-g``) composes the default ``[tools]`` group only."""
    project, home = _make_multigroup_project(ocx, tmp_path, "noflag")
    keys = _env_json_keys(ocx, project)

    assert home["default"] in keys, f"default group must be present; keys={keys}"
    assert home["lint"] not in keys, f"lint group must be absent without -g; keys={keys}"
    assert home["ci"] not in keys, f"ci group must be absent without -g; keys={keys}"


def test_env_unknown_group_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx env -g nope`` on a project → exit 64 (unknown group, project tier)."""
    project, _ = _make_multigroup_project(ocx, tmp_path, "unknowng")
    result = _run(ocx, project, "env", "-g", "nope")
    assert result.returncode == EXIT_USAGE, (
        f"unknown --group must exit 64 (project tier); got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "nope" in result.stderr, f"stderr must name the unknown group; got:\n{result.stderr}"


@pytest.mark.parametrize(
    "group_value",
    ["lint,,ci", ",lint", "lint,", ",,"],
    ids=["middle", "leading", "trailing", "degenerate"],
)
def test_env_empty_group_segment_exits_64(ocx: OcxRunner, tmp_path: Path, group_value: str) -> None:
    """``ocx env -g <value>`` with any empty comma segment → exit 64 before any config load.

    Hardens beyond the single middle-segment case (``lint,,ci``): a leading
    comma (``,lint``), a trailing comma (``lint,``), and a comma-only value
    (``,,``) all parse to at least one empty string via clap's
    ``value_delimiter``, and each must be rejected identically.
    """
    project, _ = _make_multigroup_project(ocx, tmp_path, "emptyseg")
    result = _run(ocx, project, "env", "-g", group_value)
    assert result.returncode == EXIT_USAGE, (
        f"empty --group comma segment ({group_value!r}) must exit 64; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# #176 global-tier gap — unknown `-g` group on the LENIENT global toolchain
# must degrade to an empty env (exit 0), not the project tier's exit 64.
# ---------------------------------------------------------------------------


def test_env_global_unknown_group_is_empty_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global env -g nonexistent-group`` → exit 0, empty env on both paths.

    Mirrors ``test_env_global_no_toolchain_shell_is_silent_noop``: the global
    tier is lenient (``selected_groups_global`` passes an unrecognised name
    through verbatim; a lock entry whose group never matches it is simply
    skipped — no ``UnknownGroup`` error the project tier would raise).

    Two observable channels are asserted:

    - ``--shell=sh`` (the eval-safe login-exporter channel): stdout must be
      exactly empty (the released-bug regression contract — see the
      no-toolchain sibling test).
    - ``--format json`` (the report path): stdout must decode to
      ``{"entries": []}`` — the composed env is empty. The context-format
      *plain* report path is deliberately NOT asserted byte-empty here: the
      ``Printable`` single-table convention prints its column header row
      unconditionally, even with zero data rows (see
      ``api/data/env.rs::EnvVars::print_plain`` and the sibling
      ``test_env_global_no_toml_is_empty_env``, which likewise only checks
      the exit code on that path). "Empty env" means zero composed entries,
      not zero stdout bytes, on the plain-table report path.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_gunknown"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["tool"])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    empty = tmp_path / "no_project"
    empty.mkdir()

    add_result = _run(ocx, empty, "--global", "add", fq)
    assert add_result.returncode == EXIT_SUCCESS, (
        f"add --global must succeed to build the global toolchain fixture; "
        f"rc={add_result.returncode}\nstderr:\n{add_result.stderr}"
    )

    shell_result = _run(ocx, empty, "--global", "env", "-g", "nonexistent-group", "--shell=sh")
    assert shell_result.returncode == EXIT_SUCCESS, (
        f"ocx --global env -g nonexistent-group --shell=sh must exit 0 (lenient "
        f"global tier); got {shell_result.returncode}\nstderr:\n{shell_result.stderr}"
    )
    assert shell_result.stdout == "", (
        f"unknown --group on the global --shell path must emit nothing on "
        f"stdout; got:\n{shell_result.stdout!r}"
    )

    report_result = _run(
        ocx, empty, "--format", "json", "--global", "env", "-g", "nonexistent-group"
    )
    assert report_result.returncode == EXIT_SUCCESS, (
        f"ocx --global env -g nonexistent-group (report path) must exit 0; "
        f"got {report_result.returncode}\nstderr:\n{report_result.stderr}"
    )
    data = json.loads(report_result.stdout)
    assert data.get("entries") == [], (
        f"unknown --group on the global report path must compose an empty "
        f"env; got:\n{report_result.stdout!r}"
    )


def test_env_global_group_scoped_composition(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global env -g <group>`` scopes composition on a multi-group
    GLOBAL lock, mirroring the project-tier ``-g`` membership contract.

    #176: the group-scoped composition guarantee extends to the global
    toolchain tier via ``selected_groups_global`` (default/all/named
    passthrough), not just the project tier's ``compose_tool_set``. Each
    group is built with a separate ``ocx --global add [--group <name>]``
    call (global ``add`` accepts one group per invocation).
    """
    repos: dict[str, str] = {}
    for group in ("default", "lint", "ci"):
        short = uuid4().hex[:8]
        repo = f"t_{short}_gscoped_{group}"
        make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["tool"])
        repos[group] = repo
    home = {g: r.upper().replace("-", "_") + "_HOME" for g, r in repos.items()}

    empty = tmp_path / "no_project"
    empty.mkdir()

    add_default = _run(ocx, empty, "--global", "add", f"{ocx.registry}/{repos['default']}:1.0.0")
    assert add_default.returncode == EXIT_SUCCESS, add_default.stderr
    add_lint = _run(
        ocx, empty, "--global", "add", "--group", "lint", f"{ocx.registry}/{repos['lint']}:1.0.0"
    )
    assert add_lint.returncode == EXIT_SUCCESS, add_lint.stderr
    add_ci = _run(
        ocx, empty, "--global", "add", "--group", "ci", f"{ocx.registry}/{repos['ci']}:1.0.0"
    )
    assert add_ci.returncode == EXIT_SUCCESS, add_ci.stderr

    lint_keys = _global_env_json_keys(ocx, empty, "-g", "lint")
    assert home["lint"] in lint_keys, f"lint group must be present; keys={lint_keys}"
    assert home["default"] not in lint_keys, (
        f"default group must NOT be auto-included with -g lint; keys={lint_keys}"
    )
    assert home["ci"] not in lint_keys, f"unrelated ci group must be absent; keys={lint_keys}"

    all_keys = _global_env_json_keys(ocx, empty, "-g", "all")
    for group, key in home.items():
        assert key in all_keys, f"-g all must include {group} group's {key!r}; keys={all_keys}"

    both_keys = _global_env_json_keys(ocx, empty, "-g", "default", "-g", "lint")
    assert home["default"] in both_keys, f"default group must be present; keys={both_keys}"
    assert home["lint"] in both_keys, f"lint group must be present; keys={both_keys}"
    assert home["ci"] not in both_keys, f"unrequested ci group must be absent; keys={both_keys}"
