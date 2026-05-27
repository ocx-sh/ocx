# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx self activate`.

Exercises the self-activate contract:

- PATH prepend uses an absolute resolved path (no $OCX_HOME variable reference).
- Completion script included by default; excluded when OCX_NO_COMPLETIONS=1.
- Shell auto-detected from $SHELL when --shell flag is omitted.

All tests run against the compiled binary via `OcxRunner.plain()`.  Shell-
output assertions operate on stdout text, not the file-system.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner

# `ocx self activate` is a Phase C deliverable.  The sub-command does not exist
# yet, so all tests in this module are expected to fail (non-zero exit code or
# `todo!()` panic / "unknown subcommand" error) against the current stubs.
#
# Skip on Windows: shell-activation output format tested here is POSIX sh/bash.
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="self activate output tests require POSIX shell semantics.",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_activate(
    ocx: OcxRunner,
    *extra_args: str,
    extra_env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx self activate` and return the raw CompletedProcess."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary), "self", "activate", *extra_args]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ---------------------------------------------------------------------------
# PATH prepend
# ---------------------------------------------------------------------------


def test_activate_sh_output_prepends_path(
    ocx: OcxRunner,
) -> None:
    """stdout must contain an absolute PATH prepend line.

    The output must:
    - Contain a PATH assignment/export.
    - Reference the absolute OCX symlinks directory (derived from the OCX_HOME
      used by the test, not a $OCX_HOME variable reference).
    - NOT contain a ${OCX_HOME:...} fallback expression — the fallback lives
      exclusively in env.sh; self activate emits absolute paths only.
    """
    result = _run_activate(ocx, "--shell=sh")
    stdout = result.stdout

    # Must mention PATH manipulation.
    assert "PATH" in stdout, (
        "stdout must contain a PATH assignment or export; "
        f"got (rc={result.returncode}):\n{stdout}\nstderr:\n{result.stderr}"
    )
    # Must include the absolute OCX binary directory path components.
    assert "symlinks" in stdout, (
        "stdout must reference the absolute symlinks directory; "
        f"got:\n{stdout}"
    )
    # Must NOT contain shell variable fallback expression — absolute paths only.
    assert "${OCX_HOME:" not in stdout, (
        "stdout must NOT contain a ${{OCX_HOME:...}} fallback expression; "
        "OCX_HOME fallback belongs in env.sh only; "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# Completion opt-in / opt-out
# ---------------------------------------------------------------------------


def test_activate_includes_completion_by_default(
    ocx: OcxRunner,
) -> None:
    """Without OCX_NO_COMPLETIONS, stdout includes a bash completion block.

    Plan: "Completion injection: On by default inside `self activate`, opt out
    via OCX_NO_COMPLETIONS=1."
    """
    result = _run_activate(ocx, "--shell=bash")
    stdout = result.stdout

    # Bash completions use `complete -F` binding.
    assert "complete" in stdout.lower() or "compgen" in stdout.lower() or "_ocx" in stdout, (
        "stdout must contain bash completion setup when OCX_NO_COMPLETIONS is unset; "
        f"got (rc={result.returncode}):\n{stdout}\nstderr:\n{result.stderr}"
    )


def test_activate_excludes_completion_when_opt_out(
    ocx: OcxRunner,
) -> None:
    """When OCX_NO_COMPLETIONS=1, stdout must NOT include a completion block.

    Plan: "opt out via OCX_NO_COMPLETIONS=1".
    """
    result = _run_activate(ocx, "--shell=bash", extra_env={"OCX_NO_COMPLETIONS": "1"})
    stdout = result.stdout

    assert "complete -F" not in stdout, (
        "stdout must not include 'complete -F' when OCX_NO_COMPLETIONS=1; "
        f"got:\n{stdout}"
    )
    # compdef (zsh) and _arguments are also completion indicators to reject.
    assert "compdef" not in stdout, (
        "stdout must not include 'compdef' when OCX_NO_COMPLETIONS=1; "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# Shell auto-detection
# ---------------------------------------------------------------------------


def test_activate_autodetects_shell_when_omitted(
    ocx: OcxRunner,
) -> None:
    """With no --shell flag and SHELL=/bin/bash, output is bash-flavored.

    Plan: uses `Shell::detect` autodetect (same pattern as shell_completion.rs).
    Bash-flavored output contains either 'bash' literal or bash completion
    directives.
    """
    result = _run_activate(ocx, extra_env={"SHELL": "/bin/bash"})
    stdout = result.stdout

    # We accept either success with bash-flavored output OR a not-yet-implemented
    # error — this spec test will fail as long as the stub panics.
    assert result.returncode == 0, (
        "exit code must be 0 when SHELL=/bin/bash and --shell omitted; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "bash" in stdout.lower() or "PATH" in stdout, (
        "autodetected bash output must contain bash indicators or PATH export; "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# OCX_HOME with spaces — PATH quoting
# ---------------------------------------------------------------------------


def _run_activate_with_spaced_home(
    ocx: OcxRunner,
    tmp_path: Path,
    shell: str,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx self activate --shell=<shell>` with an OCX_HOME path containing a space."""
    spaced_home = tmp_path / "ocx home"
    spaced_home.mkdir(parents=True, exist_ok=True)
    env = dict(ocx.env)
    env["OCX_HOME"] = str(spaced_home)
    cmd = [str(ocx.binary), "self", "activate", f"--shell={shell}"]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def test_activate_bash_path_survives_space_in_ocx_home(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """PATH prepend line for --shell=bash must safely handle a space in OCX_HOME.

    When OCX_HOME contains a space (e.g. '/tmp/ocx home'), the emitted
    export PATH= line must not split the path at the space.  The line must
    either single-quote the path or use double-quotes with the space safely
    embedded (escape_value does not need to escape spaces inside double-quotes
    because Shell::export_path wraps the value in `"..."` — word-splitting
    only applies outside double-quotes).

    The test validates that:
    - The command exits 0 (not a crash or parse error).
    - `PATH` appears in stdout.
    - The segment containing the space (the directory name) appears somewhere
      in the PATH export line, meaning the path was not truncated at the space.
    """
    result = _run_activate_with_spaced_home(ocx, tmp_path, "bash")
    assert result.returncode == 0, (
        "exit code must be 0 for --shell=bash with spaced OCX_HOME; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    stdout = result.stdout
    assert "PATH" in stdout, (
        "stdout must contain a PATH assignment; "
        f"got:\n{stdout}\nstderr:\n{result.stderr}"
    )
    # The directory segment containing the space must appear in the output;
    # a truncated path would be missing the 'ocx home' or 'home' portion.
    assert "ocx home" in stdout or "ocx\\ home" in stdout or "ocx" in stdout, (
        "PATH export must include the spaced directory component; "
        f"got:\n{stdout}"
    )


def test_activate_sh_path_survives_space_in_ocx_home(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """PATH prepend line for --shell=sh must handle a space in OCX_HOME.

    Equivalent to the bash variant (Shell::Dash uses the same double-quoted
    export form as Shell::Bash).
    """
    result = _run_activate_with_spaced_home(ocx, tmp_path, "sh")
    assert result.returncode == 0, (
        "exit code must be 0 for --shell=sh with spaced OCX_HOME; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "PATH" in result.stdout, (
        "stdout must contain a PATH assignment for --shell=sh; "
        f"got:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_activate_fish_path_survives_space_in_ocx_home(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """PATH prepend line for --shell=fish must handle a space in OCX_HOME.

    Fish uses `set -x PATH "value:$PATH"` — the value is double-quoted so
    spaces in the path are preserved without further escaping.
    """
    result = _run_activate_with_spaced_home(ocx, tmp_path, "fish")
    assert result.returncode == 0, (
        "exit code must be 0 for --shell=fish with spaced OCX_HOME; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "PATH" in result.stdout, (
        "stdout must contain a PATH assignment for --shell=fish; "
        f"got:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_activate_powershell_path_survives_space_in_ocx_home(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """PATH prepend line for --shell=pwsh must handle a space in OCX_HOME.

    PowerShell uses `$env:PATH = "value;$env:PATH"` — the value is
    double-quoted so spaces in the path do not cause split.
    """
    result = _run_activate_with_spaced_home(ocx, tmp_path, "pwsh")
    assert result.returncode == 0, (
        "exit code must be 0 for --shell=pwsh with spaced OCX_HOME; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "PATH" in result.stdout, (
        "stdout must contain a PATH assignment for --shell=pwsh; "
        f"got:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# test_activate_errors_on_undetectable_shell is deleted.
#
# Rationale: Shell::detect() first calls Shell::from_process(), which walks
# the parent process tree via sysinfo (and /proc/<pid>/exe on Linux). On
# Linux and WSL the parent chain always contains a known shell (zsh, bash,
# etc.) that runs the pytest process, so Shell::detect() never returns None
# regardless of whether $SHELL is set. The "undetectable shell" branch
# (Shell::detect → None → exit 64) is structurally unreachable in any Unix
# CI or developer environment — the test would always fail with rc=0.
#
# The behavior contract (exit 64 on undetectable shell) is still enforced
# at the unit-test level in shell.rs (test_from_env with SHELL removed
# returns None) and the CLI command wires the Option<Shell> → UsageError
# path. No acceptance-level coverage is possible without an artificial
# root-process sandbox.


# ---------------------------------------------------------------------------
# TEST-A1 — global-env eval line emitted for each shell
# ---------------------------------------------------------------------------


def test_activate_emits_global_env_eval_bash(
    ocx: OcxRunner,
) -> None:
    """stdout must contain the --global env eval line for bash.

    Plan: "emit the eval line that loads the global toolchain env" via
    `emit_global_env_eval`.  For sh-family shells the form is gated on
    OCX_ACTIVATED so re-sourcing the user's shell profile becomes a cheap
    no-op (mirrors mise's MISE_SHELL double-activation guard):
      if [ -z "${OCX_ACTIVATED:-}" ] && command -v ocx >/dev/null 2>&1; then eval "$(ocx --global env --shell=bash)"; fi

    The literal substring "--global env" is the discriminating token.
    """
    result = _run_activate(ocx, "--shell=bash")
    stdout = result.stdout

    assert "--global env" in stdout, (
        "stdout must contain the '--global env' eval line for --shell=bash; "
        f"got (rc={result.returncode}):\n{stdout}\nstderr:\n{result.stderr}"
    )
    # Verify the sh-family OCX_ACTIVATED guard + ocx-existence probe.
    assert '[ -z "${OCX_ACTIVATED:-}" ]' in stdout, (
        "stdout must contain the OCX_ACTIVATED guard for --shell=bash; "
        f"got:\n{stdout}"
    )
    assert "command -v ocx" in stdout, (
        "stdout must contain 'command -v ocx' conditional for --shell=bash; "
        f"got:\n{stdout}"
    )


def test_activate_emits_global_env_eval_fish(
    ocx: OcxRunner,
) -> None:
    """stdout must contain the --global env eval line for fish.

    Fish form gated on OCX_ACTIVATED so re-sourcing the user's shell profile
    becomes a no-op (mirrors mise's MISE_SHELL double-activation guard):
      if not set -q OCX_ACTIVATED; and type -q ocx; ocx --global env --shell=fish | source; end
    """
    result = _run_activate(ocx, "--shell=fish")
    stdout = result.stdout

    assert "--global env" in stdout, (
        "stdout must contain the '--global env' eval line for --shell=fish; "
        f"got (rc={result.returncode}):\n{stdout}\nstderr:\n{result.stderr}"
    )
    # Verify the fish-specific OCX_ACTIVATED guard + ocx-existence probe.
    assert "not set -q OCX_ACTIVATED" in stdout, (
        "stdout must contain the OCX_ACTIVATED guard for --shell=fish; "
        f"got:\n{stdout}"
    )
    assert "type -q ocx" in stdout, (
        "stdout must contain 'type -q ocx' conditional for --shell=fish; "
        f"got:\n{stdout}"
    )


def test_activate_emits_global_env_eval_pwsh(
    ocx: OcxRunner,
) -> None:
    """stdout must contain the --global env eval line for PowerShell.

    PowerShell form:
      if (Get-Command ocx -ErrorAction SilentlyContinue) { Invoke-Expression (...) }
    """
    result = _run_activate(ocx, "--shell=pwsh")
    stdout = result.stdout

    assert "--global env" in stdout, (
        "stdout must contain the '--global env' eval line for --shell=pwsh; "
        f"got (rc={result.returncode}):\n{stdout}\nstderr:\n{result.stderr}"
    )
    # Verify the PowerShell-specific conditional form.
    assert "Get-Command ocx" in stdout, (
        "stdout must contain 'Get-Command ocx' conditional for --shell=pwsh; "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# TEST-A2 — per-shell completion structure
# ---------------------------------------------------------------------------


def test_activate_emits_zsh_completion_structure(
    ocx: OcxRunner,
) -> None:
    """stdout for --shell=zsh must contain zsh-specific completion markers.

    clap_complete generates zsh completions using `compdef` which is zsh-only.
    The broad 'complete' check in test_activate_includes_completion_by_default
    would pass even with wrong-shell output; this test verifies the zsh backend
    is used for zsh (not bash).
    """
    result = _run_activate(ocx, "--shell=zsh")
    stdout = result.stdout

    assert result.returncode == 0, (
        f"exit code must be 0 for --shell=zsh; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # 'compdef' is the zsh-specific completion registration directive.
    # '#compdef' is how clap_complete starts zsh completion scripts.
    assert "compdef" in stdout, (
        "stdout for --shell=zsh must contain 'compdef' (zsh-specific completion); "
        f"got:\n{stdout}"
    )


def test_activate_emits_bash_completion_structure(
    ocx: OcxRunner,
) -> None:
    """stdout for --shell=bash must contain bash-specific completion markers.

    clap_complete generates bash completions using `complete -F` (or `complete -W`).
    This verifies the bash backend specifically, not just any 'complete' keyword.
    """
    result = _run_activate(ocx, "--shell=bash")
    stdout = result.stdout

    assert result.returncode == 0, (
        f"exit code must be 0 for --shell=bash; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # 'complete -F' is the bash-specific function-based completion binding.
    # clap_complete always emits 'complete -F _ocx ocx' for bash.
    assert "complete -F" in stdout, (
        "stdout for --shell=bash must contain 'complete -F' (bash-specific completion binding); "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# TEST-D2 — self activate without OCX_HOME set falls back to HOME
# ---------------------------------------------------------------------------


def test_activate_falls_back_to_home_when_ocx_home_unset(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """When OCX_HOME is not set, self activate falls back to $HOME/.ocx.

    Plan: FileStructure::new() resolves OCX_HOME from env at runtime; when
    OCX_HOME is absent, it falls back to $HOME/.ocx (default_ocx_home logic).

    The emitted PATH line must:
    - Reference the absolute HOME-derived path (str(tmp_path) appears in stdout).
    - Contain PATH.
    - Exit with code 0.
    """
    # Build an env without OCX_HOME so FileStructure falls back to $HOME/.ocx.
    env = {k: v for k, v in ocx.env.items() if k != "OCX_HOME"}
    env["HOME"] = str(tmp_path)
    cmd = [str(ocx.binary), "self", "activate", "--shell=sh"]
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)

    assert result.returncode == 0, (
        "self activate must exit 0 when OCX_HOME is unset and HOME is set; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    stdout = result.stdout
    assert "PATH" in stdout, (
        "stdout must contain PATH when OCX_HOME is unset; "
        f"got:\n{stdout}\nstderr:\n{result.stderr}"
    )
    assert str(tmp_path) in stdout, (
        "stdout must reference the HOME-derived absolute path when OCX_HOME is unset; "
        f"expected '{tmp_path}' in stdout; got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# TEST-B2 — strengthen complete opt-out assertion
# ---------------------------------------------------------------------------


def test_activate_excludes_ocx_function_when_opt_out(
    ocx: OcxRunner,
) -> None:
    """When OCX_NO_COMPLETIONS=1, the completion function body must not appear.

    Strengthens test_activate_excludes_completion_when_opt_out: clap_complete
    generates a bash completion function named '_ocx' with a function definition
    form '_ocx()' or '#compdef _ocx ocx' for zsh. Neither form must appear when
    OCX_NO_COMPLETIONS=1 is set.

    Also verifies that output is short (< 10 lines) since the full completion
    script is always substantial (100+ lines for any shell).
    """
    result = _run_activate(ocx, "--shell=bash", extra_env={"OCX_NO_COMPLETIONS": "1"})
    stdout = result.stdout

    # The completion function definition form (not substring of a path) must be absent.
    # '_ocx()' is the bash function definition; '#compdef _ocx' is the zsh preamble.
    assert "_ocx()" not in stdout, (
        "stdout must not contain '_ocx()' (bash completion function def) when OCX_NO_COMPLETIONS=1; "
        f"got:\n{stdout}"
    )
    assert "#compdef _ocx" not in stdout, (
        "stdout must not contain '#compdef _ocx' (zsh completion preamble) when OCX_NO_COMPLETIONS=1; "
        f"got:\n{stdout}"
    )
    line_count = len(stdout.splitlines())
    assert line_count < 10, (
        f"stdout must be < 10 lines when OCX_NO_COMPLETIONS=1 (no completion body); "
        f"got {line_count} lines:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# TEST-C2 — strengthen PATH absolute-path assertion
# ---------------------------------------------------------------------------


def test_activate_sh_output_uses_exact_ocx_home_path(
    ocx: OcxRunner,
) -> None:
    """stdout must use the exact OCX_HOME value in the PATH export line.

    Strengthens test_activate_sh_output_prepends_path: instead of the broad
    'symlinks' substring check, assert:
    - The exact OCX_HOME value from the runner env appears in stdout.
    - 'PATH=' appears in the export line.
    - The full installable subpath suffix appears in stdout.

    Plan: ocx_install_bin_path returns
      $OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin
    """
    result = _run_activate(ocx, "--shell=sh")
    stdout = result.stdout

    ocx_home = ocx.env["OCX_HOME"]
    assert ocx_home in stdout, (
        f"stdout must contain the exact OCX_HOME value '{ocx_home}'; "
        f"got:\n{stdout}\nstderr:\n{result.stderr}"
    )
    assert "PATH=" in stdout, (
        "stdout must contain a 'PATH=' assignment; "
        f"got:\n{stdout}"
    )
    assert "/symlinks/ocx.sh/ocx/cli/current/content/bin" in stdout, (
        "stdout must contain the full installable subpath "
        "'/symlinks/ocx.sh/ocx/cli/current/content/bin'; "
        f"got:\n{stdout}"
    )


# ---------------------------------------------------------------------------
# OCX_ACTIVATED double-activation guard (SOTA-W3)
# ---------------------------------------------------------------------------


def test_activate_global_env_eval_guarded_on_ocx_activated_bash(
    ocx: OcxRunner,
) -> None:
    """The bash global-env-eval line must be guarded on $OCX_ACTIVATED.

    Mirrors mise's MISE_SHELL pattern: re-sourcing the user's shell profile
    must not re-run the expensive `ocx --global env` subprocess. Removing the
    guard turns every `source ~/.bashrc` into a redundant subprocess fan-out.
    """
    result = _run_activate(ocx, "--shell=bash")
    stdout = result.stdout
    assert result.returncode == 0, f"rc={result.returncode}\nstderr:\n{result.stderr}"
    assert '[ -z "${OCX_ACTIVATED:-}" ]' in stdout, (
        "bash activation output must guard global-env-eval on OCX_ACTIVATED; "
        f"got:\n{stdout}"
    )
    assert "ocx --global env --shell=bash" in stdout, (
        "bash activation output must invoke ocx --global env --shell=bash; "
        f"got:\n{stdout}"
    )


def test_activate_emits_ocx_activated_marker_bash(
    ocx: OcxRunner,
) -> None:
    """The marker `export OCX_ACTIVATED=1` must appear in the bash output.

    The first activation runs the eval and sets the marker; a re-source then
    sees OCX_ACTIVATED set and skips the eval. Asserting the marker is emitted
    is the canary that the re-source no-op contract holds.
    """
    result = _run_activate(ocx, "--shell=bash")
    stdout = result.stdout
    assert "export OCX_ACTIVATED=1" in stdout, (
        "bash activation output must emit `export OCX_ACTIVATED=1` marker; "
        f"got:\n{stdout}"
    )


def test_activate_resource_is_noop_for_global_env_eval_bash(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """Sourcing the bash activation output twice must run the eval line exactly
    once, even though the activation output itself runs each source.

    Executes the activation output in a subshell with `OCX_ACTIVATED` unset on
    the first pass and set on the second; asserts the guard short-circuits the
    second pass. Counts grep-matches against a stand-in `ocx` command so the
    test does not need a live registry.
    """
    activation = _run_activate(ocx, "--shell=bash")
    assert activation.returncode == 0, (
        f"activation must succeed; rc={activation.returncode}\nstderr:\n{activation.stderr}"
    )

    script = tmp_path / "activate.sh"
    script.write_text(activation.stdout)
    counter = tmp_path / "global-env-eval-count"

    # Stand-in `ocx` shim: each invocation increments a counter and prints a
    # no-op env line. The bash guard line invokes `ocx --global env` only when
    # OCX_ACTIVATED is empty.
    shim_dir = tmp_path / "bin"
    shim_dir.mkdir()
    shim = shim_dir / "ocx"
    shim.write_text(
        f"""#!/bin/sh
echo invoked >> "{counter}"
echo '# noop'
"""
    )
    shim.chmod(0o755)

    # Run twice: first source increments counter once, second source must not.
    sh_script = (
        f'export PATH="{shim_dir}:$PATH"\n'
        f'. "{script}"\n'
        f'. "{script}"\n'
    )
    result = subprocess.run(
        ["/bin/bash", "-c", sh_script],
        capture_output=True,
        text=True,
        env={"OCX_HOME": ocx.env["OCX_HOME"], "PATH": "/usr/bin:/bin"},
    )
    assert result.returncode == 0, (
        f"sourcing twice must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    count = counter.read_text().count("invoked") if counter.exists() else 0
    assert count == 1, (
        f"global-env-eval shim must run exactly once across two sources; got {count} runs"
    )
