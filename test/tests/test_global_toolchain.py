"""Acceptance tests for the explicit `--global` toolchain tier.

Encodes adr_global_toolchain_tier.md + handshake_toolchain_cli.md (signed
2026-05-16, handshake §5 / plan C7):

- The global tier is reachable only via an explicit ``--global`` flag (no
  implicit ``$OCX_HOME/ocx.toml`` discovery).
- ``run``/``exec`` are hermetic and never consult the global file without
  ``--global``.
- ``ocx --global run`` composes the global toolchain env for the child process
  only — it never mutates the parent shell.
- Isolation is by PATH precedence only (no PATH strip).

Phase 5 rewrites (plan_toolchain_cli.md Phase 5 / handshake §2):
- Test 1: ``install --global`` (deleted) replaced with ``ocx --global add``;
  ``shell init`` (deleted) replaced with ``ocx --global env --shell=sh`` activation.
- Tests 3, 5: ``install --global`` replaced with ``ocx --global add``.
- Test 4: already uses ``ocx --global add`` (previously rewritten for the new
  PATH-precedence model — no PATH strip).

Test 4 (``test_project_strict_isolation_global_bin_absent``) previously asserted
the OLD PATH-strip model (CODEX-WARN-5 / adr_global_toolchain_tier.md Decision 6,
now SUPERSEDED by handshake §5 / C7). It has been REWRITTEN to the new contract:
isolation is by PATH precedence via ``ocx run``, not by subshell PATH strip.
"""

from __future__ import annotations

import re as _re_gt
import subprocess
from pathlib import Path

