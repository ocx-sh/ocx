# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for shell activation — rewritten to plan_toolchain_cli.md
Phase 5 contract (handshake_toolchain_cli.md §2/§5/§7).

Covers:
- ``ocx direnv export`` (unchanged, kept in full)
- Deleted commands (``ocx shell hook``, ``ocx shell init``) → exit 64
- Remote-flag safety for direnv export (shell hook gone → test direnv path only)
- Global toolchain activation via ``ocx --global env --shell=sh``
- B1: ``--global`` ⟂ ``--project`` conflict seam (clap exit 2 / UsageError)
- No-self-link GC invariant for global file
- W7: byte-stability characterisation test (written Phase 2 Specify)

Deleted behaviour that IS gone (no assertions):
- ``ocx shell hook`` per-prompt fingerprint/sentinel pattern
- ``ocx shell init`` static-file render
- ``_OCX_APPLIED`` sentinel
- PATH strip (``emit_global_path_strip``)

Tests 1-4: direnv export (kept verbatim from Phase 7 — still live)
Tests 5-12: shell hook / shell init → assert exit 64 (deleted commands)
Test 13: remote flag direnv export (kept)
Test 14: remote flag for shell hook → rewritten: assert shell hook exits 64
Test 15: shell hook empty applied → rewritten: assert exit 64
Test 16: direnv no lock (kept)
Test 17-18: top-level removed commands (kept)
Test 19: global departed → rewritten to new activation model
Test 20: shell init non-POSIX → rewritten: assert exit 64
W7: direnv byte-stability (kept, written Phase 2)
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner
from src.shell_eval import run_after_sourcing


# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE); also used for clap errors


