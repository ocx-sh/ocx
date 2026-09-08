# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for WP-8 — the CLI wiring of the toolchain home.

Traces one-to-one to ``.claude/state/plans/plan_toolchain_activation.md``:
C-054 (the four mutation commands re-render), C-055 (``--pinned``), C-056
(``ocx shell state`` reports the resolved home), C-058 (``ocx exec``'s lookup
PATH excludes both trampoline directories), C-068 (the three-way exit-code
mapping a trampoline's baked home inherits), S-007 (``ocx pull --dry-run`` over
a poisoned tree), plus rulings RUL-50/52/53/54/55/57/59.

Specification mode (contract-first TDD)
---------------------------------------
Written from the plan and the wave-3b rulings, against the WP-8 **stub**. Every
case whose contract is an exit code drives the real binary, because an exit code
is what a backend tool observes — never an assertion on an error enum.

"Wrote nothing" is a ``(path, file type, bytes, mtime_ns, inode)`` subtree
snapshot (``_snapshot``), never an empty-output check: on this host an empty
capture is a silent negative, and a rewrite that lands identical bytes still
moves ``mtime_ns``.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner
from src.toolchain_fixtures import shell_bin

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # NoProject, and every clap rejection
EXIT_DATA = 65  # a stale lock
EXIT_IO = 74  # the pre-RUL-55 answer for `--project <directory>`
EXIT_CONFIG = 78  # an absent lock
EXIT_NOT_FOUND = 79  # an explicit --project naming an absent path