from src import OcxRunner
from src.helpers import make_package
from src.shell_eval import run_after_sourcing

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # --global + --project conflict
EXIT_DATA = 65  # StaleLockOnPartial → DataError (whole-file drift on a mutator)


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
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        _ocx_cmd(ocx, *args),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _source_env_sh_script(
    ocx: OcxRunner,
    cwd: Path,
    env_sh_content: str,
    body: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``body`` in a NON-INTERACTIVE ``bash --norc`` shell that sources
    ``ocx --global env --shell=sh`` output first (new activation model).

    Uses ``run_after_sourcing`` which writes the export lines to a temp file
    and uses the POSIX dot-operator (``.``) instead of ``eval "..."`` to avoid
    quoting fragility (Block A1 fix: paths with spaces / $ / " / ! all handled
    correctly — the eval form breaks on all of those).

    Caller obtains ``env_sh_content`` via ``ocx --global env --shell=sh``.
    """
    return run_after_sourcing(
        env_sh_content,
        body,
        cwd=cwd,
        env=dict(ocx.env),
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


# ---------------------------------------------------------------------------
# 1. Global add+install+select → a fresh NON-INTERACTIVE shell sees the tool
# ---------------------------------------------------------------------------


def test_global_add_install_select_then_fresh_shell_sees_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx --global add <pkg>` records into + selects the global tier;
    a non-interactive shell that evals ``ocx --global env --shell=sh`` output
    then resolves the tool's binary on PATH (handshake §4 activation model).

    Rewritten Phase 5: replaces the deleted ``install --global`` + ``shell init``
    + static ``$OCX_HOME/init.bash`` activation. The new model uses
    ``ocx --global add`` and ``ocx --global env --shell=sh``.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # `add --global` installs AND auto-sets the `current` selection in the
    # global tier (signed handshake §1: "global IS the project toolchain — the
    # only difference is the load site"). No manual `ocx package select` —
    # `resolve_global_current_env` reads exactly the `current` symlink that
    # `add --global` just created.

    # Get the activation export lines via `ocx --global env --shell=sh`.
    env_result = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx env --global --shell=sh must succeed; stderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        f"env --global --shell=sh must emit export lines; got:\n{env_result.stdout}"
    )

    # Eval the output in a non-interactive shell → gtool must be on PATH.
    result = _source_env_sh_script(
        ocx, tmp_path, env_result.stdout, "command -v gtool && gtool"
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"sourced non-interactive shell must resolve the global tool via "
        f"eval of env --global --shell=sh output; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 2. `--global` mutator auto-creates the global file when absent (F7)
# ---------------------------------------------------------------------------


def test_global_add_when_global_file_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F7 decision: `ocx --global add` auto-creates ``$OCX_HOME/ocx.toml`` (and
    its ``ocx.lock``) when absent, mirroring project ``add`` on a fresh
    project — no pre-existing global file required."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    assert not global_toml.exists(), "precondition: no global ocx.toml yet"

    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global on absent global file must auto-init; "
        f"rc={add.returncode}\nstderr:\n{add.stderr}"
    )
    assert global_toml.exists(), "add --global must auto-create $OCX_HOME/ocx.toml"
    assert unique_repo in global_toml.read_text(), (
        "the new binding must be written into the auto-created global ocx.toml"
    )


# ---------------------------------------------------------------------------
# 3. A project tool shadows the global one inside the project
# ---------------------------------------------------------------------------


def test_project_tool_shadows_global_in_project(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Inside a project, the project toolchain is authoritative: the project's
    binding for a name supersedes the global tier's binding for the same name
    (project supersedes — strict isolation, no merge)."""
    g_repo = f"{unique_repo}_g"
    p_repo = f"{unique_repo}_p"
    make_package(ocx, g_repo, "1.0.0", tmp_path, new=True, bins=["tool"])
    make_package(ocx, p_repo, "1.0.0", tmp_path, new=True, bins=["tool"])

    # Global tier carries `tool` → g_repo.
    _run_cmd(ocx, tmp_path, "--global", "add", f"{ocx.registry}/{g_repo}:1.0.0")

    # Project declares `tool` → p_repo.
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\ntool = "{ocx.registry}/{p_repo}:1.0.0"\n',
    )
    assert _run_cmd(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run_cmd(ocx, project, "pull").returncode == EXIT_SUCCESS

    # `ocx run` in the project resolves the PROJECT's `tool`, not the global.
    result = _run_cmd(ocx, project, "run", "--", "tool")
    assert result.returncode == EXIT_SUCCESS, (
        f"project run must resolve the project tool; stderr:\n{result.stderr}"
    )
    # The project package's marker (echoed by its `tool` script) must appear,
    # proving the project binding shadowed the global one.
    assert "marker-" in result.stdout, (
        f"project tool output expected; got stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 4. Strict isolation (PATH-precedence model — REWRITTEN for handshake §5 / C7)
# ---------------------------------------------------------------------------


def test_project_strict_isolation_global_bin_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Isolation is by PATH precedence, not by subshell PATH strip (C7).

    REWRITTEN from the OLD model (CODEX-WARN-5 / adr_global_toolchain_tier.md
    Decision 6, SUPERSEDED by handshake §5): the previous test asserted that the
    global bin dir was *removed* from PATH via a ``filter_path_excluding``
    subshell — the strip mechanism deleted in Phase 1 (C4).

    NEW CONTRACT (handshake §5 / C7): ``ocx run`` inside a project composes
    the project toolchain env for the child process only. A tool present ONLY in
    the global toolchain is inaccessible via bare ``ocx run`` (no implicit merge,
    binding not found → exit 64). The parent shell's PATH is never mutated.
    Isolation is by exclusive project-tier scope, not by PATH strip.

    The companion tests in ``test_run_global_isolation.py`` cover the full C7
    matrix (bare run → 64, run --global → resolves, no strip output, parent
    env unmutated). This test focuses on the in-project binding isolation.
    """
    g_repo = unique_repo
    p_repo = f"{unique_repo}_proj"

    make_package(ocx, g_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    # Use the live `ocx --global add` (not the deleted `install --global`).
    add = _run_cmd(ocx, tmp_path, "--global", "add", f"{ocx.registry}/{g_repo}:1.0.0")
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # A project that does NOT declare `gonly`.
    make_package(ocx, p_repo, "1.0.0", tmp_path, new=True, bins=["ptool"])
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\nptool = "{ocx.registry}/{p_repo}:1.0.0"\n',
    )
    assert _run_cmd(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run_cmd(ocx, project, "pull").returncode == EXIT_SUCCESS

    # Bare `ocx run gonly` inside the project must fail (binding not in project).
    # This verifies project-tier exclusivity — the global tier is never consulted
    # by bare `run`, so `gonly` is genuinely out of scope.
    result = _run_cmd(ocx, project, "run", "gonly", "--", "gonly")
    assert result.returncode != EXIT_SUCCESS, (
        f"bare `ocx run gonly` inside a project must NOT resolve a global-only "
        f"tool (strict project-tier isolation — no PATH strip needed, scope is "
        f"exclusive); rc={result.returncode}\nstdout:\n{result.stdout}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"binding-not-found must exit {EXIT_USAGE} (UsageError); "
        f"got rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # Project's own tool is still reachable via bare `run` (regression guard).
    project_result = _run_cmd(ocx, project, "run", "--", "ptool")
    assert project_result.returncode == EXIT_SUCCESS, (
        f"project-tier tool must still be reachable via bare `ocx run`; "
        f"rc={project_result.returncode}\nstderr:\n{project_result.stderr}"
    )


# ---------------------------------------------------------------------------
# 5. `ocx run` is hermetic — it never consults the global file
# ---------------------------------------------------------------------------


def test_run_is_hermetic_ignores_global(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """adr_global_toolchain_tier.md §Decision 4: a project ``ocx run`` cannot
    resolve a tool that exists only in ``$OCX_HOME/ocx.toml`` — `run` reads
    only the in-effect project file, never the global one."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    _run_cmd(
        ocx, tmp_path, "--global", "add", f"{ocx.registry}/{unique_repo}:1.0.0"
    )

    # A project that declares a *different* tool, never `gonly`.
    other_repo = f"{unique_repo}_other"
    make_package(ocx, other_repo, "1.0.0", tmp_path, new=True, bins=["ptool"])
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\nptool = "{ocx.registry}/{other_repo}:1.0.0"\n',
    )
    assert _run_cmd(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run_cmd(ocx, project, "pull").returncode == EXIT_SUCCESS

    # `gonly` is only in the global file → project `run gonly` must fail with
    # a binding-not-found error (not silently fall back to the global tier).
    result = _run_cmd(ocx, project, "run", "gonly", "--", "gonly")
    assert result.returncode != EXIT_SUCCESS, (
        f"project run must NOT resolve a global-only tool (hermetic / strict "
        f"isolation); rc={result.returncode}\nstdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 6. No implicit `$OCX_HOME/ocx.toml` discovery without `--global`
# ---------------------------------------------------------------------------


def test_home_ocx_toml_not_discovered_without_global(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """adr_global_toolchain_tier.md §Decision 1: a present
    ``$OCX_HOME/ocx.toml`` is NOT discovered when no project is in scope and
    no ``--global`` is given — the implicit home fallback was removed."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    _run_cmd(
        ocx, tmp_path, "--global", "add", f"{ocx.registry}/{unique_repo}:1.0.0"
    )

    # Empty dir, no project ocx.toml anywhere up the tree, NO --global.
    empty = tmp_path / "no_project"
    empty.mkdir()
    result = _run_cmd(
        ocx,
        empty,
        "run",
        "gonly",
        "--",
        "gonly",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert result.returncode != EXIT_SUCCESS, (
        f"`ocx run` with no project and no --global must NOT fall back to "
        f"$OCX_HOME/ocx.toml; rc={result.returncode}\nstdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 7. `--global` + `--project` is a hard usage error (exit 64)
# ---------------------------------------------------------------------------


def test_global_and_project_flags_conflict(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """adr_global_toolchain_tier.md §Decision 2: `--global` and `--project`
    both pick a project file → mutually exclusive → ``UsageError`` (64)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    explicit = tmp_path / "explicit.toml"
    explicit.write_text("[tools]\n")

    result = _run_cmd(ocx, tmp_path, "--project", str(explicit), "--global", "add", fq)
    assert result.returncode == EXIT_USAGE, (
        f"--global + --project must exit {EXIT_USAGE} (UsageError); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 7b. B1: the env-sourced global selector (OCX_GLOBAL) + an explicit project
#     selection is ALSO a hard usage error (exit 64) — no env bypass
# ---------------------------------------------------------------------------


def test_env_global_with_explicit_project_flag_conflict(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Plan §"Living Design — Review-Fix Amendments" B1: the
    ``--global``/``--project`` conflict fires on the **effective** selector,
    not just a per-command ``--global`` flag.

    ``OCX_GLOBAL=1 ocx <cmd> --project <file>`` (no per-command ``--global``
    flag at all) selects the global tier via the env var while an explicit
    ``--project`` is also given. Pre-B1 the env-sourced global selector
    evaded the ``with_command_global`` seam (it only checked the per-command
    flag), so the explicit project was silently ignored and the command did
    NOT exit 64 (the review's exit-79-not-64 anomaly). B1 makes the conflict
    check fire on ``(config_view.global || command_global) &&
    has_explicit_project_selection()``.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    explicit = tmp_path / "explicit.toml"
    explicit.write_text("[tools]\n")

    result = _run_cmd(
        ocx,
        tmp_path,
        "--project",
        str(explicit),
        "add",
        fq,
        extra_env={"OCX_GLOBAL": "1"},
    )
    assert result.returncode == EXIT_USAGE, (
        f"OCX_GLOBAL=1 + explicit --project must exit {EXIT_USAGE} "
        f"(UsageError) — the env-sourced global selector must not bypass the "
        f"conflict seam (B1); rc={result.returncode}\nstderr:\n{result.stderr}"
    )


def test_env_global_with_env_project_conflict(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Plan B1, env-only variant: ``OCX_PROJECT=<file> OCX_GLOBAL=1 ocx
    <cmd>`` — both selectors sourced purely from the environment, no CLI
    flags at all — must still exit 64. ``has_explicit_project_selection()``
    counts a non-empty ``OCX_PROJECT`` as an explicit selection, so the
    effective-selector conflict check (B1) must reject this exactly as it
    rejects the flag form. Pre-B1 this slips through entirely (neither the
    flag-only seam nor clap's top-level ``conflicts_with`` sees an env pair).
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    explicit = tmp_path / "explicit.toml"
    explicit.write_text("[tools]\n")

    result = _run_cmd(
        ocx,
        tmp_path,
        "add",
        fq,
        extra_env={"OCX_GLOBAL": "1", "OCX_PROJECT": str(explicit)},
    )
    assert result.returncode == EXIT_USAGE, (
        f"OCX_GLOBAL=1 + OCX_PROJECT=<file> must exit {EXIT_USAGE} "
        f"(UsageError) on the effective selector (B1); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 8. The global file gets NO `$OCX_HOME/projects/` self-link (W1 cross-check)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# 9. NEW CONTRACT: global env follows lock pin, NOT current symlink
# ---------------------------------------------------------------------------


def test_global_env_follows_lock_pin_not_current_symlink(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx --global env`` output follows the lock-pinned digest, not the
    ``current`` symlink.

    NEW CONTRACT (adr_global_toolchain_tier.md D5 amended 2026-05-19):
    ``resolve_global_pinned_env`` reads ``$OCX_HOME/ocx.lock`` and resolves
    each tool offline by its pinned digest.  The ``current`` symlink is a
    SEPARATE install/uninstall/select-only abstraction; it is NOT consulted
    by ``ocx --global env``.

    Consequence: deleting (or repointing) the ``current`` symlink for a
    globally-installed tool must NOT change ``ocx --global env`` output.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Capture the baseline `ocx --global env --shell=sh` output.
    env_before = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_before.returncode == EXIT_SUCCESS, (
        f"ocx --global env --shell=sh must succeed after add; "
        f"rc={env_before.returncode}\nstderr:\n{env_before.stderr}"
    )
    assert "export" in env_before.stdout, (
        f"env --global --shell=sh must emit export lines before symlink removal; "
        f"got:\n{env_before.stdout!r}"
    )
    path_before = env_before.stdout

    # Delete the `current` symlink for this tool so the install-symlink
    # layer is gone — simulating a manual removal or a deselect step.
    ocx_home = Path(ocx.env["OCX_HOME"])
    from src.runner import registry_dir
    reg_slug = registry_dir(ocx.registry)
    current_link = ocx_home / "symlinks" / reg_slug / unique_repo / "current"
    if current_link.exists() or current_link.is_symlink():
        current_link.unlink()

    # `ocx --global env` must produce the SAME output as before — the lock pin
    # is the authoritative source, not the `current` symlink.
    env_after = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_after.returncode == EXIT_SUCCESS, (
        f"ocx --global env --shell=sh must still succeed after current-symlink "
        f"removal; rc={env_after.returncode}\nstderr:\n{env_after.stderr}"
    )
    assert env_after.stdout == path_before, (
        "ocx --global env output must be identical before and after removing the "
        "`current` symlink (lock pin is authoritative, not the current symlink);\n"
        f"  before: {path_before!r}\n"
        f"  after:  {env_after.stdout!r}"
    )


def test_global_upgrade_takes_effect_without_select(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx --global upgrade`` re-pins the lock and ``ocx --global env``
    reflects the new pin immediately — no ``select`` step required.

    NEW CONTRACT (adr_global_toolchain_tier.md D5 amended 2026-05-19):
    because ``resolve_global_pinned_env`` reads the lock directly, any
    change to ``$OCX_HOME/ocx.lock`` (e.g. via ``--global upgrade`` or
    ``--global add`` of a new tag) is reflected in the next
    ``ocx --global env`` invocation without an intervening
    ``ocx package select``.

    Flow:
    1. Push v1 under a rolling tag (``latest``, created by cascade).
       ``add --global`` binds to ``latest`` → lock records v1 digest.
    2. Push v2 with ``new=False`` (cascade overwrites ``latest`` with v2 digest).
    3. ``upgrade --global`` re-resolves ``latest`` → new digest → lock re-pinned.
    4. ``pull --global`` materialises the new content into the local blob store.
       (``upgrade`` re-pins the lock but does not download blobs; ``pull`` does.)
    5. ``ocx --global env --shell=sh`` must emit the v2 content/bin path with NO
       ``ocx package select`` step — the proof that env reads the lock pin directly,
       not the ``current`` symlink.
    6. The v2 binary must be reachable (runs and prints its v2 marker).

    Using a rolling tag (``latest``) is essential: a pinned version tag like
    ``1.0.0`` always resolves to the same digest, so ``upgrade`` would find
    nothing to advance.  ``latest`` spans both pushes; after step 2 it points
    at v2, giving ``upgrade`` a real pin change to record.
    """
    bin_name = "gtool"

    # Step 1: push v1 with cascade — creates ``latest`` pointing at v1.
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=[bin_name])
    # Bind the global tier to ``latest`` (rolling tag), not the pinned ``1.0.0``.
    fq_latest = f"{ocx.registry}/{unique_repo}:latest"
    add_v1 = _run_cmd(ocx, tmp_path, "--global", "add", fq_latest)
    assert add_v1.returncode == EXIT_SUCCESS, (
        f"add --global latest must succeed; rc={add_v1.returncode}\nstderr:\n{add_v1.stderr}"
    )

    # Step 2: push v2 with cascade + new=False — overwrites ``latest`` with v2 digest.
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, bins=[bin_name])
    assert v1.marker != v2.marker, "precondition: markers differ between versions"

    # Step 3: upgrade the global toolchain.
    # ``ocx --global upgrade`` re-resolves all tools in the global ocx.toml and
    # rewrites ocx.lock with the new digests.  ``latest`` now resolves to the v2
    # digest → lock is updated.  Note: upgrade re-pins the lock only; it does NOT
    # download the new manifest blobs — that is pull's responsibility (step 4).
    upgrade = _run_cmd(ocx, tmp_path, "--global", "upgrade")
    assert upgrade.returncode == EXIT_SUCCESS, (
        f"ocx --global upgrade must succeed; rc={upgrade.returncode}\n"
        f"stderr:\n{upgrade.stderr}"
    )

    # Step 4: pull the new content into the local blob store so that the offline
    # env resolver can find the v2 manifests.  No ``ocx package select`` call is
    # made here or below — that is the contract under test (env reads the lock pin
    # directly, not the ``current`` symlink).
    pull = _run_cmd(ocx, tmp_path, "--global", "pull")
    assert pull.returncode == EXIT_SUCCESS, (
        f"ocx --global pull must succeed after upgrade; rc={pull.returncode}\n"
        f"stderr:\n{pull.stderr}"
    )

    # Step 5: env output must reference the v2 content path.
    env_result = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx --global env --shell=sh must succeed after upgrade; "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        f"env must emit export lines after upgrade; got:\n{env_result.stdout!r}"
    )

    # Step 6: source the export lines and run the tool; it must print the v2 marker.
    shell_result = _source_env_sh_script(
        ocx, tmp_path, env_result.stdout, f"command -v {bin_name} && {bin_name}"
    )
    assert shell_result.returncode == EXIT_SUCCESS, (
        f"global tool must be reachable after --global upgrade with no select step; "
        f"rc={shell_result.returncode}\n"
        f"stdout:\n{shell_result.stdout}\nstderr:\n{shell_result.stderr}"
    )
    assert v2.marker in shell_result.stdout, (
        f"v2 marker must appear after upgrade (proving lock was re-pinned to v2 digest); "
        f"v2.marker={v2.marker!r}\nstdout:\n{shell_result.stdout!r}"
    )


def test_clean_keeps_global_lock_pinned_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx clean`` must NOT collect a package pinned by ``$OCX_HOME/ocx.lock``.

    NEW CONTRACT (adr_global_toolchain_tier.md D5 amended 2026-05-19 +
    clean.rs::collect_project_roots): the global lock is an implicit GC root.
    ``collect_project_roots`` appends ``$OCX_HOME/ocx.lock`` to the root set
    unconditionally; the global lock-pinned package must survive ``ocx clean``
    even when no project-registry ledger entry points at it.

    Flow:
    1. ``ocx --global add`` installs the package and writes the global lock.
    2. ``ocx package deselect`` removes the ``current`` symlink so no install
       symlink protects the package from GC.
    3. ``ocx clean --dry-run --format json`` must report the package as
       HELD (in ``held_by``) and NOT as unreferenced.
    4. ``ocx clean`` (real) must not delete the package directory.
    """
    import json as _json

    bin_name = "gtool"
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=[bin_name])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    # Step 1: add to global toolchain — installs + writes lock + selects current.
    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Locate the installed package's content directory via install JSON output.
    install_info = _run_cmd(ocx, tmp_path, "--format", "json", "package", "install",
                            "--select", fq)
    assert install_info.returncode == EXIT_SUCCESS, (
        f"package install --select must succeed to locate content dir; "
        f"rc={install_info.returncode}\nstderr:\n{install_info.stderr}"
    )
    install_data = _json.loads(install_info.stdout)
    # The JSON is keyed by the package short name; value has a "path" field.
    short_key = next(iter(install_data))
    content_path = Path(install_data[short_key]["path"]).resolve()
    assert content_path.is_dir(), (
        f"package content directory must exist after install; path={content_path}"
    )

    # Step 2: deselect (remove the `current` symlink) so install symlinks no
    # longer protect the package.  The global lock remains.
    deselect = _run_cmd(ocx, tmp_path, "package", "deselect", fq)
    # deselect may exit 0 (removed) or 79 (already absent) — both acceptable.
    assert deselect.returncode in (EXIT_SUCCESS, 79), (
        f"package deselect must exit 0 or 79; "
        f"rc={deselect.returncode}\nstderr:\n{deselect.stderr}"
    )

    # Step 3: dry-run clean from a directory without its own project (no
    # project-level lock protects the package; only the global lock does).
    no_project = tmp_path / "no_project"
    no_project.mkdir(exist_ok=True)
    dry_run_result = _run_cmd(
        ocx, no_project,
        "--format", "json", "clean", "--dry-run",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert dry_run_result.returncode == EXIT_SUCCESS, (
        f"ocx clean --dry-run must succeed; "
        f"rc={dry_run_result.returncode}\nstderr:\n{dry_run_result.stderr}"
    )

    # The global lock-pinned package must appear in held_by (not unreferenced).
    entries = _json.loads(dry_run_result.stdout)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    # Unreferenced entries (would-be-collected) have an empty held_by.
    unreferenced_paths = {e["path"] for e in object_entries if not e.get("held_by")}
    assert str(content_path) not in unreferenced_paths, (
        f"global lock-pinned package must NOT appear as unreferenced in dry-run "
        f"output; content_path={content_path}\n"
        f"unreferenced={unreferenced_paths}"
    )

    # Step 4: real clean must not delete the package.
    real_clean = _run_cmd(
        ocx, no_project, "clean",
        extra_env={"OCX_NO_PROJECT": "1"},
    )
    assert real_clean.returncode == EXIT_SUCCESS, (
        f"ocx clean (real) must succeed; "
        f"rc={real_clean.returncode}\nstderr:\n{real_clean.stderr}"
    )
    assert content_path.is_dir(), (
        f"global lock-pinned package must survive ocx clean; "
        f"content_path={content_path}"
    )


# ---------------------------------------------------------------------------
# 10. (original 8) No self-link for global file
# ---------------------------------------------------------------------------


def test_no_self_link_for_global_file(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """adr_global_toolchain_tier.md §Decision 5 + adr_project_gc_symlink_ledger.md:
    the global file's project dir is ``$OCX_HOME`` itself, so the no-self-link
    rule applies — no ``$OCX_HOME/projects/<hash>`` symlink may resolve to
    ``$OCX_HOME``. The global toolchain is GC-protected purely by its
    ``current`` install symlinks."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; stderr:\n{add.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    ocx_home_resolved = ocx_home.resolve()
    projects_dir = ocx_home / "projects"
    if projects_dir.exists():
        for link in projects_dir.iterdir():
            if link.name.startswith(".tmp-"):
                continue  # in-flight staging entry — not a ledger root
            try:
                target = link.resolve()
            except OSError:
                continue  # dangling link mid-prune — acceptable
            assert target != ocx_home_resolved, (
                f"the global file (project dir = $OCX_HOME) must get NO "
                f"projects/ self-link; found {link} -> {target}"
            )


# ---------------------------------------------------------------------------
# V2 global lock: ``ocx --global env`` resolves via per-platform leaf digest
# ---------------------------------------------------------------------------

_LEAF_RE_GT = _re_gt.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_global_env_resolves_v2_leaf_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``$OCX_HOME/ocx.lock`` is written in V2 shape after ``ocx --global add``;
    ``ocx --global env --shell=sh`` resolves the package via the per-platform
    leaf digest from ``[tool.platforms]`` — not a legacy ``pinned`` index digest.

    ADR §toolchain_env.rs: "V2: host-platform leaf; V1: legacy index-digest path."

    Assertions:
    1. The global lock is V2 (``lock_version = 2``, ``[tool.platforms]`` present,
       no ``pinned =`` line).
    2. ``ocx --global env --shell=sh`` exits 0 and emits ``export`` lines.
    3. The export lines reference a content path that exists on disk (proving
       the V2 leaf was resolved to an installed package).
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Verify the global lock is V2.
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_lock = ocx_home / "ocx.lock"
    assert global_lock.is_file(), "ocx --global add must write $OCX_HOME/ocx.lock"
    lock_text = global_lock.read_text()

    assert "lock_version = 2" in lock_text, (
        "global lock must be V2 after add --global; got:\n" + lock_text[:400]
    )
    assert "[tool.platforms]" in lock_text, (
        "global V2 lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_GT.findall(lock_text)
    assert leaf_digests, "global V2 lock must record at least one leaf digest"
    assert "pinned =" not in lock_text, (
        "global V2 lock must not carry a legacy `pinned` line"
    )

    # ``ocx --global env --shell=sh`` must resolve the V2 leaf and emit export lines.
    env_result = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_result.returncode == EXIT_SUCCESS, (
        f"ocx --global env --shell=sh must succeed after add (V2 leaf path); "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        f"V2 --global env --shell=sh must emit export lines; got:\n{env_result.stdout!r}"
    )


def test_global_env_v1_lock_still_works_offline(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A committed V1 global lock (``lock_version = 1``, ``pinned`` field) still
    lets ``ocx --global env`` work via the legacy index-digest path — no forced
    upgrade, no network if the index is cached.

    ADR: "Read both V1 and V2, write only V2. A committed V1 lock keeps
    installing/running offline with no forced upgrade and no read-path mutation."

    Flow:
    1. ``ocx --global add`` writes a V2 global lock and pulls the package.
    2. Overwrite the global lock with a hand-authored V1 form (preserving the
       real declaration_hash so stale-check passes; using the real pinned
       identifier from the V2 lock's bare-repo + one leaf).
    3. ``ocx --global env --shell=sh`` must still exit 0 and emit ``export``
       lines (the package blobs are cached from step 1).
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    add = _run_cmd(ocx, tmp_path, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"add --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    global_lock = ocx_home / "ocx.lock"
    v2_lock_text = global_lock.read_text()

    # Extract real coordinates from the V2 lock to build an honest V1 lock.
    repo_match = _re_gt.search(r'repository\s*=\s*"([^"]+)"', v2_lock_text)
    leaf_match = _LEAF_RE_GT.search(v2_lock_text)
    decl_hash_match = _re_gt.search(
        r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_lock_text
    )
    assert repo_match and leaf_match and decl_hash_match, (
        "V2 global lock must carry repository + leaf + declaration_hash;\n"
        + v2_lock_text[:400]
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)
    decl_hash = decl_hash_match.group(1)

    # Overwrite with a V1 lock (this is what a consumer has if they committed
    # a lock before V2 shipped).
    global_lock.write_text(
        f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "{unique_repo}"
group = "default"
pinned = "{bare_repo}@sha256:{leaf_hex}"
"""
    )

    # ``ocx --global env --shell=sh`` must succeed via the legacy path (blobs cached).
    env_result = _run_cmd(ocx, tmp_path, "--global", "env", "--shell=sh")
    assert env_result.returncode == EXIT_SUCCESS, (
        "V1 global lock must still allow ocx --global env --shell=sh to succeed "
        "(legacy index-digest path, blobs cached); "
        f"rc={env_result.returncode}\nstderr:\n{env_result.stderr}"
    )
    assert "export" in env_result.stdout, (
        "V1 global lock: --global env --shell=sh must emit export lines when "
        f"blobs are cached; got:\n{env_result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Tier-neutral fail-closed message (design spec §4.1)
# ---------------------------------------------------------------------------


def test_global_partial_mutator_fail_closed_message_not_project_only(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A `ocx --global add` that drifts the global lock fails closed (exit 65)
    and the stderr message is **tier-aware**: it must not hand a `--global`
    user a bare project-only ``ocx lock`` that would reconcile the wrong
    toolchain. Per design spec §4.1, the error layer has no tier context, so
    the remedy names ``--global`` for the global toolchain.

    Setup:
    1. ``ocx --global add A`` — creates ``$OCX_HOME/ocx.{toml,lock}`` current
       with A.
    2. Hand-edit ``$OCX_HOME/ocx.toml`` to add an unrelated binding line —
       the declaration_hash now drifts from the global lock's recorded hash.
    3. ``ocx --global add B`` — the whole-file freshness gate refuses to carry
       A forward against a stale lock → exit 65, tier-neutral remedy.
    """
    repo_a = f"{unique_repo}_a"
    repo_b = f"{unique_repo}_b"
    repo_extra = f"{unique_repo}_extra"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, bins=["atool"])
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, bins=["btool"])
    make_package(ocx, repo_extra, "1.0.0", tmp_path, new=True, bins=["xtool"])

    # Step 1: bind A into the global tier (auto-creates ocx.toml + ocx.lock).
    add_a = _run_cmd(
        ocx, tmp_path, "--global", "add", "--no-pull", f"{ocx.registry}/{repo_a}:1.0.0"
    )
    assert add_a.returncode == EXIT_SUCCESS, (
        f"baseline --global add A must succeed; rc={add_a.returncode}\n"
        f"stderr:\n{add_a.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    assert (ocx_home / "ocx.lock").exists(), "baseline --global add must write ocx.lock"

    # Step 2: hand-edit the global ocx.toml — declare an unrelated binding
    # INSIDE the [tools] table so the declaration_hash drifts from the lock's
    # recorded hash WITHOUT re-locking. This is the "edited since lock"
    # condition. Insert immediately after the `[tools]` header so the new key
    # belongs to that table (a bare top-level key would be invalid TOML).
    original = global_toml.read_text()
    assert "[tools]" in original, (
        f"auto-created global ocx.toml must carry a [tools] table; got:\n{original}"
    )
    drifted = original.replace(
        "[tools]\n",
        f'[tools]\nextra = "{ocx.registry}/{repo_extra}:1.0.0"\n',
        1,
    )
    assert drifted != original, "drift edit must change the global ocx.toml"
    global_toml.write_text(drifted)

    # Step 3: a second --global add must fail closed against the stale lock.
    add_b = _run_cmd(
        ocx, tmp_path, "--global", "add", "--no-pull", f"{ocx.registry}/{repo_b}:1.0.0"
    )
    assert add_b.returncode == EXIT_DATA, (
        f"--global add on a drifted global ocx.toml must exit {EXIT_DATA}; "
        f"rc={add_b.returncode}\nstderr:\n{add_b.stderr}"
    )

    combined = (add_b.stderr + add_b.stdout).lower()
    # The remedy must be tier-aware: it names `--global` so a global user is not
    # handed a project-only reconcile command (spec §4.1).
    assert "--global" in combined, (
        "fail-closed remedy under --global must be tier-aware (name `--global` "
        "for the global toolchain), not a bare project-only `ocx lock`; "
        f"stderr:\n{add_b.stderr}\nstdout:\n{add_b.stdout}"
    )
    # It still names the reconcile verb so the user knows what to run.
    assert "ocx lock" in combined, (
        "fail-closed remedy must still name the `ocx lock` reconcile verb; "
        f"stderr:\n{add_b.stderr}\nstdout:\n{add_b.stdout}"
    )
