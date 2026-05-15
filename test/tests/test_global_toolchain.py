"""Acceptance tests for the explicit `--global` toolchain tier.

Encodes plan_project_toolchain_hardening.md W2-P3 + adr_global_toolchain_tier.md:
the global tier is reachable only via an explicit `--global` flag (no implicit
`$OCX_HOME/ocx.toml` discovery), strictly isolated from project resolution
(`run`/`exec` are hermetic and never consult the global file), and surfaced to
a sourced non-interactive shell via the static `$OCX_HOME/init.<shell>`
PATH-prepend entrypoint.

The `--global` implementation (`Context::with_command_global`,
`ProjectConfig::resolve` global branch, the static `init.<shell>` writer) is
fully wired; these tests assert real success against the compiled binary.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from src import OcxRunner
from src.helpers import make_package

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # --global + --project conflict


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


def _source_init_script(
    ocx: OcxRunner,
    cwd: Path,
    body: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``body`` in a NON-INTERACTIVE ``bash --norc -c`` shell that sources
    ``$OCX_HOME/init.bash`` first.

    Deliberately NOT an interactive prompt-hook session: the per-prompt hook
    never fires in ``bash --norc`` / ``bash -c`` / CI shells, so this is the
    surface that catches the CI-invisible-global regression (SOTA-2a). The
    static ``init.bash`` entrypoint must prepend the global ``current`` bin
    dir to PATH without invoking the hook.
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    init_file = ocx_home / "init.bash"
    # POSIX `.` (dot), not bash `source`, for dash/sh CI compatibility
    # (SOTA-2b) — bash accepts `.` too.
    script = f'. "{init_file}"\n{body}\n'
    env = dict(ocx.env)
    return subprocess.run(
        ["bash", "--norc", "-c", script],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
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
    """`ocx install --global <pkg>` records into + selects the global tier;
    a sourced non-interactive shell (CI shape) then resolves the tool's binary
    on PATH via the static ``$OCX_HOME/init.bash`` entrypoint (SOTA-2a)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"

    add = _run_cmd(ocx, tmp_path, "install", "--global", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"install --global must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    # Materialize the static entrypoint, then a non-interactive shell that
    # sources it must find `gtool` on PATH.
    init = _run_cmd(ocx, tmp_path, "shell", "init", "--shell", "bash")
    assert init.returncode == EXIT_SUCCESS, (
        f"shell init must succeed; stderr:\n{init.stderr}"
    )

    result = _source_init_script(ocx, tmp_path, "command -v gtool && gtool")
    assert result.returncode == EXIT_SUCCESS, (
        f"sourced non-interactive shell must resolve the global tool; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 2. `--global` mutator auto-creates the global file when absent (F7)
# ---------------------------------------------------------------------------


def test_global_add_when_global_file_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F7 decision: `ocx add --global` auto-creates ``$OCX_HOME/ocx.toml`` (and
    its ``ocx.lock``) when absent, mirroring project ``add`` on a fresh
    project — no pre-existing global file required."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gtool"])
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    assert not global_toml.exists(), "precondition: no global ocx.toml yet"

    add = _run_cmd(ocx, tmp_path, "add", "--global", fq)
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
    _run_cmd(ocx, tmp_path, "install", "--global", f"{ocx.registry}/{g_repo}:1.0.0")

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
# 4. Strict isolation: inside a project the global bin dir is absent from PATH
# ---------------------------------------------------------------------------


def test_project_strict_isolation_global_bin_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """CODEX-WARN-5: source ``init.bash``, enter a project, and assert a tool
    present ONLY in the global tier is NOT on PATH AND the global ``current``
    bin dir itself is absent from PATH — proving isolation (the global bin dir
    is *removed*, not merely shadowed), not mere precedence."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, bins=["gonly"])
    _run_cmd(
        ocx, tmp_path, "install", "--global", f"{ocx.registry}/{unique_repo}:1.0.0"
    )

    # A project that does NOT declare `gonly`.
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

    _run_cmd(ocx, tmp_path, "shell", "init", "--shell", "bash")

    # Inside the project (the prompt hook would rebuild PATH from the saved
    # baseline and explicitly strip the global bin dir), a global-only tool
    # must not resolve, and `$PATH` must not contain the global current bin
    # dir at all.
    probe = 'command -v gonly && echo FOUND || echo ABSENT; echo "PATH=$PATH"'
    result = _source_init_script(ocx, project, probe)
    assert "ABSENT" in result.stdout, (
        f"global-only tool must NOT be on PATH inside a project (strict "
        f"isolation); stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    # The global current bin dir itself must be gone from PATH (replace, not
    # shadow). The dir lives under $OCX_HOME/symlinks/.../current/bin.
    ocx_home = str(Path(ocx.env["OCX_HOME"]) / "symlinks")
    path_line = next(
        (ln for ln in result.stdout.splitlines() if ln.startswith("PATH=")), ""
    )
    assert ocx_home not in path_line, (
        f"the global install bin dir must be removed from PATH inside a "
        f"project, not merely shadowed; PATH line:\n{path_line}"
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
        ocx, tmp_path, "install", "--global", f"{ocx.registry}/{unique_repo}:1.0.0"
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
        ocx, tmp_path, "install", "--global", f"{ocx.registry}/{unique_repo}:1.0.0"
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

    result = _run_cmd(ocx, tmp_path, "--project", str(explicit), "add", "--global", fq)
    assert result.returncode == EXIT_USAGE, (
        f"--global + --project must exit {EXIT_USAGE} (UsageError); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 8. The global file gets NO `$OCX_HOME/projects/` self-link (W1 cross-check)
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
    add = _run_cmd(ocx, tmp_path, "add", "--global", fq)
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