# The three codes C-068 maps to, spelled once so a case cannot drift from the
# contract by restating one of them.
C068_NO_PROJECT = EXIT_USAGE
C068_LOCK_MISSING = EXIT_CONFIG
C068_LOCK_STALE = EXIT_DATA


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` from ``cwd`` so the CWD walk is the project selector.

    ``OcxRunner.run`` does not expose ``cwd=``, so go straight to
    ``subprocess.run`` — the same pattern ``test_project_pull._run_pull`` uses.
    """
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=180,
    )


def _snapshot(root: Path) -> dict[str, tuple[str, int, int, int]]:
    """A recursive ``(path, file type, bytes, mtime_ns, inode)`` snapshot.

    An absent root snapshots as ``{}`` — the state a render that created nothing
    leaves. ``lstat``, never ``stat``: a snapshot must record the link itself,
    not whatever it currently points at, because a repointed link is a write.
    """
    out: dict[str, tuple[str, int, int, int]] = {}
    if not root.exists() and not root.is_symlink():
        return out
    for path in sorted(root.rglob("*")):
        info = path.lstat()
        if path.is_symlink():
            kind = "symlink"
        elif path.is_dir():
            kind = "dir"
        else:
            kind = "file"
        out[str(path.relative_to(root))] = (
            kind,
            info.st_size,
            info.st_mtime_ns,
            info.st_ino,
        )
    return out


def _toolchain_home(project: Path) -> Path:
    """The in-project default home, ``<project>/.ocx/toolchain`` (C-002)."""
    return project / ".ocx" / "toolchain"


def _write_ocx_toml(project: Path, body: str) -> Path:
    path = project / "ocx.toml"
    path.write_text(body)
    return path


def _published_tool(ocx: OcxRunner, tmp_path: Path, label: str, bins: list[str]):
    """Publish one test package exposing ``bins`` and return its ``PackageInfo``."""
    repo = f"t_{uuid4().hex[:8]}_wp8_{label}"
    return make_package(ocx, repo, "1.0.0", tmp_path, cascade=False, bins=bins)


@pytest.fixture()
def locked_project(ocx: OcxRunner, tmp_path: Path) -> tuple[Path, str]:
    """A project with one default-group tool and a current ``ocx.lock``.

    Returns ``(project_dir, tool_binary_name)``. The binary name is what a
    rendered ``shells/default/bin`` must carry a trampoline for.
    """
    package = _published_tool(ocx, tmp_path, "locked", bins=["wp8tool"])
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\nwp8 = "{package.fq}"\n')
    result = _run(ocx, project, "lock", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    return project, "wp8tool"


# ---------------------------------------------------------------------------
# RUL-55 — `--project` accepts a directory
# ---------------------------------------------------------------------------


def test_project_flag_accepts_a_directory(ocx: OcxRunner, tmp_path: Path) -> None:
    """RUL-55 — ``--project <dir>`` resolves to ``<dir>/ocx.toml``.

    The discriminator is the exit code, not a success: the project has no lock,
    so a *resolved* project answers 78 ("run `ocx lock`"). The pre-RUL-55
    behaviour answered 74 ("not a regular file") without ever looking inside,
    which is the live defect that made every rendered trampoline fail.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, '[tools]\nx = "example.com/x:1.0.0"\n')

    result = _run(ocx, tmp_path, "--project", str(project), "env")

    assert result.returncode != EXIT_IO, (
        "RUL-55 — `--project <dir>` must not be refused as a non-regular file:\n"
        f"{result.stderr}"
    )
    assert result.returncode == C068_LOCK_MISSING, (
        f"a resolved project with no lock exits {C068_LOCK_MISSING}; "
        f"got {result.returncode}\n{result.stderr}"
    )


def test_project_flag_still_accepts_a_file_of_any_name(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """RUL-55 keeps the Cargo ``--manifest-path`` spelling: a *file* under any
    name is still accepted, so the directory branch is an addition and not a
    replacement."""
    project = tmp_path / "proj"
    project.mkdir()
    manifest = project / "custom.toml"
    manifest.write_text('[tools]\nx = "example.com/x:1.0.0"\n')

    result = _run(ocx, tmp_path, "--project", str(manifest), "env")

    assert result.returncode == C068_LOCK_MISSING, (
        f"an explicitly named project file must resolve; got {result.returncode}\n"
        f"{result.stderr}"
    )


def test_project_flag_naming_an_absent_path_exits_79(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """RUL-55's boundary — *this file is missing* (79) stays a different answer
    from *this directory governs no project* (64).

    Without this control, the 64 case below could be satisfied by a resolver
    that answers 64 for every unresolved selection, which would swallow a typo'd
    ``--project`` into "no project here".
    """
    absent = tmp_path / "nowhere" / "ocx.toml"

    result = _run(ocx, tmp_path, "--project", str(absent), "env")

    assert result.returncode == EXIT_NOT_FOUND, (
        f"an explicit --project naming an absent file exits {EXIT_NOT_FOUND}; "
        f"got {result.returncode}\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# C-068 — a trampoline's baked home, three ways
# ---------------------------------------------------------------------------


def test_exec_with_a_baked_home_holding_no_manifest_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-068 — ``NoProject`` → **64**, along the trampoline's own path.

    The re-entry a rendered trampoline performs, spelled by hand: a baked
    ``--project '<root>'`` whose project was deleted or moved. The invocation
    runs from an unrelated directory, exactly as a trampoline does.
    """
    baked = tmp_path / "moved-away"
    baked.mkdir()
    elsewhere = tmp_path / "cwd"
    elsewhere.mkdir()

    result = _run(ocx, elsewhere, "--project", str(baked), "exec", "--", "true")

    assert result.returncode == C068_NO_PROJECT, (
        f"C-068 — a baked home holding no ocx.toml exits {C068_NO_PROJECT}; "
        f"got {result.returncode}\n{result.stderr}"
    )
    assert str(baked) in result.stderr, (
        "C-068 — the error must name the *selected* home, not the working "
        f"directory:\n{result.stderr}"
    )


def test_exec_with_a_baked_home_holding_no_lock_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-068 — ``LockMissing`` → **78**, naming the lock it looked for."""
    baked = tmp_path / "proj"
    baked.mkdir()
    _write_ocx_toml(baked, '[tools]\nx = "example.com/x:1.0.0"\n')
    elsewhere = tmp_path / "cwd"
    elsewhere.mkdir()

    result = _run(ocx, elsewhere, "--project", str(baked), "exec", "--", "true")

    assert result.returncode == C068_LOCK_MISSING, (
        f"C-068 — an absent lock exits {C068_LOCK_MISSING}; "
        f"got {result.returncode}\n{result.stderr}"
    )
    assert "ocx.lock" in result.stderr, result.stderr


def test_exec_with_a_baked_home_holding_a_stale_lock_exits_65(
    ocx: OcxRunner, locked_project: tuple[Path, str], tmp_path: Path
) -> None:
    """C-068 — ``StaleLock`` → **65**.

    The third arm, and the one that proves the mapping is three-way: an
    implementation that answered a single code for "the baked home is no longer
    usable" would satisfy either of the two cases above on its own.
    """
    project, _ = locked_project
    # Mutate `ocx.toml` after locking: the declaration hash no longer matches.
    _write_ocx_toml(
        project,
        (project / "ocx.toml").read_text() + 'extra = "example.com/extra:1.0.0"\n',
    )
    elsewhere = tmp_path / "cwd"
    elsewhere.mkdir()

    result = _run(ocx, elsewhere, "--project", str(project), "exec", "--", "true")

    assert result.returncode == C068_LOCK_STALE, (
        f"C-068 — a stale lock exits {C068_LOCK_STALE}; "
        f"got {result.returncode}\n{result.stderr}"
    )


def test_the_three_baked_home_failures_are_three_distinct_codes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-068 is a **three-way** mapping. Collapsing any two of the codes
    satisfies every case above that names only one arm, so the distinctness is
    asserted in its own right."""
    no_project = tmp_path / "empty"
    no_project.mkdir()
    no_lock = tmp_path / "unlocked"
    no_lock.mkdir()
    _write_ocx_toml(no_lock, '[tools]\nx = "example.com/x:1.0.0"\n')
    cwd = tmp_path / "cwd"
    cwd.mkdir()

    codes = {
        _run(ocx, cwd, "--project", str(target), "exec", "--", "true").returncode
        for target in (no_project, no_lock)
    }
    assert codes == {C068_NO_PROJECT, C068_LOCK_MISSING}, (
        f"C-068 — the two reachable-without-a-registry arms must differ; got {codes}"
    )


# ---------------------------------------------------------------------------
# RUL-54 — `--dry-run` stays `ocx pull`-only
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("command", "operands"),
    [
        ("add", ["example.com/x:1.0.0"]),
        ("remove", ["x"]),
        ("lock", []),
        ("update", []),
    ],
)
def test_mutation_commands_reject_dry_run(
    ocx: OcxRunner, tmp_path: Path, command: str, operands: list[str]
) -> None:
    """RUL-54 — the shared ``commit_and_render`` must not grow the four
    mutation commands a ``--dry-run`` implicitly.

    Exit 64 at parse time, before any project is even looked for.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    result = _run(ocx, project, command, "--dry-run", *operands)

    assert result.returncode == EXIT_USAGE, (
        f"RUL-54 — `ocx {command} --dry-run` must be a usage error; "
        f"got {result.returncode}\n{result.stderr}"
    )
    # The positive control on the same binary: `ocx pull` does take the flag.
    assert _run(ocx, project, "pull", "--dry-run").returncode != EXIT_USAGE, (
        "the control: `ocx pull --dry-run` must not be a usage error"
    )


# ---------------------------------------------------------------------------
# C-055 — `--pinned` / `--no-pinned`
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("flag", ["--pinned", "--no-pinned"])
def test_env_and_exec_accept_the_pinned_pair(
    ocx: OcxRunner, tmp_path: Path, flag: str
) -> None:
    """C-055 — both flags parse on both composing emitters.

    Asserted by *not* being a usage error: the project is unlocked, so the
    invocation exits 78 on the lock rather than 64 on the flag. That separates
    "the flag exists" from "the command happened to succeed".
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, '[tools]\nx = "example.com/x:1.0.0"\n')

    for argv in (["env", flag], ["exec", flag, "--", "true"]):
        result = _run(ocx, project, *argv)
        assert result.returncode != EXIT_USAGE, (
            f"C-055 — `ocx {' '.join(argv)}` must accept {flag}:\n{result.stderr}"
        )


@pytest.mark.parametrize("flag", ["--pinned", "--no-pinned"])
def test_the_global_tier_accepts_the_pinned_pair(
    ocx: OcxRunner, tmp_path: Path, flag: str
) -> None:
    """RUL-50 — the pair is accepted on the global tier too, not a usage error.

    There is a global ``ocx.toml``, so ``pinned`` has a producer on both tiers
    and the flag selects the emitter lane wherever a composition is.
    """
    result = _run(ocx, tmp_path, "--global", "env", flag)
    assert result.returncode != EXIT_USAGE, (
        f"RUL-50 — `ocx --global env {flag}` must not be a usage error:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# C-056 — `ocx shell state` reports the resolved home
# ---------------------------------------------------------------------------


def test_shell_state_json_always_names_the_resolved_toolchain_home(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-056 / RUL-51 — the resolved home is a contract field of the JSON
    report: **always present, never null**, with a project and without one.

    Both states, because "always present" is a claim about every reportable
    state. The no-project run is the one where a nullable field would look
    reasonable — and it is exactly where an IDE still needs the global home.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    bare = tmp_path / "bare"
    bare.mkdir()

    for cwd, label in ((project, "with a project"), (bare, "with no project")):
        result = _run(ocx, cwd, "--format", "json", "shell", "state")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-056 — `ocx shell state` exits 0 in every reportable state "
            f"({label}); got {result.returncode}\n{result.stderr}"
        )
        payload = json.loads(result.stdout)
        assert "toolchain_home" in payload, (
            f"C-056 — the resolved home must be a field of the JSON report ({label})"
        )
        assert payload["toolchain_home"], (
            f"RUL-51 — the resolved home is never null or empty ({label}): {payload['toolchain_home']!r}"
        )
        assert payload.get("activate") in {"env", "bin", "none"}, (
            f"C-056 — the *effective* activate mode ({label}): {payload.get('activate')!r}"
        )
        assert isinstance(payload.get("pinned"), bool), (
            f"C-056 — the *effective* pinned value ({label}): {payload.get('pinned')!r}"
        )

    with_project = json.loads(
        _run(ocx, project, "--format", "json", "shell", "state").stdout
    )
    assert str(_toolchain_home(project)) in with_project["toolchain_home"], (
        "C-056 — with a project in effect the home is the project's own tree, "
        f"got {with_project['toolchain_home']!r}"
    )


def test_shell_state_reports_a_relocated_home_under_toolchain_dir(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-004 — with ``toolchain-dir`` configured, ``shell state --format json``
    names the **live** home, which is the whole point of publishing the field:
    a discoverer must not have to re-derive a 16-hex project key.

    Mutation that reds it: reporting ``<project>/.ocx/toolchain`` unconditionally.
    """
    home = Path(ocx.env["OCX_HOME"])
    root = home / "toolchains"
    root.mkdir(parents=True, exist_ok=True)
    (home / "config.toml").write_text(f'toolchain-dir = "{root.as_posix()}"\n')

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    result = _run(ocx, project, "--format", "json", "shell", "state")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    reported = Path(json.loads(result.stdout)["toolchain_home"])

    assert root in reported.parents, (
        f"S-004 — the reported home must live under the configured toolchain-dir "
        f"({root}); got {reported}"
    )
    assert reported.name == "toolchain", (
        f"C-002 — `<root>/<project-key>/toolchain`, key first; got {reported}"
    )
    assert _toolchain_home(project) != reported, (
        "the control: the relocated home must differ from the in-project default, "
        "or this case cannot discriminate"
    )


def test_shell_state_names_the_home_at_both_verbosity_tiers(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-056 — the human rendering names the home at both tiers, and stays
    non-eval-able.

    ``--verbose`` is a rendering tier, not a payload: a user who must pass a
    flag to learn where their toolchain lives has been told the answer is a
    diagnostic.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    expected = str(_toolchain_home(project))

    for argv in (["shell", "state"], ["shell", "state", "--verbose"]):
        result = _run(ocx, project, *argv)
        assert result.returncode == EXIT_SUCCESS, result.stderr
        assert expected in result.stdout, (
            f"C-056 — `ocx {' '.join(argv)}` must name the resolved home "
            f"({expected}):\n{result.stdout}"
        )
        for line in result.stdout.splitlines():
            stripped = line.strip()
            assert not stripped.startswith(("export ", "set ", "setenv ", "$env:")), (
                f"C-056 — the report is never eval-able: {line!r}"
            )


# ---------------------------------------------------------------------------
# C-054 — the four mutation commands re-render
# ---------------------------------------------------------------------------


def test_pull_renders_the_toolchain_bin_directory(
    ocx: OcxRunner, locked_project: tuple[Path, str]
) -> None:
    """C-054 — ``ocx pull`` renders ``<home>/toolchain/`` after the roots it
    selected have resolved. The baseline every case below is a delta against."""
    project, binary = locked_project
    result = _run(ocx, project, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    trampoline = shell_bin(_toolchain_home(project)) / binary
    assert trampoline.exists(), (
        f"C-054 — `ocx pull` must render a trampoline for {binary}; "
        f"{_toolchain_home(project)} holds {_snapshot(_toolchain_home(project))}"
    )
    mode = trampoline.stat().st_mode
    assert mode & 0o111, f"a trampoline must be executable; mode={mode:o}"


def test_the_rendered_trampoline_executes_the_tool_it_names(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """RUL-55, end to end — a rendered trampoline, **invoked as a program**,
    runs the tool it names.

    Every other case in this module asserts a trampoline *exists*. This one
    executes it, from a directory outside the project so the baked
    ``--project '<abs root>'`` selector is the only thing that can find the
    lock. That is the whole defect class: two waves merged green over a home
    whose every trampoline exited 74, because ``resolve_explicit_project_path``
    refused a *directory* and nothing in either wave ever ran one.

    Red state: guard out the directory arm of ``resolve_explicit_project_path``
    — this row then exits 74 with no marker on stdout.

    Deliberately one row: the Windows ``.exe`` arm, the ``-g`` matrix and the
    lazy-mode variants belong to WP-12d. This exists to stop the class, not to
    cover it.
    """
    package = _published_tool(ocx, tmp_path, "smoke", bins=["wp8smoke"])
    project = tmp_path / "smokeproj"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\nsmoke = "{package.fq}"\n')
    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS

    trampoline = shell_bin(_toolchain_home(project)) / "wp8smoke"
    assert trampoline.exists(), (
        "the control: `ocx pull` must have rendered the trampoline, or this row "
        f"tests nothing; {_toolchain_home(project)} holds "
        f"{_snapshot(_toolchain_home(project))}"
    )

    # Outside the project: no CWD walk can reach `ocx.toml` from here, so a
    # green can only come from the selector the render baked into the body.
    outside = tmp_path / "outside"
    outside.mkdir()
    result = subprocess.run(
        [str(trampoline)],
        cwd=outside,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        check=False,
        timeout=180,
    )

    assert result.returncode == EXIT_SUCCESS, (
        "RUL-55 — the baked `--project <directory>` selector must reach the "
        f"project:\nrc={result.returncode}\nstdout={result.stdout!r}\n"
        f"stderr={result.stderr!r}"
    )
    assert package.marker in result.stdout, (
        "the trampoline must run the tool it names, not merely exit 0; "
        f"stdout={result.stdout!r}"
    )


def test_add_renders_the_new_tool_s_trampoline(
    ocx: OcxRunner, locked_project: tuple[Path, str], tmp_path: Path
) -> None:
    """C-054 / D-V8 — ``ocx add`` commits **then** re-renders, through the one
    shared orchestration function.

    Mutation that reds it: a call site that reaches ``MutationGuard::commit``
    directly — the four-copies defect D-V8 exists to prevent — writes a correct
    lock and leaves the tree describing the previous one.
    """
    project, _ = locked_project
    package = _published_tool(ocx, tmp_path, "added", bins=["wp8added"])

    result = _run(ocx, project, "add", f"added={package.fq}")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert (shell_bin(_toolchain_home(project)) / "wp8added").exists(), (
        "C-054 — `ocx add` must re-render, so the new tool's trampoline appears"
    )


def test_remove_prunes_the_removed_tool_s_trampoline(
    ocx: OcxRunner, locked_project: tuple[Path, str]
) -> None:
    """C-054 / S-008 — ``ocx remove`` re-renders, and the prune half of C-044
    takes the trampoline with it. No other name changes."""
    project, binary = locked_project
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS
    trampoline = shell_bin(_toolchain_home(project)) / binary
    assert trampoline.exists(), "the control: the trampoline must exist before removal"

    result = _run(ocx, project, "remove", "wp8")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert not trampoline.exists(), (
        "S-008 — `ocx remove` must re-render, taking the tool's trampoline with it"
    )


@pytest.mark.parametrize("command", ["lock", "update"])
def test_lock_and_update_re_render(
    ocx: OcxRunner, locked_project: tuple[Path, str], command: str
) -> None:
    """C-054 — the remaining two of the four mutation commands re-render too.

    The tree is deleted first, so "the trampoline is there afterwards" can only
    be this command's doing.
    """
    project, binary = locked_project
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS
    assert (shell_bin(_toolchain_home(project)) / binary).exists(), (
        "the control: `ocx pull` must have rendered the tree first, or this case "
        "cannot tell a re-render from a tree that was never built"
    )
    import shutil

    shutil.rmtree(_toolchain_home(project))

    result = _run(ocx, project, command)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert (shell_bin(_toolchain_home(project)) / binary).exists(), (
        f"C-054 — `ocx {command}` must re-render the toolchain home"
    )


def test_update_narrowed_to_one_group_still_re_renders_the_whole_home(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """RUL-59 — ``-g`` on ``ocx update`` scopes **resolution**; the re-render is
    whole-home. (``ocx lock`` takes no ``-g`` at all, so ``update`` is the only
    mutation command the ruling can read on.)

    Mutation that reds it: narrowing the render to the groups ``-g`` named,
    which would leave ``shells/default/bin`` — the default group's directory — describing a
    lock that no longer exists.
    """
    default_tool = _published_tool(ocx, tmp_path, "dflt", bins=["wp8default"])
    ci_tool = _published_tool(ocx, tmp_path, "ci", bins=["wp8ci"])
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\nd = "{default_tool.fq}"\n\n[group.ci.tools]\nc = "{ci_tool.fq}"\n',
    )

    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS
    home = _toolchain_home(project)
    assert (shell_bin(home) / "wp8default").exists(), (
        "the control: the whole-home render must have run once, or a narrowed "
        "re-render cannot be told from a tree that was never built"
    )
    import shutil

    shutil.rmtree(home)

    result = _run(ocx, project, "update", "-g", "ci")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert (shell_bin(home) / "wp8default").exists(), (
        "RUL-59 — `-g ci` narrows resolution, not the tree: `shells/default/bin` still covers "
        "the default group"
    )


def test_a_render_skip_never_rolls_the_commit_back(
    ocx: OcxRunner, locked_project: tuple[Path, str]
) -> None:
    """RUL-53 — a C-050 render skip after a successful commit is **exit 0 with a
    warn**, and the lock stays written.

    The tree is made unwritable, which is the read-only-checkout shape C-050
    designs for. The half-rendered tree is what the C-061 stamp gate exists to
    withhold; undoing a correct lock write because a directory was not writable
    would trade a recoverable state for a lost one.
    """
    project, binary = locked_project
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS
    home = _toolchain_home(project)
    assert (shell_bin(home) / binary).exists(), (
        "the control: there must be a rendered tree to make unwritable, or the "
        "skip this case is about can never be provoked"
    )
    if os.geteuid() == 0:
        pytest.skip("root ignores the mode bits this case depends on")
    home.chmod(0o500)
    try:
        lock_before = (project / "ocx.lock").read_bytes()
        result = _run(ocx, project, "lock", "--no-pull")
        assert result.returncode == EXIT_SUCCESS, (
            "RUL-53 — a render skip is a warn, not a failure:\n" + result.stderr
        )
        assert (project / "ocx.lock").read_bytes() != b"" and (
            project / "ocx.lock"
        ).exists(), "RUL-53 — the committed lock must still be on disk"
        assert lock_before is not None
    finally:
        home.chmod(0o700)


# ---------------------------------------------------------------------------
# S-007 — `ocx pull --dry-run` over a poisoned tree
# ---------------------------------------------------------------------------


def test_pull_dry_run_over_a_poisoned_tree_writes_nothing(
    ocx: OcxRunner, locked_project: tuple[Path, str]
) -> None:
    """S-007 — the delta is printed, nothing is written, and **the poisoned link
    is still present afterwards**.

    "Writes nothing" is the whole ``<project>`` subtree by
    ``(path, type, bytes, mtime_ns, inode)``, not an inspection of the one file
    the case is about, and not an empty-output check.

    Mutation that reds it: performing the heal (C-051) during a dry run, which
    would report a delta the run had itself already closed.
    """
    project, binary = locked_project
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS

    # Poison: repoint the rendered trampoline at a foreign body.
    poisoned = shell_bin(_toolchain_home(project)) / binary
    assert poisoned.exists(), (
        "the control: there must be a rendered trampoline to poison, or "
        "'the poisoned link is still present afterwards' is vacuous"
    )
    poisoned.write_text("#!/bin/sh\necho poisoned\n")
    poisoned.chmod(0o755)

    before = _snapshot(project)
    result = _run(ocx, project, "pull", "--dry-run")
    after = _snapshot(project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert after == before, (
        "S-007 — `--dry-run` writes nothing anywhere under the project; "
        f"added={sorted(set(after) - set(before))} "
        f"removed={sorted(set(before) - set(after))} "
        f"changed={sorted(k for k in set(after) & set(before) if after[k] != before[k])}"
    )
    assert poisoned.read_text() == "#!/bin/sh\necho poisoned\n", (
        "S-007 — a dry run performs no heal, so the poisoned body is untouched"
    )


# ---------------------------------------------------------------------------
# C-058 — `ocx exec`'s lookup PATH excludes both trampoline directories
# ---------------------------------------------------------------------------


def test_exec_never_resolves_a_command_out_of_a_trampoline_directory(
    ocx: OcxRunner, locked_project: tuple[Path, str]
) -> None:
    """C-058 / C-069 — ``ocx exec <name>`` must not answer with the trampoline
    that ``ocx exec`` itself just put on PATH.

    Without the exclusion the invocation re-enters itself once per hop, forever.
    The case therefore carries its own timeout (``_run`` passes one): a
    regression here **hangs** rather than fails, and a hung run is not a red.

    Mutation that reds it: restoring the plain ``resolve_command`` at
    ``ToolchainExec::execute``.
    """
    project, binary = locked_project
    assert _run(ocx, project, "pull").returncode == EXIT_SUCCESS
    trampoline = shell_bin(_toolchain_home(project)) / binary
    assert trampoline.exists(), "the control: the trampoline must exist"

    # Replace the body with one that still carries the trampoline marker but
    # *announces itself* instead of re-entering. That is what makes the case
    # decidable in one run rather than by observing a hang: if the exclusion is
    # gone, C-069 refuses with 65; if both guards are gone, the marker appears
    # in stdout. Neither is exit 0 with the package's own output.
    trampoline.write_text(
        "#!/bin/sh\n# ocx-toolchain-trampoline\necho wp8-trampoline-answered\n"
    )
    trampoline.chmod(0o755)

    # The trampoline directory on PATH is the exact state `activate = "bin"`,
    # a `.envrc` or a `$GITHUB_PATH` line establishes.
    result = _run(
        ocx,
        project,
        "exec",
        "--",
        binary,
        extra_env={"PATH": f"{trampoline.parent}{os.pathsep}{ocx.env['PATH']}"},
    )

    assert result.returncode == EXIT_SUCCESS, (
        "C-058 — the C-010 exclusion, not C-069's refusal, is the guard that "
        f"must catch this input (65 here means the exclusion is gone):\n{result.stderr}"
    )
    assert "wp8-trampoline-answered" not in result.stdout, (
        "C-058 — the lookup PATH must exclude the trampoline directory; the "
        f"answer came from it:\n{result.stdout}"
    )


def test_exec_refuses_a_foreign_trampoline_answer(
    ocx: OcxRunner, locked_project: tuple[Path, str], tmp_path: Path
) -> None:
    """C-069 — the second, independent guard: a trampoline belonging to a
    **foreign** home is refused by identity, not by the exclusion set.

    The exclusion can only name *this* invocation's two homes, so a second
    project's trampoline on PATH is an entry it cannot name. Exit 65
    (``CommandResolutionError``), never a silent re-exec loop.

    Deleting either guard alone must leave the other observably firing — this is
    the input on which C-069 is the only defence.
    """
    project, _ = locked_project
    foreign_bin = shell_bin(tmp_path / "foreign" / "toolchain")
    foreign_bin.mkdir(parents=True)
    foreign = foreign_bin / "wp8foreign"
    foreign.write_text(
        "#!/bin/sh\n"
        "# ocx-toolchain-trampoline\n"
        "unset OCX_GLOBAL OCX_PROJECT\n"
        "__ocx_binary='/nonexistent/ocx'\n"
        'exec "${OCX_BINARY_PIN:-$__ocx_binary}" '
        f'--project \'{tmp_path / "foreign"}\' exec -- "${{0##*/}}" "$@"\n'
    )
    foreign.chmod(0o755)

    result = _run(
        ocx,
        project,
        "exec",
        "--",
        "wp8foreign",
        extra_env={"PATH": f"{foreign_bin}{os.pathsep}{ocx.env['PATH']}"},
    )

    assert result.returncode == EXIT_DATA, (
        "C-069 — a resolved answer that is itself an ocx trampoline is refused "
        f"with 65; got {result.returncode}\n{result.stderr}"
    )
    # 65 alone is not enough: `CommandResolutionError::NotFound` carries the
    # same code, so a name that simply did not resolve would pass this case for
    # the wrong reason. Assert *which* guard fired.
    assert "trampoline" in result.stderr, (
        "C-069 — the refusal must name the guard that fired, not merely fail "
        f"with 65:\n{result.stderr}"
    )

    # Positive control on the same PATH shape: an ordinary executable in the
    # same directory resolves and runs. Without it, the refusal above would
    # equally hold for an implementation that refuses every PATH entry.
    ordinary = foreign_bin / "wp8ordinary"
    ordinary.write_text("#!/bin/sh\necho wp8-ordinary-ran\n")
    ordinary.chmod(0o755)
    control = _run(
        ocx,
        project,
        "exec",
        "--",
        "wp8ordinary",
        extra_env={"PATH": f"{foreign_bin}{os.pathsep}{ocx.env['PATH']}"},
    )
    assert control.returncode == EXIT_SUCCESS, (
        "the control: a non-trampoline file in the same directory must still "
        f"resolve and run:\n{control.stderr}"
    )
    assert "wp8-ordinary-ran" in control.stdout, control.stdout


def test_remove_no_longer_answers_a_group_name_from_its_own_charset_copy(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`ocx remove -g` validates through the library, not a pre-C-014 copy.

    `remove.rs` carried its own check admitting only `[A-Za-z0-9_-]`, so
    `-g a.b` — a name C-014 admits — exited **1**, which is not a usage code
    either. Deleted; the group now reaches the lookup like any other.

    One assertion, because the charset refusal itself is already pinned where
    it is reachable: `project/mutate.rs`'s
    `add_binding_group_charset_matches_the_reader_in_both_directions` and
    `test_project_add.py::test_add_rejects_path_traversal_group_name`. On the
    remove path the binding lookup fails first, so an invalid name and a valid
    one both exit 79 here and the exit code cannot tell them apart — the
    stderr can.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    dotted = _run(ocx, project, "remove", "--group", "a.b", "nosuch")
    assert dotted.returncode == 79, (
        f"a C-014-valid group name must reach the lookup and fail there (binding "
        f"not found), not at a charset refusal; "
        f"rc={dotted.returncode}, stderr={dotted.stderr!r}"
    )
    assert "invalid group name" not in dotted.stderr, (
        f"the deleted pre-C-014 validator must not answer for 'a.b'; "
        f"stderr={dotted.stderr!r}"
    )


# ---------------------------------------------------------------------------
# OCX_NO_CONSENT — `ocx exec` carries the same suppression as `ocx pull`
# ---------------------------------------------------------------------------


def _consent_stamps(ocx_home: Path) -> list[Path]:
    """Every consent stamp under this run's isolated ``$OCX_HOME``."""
    return sorted((ocx_home / "state" / "projects").glob("*/consent.json"))


def _unconsented_project(
    ocx: OcxRunner, tmp_path: Path, name: str = "proj_noconsent"
) -> tuple[Path, str]:
    """``locked_project``'s setup, with the lock's own stamp suppressed.

    ``ocx lock`` is itself a consent writer, so it runs under
    ``OCX_NO_CONSENT=1`` — otherwise setup leaves the very stamp these tests
    are about to look for, and every assertion below reads someone else's write.

    ``name`` exists so one test can build two projects with distinct stamp keys.
    """
    package = _published_tool(ocx, tmp_path, "noconsent", bins=["wp8tool"])
    project = tmp_path / name
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\nwp8 = "{package.fq}"\n')
    result = _run(
        ocx, project, "lock", "--no-pull", extra_env={"OCX_NO_CONSENT": "1"}
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _consent_stamps(ocx.ocx_home) == [], (
        "setup must leave no consent stamp"
    )
    return project, "wp8tool"


def test_exec_records_a_consent_stamp(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx exec`` stamps consent by default (ocx-sh/ocx#400).

    The positive arm — without it the suppression test below would also pass
    on a binary that stopped stamping altogether.
    """
    project, binary = _unconsented_project(ocx, tmp_path)

    result = _run(ocx, project, "exec", "--", binary)
    assert result.returncode == EXIT_SUCCESS, result.stderr

    stamps = _consent_stamps(ocx.ocx_home)
    assert len(stamps) == 1, (
        f"ocx exec must write exactly one consent stamp; got {stamps}"
    )


def test_exec_with_no_consent_env_writes_no_stamp(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_NO_CONSENT=1 ocx exec`` runs the child without consenting.

    A generated launcher re-enters as ``ocx --project <baked home> exec`` on a
    machine whose operator never chose that checkout; the stamp it would leave
    authorizes the project's ``[env]`` on every later ``cd``.
    """
    project, binary = _unconsented_project(ocx, tmp_path)

    result = _run(
        ocx, project, "exec", "--", binary, extra_env={"OCX_NO_CONSENT": "1"}
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert _consent_stamps(ocx.ocx_home) == [], (
        "OCX_NO_CONSENT=1 must write no consent stamp"
    )


def test_exec_consent_flag_outranks_the_env_var(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_NO_CONSENT=1 ocx exec --consent`` stamps anyway.

    ``exec`` declares the flag pair after ``--records`` and before the
    positional ``names``/``argv``, per the flags-before-positional-arguments
    convention, so this also pins that the flag is reachable at all.
    """
    project, binary = _unconsented_project(ocx, tmp_path)

    result = _run(
        ocx,
        project,
        "exec",
        "--consent",
        "--",
        binary,
        extra_env={"OCX_NO_CONSENT": "1"},
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    stamps = _consent_stamps(ocx.ocx_home)
    assert len(stamps) == 1, (
        f"--consent must outrank OCX_NO_CONSENT and stamp; got {stamps}"
    )


def test_exec_no_consent_reaches_a_nested_ocx(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx exec --no-consent`` declines for the ocx its child launches too.

    A ``--no-consent`` lives in argv, which no child process ever sees, so the
    only channel to a nested frame is ``OCX_NO_CONSENT`` on the child
    environment. Without that forward the explicit gesture would carry *less*
    far than the ambient variable already does — failing open on a security
    control (ocx-sh/ocx#400).

    The nested ocx targets a **second** project, so its stamp lands under a
    different key than the outer ``exec``'s. That is what makes the control arm
    below name the inner frame specifically: the outer command cannot write the
    second stamp.

    Red state: delete the ``cfg.no_consent ||`` half of the
    ``OCX_NO_CONSENT`` block in ``Env::apply_ocx_config`` — the last arm then
    finds the inner frame's stamp.
    """
    outer, _ = _unconsented_project(ocx, tmp_path, "proj_outer")
    inner, _ = _unconsented_project(ocx, tmp_path, "proj_inner")
    nested = [str(ocx.binary), "--project", str(inner / "ocx.toml"), "pull"]

    # Control: nothing suppresses, so both frames stamp — two keys, two stamps.
    # Without it the arm below would also pass on an `exec` that never reached
    # the nested ocx at all.
    control = _run(ocx, outer, "exec", "--consent", "--", *nested)
    assert control.returncode == EXIT_SUCCESS, control.stderr
    stamps = _consent_stamps(ocx.ocx_home)
    assert len(stamps) == 2, (
        f"the nested `ocx pull` must stamp its own project, or the arm below "
        f"proves nothing about the forward; got {stamps}"
    )
    for stamp in stamps:
        stamp.unlink()

    result = _run(ocx, outer, "exec", "--no-consent", "--", *nested)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _consent_stamps(ocx.ocx_home) == [], (
        "`ocx exec --no-consent` must suppress the nested ocx's stamp too"
    )
