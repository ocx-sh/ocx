# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for Phase 7 — shell hook trio.

Covers ``ocx shell direnv``, ``ocx shell hook``, ``ocx shell init`` per
``.claude/state/plans/plan_project_toolchain.md`` lines 755–798 and ADR §5B
(decision 5B contracts at lines 320–365 of
``.claude/artifacts/adr_project_toolchain_config.md``).

Specification mode (contract-first TDD)
---------------------------------------
The Phase 7 stubs at ``crates/ocx_cli/src/command/{shell_direnv,shell_hook,shell_init}.rs``
return ``unimplemented!()``. Every test in this file is therefore expected to
FAIL against today's binary — the contract they encode is the Phase 7
implementation target. Tests assert on:

- exit codes (sysexits-aligned via ``crates/ocx_lib/src/cli/exit_code.rs``)
- shell-syntax validity (``bash -n`` / ``shellcheck`` of generated output)
- stdout shape (presence/absence of ``export`` lines, ``unset`` lines, the
  ``_OCX_APPLIED`` sentinel)
- stderr substrings for missing-tool / stale-lock notes
- security-boundary invariants (no network when ``--remote`` is set)

Test inventory
--------------
1.  ``test_shell_direnv_emits_bash_exports``
2.  ``test_shell_direnv_skips_uninstalled_tool_with_stderr_note``
3.  ``test_shell_direnv_warns_on_stale_lock``
4.  ``test_shell_direnv_no_project_emits_nothing``
5.  ``test_shell_hook_unchanged_fingerprint_emits_empty``
6.  ``test_shell_hook_changed_fingerprint_emits_unset_and_reexport``
7.  ``test_shell_hook_no_prior_applied_emits_full_set``
8.  ``test_shell_hook_v2_payload_treated_as_changed`` (architect-mandated)
9.  ``test_shell_init_bash_outputs_prompt_command_snippet``
10. ``test_shell_init_zsh_uses_add_zsh_hook``
11. ``test_shell_init_fish_uses_prompt_event_hook``
12. ``test_shell_init_nushell_writes_autoload_path``
13. ``test_remote_flag_does_not_force_network_for_shell_direnv``
14. ``test_remote_flag_does_not_force_network_for_shell_hook``
15. ``test_shell_hook_empty_applied_treated_as_no_prior_state`` (Round 2)
16. ``test_shell_direnv_missing_lock_present_toml_emits_stderr_and_exits_zero`` (Round 2)
17. ``test_top_level_hook_env_removed``
18. ``test_top_level_shell_hook_removed``
"""
from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0


# ---------------------------------------------------------------------------
# Helpers — co-located with tests (DAMP > DRY for acceptance tests).
# Mirror shapes in ``test_project_pull.py`` and ``test_exec_compose.py``.
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


_APPLIED_RE = re.compile(r'_OCX_APPLIED\s*=\s*"(v1:[0-9a-f]{64})"')


def _extract_applied(stdout: str) -> str | None:
    """Pull the ``v1:<64-hex>`` value from an ``export _OCX_APPLIED=...`` line."""
    m = _APPLIED_RE.search(stdout)
    return m.group(1) if m else None


def _bash_syntax_ok(script: str) -> tuple[bool, str]:
    """Return ``(ok, stderr)`` after running ``bash -n -c <script>``."""
    proc = subprocess.run(
        ["bash", "-n", "-c", script],
        capture_output=True,
        text=True,
    )
    return proc.returncode == 0, proc.stderr


# ---------------------------------------------------------------------------
# 1. shell direnv emits valid bash exports for installed tools
# ---------------------------------------------------------------------------


def test_shell_direnv_emits_bash_exports(ocx: OcxRunner, tmp_path: Path) -> None:
    """Plan §7 line 787: project with installed tool → valid bash exports."""
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

    result = _run(ocx, project, "shell", "direnv", "--shell", "bash")

    assert result.returncode == EXIT_SUCCESS, (
        f"shell direnv must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"expected at least one export line; got stdout:\n{result.stdout}"
    )
    ok, syn_err = _bash_syntax_ok(result.stdout)
    assert ok, f"shell direnv output failed bash -n: {syn_err}\nstdout:\n{result.stdout}"


# ---------------------------------------------------------------------------
# 2. shell direnv skips uninstalled tool with a stderr note
# ---------------------------------------------------------------------------


def test_shell_direnv_skips_uninstalled_tool_with_stderr_note(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §7 line 768: missing tool → stderr ``# ocx: <name> not installed``.

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
    # Pull only the first one. `--group default` would pull both; we want
    # exactly one in the store, so we drive `package pull` directly with the
    # fully-qualified identifier.
    assert (
        _run(ocx, project, "package", "pull", f"{ocx.registry}/{repo_installed}:{tag_installed}").returncode
        == EXIT_SUCCESS
    )

    result = _run(ocx, project, "shell", "direnv", "--shell", "bash")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    # The missing tool's stderr note format from plan §7 line 768.
    assert "beta" in result.stderr, (
        f"missing-tool note must reference 'beta'; stderr:\n{result.stderr}"
    )
    assert "not installed" in result.stderr, (
        f"missing-tool note must say 'not installed'; stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 3. shell direnv with stale lock → stderr warning, exports use stale digests
# ---------------------------------------------------------------------------


def test_shell_direnv_warns_on_stale_lock(ocx: OcxRunner, tmp_path: Path) -> None:
    """Plan §7 line 770: stale lock → stderr warning, continue with stale digests.

    Distinct from ``ocx exec`` which exits 65 on stale lock. ``shell direnv``
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

    result = _run(ocx, project, "shell", "direnv", "--shell", "bash")

    assert result.returncode == EXIT_SUCCESS, (
        f"shell direnv with stale lock must NOT fail (unlike ocx exec); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    stderr_lower = result.stderr.lower()
    assert "stale" in stderr_lower or "ocx.toml changed" in stderr_lower, (
        f"stale-lock warning expected on stderr; got:\n{result.stderr}"
    )
    # Even with the stale lock, exports for the originally-locked tool stay.
    assert "export" in result.stdout, (
        f"stale lock still emits exports based on locked digests; got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 4. No project, no output (Phase 7 line 762, 795)
# ---------------------------------------------------------------------------


def test_shell_direnv_no_project_emits_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """Plan §7 line 762/795: no ocx.toml in tree → exit 0, nothing written.

    Phase 9 introduces home-tier fallback; Phase 7 must no-op. Set
    ``OCX_NO_PROJECT=1`` so the home-tier fallback (when added in Phase 9)
    cannot mask a missing project file.
    """
    empty = tmp_path / "no_project"
    empty.mkdir()

    result = _run(
        ocx, empty, "shell", "direnv", "--shell", "bash",
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"no-project case must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" not in result.stdout, (
        f"no-project case must emit nothing; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 5. shell hook fast path: unchanged fingerprint → empty output
# ---------------------------------------------------------------------------


def test_shell_hook_unchanged_fingerprint_emits_empty(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §7 line 775: unchanged ``_OCX_APPLIED`` → empty stdout."""
    repo, tag = _published_tool(ocx, tmp_path, "fp_same")
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

    first = _run(ocx, project, "shell", "hook", "--shell", "bash")
    assert first.returncode == EXIT_SUCCESS, first.stderr
    applied = _extract_applied(first.stdout)
    assert applied is not None, (
        f"first shell hook must export _OCX_APPLIED; got:\n{first.stdout}"
    )

    second = _run(
        ocx, project, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": applied},
    )

    assert second.returncode == EXIT_SUCCESS, second.stderr
    # Tightened (Round 2 W4): plain `==` against `""` so a spurious
    # newline in the fast path is caught — `.strip()` would mask it.
    assert second.stdout == "", (
        f"unchanged fingerprint must emit empty stdout; got:\n{second.stdout!r}"
    )


# ---------------------------------------------------------------------------
# 6. shell hook diff path: change → unset old + new export + new applied
# ---------------------------------------------------------------------------


def test_shell_hook_changed_fingerprint_emits_unset_and_reexport(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §7 line 776: fingerprint change → unset + new exports + new sentinel."""
    repo_a, tag_a = _published_tool(ocx, tmp_path, "diff_a", bin_name="alpha")
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

    first = _run(ocx, project, "shell", "hook", "--shell", "bash")
    old_applied = _extract_applied(first.stdout)
    assert old_applied is not None, first.stdout

    # Add a second tool; re-lock + re-pull.
    repo_b, tag_b = _published_tool(ocx, tmp_path, "diff_b", bin_name="beta")
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS
    assert _run_pull(ocx, project).returncode == EXIT_SUCCESS

    second = _run(
        ocx, project, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": old_applied},
    )

    assert second.returncode == EXIT_SUCCESS, second.stderr
    assert "unset" in second.stdout, (
        f"changed fingerprint must emit unset for previously-applied vars; "
        f"got stdout:\n{second.stdout}"
    )
    assert "export" in second.stdout, (
        f"changed fingerprint must emit new exports; got stdout:\n{second.stdout}"
    )
    new_applied = _extract_applied(second.stdout)
    assert new_applied is not None and new_applied != old_applied, (
        f"new _OCX_APPLIED must be exported and differ from old; "
        f"old={old_applied!r}, found={new_applied!r}"
    )


# ---------------------------------------------------------------------------
# 7. shell hook with no prior applied → full export set, no unset
# ---------------------------------------------------------------------------


def test_shell_hook_no_prior_applied_emits_full_set(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """No ``_OCX_APPLIED`` set → emit full export set + sentinel; nothing to unset."""
    repo, tag = _published_tool(ocx, tmp_path, "fp_none")
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

    # Drop _OCX_APPLIED from the runner's env if present (it isn't by default).
    env_no_applied = {k: v for k, v in ocx.env.items() if k != "_OCX_APPLIED"}
    cmd = _ocx_cmd(ocx, "shell", "hook", "--shell", "bash")
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=env_no_applied,
    )

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "export" in result.stdout, (
        f"first shell hook run must emit exports; got stdout:\n{result.stdout}"
    )
    assert _extract_applied(result.stdout) is not None, (
        f"first shell hook run must set _OCX_APPLIED; got stdout:\n{result.stdout}"
    )
    assert "unset" not in result.stdout, (
        f"no prior applied state → nothing to unset; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 8. (architect-mandated) v2 payload must NOT silently match v1
# ---------------------------------------------------------------------------


def test_shell_hook_v2_payload_treated_as_changed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Forged ``_OCX_APPLIED=v2:...`` must NOT be treated as fingerprint match.

    Architect Phase 4 finding: a future v2 wire format must trigger a full
    re-export (parse rejects v2 → unknown prior state → full export). The
    output must include a fresh ``v1:<hex>`` sentinel — never re-exporting
    the v2 payload that was forged in.
    """
    repo, tag = _published_tool(ocx, tmp_path, "v2_payload")
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

    forged = "v2:" + ("de" * 32)  # 64-hex body, future-version prefix
    result = _run(
        ocx, project, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": forged},
    )

    assert result.returncode == EXIT_SUCCESS, result.stderr
    new_applied = _extract_applied(result.stdout)
    assert new_applied is not None, (
        f"v2 payload must be treated as unknown → emit fresh v1: sentinel; "
        f"got stdout:\n{result.stdout}"
    )
    assert new_applied.startswith("v1:"), (
        f"sentinel must always be v1: in this wire-version; got {new_applied!r}"
    )
    assert "export" in result.stdout, (
        f"v2 payload must trigger full re-export; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 9. shell init bash → PROMPT_COMMAND, syntactically valid
# ---------------------------------------------------------------------------


def test_shell_init_bash_outputs_prompt_command_snippet(ocx: OcxRunner) -> None:
    """Plan §7 line 781: Bash init wires into ``PROMPT_COMMAND``."""
    cmd = _ocx_cmd(ocx, "shell", "init", "bash")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "PROMPT_COMMAND" in result.stdout, (
        f"bash init must reference PROMPT_COMMAND; got:\n{result.stdout}"
    )

    if shutil.which("shellcheck") is None:
        # Fall back to bash -n if shellcheck isn't installed.
        ok, syn_err = _bash_syntax_ok(result.stdout)
        assert ok, f"shell init bash failed bash -n: {syn_err}\n{result.stdout}"
        return

    sc = subprocess.run(
        ["shellcheck", "-s", "bash", "-"],
        input=result.stdout,
        capture_output=True,
        text=True,
    )
    assert sc.returncode == 0, (
        f"shellcheck must accept shell init bash output; "
        f"rc={sc.returncode}\nstderr:\n{sc.stderr}\nstdout:\n{sc.stdout}\n"
        f"snippet:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 10. shell init zsh → add-zsh-hook precmd
# ---------------------------------------------------------------------------


def test_shell_init_zsh_uses_add_zsh_hook(ocx: OcxRunner) -> None:
    """Plan §7 line 782: Zsh init uses ``add-zsh-hook precmd``."""
    cmd = _ocx_cmd(ocx, "shell", "init", "zsh")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "add-zsh-hook precmd" in result.stdout, (
        f"zsh init must use 'add-zsh-hook precmd'; got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 11. shell init fish → on-variable PWD
# ---------------------------------------------------------------------------


def test_shell_init_fish_uses_prompt_event_hook(ocx: OcxRunner) -> None:
    """Plan §7 line 783 (Round 2 W2): Fish init fires on every prompt.

    The original ``--on-variable PWD`` choice fired only on ``cd``; the
    correct cadence for re-evaluating the project toolchain is
    ``--on-event fish_prompt``, which matches mise/direnv. The substring
    we assert on is the function definition prefix so the test stays
    lenient if the snippet's body wording shifts.
    """
    cmd = _ocx_cmd(ocx, "shell", "init", "fish")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "--on-event fish_prompt" in result.stdout, (
        f"fish init must use '--on-event fish_prompt'; got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 12. shell init nushell → autoload path, no eval
# ---------------------------------------------------------------------------


def test_shell_init_nushell_writes_autoload_path(ocx: OcxRunner) -> None:
    """Plan §7 line 784: Nushell init references ``NU_VENDOR_AUTOLOAD_DIR`` (file-based, no eval)."""
    cmd = _ocx_cmd(ocx, "shell", "init", "nushell")
    result = subprocess.run(cmd, capture_output=True, text=True, env=ocx.env)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "NU_VENDOR_AUTOLOAD_DIR" in result.stdout, (
        f"nushell init must reference NU_VENDOR_AUTOLOAD_DIR (file-based init); "
        f"got:\n{result.stdout}"
    )
    # Plan line 784: "no stdout eval".
    assert "eval" not in result.stdout.lower(), (
        f"nushell init must NOT use eval (file-based init); got:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 13/14. (architect-mandated boundary) --remote must NOT force network
# ---------------------------------------------------------------------------


def test_remote_flag_does_not_force_network_for_shell_direnv(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Architect: ``--remote shell direnv`` with ``OCX_OFFLINE=1`` → still succeeds.

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
        ocx, project, "--remote", "shell", "direnv", "--shell", "bash",
        extra_env={"OCX_OFFLINE": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"--remote + OCX_OFFLINE=1 must still succeed (hook never contacts "
        f"registry); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "export" in result.stdout, (
        f"shell direnv must still emit exports under --remote; got stdout:\n{result.stdout}"
    )


def test_remote_flag_does_not_force_network_for_shell_hook(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Architect: ``--remote shell hook`` with ``OCX_OFFLINE=1`` → still succeeds."""
    repo, tag = _published_tool(ocx, tmp_path, "remote_hookenv")
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
        ocx, project, "--remote", "shell", "hook", "--shell", "bash",
        extra_env={"OCX_OFFLINE": "1"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"--remote + OCX_OFFLINE=1 must still succeed for shell hook; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _extract_applied(result.stdout) is not None, (
        f"shell hook must still emit _OCX_APPLIED under --remote; "
        f"got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 15. (Round 2 W4) Empty `_OCX_APPLIED` env var → treated as no prior state
# ---------------------------------------------------------------------------


def test_shell_hook_empty_applied_treated_as_no_prior_state(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """An empty ``_OCX_APPLIED=""`` value behaves like the env var being unset.

    Edge case the strict v1 parser must handle gracefully: ``parse_applied``
    rejects an empty string (not `v1:` prefixed), so the hook must take the
    no-prior-state branch (full export set, fresh sentinel) without error.
    """
    repo, tag = _published_tool(ocx, tmp_path, "fp_empty")
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
        ocx, project, "shell", "hook", "--shell", "bash",
        extra_env={"_OCX_APPLIED": ""},
    )

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _extract_applied(result.stdout) is not None, (
        f"empty _OCX_APPLIED must trigger full re-export with fresh sentinel; "
        f"got stdout:\n{result.stdout}"
    )
    assert "export" in result.stdout, (
        f"empty _OCX_APPLIED must emit exports; got stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 16. (Round 2 W4) shell direnv with ocx.toml but no ocx.lock
# ---------------------------------------------------------------------------


def test_shell_direnv_missing_lock_present_toml_emits_stderr_and_exits_zero(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Project has ``ocx.toml`` but no ``ocx.lock`` → exit 0, stderr mentions lock.

    Mirrors the prompt-hook contract from plan §7 line 770: shell direnv
    must NEVER fail the prompt cycle. A freshly cloned project (toml without
    lock) emits a one-line stderr note pointing at ``ocx lock`` and produces
    no stdout exports.
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

    result = _run(ocx, project, "shell", "direnv", "--shell", "bash")

    assert result.returncode == EXIT_SUCCESS, (
        f"missing lock must NOT fail shell direnv; rc={result.returncode}\n"
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
    """``ocx hook-env`` must exit non-zero (clap unrecognized-subcommand error).

    The command was moved to ``ocx shell hook``. No deprecation shim — the
    old top-level name must hard-fail per the breaking-compat memo.
    """
    result = _run(ocx, tmp_path, "hook-env")

    assert result.returncode != 0, (
        f"ocx hook-env must fail with non-zero exit; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # ocx classifies clap usage errors as ExitCode::UsageError (64, sysexits).
    assert result.returncode == 64, (
        f"expected UsageError exit code 64; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "hook-env" in result.stderr, (
        f"expected clap unrecognized-subcommand stderr mentioning 'hook-env'; got:\n"
        f"{result.stderr}"
    )


def test_top_level_shell_hook_removed(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell-hook`` must exit non-zero (clap unrecognized-subcommand error).

    The command was moved to ``ocx shell direnv``. No deprecation shim — the
    old top-level name must hard-fail per the breaking-compat memo.
    """
    result = _run(ocx, tmp_path, "shell-hook")

    assert result.returncode != 0, (
        f"ocx shell-hook must fail with non-zero exit; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    # ocx classifies clap usage errors as ExitCode::UsageError (64, sysexits).
    assert result.returncode == 64, (
        f"expected UsageError exit code 64; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "unrecognized" in result.stderr.lower() or "shell-hook" in result.stderr, (
        f"expected clap unrecognized-subcommand stderr mentioning 'shell-hook'; got:\n"
        f"{result.stderr}"
    )