# ---------------------------------------------------------------------------
# Helpers — co-located with tests (DAMP > DRY for acceptance tests).
# ---------------------------------------------------------------------------


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    return [str(ocx.binary), *args]


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` with ``cwd`` so the project CWD-walk fires."""
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


def _run_lock(
    ocx: OcxRunner, cwd: Path, *extra: str
) -> subprocess.CompletedProcess[str]:
    return _run(ocx, cwd, "lock", *extra)


def _run_pull(
    ocx: OcxRunner, cwd: Path, *extra: str
) -> subprocess.CompletedProcess[str]:
    return _run(ocx, cwd, "pull", *extra)


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _published_tool(
    ocx: OcxRunner, tmp_path: Path, label: str, bin_name: str = "hello"
) -> tuple[str, str]:
    """Publish a single test package (one tag) and return ``(repo, tag)``."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_hook_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, bins=[bin_name])
    return repo, tag


def _bash_syntax_ok(script: str) -> tuple[bool, str]:
    """Return ``(ok, stderr)`` after running ``bash -n -c <script>``."""
    proc = subprocess.run(
        ["bash", "-n", "-c", script],
        capture_output=True,
        text=True,
    )
    return proc.returncode == 0, proc.stderr


# ---------------------------------------------------------------------------
# 1. direnv export emits valid bash exports for installed tools
# ---------------------------------------------------------------------------


def test_direnv_export_emits_bash_exports(ocx: OcxRunner, tmp_path: Path) -> None:
    """Project with installed tool → valid bash exports."""
    repo, tag = _published_tool(ocx, tmp_path, "bash_export")
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    assert _run_pull(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "direnv", "export")

    assert result.returncode == EXIT_SUCCESS, (
        f"direnv export must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"expected at least one export line; got stdout:\n{result.stdout}"
    )
    ok, syn_err = _bash_syntax_ok(result.stdout)
    assert ok, f"direnv export output failed bash -n: {syn_err}\nstdout:\n{result.stdout}"


# ---------------------------------------------------------------------------
# 2. direnv export skips uninstalled tool with a stderr note
# ---------------------------------------------------------------------------


def test_direnv_export_skips_uninstalled_tool_with_stderr_note(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Missing tool → stderr ``# ocx: <name> not installed``.

    Two tools, only one pulled — the other gets a one-line stderr note and
    is silently dropped from stdout exports.
    """
    repo_installed, tag_installed = _published_tool(ocx, tmp_path, "ok", bin_name="alpha")
    repo_missing, tag_missing = _published_tool(ocx, tmp_path, "miss", bin_name="beta")
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_installed}:{tag_installed}"
beta = "{ocx.registry}/{repo_missing}:{tag_missing}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    assert (
        _run(ocx, project, "package", "pull", f"{ocx.registry}/{repo_installed}:{tag_installed}").returncode
        == EXIT_SUCCESS
    )

    result = _run(ocx, project, "direnv", "export")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "beta" in result.stderr, (
        f"missing-tool note must reference 'beta'; stderr:\n{result.stderr}"
    )
    assert "not installed" in result.stderr, (
        f"missing-tool note must say 'not installed'; stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 3. direnv export with stale lock → stderr warning, exports use stale digests
# ---------------------------------------------------------------------------


def test_direnv_export_warns_on_stale_lock(ocx: OcxRunner, tmp_path: Path) -> None:
    """Stale lock → stderr warning, continue with stale digests.

    Distinct from ``ocx exec`` which exits 65 on stale lock. `direnv export`
    must NOT fail — interactive shells should keep working with the last
    known good environment until the user manually re-locks.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "stale_a", bin_name="alpha")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "stale_b", bin_name="beta")
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    assert _run_pull(ocx, project).returncode == EXIT_SUCCESS

    # Mutate ocx.toml AFTER locking — declaration_hash now mismatches.
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    result = _run(ocx, project, "direnv", "export")

    assert result.returncode == EXIT_SUCCESS, (
        f"direnv export with stale lock must NOT fail (unlike ocx exec); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    stderr_lower = result.stderr.lower()
    assert "stale" in stderr_lower or "ocx.toml changed" in stderr_lower, (
        f"stale-lock warning expected on stderr; got:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"stale lock still emits exports based on locked digests; got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 4. No project, no output
# ---------------------------------------------------------------------------


def test_direnv_export_no_project_emits_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """No ocx.toml in tree → exit 0, nothing written."""
    empty = tmp_path / "no_project"
    empty.mkdir()

    result = _run(
        ocx, empty, "direnv", "export",
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"no-project case must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" not in result.stdout, (
        f"no-project case must emit nothing; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 5-8. shell hook → DELETED — assert exit 64 (unrecognised subcommand)
#
# Contract (handshake §2 / plan C4): ocx shell hook is removed. Clap
# unrecognised subcommand error → exit 64 (ocx maps clap errors to UsageError).
# ---------------------------------------------------------------------------


def test_shell_hook_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell hook`` is deleted → exit 64 + clap unrecognised-subcommand."""
    result = _run(ocx, tmp_path, "shell", "hook", "--shell", "bash")

    assert result.returncode != EXIT_SUCCESS, (
        "ocx shell hook must fail (deleted); got exit 0"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (UsageError/clap); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "hook" in result.stderr.lower(), (
        f"expected clap unrecognized-subcommand stderr; got:\n{result.stderr}"
    )


def test_shell_hook_unchanged_fingerprint_emits_empty_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` with any ``_OCX_APPLIED`` value → exit 64 (deleted).

    The per-prompt fingerprint/sentinel mechanism (``_OCX_APPLIED``) is removed
    along with the command. No shell activation happens via this path.
    """
    result = _run(
        ocx, tmp_path, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": "v1:" + "ab" * 32},
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_shell_hook_changed_fingerprint_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` (any fingerprint change path) → exit 64 (deleted)."""
    result = _run(ocx, tmp_path, "shell", "hook", "--shell", "bash")
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_shell_hook_no_prior_applied_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` with no ``_OCX_APPLIED`` → exit 64 (deleted)."""
    env_no_applied = {k: v for k, v in ocx.env.items() if k != "_OCX_APPLIED"}
    cmd = _ocx_cmd(ocx, "shell", "hook", "--shell", "bash")
    result = subprocess.run(
        cmd,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        env=env_no_applied,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_shell_hook_v2_payload_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` with forged ``v2:`` payload → exit 64 (deleted)."""
    forged = "v2:" + ("de" * 32)
    result = _run(
        ocx, tmp_path, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": forged},
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 9-12. shell init → DELETED — assert exit 64
#
# Contract (handshake §2 / plan C4): ocx shell init is removed. The OCX
# installer now owns profile modification via $OCX_HOME/env.sh + block-marker.
# ---------------------------------------------------------------------------


def test_shell_init_bash_removed(ocx: OcxRunner) -> None:
    """``ocx shell init --shell bash`` → exit 64 (deleted)."""
    cmd = _ocx_cmd(ocx, "shell", "init", "--shell", "bash")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)

    assert result.returncode == EXIT_USAGE, (
        f"ocx shell init must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "init" in result.stderr.lower(), (
        f"expected clap unrecognized-subcommand stderr; got:\n{result.stderr}"
    )


def test_shell_init_zsh_removed(ocx: OcxRunner) -> None:
    """``ocx shell init --shell zsh`` → exit 64 (deleted)."""
    cmd = _ocx_cmd(ocx, "shell", "init", "--shell", "zsh")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell init must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_shell_init_fish_removed(ocx: OcxRunner) -> None:
    """``ocx shell init --shell fish`` → exit 64 (deleted)."""
    cmd = _ocx_cmd(ocx, "shell", "init", "--shell", "fish")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell init must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_shell_init_nushell_removed(ocx: OcxRunner) -> None:
    """``ocx shell init --shell nushell`` → exit 64 (deleted)."""
    cmd = _ocx_cmd(ocx, "shell", "init", "--shell", "nushell")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell init must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 13. --remote flag does NOT force network for direnv export (security boundary)
# ---------------------------------------------------------------------------


def test_remote_flag_does_not_force_network_for_direnv_export(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--remote direnv export`` with ``OCX_OFFLINE=1`` → still succeeds.

    The hook never contacts the registry regardless of ``--remote``;
    setting ``OCX_OFFLINE=1`` is the harness assertion that no network
    call was attempted (an offline-mode network call would exit non-zero).
    """
    repo, tag = _published_tool(ocx, tmp_path, "remote_hook")
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    assert _run_pull(ocx, project).returncode == EXIT_SUCCESS

    result = _run(
        ocx, project, "--remote", "direnv", "export",
        extra_env={"OCX_OFFLINE": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"--remote + OCX_OFFLINE=1 must still succeed (hook never contacts "
        f"registry); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"direnv export must still emit exports under --remote; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 14. --remote flag for shell hook → DELETED (assert exit 64)
# ---------------------------------------------------------------------------


def test_remote_flag_does_not_force_network_for_shell_hook_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` (even with ``--remote``) → exit 64 (deleted).

    The shell hook command is removed. This test replaces the prior
    'remote-does-not-force-network-for-shell-hook' contract by asserting the
    command no longer exists.
    """
    result = _run(
        ocx, tmp_path, "--remote", "shell", "hook", "--shell", "bash",
        extra_env={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook (even with --remote) must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 15. Empty _OCX_APPLIED → deleted command exits 64
# ---------------------------------------------------------------------------


def test_shell_hook_empty_applied_treated_as_no_prior_state_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell hook`` with empty ``_OCX_APPLIED`` → exit 64 (deleted).

    The _OCX_APPLIED sentinel mechanism is removed along with the shell hook
    command. Any invocation exits 64.
    """
    result = _run(
        ocx, tmp_path, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": ""},
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell hook must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 16. direnv export with ocx.toml but no ocx.lock
# ---------------------------------------------------------------------------


def test_direnv_export_missing_lock_present_toml_emits_stderr_and_exits_zero(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Project has ``ocx.toml`` but no ``ocx.lock`` → exit 0, stderr mentions lock.

    direnv export must NEVER fail the prompt cycle. A freshly cloned project
    (toml without lock) emits a one-line stderr note pointing at ``ocx lock``
    and produces no stdout exports.
    """
    repo, tag = _published_tool(ocx, tmp_path, "no_lock")
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
hello = "{ocx.registry}/{repo}:{tag}"
""",
    )
    # Deliberately skip `ocx lock` and `ocx pull`.

    result = _run(ocx, project, "direnv", "export")

    assert result.returncode == EXIT_SUCCESS, (
        f"missing lock must NOT fail direnv export; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "lock" in result.stderr.lower(), (
        f"stderr must mention 'lock' so the user knows what to run; "
        f"got:\n{result.stderr}"
    )
    assert "export" not in result.stdout, (
        f"missing lock must produce no exports; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 17/18. Top-level hook-env and shell-hook must be removed (hard-fail)
# ---------------------------------------------------------------------------


def test_top_level_hook_env_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx hook-env`` must exit non-zero (clap unrecognized-subcommand error)."""
    result = _run(ocx, tmp_path, "hook-env")

    assert result.returncode != 0, (
        f"ocx hook-env must fail with non-zero exit; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"expected UsageError exit code {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "hook-env" in result.stderr, (
        f"expected clap unrecognized-subcommand stderr mentioning 'hook-env'; got:\n"
        f"{result.stderr}"
    )


def test_top_level_shell_hook_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell-hook`` must exit non-zero (clap unrecognized-subcommand error)."""
    result = _run(ocx, tmp_path, "shell-hook")

    assert result.returncode != 0, (
        f"ocx shell-hook must fail with non-zero exit; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"expected UsageError exit code {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "shell-hook" in result.stderr, (
        f"expected clap unrecognized-subcommand stderr mentioning 'shell-hook'; got:\n"
        f"{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 19. Global activation via `ocx --global env --shell=sh` (replaces departed
#     shell hook test — plan Phase 5 / handshake §4 / C6)
#
# Contract: `ocx --global add` records into global tier; `ocx --global env --shell=sh`
# emits POSIX export lines; eval them in a subshell → tool is on PATH.
# ---------------------------------------------------------------------------


def test_global_env_sh_activation_replaces_departed_hook(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Global activation via ``ocx --global env --shell=sh`` (handshake §4 / C6).

    Replaces the prior ``test_shell_hook_global_departed_clears_stale_state``
    which tested the deleted shell-hook command. The new contract:

    1. ``ocx --global add <pkg>`` records the tool in the global toolchain.
    2. ``ocx --global env --shell=sh`` emits POSIX ``export …`` lines.
    3. Eval the output in a subshell → the tool's bin dir is on PATH.
    4. Remove the tool (``ocx --global remove``).
    5. ``ocx --global env --shell=sh`` with no global toolchain → exit 64.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_gdepart"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["gtool"])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    # Add into the global tier.
    add = _run(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; stderr:\n{add.stderr}"
    )

    # `add --global` installs (creates candidate symlink) but does NOT set the
    # `current` symlink.  `resolve_global_current_env` requires `current` symlinks.
    # Select the package so it appears in `ocx env --global` output.
    sel = _run(ocx, tmp_path, "package", "select", fq)
    assert sel.returncode == EXIT_SUCCESS, (
        f"package select must succeed after add --global; rc={sel.returncode}\nstderr:\n{sel.stderr}"
    )

    # `ocx --global env --shell=sh` must emit sourceable POSIX export lines.
    empty = tmp_path / "no_project"
    empty.mkdir()
    env_result = _run(
        ocx, empty, "--global", "env", "--shell=sh",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx --global env --shell=sh must succeed; "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export PATH=" in env_result.stdout or "export" in env_result.stdout, (
        f"--global env --shell=sh must emit export lines; got:\n{env_result.stdout}"
    )

    # Source the output in a subshell → gtool bin dir on PATH.
    # Block A1 fix: use run_after_sourcing (temp-file + dot-operator) instead of
    # eval "..." to handle paths with spaces, $, ", !, and \ correctly.
    check_result = run_after_sourcing(
        env_result.stdout,
        'command -v gtool || echo "NOT_FOUND"',
        cwd=empty,
        env=dict(ocx.env),
    )
    assert "NOT_FOUND" not in check_result.stdout, (
        f"after sourcing --global env --shell=sh output, gtool must be on PATH; "
        f"stdout:\n{check_result.stdout}\nstderr:\n{check_result.stderr}"
    )

    # Remove the tool — global toolchain is now empty (no tools, no current symlinks).
    remove = _run(ocx, tmp_path, "--global", "remove", fq)
    assert remove.returncode == EXIT_SUCCESS, (
        f"remove --global must succeed; stderr:\n{remove.stderr}"
    )

    # After removal, all tools are gone and no `current` symlinks exist.
    # `resolve_global_current_env` returns None when no current-selected tools
    # exist (same code path as "no global toolchain configured") → exit 64.
    # This matches the docstring step 5: "no global toolchain → exit 64".
    # The runtime maps "empty current-symlink set" to "no global toolchain
    # configured" because the global tier is only meaningful when tools are
    # installed AND selected.
    env_empty = _run(
        ocx, empty, "--global", "env", "--shell=sh",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert env_empty.returncode == EXIT_USAGE, (
        f"--global env --shell=sh after removing all tools must exit {EXIT_USAGE} "
        f"(no current-selected tools = 'no global toolchain configured'); "
        f"got {env_empty.returncode}\nstderr:\n{env_empty.stderr}"
    )


# ---------------------------------------------------------------------------
# 20. shell init non-POSIX → DELETED (assert exit 64)
# ---------------------------------------------------------------------------


def test_shell_init_non_posix_static_file_omits_global_prepend_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell init --shell {fish,nushell,powershell,elvish}`` → exit 64 (deleted).

    The static per-shell init file model (Fish / Nushell / PowerShell / Elvish)
    is removed along with ``ocx shell init``. The replacement activation model
    is ``ocx --global env --shell=<family>`` (C5 / handshake §4).
    """
    for shell in ("fish", "nushell", "powershell", "elvish"):
        result = _run(ocx, tmp_path, "shell", "init", "--shell", shell)
        assert result.returncode == EXIT_USAGE, (
            f"ocx shell init --shell {shell} must exit {EXIT_USAGE} (deleted); "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )


# ---------------------------------------------------------------------------
# W7 — Run-to-run determinism guard: `ocx direnv export` byte-stability
#
# Proves that `ocx direnv export` produces byte-identical output on
# consecutive invocations against the same locked project (no non-determinism
# from ordering, timestamps, random UUIDs, etc.).  This guard is independent
# of any particular refactor — it remains a permanent regression fence.
# ---------------------------------------------------------------------------


def test_direnv_export_byte_stability_after_emit_lines_extraction(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx direnv export`` output is byte-identical across consecutive runs.

    This test proves run-to-run determinism: given the same locked project,
    two back-to-back ``ocx direnv export`` invocations must produce exactly
    the same bytes.  Any non-determinism (output ordering, stray timestamps,
    random IDs, non-reproducible quoting) is caught here.

    The name retains the historical ``emit_lines_extraction`` suffix for
    traceability to plan_toolchain_cli.md Phase 2 where it was authored;
    the assertion itself is a permanent determinism fence, not a pre/post-
    refactor identity check.

    Setup:
    1. Publish a package with one PATH-modifier and one CONSTANT-modifier
       env entry so both modifier paths are exercised in a single run.
    2. Lock + pull the project.
    3. Run ``ocx direnv export`` twice and assert byte-identical stdout.

    Shell is always ``Bash`` (``direnv_export.rs`` hardcodes ``Shell::Bash``
    and does not expose ``--shell``; that is part of the characterised
    contract — do not add a ``--shell`` flag to ``direnv export``).
    """
    from uuid import uuid4 as _uuid4
    from src.helpers import make_package as _make_package

    short = _uuid4().hex[:8]
    repo = f"t_{short}_w7stab"

    # Publish a package with explicit PATH + CONSTANT env entries so both
    # modifier kinds are exercised (full coverage of the emit loop).
    pkg_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin", "visibility": "public"},
        {
            "key": repo.upper().replace("-", "_") + "_HOME",
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
    ]
    _make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, env=pkg_env)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project = tmp_path / "proj_w7"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f"[tools]\ntool = \"{fq}\"\n"
    )

    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS, (
        "ocx lock must succeed for the W7 characterization fixture"
    )
    assert _run_pull(ocx, project).returncode == EXIT_SUCCESS, (
        "ocx pull must succeed for the W7 characterization fixture"
    )

    # First run: capture baseline output.
    first = _run(ocx, project, "direnv", "export")
    assert first.returncode == EXIT_SUCCESS, (
        f"W7 baseline: direnv export must exit 0; stderr:\n{first.stderr}"
    )
    baseline = first.stdout
    assert "export" in baseline, (
        f"W7 baseline: expected at least one export line; got:\n{baseline!r}"
    )

    # Second run: output must be byte-identical (characterisation contract).
    second = _run(ocx, project, "direnv", "export")
    assert second.returncode == EXIT_SUCCESS, (
        f"W7 second run: direnv export must exit 0; stderr:\n{second.stderr}"
    )
    assert second.stdout == baseline, (
        "W7 byte-stability: direnv export output is not deterministic across runs; "
        "output changed between the first and second invocation.\n"
        f"Expected:\n{baseline!r}\nGot:\n{second.stdout!r}"
    )
