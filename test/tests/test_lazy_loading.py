# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for lazy package loading (issue #302).

A tool whose `lazy-mode` resolves to `always` is **deferred**: it reaches
`PATH` as a directory of generated launchers under `$OCX_HOME/shims/` and its
content materializes on the first invocation of one of its declared names.

Two families live here, both from `plan_lazy_package_loading.md`:

- one test per user-experience scenario **S-001 … S-012** (S-011 is the direnv
  scenario and lives in `test_lazy_direnv.py`);
- eight **state sequences** — compose / invoke / `clean` / recompose orders that
  no single scenario covers, because each one is about what the *previous* step
  left on disk.

Two techniques recur:

- **Shell-free.** Where nothing has to actually run, the assertion reads
  `ocx --format json env` and compares a `PATH` entry against the shim `bin/`
  path derived from the store layout. No shell, no PATH resolution of ours.
- **`OCX_BINARY_PIN` as the observation point.** A generated launcher execs
  `"${OCX_BINARY_PIN:-ocx}" launcher shim …`. Pointing that variable at a
  wrapper script makes every shim (and entry-point launcher) re-entry an
  observable, appended log line — which is how S-004 proves the shim was *not*
  entered a second time without measuring time.

Fetch / no-fetch is never inferred from timing or traffic: the suite has no
network observer and this file does not add one. It uses the two idioms the
suite already trusts — `--offline` plus its exit code, and store-state probes
through `ocx package which`.
"""
from __future__ import annotations

import json
import os
import shutil
import signal
import stat
import subprocess
import sys
from pathlib import Path

import pexpect
import pytest

from src import OcxRunner, PackageInfo
from src.helpers import (
    assert_shim_dir_absent,
    assert_shim_dir_exists,
    make_package,
    make_package_with_entrypoints,
    shim_bin_dirs,
    write_ocx_toml,
)
from src.shell_eval import run_after_sourcing
from tests.test_patches import assert_no_index_footprint

# Exit codes — mirror crates/ocx_lib/src/cli/exit_code.rs.
EXIT_SUCCESS = 0
EXIT_DATA_ERROR = 65
EXIT_NOT_FOUND = 79
EXIT_POLICY_BLOCKED = 81

# A consumer-visible `bin/` so the composed PATH carries the package's own
# executables under the default `--mode=consumer`. `make_package`'s default env
# additionally declares a `<REPO>_HOME` **constant** rooted at `${installPath}`,
# which raises a lazy advisory — fine where an advisory is the subject
# (sequence 7), noise everywhere else, so most fixtures below pass this instead.
PUBLIC_BIN_PATH = [
    {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin", "visibility": "public"}
]


# ---------------------------------------------------------------------------
# Helpers (DAMP — descriptive and meaningful, co-located with the tests)
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx` from `cwd` (the project CWD-walk) with the runner's env."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=env, check=False
    )


def _lazy_project(ocx: OcxRunner, tmp_path: Path, body: str, name: str = "project") -> Path:
    """Create a project from `body` and lock it **without pulling**.

    `ocx lock` installs on a miss by default, which would materialize the very
    content a deferred tool is supposed to reach `PATH` without. `--no-pull`
    is what leaves the store genuinely cold.
    """
    project = tmp_path / name
    project.mkdir()
    write_ocx_toml(project, body)
    result = _run(ocx, project, "lock", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock --no-pull must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    return project


def _toolchain_lazy(pkg: PackageInfo, binding: str = "hello", extra: str = "") -> str:
    """An `ocx.toml` binding one tool, deferred at the toolchain tier."""
    return f'lazy-mode = "always"\n{extra}\n[tools]\n{binding} = "{pkg.fq}"\n'


def _env_json(ocx: OcxRunner, project: Path, *args: str) -> dict:
    """`ocx --format json env` in `project`, parsed. Asserts exit 0."""
    result = _run(ocx, project, "--format", "json", "env", *args)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx env must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    return json.loads(result.stdout)


def _path_values(env_payload: dict) -> list[str]:
    """Every `PATH` entry's value, in emitted (apply) order."""
    return [entry["value"] for entry in env_payload["entries"] if entry["key"] == "PATH"]


def _is_materialized(ocx: OcxRunner, project: Path, pkg: PackageInfo) -> bool:
    """Whether `pkg`'s content is in the package store.

    `ocx package which` resolves locally and never installs, so it is a pure
    store probe: exit 79 when the package is absent, exit 0 plus a
    `{"path": ..., "kind": ...}` object when it is present. Preferred over
    globbing `$OCX_HOME/packages`, which is keyed by digest alone and cannot
    tell two fixtures apart.

    `kind` is what makes this unambiguous under lazy loading: without a policy
    flag `which` reports the package store, so a `shim` answer here would mean
    the probe is reading the wrong tier rather than that content exists.
    """
    result = _run(ocx, project, "--format", "json", "package", "which", pkg.short)
    if result.returncode == EXIT_NOT_FOUND:
        return False
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx package which must exit 0 or {EXIT_NOT_FOUND}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    located = json.loads(result.stdout)[pkg.short]
    assert located["kind"] == "package", (
        f"a store probe with no policy flag must answer from the package store; got {located}"
    )
    root = Path(located["path"])
    assert (root / "content").is_dir(), f"which reported {root}, which has no content/"
    return True


def _observer(tmp_path: Path, ocx: OcxRunner, name: str = "ocx-observer") -> tuple[Path, Path]:
    """A stand-in for the inner ocx that logs every re-entry, then execs it.

    Returns `(script, log)`. Point `OCX_BINARY_PIN` at `script`: a generated
    launcher body runs `"${OCX_BINARY_PIN:-ocx}" <subcommand> …`, so every line
    in `log` is one launcher re-entry and its argv. This is the process
    observation S-004 requires — the alternative, timing the second call, is
    explicitly not a proof.
    """
    log = tmp_path / f"{name}.log"
    script = tmp_path / name
    script.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{log}"\nexec "{ocx.binary}" "$@"\n')
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script, log


def _shell_env(ocx: OcxRunner, pin: Path | None = None, **extra: str) -> dict[str, str]:
    """The runner env plus an explicit inner-ocx pin.

    Without `OCX_BINARY_PIN` a generated launcher falls back to whatever `ocx`
    the ambient `PATH` finds — on a developer machine that is a *different*
    build, and the re-entry fails with an unrelated usage error. Every command
    that spawns a child sets this pin itself; a test that sources export lines
    into its own shell has to set it too.
    """
    env = dict(ocx.env)
    env["OCX_BINARY_PIN"] = str(pin or ocx.binary)
    env.update(extra)
    return env


def _observed_shim_calls(log: Path) -> list[str]:
    """Every `launcher shim` re-entry recorded by an `_observer` script."""
    if not log.exists():
        return []
    return [line for line in log.read_text().splitlines() if line.startswith("launcher shim ")]


def _run_in_its_own_process_group(
    argv: list[str], cwd: Path, env: dict[str, str], timeout: float
) -> subprocess.CompletedProcess[str]:
    """Run `argv` under a deadline, killing the **whole group** if it expires.

    For the one regression whose failure mode is a hang rather than a wrong
    answer. `subprocess.run(timeout=...)` kills only the direct child — here a
    `bash` that forked before exec'ing — so a grandchild spinning in an
    `execve` loop would survive the timeout and stay pinned at 100% CPU on the
    machine running the suite. `start_new_session=True` makes the group
    addressable, and `killpg` takes the loop with it.

    Raises `subprocess.TimeoutExpired`, which fails the test — the point being
    that a test that hangs forever is worse than one that fails.
    """
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(os.getpgid(process.pid), signal.SIGKILL)
        process.communicate()
        raise
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def _trigger_on_a_terminal(
    script: Path, cwd: Path, env: dict[str, str], timeout: int = 300
) -> tuple[int, str]:
    """Run `bash --norc <script>` attached to a pty, returning `(status, terminal)`.

    `pexpect` puts the child in its own session with the pty as its
    **controlling terminal**, which is the state a developer's shell is in.
    The script is expected to redirect its own stdout and stderr into files, so
    everything in the returned string was written to the terminal *directly* —
    that separation is the entire assertion, because a shim runs inside another
    tool's process tree and its standard streams belong to whatever invoked it.

    A wide window keeps the terminal from truncating a rendered line before the
    token the caller greps for; 24x80 would clip a long pinned identifier.
    """
    child = pexpect.spawn(
        "bash",
        ["--norc", str(script)],
        cwd=str(cwd),
        env=env,
        timeout=timeout,
        encoding="utf-8",
        codec_errors="replace",
        dimensions=(40, 200),
    )
    child.expect(pexpect.EOF)
    terminal = child.before or ""
    child.close()
    return child.exitstatus, terminal


# ---------------------------------------------------------------------------
# S-001 — `ocx env` with `lazy-mode = "always"`
# ---------------------------------------------------------------------------


def test_s001_lazy_mode_always_puts_a_shim_dir_on_path_with_no_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-001: shim dir on PATH, no `content/` on disk, env otherwise eager-identical.

    The three clauses are asserted separately, and the third against a real
    eager compose of the *same* project (`--lazy-mode never` is the top tier of
    the ladder, so it overrides the toolchain-tier declaration): the deferred
    env must be the eager env plus exactly one entry, the shim slot.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    assert not _is_materialized(ocx, project, pkg), (
        "precondition: `ocx lock --no-pull` must leave the store cold, or the "
        "'no content on disk' clause below is asserted against a warm store"
    )

    lazy = _env_json(ocx, project)
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-001: a deferred tool publishes a shim tree")

    assert _path_values(lazy)[0] == str(shim_bin), (
        f"S-001: the shim `bin/` must be the first PATH entry emitted (last to "
        f"resolve, so the real bin/ and entrypoints/ shadow it once they exist); "
        f"got {_path_values(lazy)}"
    )
    assert not _is_materialized(ocx, project, pkg), (
        "S-001: composing a deferred tool must not materialize its content"
    )

    eager = _env_json(ocx, project, "--lazy-mode", "never")
    assert [entry for entry in lazy["entries"] if entry["value"] != str(shim_bin)] == eager["entries"], (
        "S-001: apart from the shim slot the deferred env must equal the eager env\n"
        f"deferred: {json.dumps(lazy['entries'], indent=2)}\n"
        f"eager:    {json.dumps(eager['entries'], indent=2)}"
    )
    assert lazy["binaries"] == eager["binaries"], (
        "S-001: the admitted `binaries` attribution must not depend on lazy-mode"
    )


# ---------------------------------------------------------------------------
# S-002 — invoking a shimmed binary name
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s002_invoking_a_shimmed_binary_materializes_and_execs_the_target(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-002: invoking a shimmed name materializes the package and execs the target."""
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    export = _run(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"
    assert not _is_materialized(ocx, project, pkg), "precondition: the store starts cold"

    result = run_after_sourcing(
        export.stdout, "hello", cwd=project, env=_shell_env(ocx)
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"S-002: the shim must materialize and exec the target; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"S-002: the real binary's output must reach the caller's stdout; "
        f"marker={pkg.marker!r} stdout={result.stdout!r}"
    )
    assert _is_materialized(ocx, project, pkg), (
        "S-002: the package must be materialized by the time the target has run"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_c011_a_claimed_name_the_package_does_not_ship_exits_instead_of_looping(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-011: a `binaries` claim naming an executable the package lacks **terminates**.

    The fixture is the whole attack: a package that ships `bin/real` and claims
    `ghost`. `prepare_lazy` cannot verify the claim — no content exists yet, by
    construction — so a launcher for `ghost` reaches `PATH`, and invoking it
    materializes a package that does not contain it.

    What must not happen is a loop. The shim process inherits the `PATH` that
    found the launcher, and composed entries prepend rather than replace, so
    the shim `bin/` survives lower down; without stripping it, `ghost` resolves
    back to the same launcher, `execvp` re-enters the same process, and the
    build hangs at 100% CPU with no output and no exit. Publishing a package is
    the only capability that takes.

    Asserted as an *exit*, under a deadline enforced on the whole process
    group, so a regression fails the suite instead of hanging it.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["real"],
        binaries=["ghost"],
        no_bin_scan=True,
        env=PUBLIC_BIN_PATH,
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg, binding="ghost"))
    export = _run(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "C-011: the unfulfillable name is shimmed")
    assert (shim_bin / "ghost").exists(), (
        f"precondition: a launcher for the claimed-but-unshipped name must exist, or the "
        f"trigger never enters the shim; {shim_bin} holds {sorted(p.name for p in shim_bin.iterdir())}"
    )

    script = tmp_path / "trigger.sh"
    script.write_text(export.stdout + "\nghost\n")
    result = _run_in_its_own_process_group(
        ["bash", "--norc", str(script)], cwd=project, env=_shell_env(ocx), timeout=90
    )

    assert result.returncode == EXIT_DATA_ERROR, (
        f"C-011: an unfulfilled claim must exit {EXIT_DATA_ERROR}; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "ghost" in result.stderr and pkg.repo in result.stderr, (
        f"C-011: the refusal must name the package and the claimed name, so the defect is "
        f"attributed to the publisher; got:\n{result.stderr}"
    )
    assert _is_materialized(ocx, project, pkg), (
        "C-011: the package still materializes — the claim is what goes unfulfilled, not the pull"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_c011_a_shim_tree_reached_by_a_second_spelling_exits_instead_of_looping(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-011: the strip is a string compare, so the belt behind it must not be one.

    The same unfulfillable claim as the test above, with one thing changed:
    the process that composes the `PATH` and the process the launcher
    re-enters spell `$OCX_HOME` differently — a symlink to the one directory.
    Nothing canonicalizes that value, so the composed `PATH` carries the real
    spelling while the shim process derives its own shim `bin/` from the alias.

    That defeats the exact-segment strip **and** a lexical belt identically,
    because both compare the same two strings. Only comparing the *resolved*
    forms tells them apart: this test reds on a belt written as
    `resolved.starts_with(shim_bin)` and greens on one that resolves both
    sides first. The regression it guards is the unbounded `execve` loop
    returning in full, which is why the deadline is on the whole process
    group and not on the direct child.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["real"],
        binaries=["ghost"],
        no_bin_scan=True,
        env=PUBLIC_BIN_PATH,
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg, binding="ghost"))

    # Composed under the real `$OCX_HOME`, so these export lines carry the shim
    # `bin/` spelled the way the strip will later fail to find it.
    export = _run(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "C-011: the unfulfillable name is shimmed")
    assert str(shim_bin) in export.stdout, (
        "precondition: the composed PATH must carry the real spelling of the shim `bin/`, or "
        f"the two spellings never diverge and the test measures nothing:\n{export.stdout}"
    )

    alias_home = tmp_path / "ocx-home-alias"
    alias_home.symlink_to(ocx.ocx_home, target_is_directory=True)
    assert alias_home.resolve() == ocx.ocx_home.resolve(), (
        "precondition: the alias must name the same directory, or the trigger runs against a "
        "second, empty store and refuses for an unrelated reason"
    )

    script = tmp_path / "trigger-aliased-home.sh"
    script.write_text(export.stdout + "\nghost\n")
    result = _run_in_its_own_process_group(
        ["bash", "--norc", str(script)],
        cwd=project,
        env=_shell_env(ocx, OCX_HOME=str(alias_home)),
        timeout=90,
    )

    assert result.returncode == EXIT_DATA_ERROR, (
        f"C-011: an unfulfilled claim must exit {EXIT_DATA_ERROR} however the shim tree is "
        f"spelled; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "ghost" in result.stderr and pkg.repo in result.stderr, (
        f"C-011: the refusal must still name the package and the claimed name; got:\n{result.stderr}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s002_invoking_a_shimmed_binary_offline_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-002 error case: `--offline` at the trigger is a policy block (exit 81).

    Also the file's fetch/no-fetch proof: the same invocation succeeds online
    (test above) and is refused offline against a store that was never warmed,
    so a fetch is exactly what the deferred tool needed and did not have.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    export = _run(ocx, project, "env", "--shell=sh")
    assert_shim_dir_exists(ocx, pkg.repo, "S-002: the shim exists before the offline trigger")

    result = run_after_sourcing(
        export.stdout, "hello", cwd=project, env=_shell_env(ocx, OCX_OFFLINE="1")
    )

    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"S-002: a cold trigger under --offline must exit {EXIT_POLICY_BLOCKED} "
        f"(PolicyBlocked); rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not _is_materialized(ocx, project, pkg), (
        "S-002: a refused trigger must leave the store cold"
    )


# ---------------------------------------------------------------------------
# S-003 — invoking a shimmed entry-point name
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s003_invoking_a_shimmed_entrypoint_applies_the_real_launcher_dispatch(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-003: an entry-point name shimmed and triggered dispatches like an eager one.

    The shim name set is the interface surface — `binaries` claims *and* entry
    points — so an entry-point-only package still gets a launcher. After the
    trigger the real generated launcher runs, which is observable in the
    output: the fixture's entry point echoes its own name, its marker and the
    arguments it was handed.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["ep"],
        bins=["ep"],
        env=PUBLIC_BIN_PATH,
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg, binding="ep"))
    export = _run(ocx, project, "env", "--shell=sh")
    script, log = _observer(tmp_path, ocx)

    result = run_after_sourcing(
        export.stdout, "ep one two", cwd=project, env=_shell_env(ocx, pin=script)
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"S-003: a shimmed entry point must run; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert f"entry-point-ep {pkg.marker} one two" in result.stdout, (
        f"S-003: the real launcher must dispatch with the user's arguments intact; "
        f"stdout={result.stdout!r}"
    )
    assert _observed_shim_calls(log) == [
        f"launcher shim {_pinned(ocx, project, pkg)} -- ep one two"
    ], (
        f"S-003: the trigger must re-enter ocx as `launcher shim <pinned> -- <argv0> <args>`; "
        f"observed {_observed_shim_calls(log)}"
    )


def _pinned(ocx: OcxRunner, project: Path, pkg: PackageInfo) -> str:
    """The digest-pinned identifier the composer attributes `pkg`'s names to.

    Read back from `ocx env`'s own attribution array rather than parsed out of
    `ocx.lock`, so the expected value comes from the command under test's
    published wire shape and not from a second reader of the lock format.
    """
    payload = _env_json(ocx, project)
    owners = {claim["package"] for claim in payload["binaries"] + payload["entrypoints"]}
    matching = sorted(owner for owner in owners if pkg.repo in owner)
    assert len(matching) == 1, f"expected one attributed identifier for {pkg.repo}; got {matching}"
    return matching[0]


# ---------------------------------------------------------------------------
# S-004 — the second invocation of the same name
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s004_a_second_invocation_of_the_same_name_does_not_re_enter_the_shim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-004: after materialization the name resolves to the real `bin/`, not the shim.

    Verified by **process observation, never by timing**: `OCX_BINARY_PIN`
    points at a wrapper that appends one line per launcher re-entry, so "the
    shim did not run again" is the literal absence of a second `launcher shim`
    line — a fact, not an inference from how long the call took.

    `hash -r` between the two calls clears bash's own command hash; without it
    the second lookup would be answered from the cache rather than by PATH, and
    the test would prove nothing about PATH precedence (C-012).
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    export = _run(ocx, project, "env", "--shell=sh")
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-004: the first call must go through a shim")
    script, log = _observer(tmp_path, ocx)

    result = run_after_sourcing(
        export.stdout,
        'command -v hello\nhello\nhash -r\ncommand -v hello\nhello',
        cwd=project,
        env=_shell_env(ocx, pin=script),
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"S-004: both invocations must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    resolved = [line for line in result.stdout.splitlines() if line.endswith("/hello")]
    assert len(resolved) == 2, f"S-004: expected two `command -v` answers; got {result.stdout!r}"
    assert resolved[0] == str(shim_bin / "hello"), (
        f"S-004: the first lookup must resolve to the shim; got {resolved[0]}"
    )
    assert resolved[1] != resolved[0] and resolved[1].endswith("/content/bin/hello"), (
        f"S-004: after materialization the same name must resolve to the real "
        f"`content/bin/`; got {resolved[1]}"
    )
    assert len(_observed_shim_calls(log)) == 1, (
        f"S-004: the shim must be entered exactly once across both invocations; "
        f"observed {_observed_shim_calls(log)}"
    )


# ---------------------------------------------------------------------------
# S-005 — `ocx env` twice, cold store then warm
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s005_env_output_is_byte_identical_cold_store_and_warm(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-005: the composed env does not depend on content-cache state (C-013).

    Byte comparison of the two raw stdout buffers, not of parsed objects: the
    claim is about output, and a difference in key order or formatting would be
    just as much a difference to a caller diffing two runs.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    cold = _run(ocx, project, "--format", "json", "env")
    assert cold.returncode == EXIT_SUCCESS, f"cold compose failed:\n{cold.stderr}"
    assert not _is_materialized(ocx, project, pkg), "precondition: the first compose is cold"

    export = _run(ocx, project, "env", "--shell=sh")
    triggered = run_after_sourcing(export.stdout, "hello", cwd=project, env=_shell_env(ocx))
    assert triggered.returncode == EXIT_SUCCESS, f"trigger failed:\n{triggered.stderr}"
    assert _is_materialized(ocx, project, pkg), "precondition: the second compose is warm"

    warm = _run(ocx, project, "--format", "json", "env")
    assert warm.stdout == cold.stdout, (
        "S-005: a warm store must produce byte-identical env output\n"
        f"cold:\n{cold.stdout}\nwarm:\n{warm.stdout}"
    )


# ---------------------------------------------------------------------------
# S-006 — `ocx --frozen run` on a cold lazy tool
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s006_frozen_run_materializes_by_digest_and_writes_nothing_under_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """S-006: a frozen run of a cold deferred tool touches the local index not at all.

    The local index is **deleted** after locking, and the run still succeeds:
    the lock pins a digest, so nothing has to be resolved through a tag. That
    also makes the zero-footprint assertion sharp rather than a no-growth one —
    the repository owns literally nothing under `index/` afterwards, and any
    dispatch object or root document the run persisted would show up.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    shutil.rmtree(ocx.ocx_home / "index")

    result = _run(ocx, project, "--frozen", "exec", "--", "hello")

    assert result.returncode == EXIT_SUCCESS, (
        f"S-006: `ocx --frozen run` on a cold deferred tool must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"S-006: the tool must actually run; marker={pkg.marker!r} stdout={result.stdout!r}"
    )
    # Before the materialization probe, not after: `ocx package which` takes a
    # tagged identifier and resolves it through the index, which repopulates the
    # very directory this assertion is about.
    assert_no_index_footprint(
        ocx, registry, pkg.repo, "S-006: a frozen run resolves no tag and writes no index entry"
    )
    # Anchors the scenario to the deferred path: every other assertion in this
    # test also holds for an eagerly composed frozen run, so without this the
    # test would stay green with `lazy-mode` set to `never`.
    assert_shim_dir_exists(ocx, pkg.repo, "S-006: the frozen run composed the tool as a shim")
    assert _is_materialized(ocx, project, pkg), "S-006: the frozen run must materialize by digest"


# ---------------------------------------------------------------------------
# S-007 — `ocx package which` across policy and state
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s007_which_answers_all_four_policy_and_state_cells(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-007: the four policy/state cells `ocx package which` answers (C-016).

    | policy | state | answer |
    |---|---|---|
    | none | nothing on disk | exit 79, not found |
    | `--lazy-mode always` | shim tree, no content | the **shim** path, `kind: "shim"` |
    | none | materialized | the package root, `kind: "package"` |
    | `--lazy-mode always` | materialized | the package root — content outranks a shim |

    `kind` is the discriminator, so each cell asserts it rather than
    pattern-matching the path string: a `path` under `shims/` and a `kind` of
    `package` would be a contradiction the string check alone would miss.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    # Cell 1 — nothing composed, nothing materialized.
    cold = _run(ocx, project, "--format", "json", "package", "which", pkg.short)
    assert cold.returncode == EXIT_NOT_FOUND, (
        f"S-007: an unmaterialized package must answer {EXIT_NOT_FOUND}; "
        f"rc={cold.returncode}\nstderr:\n{cold.stderr}"
    )

    export = _run(ocx, project, "env", "--shell=sh")
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-007: the tool under test is deferred")

    # Cell 2 — a shim tree and no content: the lazy policy answers with the shim.
    deferred = _run(
        ocx, project, "--format", "json", "package", "which", "--lazy-mode", "always", pkg.short
    )
    assert deferred.returncode == EXIT_SUCCESS, (
        f"S-007: --lazy-mode always must resolve a deferred tool through its shim tree; "
        f"rc={deferred.returncode}\nstderr:\n{deferred.stderr}"
    )
    located = json.loads(deferred.stdout)[pkg.short]
    assert located["kind"] == "shim", (
        f"S-007: a tool with no content resolves as a shim under the lazy policy; got {located}"
    )
    assert Path(located["path"]) == shim_bin.parent, (
        f"S-007: the reported shim path must be the shim root the store published; "
        f"got {located['path']}, expected {shim_bin.parent}"
    )

    triggered = run_after_sourcing(export.stdout, "hello", cwd=project, env=_shell_env(ocx))
    assert triggered.returncode == EXIT_SUCCESS, f"trigger failed:\n{triggered.stderr}"

    # Cells 3 and 4 — once content exists it outranks the shim under either policy.
    for policy in ([], ["--lazy-mode", "never"], ["--lazy-mode", "always"]):
        warm = _run(ocx, project, "--format", "json", "package", "which", *policy, pkg.short)
        assert warm.returncode == EXIT_SUCCESS, (
            f"S-007: a materialized package must resolve under {policy or 'no policy flag'}; "
            f"rc={warm.returncode}\nstderr:\n{warm.stderr}"
        )
        located = json.loads(warm.stdout)[pkg.short]
        assert located["kind"] == "package", (
            f"S-007: {policy or 'no policy flag'} must report the package store once content "
            f"exists; got {located}"
        )
        root = Path(located["path"])
        assert (root / "content" / "bin" / "hello").exists(), (
            f"S-007: {policy or 'no policy flag'} must report the real package root; got {root}"
        )


# ---------------------------------------------------------------------------
# S-008 — `ocx clean` then compose again
# ---------------------------------------------------------------------------


def test_s008_clean_keeps_a_lock_pinned_shim_and_the_next_compose_is_unchanged(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-008: `ocx clean` retains a shim the lock still pins; compose is unchanged.

    Shim liveness is rooted directly in the lock pins — a deferred tool has no
    package directory to carry an edge to its shim — so the interesting failure
    is a GC that cannot see the shim store at all and collects a live tool.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    before = _run(ocx, project, "--format", "json", "env")
    assert before.returncode == EXIT_SUCCESS, f"compose failed:\n{before.stderr}"
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-008: composed before the clean")

    clean = _run(ocx, project, "--format", "json", "clean")
    assert clean.returncode == EXIT_SUCCESS, f"ocx clean failed:\n{clean.stderr}"

    assert assert_shim_dir_exists(ocx, pkg.repo, "S-008: the lock still pins the tool") == shim_bin, (
        "S-008: `ocx clean` must not collect a shim the lock still pins"
    )
    after = _run(ocx, project, "--format", "json", "env")
    assert after.stdout == before.stdout, (
        f"S-008: compose after clean must be unchanged\nbefore:\n{before.stdout}\nafter:\n{after.stdout}"
    )


# ---------------------------------------------------------------------------
# S-009 — `--no-pull` with `lazy-mode = "always"`
# ---------------------------------------------------------------------------


def test_s009_no_pull_composes_shims_for_local_metadata_and_omits_the_rest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-009: under `--no-pull` a deferred tool composes iff its metadata is local.

    Selectivity is the point, so one compose carries both outcomes: the first
    tool was composed online once (its metadata is in the blob store), the
    second never was. A run that warned about both, or about neither, fails.
    """
    local = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["aa"], binaries=["aa"], env=PUBLIC_BIN_PATH
    )
    absent = make_package(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, bins=["bb"], binaries=["bb"], env=PUBLIC_BIN_PATH
    )

    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(local, binding="aa"))
    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS
    assert_shim_dir_exists(ocx, local.repo, "S-009: the first tool's metadata is now local")

    write_ocx_toml(
        project,
        f'lazy-mode = "always"\n\n[tools]\naa = "{local.fq}"\nbb = "{absent.fq}"\n',
    )
    assert _run(ocx, project, "lock", "--no-pull").returncode == EXIT_SUCCESS

    result = _run(ocx, project, "--format", "json", "env", "--no-pull")

    assert result.returncode == EXIT_SUCCESS, (
        f"S-009: an omitted tool is not a failure; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    shim_bin = assert_shim_dir_exists(ocx, local.repo, "S-009: local metadata still composes")
    assert_shim_dir_absent(ocx, absent.repo, "S-009: --no-pull must not fetch the absent tool's metadata")
    assert str(shim_bin) in _path_values(json.loads(result.stdout)), (
        f"S-009: the composable tool must still reach PATH; got {_path_values(json.loads(result.stdout))}"
    )
    assert "bb not installed" in result.stderr, (
        f"S-009: the omitted tool must be named on stderr; got:\n{result.stderr}"
    )
    assert "aa not installed" not in result.stderr, (
        f"S-009: the composable tool must not be warned about; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# S-010 — Windows composes eagerly in this phase
# ---------------------------------------------------------------------------


def test_s010_windows_composes_eagerly_while_other_hosts_compose_a_shim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-010: on Windows `lazy-mode = "always"` composes eagerly — no user-visible break.

    Two host-gated arms rather than one expectation parameterised by
    `sys.platform`: an assertion that restates the production platform gate
    agrees with the code on every host, including a host where the code is
    wrong. Each arm below asserts a literal outcome and runs on the host it
    describes. On this suite's Linux/macOS legs only the second arm executes;
    the first is live on the `windows-latest` leg.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    payload = _env_json(ocx, project)

    if sys.platform == "win32":
        assert shim_bin_dirs(ocx, pkg.repo) == [], (
            "S-010: Windows has no shim producer in this phase, so a tool asking "
            f"for lazy-mode=always must compose eagerly; found {shim_bin_dirs(ocx, pkg.repo)}"
        )
        assert _is_materialized(ocx, project, pkg), (
            "S-010: composing eagerly means the content is materialized, not deferred"
        )
    else:
        shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-010: a non-Windows host defers")
        assert str(shim_bin) in _path_values(payload), (
            f"S-010: the host floor is Windows-only; elsewhere the shim slot must be "
            f"emitted; got {_path_values(payload)}"
        )
        assert not _is_materialized(ocx, project, pkg), (
            "S-010: a deferred tool has no content on a host that supports shims"
        )


# ---------------------------------------------------------------------------
# S-012 — `lazy-report = "progress"` with no controlling terminal
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s012_progress_report_degrades_silently_without_a_controlling_terminal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-012: no controlling terminal degrades progress to silence, never to an error.

    `start_new_session=True` makes the child a session leader with no
    controlling terminal — the same state `setsid`, a Docker build and a CI
    runner produce — so the `open("/dev/tty")` the `progress` arm performs
    genuinely fails with `ENXIO` rather than being assumed to.

    This test pins the **degrade** half only: it cannot tell `progress` from
    `silent`, because neither renders here. The half that discriminates is
    `test_s012_lazy_report_selects_the_controlling_terminal_channel` below,
    which supplies a real terminal.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(
        ocx, tmp_path, _toolchain_lazy(pkg, extra='lazy-report = "progress"\n')
    )
    export = _run(ocx, project, "env", "--shell=sh")
    # Without this the trigger below is just "a binary on PATH ran", which is
    # equally true of an eagerly composed tool — the scenario is about a *shim*
    # firing with no progress channel available.
    assert_shim_dir_exists(ocx, pkg.repo, "S-012: the trigger goes through a shim")
    script = tmp_path / "trigger.sh"
    script.write_text(export.stdout + "\nhello\n")

    result = subprocess.run(
        ["bash", "--norc", str(script)],
        cwd=project,
        capture_output=True,
        text=True,
        env=_shell_env(ocx),
        start_new_session=True, check=False,
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"S-012: an unavailable progress channel must never fail the trigger; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"S-012: the tool must still run; marker={pkg.marker!r} stdout={result.stdout!r}"
    )
    assert _is_materialized(ocx, project, pkg), "S-012: materialization must complete normally"


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s012_errors_still_reach_stderr_under_progress_reporting(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-012, second clause: `lazy-report` never suppresses the error channel.

    Same no-controlling-terminal session as above, but the trigger is refused
    (`--offline` against a cold store), so the run must still exit non-zero and
    say why on stderr.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(
        ocx, tmp_path, _toolchain_lazy(pkg, extra='lazy-report = "progress"\n')
    )
    export = _run(ocx, project, "env", "--shell=sh")
    script = tmp_path / "trigger.sh"
    script.write_text(export.stdout + "\nhello\n")

    result = subprocess.run(
        ["bash", "--norc", str(script)],
        cwd=project,
        capture_output=True,
        text=True,
        env=_shell_env(ocx, OCX_OFFLINE="1"),
        start_new_session=True, check=False,
    )

    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"S-012: a refused trigger must still exit {EXIT_POLICY_BLOCKED}; rc={result.returncode}"
    )
    assert "offline" in result.stderr.lower(), (
        f"S-012: the reason must still reach stderr under lazy-report=progress; got:\n{result.stderr}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
@pytest.mark.parametrize(
    ("report", "renders"),
    [("progress", True), ("silent", False)],
    ids=["progress-renders", "silent-stays-quiet"],
)
def test_s012_lazy_report_selects_the_controlling_terminal_channel(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, report: str, renders: bool
) -> None:
    """S-012: `lazy-report` decides whether the **terminal** sees the download.

    The setting's only observable effect is a channel, so a channel is what
    this asserts. The trigger runs on a pty with its own stdout and stderr
    redirected into files — the shape of a tool invoked by `make`, or by any
    wrapper that captures output — and the assertion is on the pty bytes, which
    only a write to the controlling terminal can reach.

    Redirecting the standard streams is what makes the two arms distinguishable
    at all: leave them on the terminal and both settings look identical from
    outside, which is why the two subprocess-based S-012 tests above stay green
    whichever value the fixture carries.

    The token grepped for is the package's own repository name, which the
    materialization spinner (`Resolving '<pinned>'`) renders and which cannot
    reach the pty by any other route — the tool's own output is in the files.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(
        ocx, tmp_path, _toolchain_lazy(pkg, extra=f'lazy-report = "{report}"\n')
    )
    export = _run(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"
    assert_shim_dir_exists(ocx, pkg.repo, "S-012: the trigger goes through a shim")

    out_log = tmp_path / "trigger.out"
    err_log = tmp_path / "trigger.err"
    script = tmp_path / "trigger.sh"
    script.write_text(f'{export.stdout}\nhello >"{out_log}" 2>"{err_log}"\n')

    status, terminal = _trigger_on_a_terminal(script, cwd=project, env=_shell_env(ocx))

    assert status == EXIT_SUCCESS, (
        f"S-012: the trigger must succeed under lazy-report={report}; status={status}\n"
        f"terminal:\n{terminal}\nstderr:\n{err_log.read_text() if err_log.exists() else '<absent>'}"
    )
    assert pkg.marker in out_log.read_text(), (
        f"S-012: the tool's own output must go to its redirected stdout, not the terminal; "
        f"marker={pkg.marker!r}"
    )
    assert _is_materialized(ocx, project, pkg), "S-012: the trigger must have materialized the package"

    rendered = pkg.repo in terminal
    if renders:
        assert rendered, (
            f"S-012: lazy-report={report} must render the materialization on the controlling "
            f"terminal even though stderr is redirected; the pty saw:\n{terminal!r}"
        )
    else:
        assert not rendered, (
            f"S-012: lazy-report={report} must open no channel at all; the pty saw:\n{terminal!r}"
        )
    assert pkg.marker not in terminal, (
        f"S-012: the tool's own stdout was redirected, so it must not reach the terminal; "
        f"got:\n{terminal!r}"
    )


# ===========================================================================
# State sequences — compose / invoke / clean / recompose orders
# ===========================================================================


def test_sequence_1_compose_clean_compose_keeps_one_shim_tree_throughout(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 1: compose lazily, `ocx clean`, compose again.

    The filesystem view of S-008 (which asserts the *output* is unchanged):
    the same shim tree is present at every step and is never republished under
    a second path.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS
    first = assert_shim_dir_exists(ocx, pkg.repo, "sequence 1: after the first compose")

    assert _run(ocx, project, "--format", "json", "clean").returncode == EXIT_SUCCESS
    survived = assert_shim_dir_exists(ocx, pkg.repo, "sequence 1: after clean")

    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS
    third = assert_shim_dir_exists(ocx, pkg.repo, "sequence 1: after the second compose")

    assert first == survived == third, (
        f"sequence 1: the shim tree is identity-keyed by the pinned digest, so all "
        f"three steps must name one path; got {first}, {survived}, {third}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_sequence_2_clean_after_materialization_keeps_both_the_package_and_the_shim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 2: compose lazily, invoke, `ocx clean`, compose again.

    Materialization does not retire the shim tree — `materialize_lazy` never
    touches the shim store — so after the trigger *both* directories exist and
    the lock pins both. A `clean` that collected either would break the next
    compose.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    export = _run(ocx, project, "env", "--shell=sh")
    before = _run(ocx, project, "--format", "json", "env")

    triggered = run_after_sourcing(export.stdout, "hello", cwd=project, env=_shell_env(ocx))
    assert triggered.returncode == EXIT_SUCCESS, f"trigger failed:\n{triggered.stderr}"
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "sequence 2: the shim survives materialization")
    assert _is_materialized(ocx, project, pkg), "sequence 2: the trigger materialized the package"

    assert _run(ocx, project, "--format", "json", "clean").returncode == EXIT_SUCCESS

    assert assert_shim_dir_exists(ocx, pkg.repo, "sequence 2: shim after clean") == shim_bin, (
        "sequence 2: `ocx clean` must keep the lock-pinned shim"
    )
    assert _is_materialized(ocx, project, pkg), (
        "sequence 2: `ocx clean` must keep the lock-pinned package"
    )
    after = _run(ocx, project, "--format", "json", "env")
    assert after.stdout == before.stdout, (
        f"sequence 2: compose is unchanged across materialize+clean\n"
        f"before:\n{before.stdout}\nafter:\n{after.stdout}"
    )


def test_sequence_3_removing_the_tool_lets_clean_collect_its_shim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 3: compose lazily, `ocx remove` the tool, `ocx clean`.

    `ocx remove` rewrites `ocx.toml` and `ocx.lock` in one step, so the pin
    that rooted the shim is gone and an ordinary `clean` — no `--force` —
    collects it. This is the only sequence in which a shim is collected while
    the project registry is still honoured.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS
    assert_shim_dir_exists(ocx, pkg.repo, "sequence 3: composed before the removal")

    removed = _run(ocx, project, "remove", "hello")
    assert removed.returncode == EXIT_SUCCESS, f"ocx remove failed:\n{removed.stderr}"
    assert "[tools]" in (project / "ocx.toml").read_text(), "ocx remove must leave a valid ocx.toml"

    assert _run(ocx, project, "--format", "json", "clean").returncode == EXIT_SUCCESS

    assert_shim_dir_absent(ocx, pkg.repo, "sequence 3: nothing pins the shim once the tool is removed")


def test_sequence_4_force_clean_collects_the_shim_and_the_next_compose_regenerates_it(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 4: compose lazily, `ocx clean --force`, compose again.

    `--force` drops the project registry, so the lock pin no longer roots the
    shim and it is collected. The next compose republishes it at the same path
    and emits byte-identical output — the regeneration clause of C-013.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    before = _run(ocx, project, "--format", "json", "env")
    assert before.returncode == EXIT_SUCCESS, f"compose failed:\n{before.stderr}"
    original = assert_shim_dir_exists(ocx, pkg.repo, "sequence 4: composed before the force clean")

    assert _run(ocx, project, "--format", "json", "clean", "--force").returncode == EXIT_SUCCESS
    assert_shim_dir_absent(ocx, pkg.repo, "sequence 4: --force ignores the lock pin")

    after = _run(ocx, project, "--format", "json", "env")
    assert after.returncode == EXIT_SUCCESS, f"recompose failed:\n{after.stderr}"
    assert assert_shim_dir_exists(ocx, pkg.repo, "sequence 4: regenerated") == original, (
        "sequence 4: the regenerated shim must land at the same identity-keyed path"
    )
    assert after.stdout == before.stdout, (
        f"sequence 4: regeneration must be invisible in the output\n"
        f"before:\n{before.stdout}\nafter:\n{after.stdout}"
    )


def test_sequence_5_clean_collects_neither_an_eager_package_nor_a_lazy_shim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 5: one eager tool and one lazy tool in one project, then `ocx clean`.

    Both are pinned by the same lock and both must survive. This assertion was
    unfalsifiable before the GC learned about the shim store — "the shim was
    not collected" was true because nothing could collect it — so the guard
    that makes it meaningful is sequence 4 above, which shows the same shim
    *is* collectable when its root goes away.
    """
    lazy = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["lz"], binaries=["lz"], env=PUBLIC_BIN_PATH
    )
    eager = make_package(
        ocx, f"{unique_repo}_e", "1.0.0", tmp_path, bins=["eg"], binaries=["eg"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(
        ocx,
        tmp_path,
        f'[tools]\nlz = "{lazy.fq}"\neg = "{eager.fq}"\n\n'
        f'[package."{eager.fq}"]\nlazy-mode = "never"\n\n'
        f'[package."{lazy.fq}"]\nlazy-mode = "always"\n',
    )

    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS
    shim_bin = assert_shim_dir_exists(ocx, lazy.repo, "sequence 5: the lazy tool defers")
    assert_shim_dir_absent(ocx, eager.repo, "sequence 5: the eager tool publishes no shim")
    assert _is_materialized(ocx, project, eager), "sequence 5: the eager tool materialized"
    assert not _is_materialized(ocx, project, lazy), "sequence 5: the lazy tool did not"

    assert _run(ocx, project, "--format", "json", "clean").returncode == EXIT_SUCCESS

    assert assert_shim_dir_exists(ocx, lazy.repo, "sequence 5: shim after clean") == shim_bin, (
        "sequence 5: the lock pins the deferred tool, so its shim survives"
    )
    assert _is_materialized(ocx, project, eager), (
        "sequence 5: the lock pins the eager tool, so its package survives"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_sequence_6_force_clean_after_materialization_returns_the_tool_to_shim_only(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 6: compose lazily, invoke, `ocx clean --force`, compose again.

    The real "back to deferred" transition. `ocx package uninstall` is not it:
    a shim trigger materializes through `find_or_install_all`, which creates no
    candidate symlink, so `uninstall` has nothing to remove. `--force` drops
    the project roots, which collects *both* directories at once, and the next
    compose brings back the shim tree and only the shim tree.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    export = _run(ocx, project, "env", "--shell=sh")
    triggered = run_after_sourcing(export.stdout, "hello", cwd=project, env=_shell_env(ocx))
    assert triggered.returncode == EXIT_SUCCESS, f"trigger failed:\n{triggered.stderr}"
    original = assert_shim_dir_exists(ocx, pkg.repo, "sequence 6: shim present after the trigger")
    assert _is_materialized(ocx, project, pkg), "sequence 6: package present after the trigger"

    assert _run(ocx, project, "--format", "json", "clean", "--force").returncode == EXIT_SUCCESS
    assert_shim_dir_absent(ocx, pkg.repo, "sequence 6: --force collects the shim")
    assert not _is_materialized(ocx, project, pkg), "sequence 6: --force collects the package too"

    assert _run(ocx, project, "--format", "json", "env").returncode == EXIT_SUCCESS

    assert assert_shim_dir_exists(ocx, pkg.repo, "sequence 6: recomposed") == original, (
        "sequence 6: the shim tree returns at its identity-keyed path"
    )
    assert not _is_materialized(ocx, project, pkg), (
        "sequence 6: recomposing a deferred tool must not re-materialize its content"
    )


def test_sequence_7_the_deferred_advisory_is_raised_once_and_names_the_deferred_tool(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 7: one lazy tool and one eager tool, both with advisory-raising metadata.

    C-015 (d) says advisories are classified for a **deferred** tool only. Two
    identically-shaped packages make that falsifiable: a classifier that also
    ran over eagerly-composed tools would report two advisories instead of one.

    The advisory kind here is `install-path-rooted-non-path-var`, which
    `make_package`'s default env raises on its own (`<REPO>_HOME` is a
    `constant` rooted at `${installPath}`). Any kind proves the "deferred only"
    clause; this one needs no fixture surgery to produce.
    """
    lazy = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["lz"], binaries=["lz"])
    eager = make_package(ocx, f"{unique_repo}_e", "1.0.0", tmp_path, bins=["eg"], binaries=["eg"])
    project = _lazy_project(
        ocx,
        tmp_path,
        f'[tools]\nlz = "{lazy.fq}"\neg = "{eager.fq}"\n\n'
        f'[package."{lazy.fq}"]\nlazy-mode = "always"\n',
    )

    payload = _env_json(ocx, project)

    advisories = payload["advisories"]
    assert len(advisories) == 1, (
        f"sequence 7: exactly one advisory — the deferred tool's; got "
        f"{json.dumps(advisories, indent=2)}"
    )
    assert lazy.repo in advisories[0]["package"], (
        f"sequence 7: the advisory must name the deferred package; got {advisories[0]}"
    )
    assert eager.repo not in advisories[0]["package"], (
        f"sequence 7: the eagerly-composed package raises none; got {advisories[0]}"
    )
    assert advisories[0]["kind"] == "install-path-rooted-non-path-var", (
        f"sequence 7: unexpected advisory kind; got {advisories[0]}"
    )


def test_c015_pull_serializes_the_advisories_it_warns_about(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-015: `ocx pull --format json` carries the advisories it raises.

    Both channels, asserted together. An advisory that only reaches stderr is
    unreadable to the tooling ocx is a backend for, and the identical advisory
    for the identical package is already serialized by `ocx env` — one wire
    surface carrying C-015's payload while its sibling drops it is the defect.

    The advisory kind is `install-path-rooted-non-path-var`, which
    `make_package`'s default env raises on its own (`<REPO>_HOME` is a
    `constant` rooted at `${installPath}`), so no fixture surgery is needed.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["lz"], binaries=["lz"])
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg, binding="lz"))

    result = _run(ocx, project, "--format", "json", "pull")

    assert result.returncode == EXIT_SUCCESS, (
        f"C-015: a deferred pull must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    advisories = payload["advisories"]
    assert len(advisories) == 1, (
        f"C-015: the deferred tool's advisory must reach --format json; got "
        f"{json.dumps(payload, indent=2)}"
    )
    assert advisories[0]["kind"] == "install-path-rooted-non-path-var", (
        f"C-015: unexpected advisory kind; got {advisories[0]}"
    )
    assert pkg.repo in advisories[0]["package"], (
        f"C-015: the advisory must name the deferred package; got {advisories[0]}"
    )
    assert advisories[0]["message"] in result.stderr, (
        f"C-015: the same advisory must also reach the human channel; stderr:\n{result.stderr}"
    )
    rows = {key: value for key, value in payload.items() if key != "advisories"}
    assert len(rows) == 1 and next(iter(rows.values()))["kind"] == "shim", (
        f"C-015: the reserved advisories key must not displace the pulled row; got {rows}"
    )


def test_sequence_8_offline_regenerates_a_collected_shim_when_the_metadata_is_local(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 8: a collected shim, recomposed offline.

    **The predicate is metadata availability, not offline-ness.** All three
    legs below run with `--offline`; what differs is whether the closure's
    config blobs are still in the blob store:

    1. metadata local -> the shim is regenerated, exit 0. `ocx package install`
       leaves a live candidate symlink, which `ocx clean --force` honours, so
       the blobs survive the sweep that collects the shim.
    2. metadata absent -> `ocx env` fails with exit 79 (NotFound). This is
       **deliberately not** the 81 (`PolicyBlocked`) that `--offline` returns
       elsewhere: the composer asked for something that is simply not in the
       store, and the offline policy is what stops it being fetched, not what
       refuses the request. Do not "fix" this to 81.
    3. metadata absent **and** `--no-pull` -> exit 0, warned and omitted
       (S-009's contract, which is where warn-and-omit actually lives).
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))
    online = _run(ocx, project, "--format", "json", "env")
    assert online.returncode == EXIT_SUCCESS, f"compose failed:\n{online.stderr}"
    original = assert_shim_dir_exists(ocx, pkg.repo, "sequence 8: composed online first")

    assert _run(ocx, project, "package", "install", pkg.short).returncode == EXIT_SUCCESS
    assert _run(ocx, project, "--format", "json", "clean", "--force").returncode == EXIT_SUCCESS
    assert_shim_dir_absent(ocx, pkg.repo, "sequence 8: --force collected the shim")

    # Leg 1 — metadata local (the install symlink kept the blobs reachable).
    regenerated = _run(ocx, project, "--offline", "--format", "json", "env")
    assert regenerated.returncode == EXIT_SUCCESS, (
        f"sequence 8: with metadata local, an offline compose must regenerate the shim; "
        f"rc={regenerated.returncode}\nstderr:\n{regenerated.stderr}"
    )
    assert assert_shim_dir_exists(ocx, pkg.repo, "sequence 8: regenerated offline") == original
    assert regenerated.stdout == online.stdout, (
        f"sequence 8: the offline recompose must be byte-identical to the online one\n"
        f"online:\n{online.stdout}\noffline:\n{regenerated.stdout}"
    )

    # Legs 2 and 3 — metadata absent: uninstall drops the symlink root, then a
    # forced sweep collects the blobs the offline compose would have read.
    assert _run(ocx, project, "package", "uninstall", "--purge", pkg.short).returncode == EXIT_SUCCESS
    assert _run(ocx, project, "--format", "json", "clean", "--force").returncode == EXIT_SUCCESS
    assert_shim_dir_absent(ocx, pkg.repo, "sequence 8: the shim is collected again")

    blocked = _run(ocx, project, "--offline", "--format", "json", "env")
    assert blocked.returncode == EXIT_NOT_FOUND, (
        f"sequence 8: with metadata absent an offline compose must exit "
        f"{EXIT_NOT_FOUND}; rc={blocked.returncode}\nstderr:\n{blocked.stderr}"
    )

    omitted = _run(ocx, project, "--offline", "--format", "json", "env", "--no-pull")
    assert omitted.returncode == EXIT_SUCCESS, (
        f"sequence 8: --no-pull turns the same absence into an omission, not a failure; "
        f"rc={omitted.returncode}\nstderr:\n{omitted.stderr}"
    )
    assert json.loads(omitted.stdout)["entries"] == [], (
        f"sequence 8: the omitted tool contributes no entries; got {omitted.stdout}"
    )
    assert "hello not installed" in omitted.stderr, (
        f"sequence 8: the omission must be named on stderr; got:\n{omitted.stderr}"
    )
    assert_shim_dir_absent(ocx, pkg.repo, "sequence 8: --no-pull publishes no shim")


def test_sequence_9_recomposing_a_published_shim_stages_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Sequence 9: a warm compose reuses the published tree instead of rebuilding it.

    `$OCX_HOME/temp` is replaced by a **file** between the two composes, which
    is the one thing that makes the staging block unrunnable: `prepare_lazy`
    stages into a fresh temp directory under that root and `create_dir_all`
    fails against a file. Nothing else on a warm metadata-only compose touches
    it — blob writes stage beside their own CAS entry, not here — so a second
    compose that still exits 0 with byte-identical output can only have skipped
    staging entirely.

    Reds without the hoisted completeness probe: the old order staged the whole
    tree, wrote the digest file, linked every closure config blob and wrote one
    launcher per claimed name before `publish_shim_dir` discovered the
    destination already existed and deleted all of it — hundreds of wasted
    filesystem operations per deferred tool on every `ocx env`, and therefore
    on every direnv reload.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _lazy_project(ocx, tmp_path, _toolchain_lazy(pkg))

    first = _run(ocx, project, "--format", "json", "env")
    assert first.returncode == EXIT_SUCCESS, f"first compose failed:\n{first.stderr}"
    assert_shim_dir_exists(ocx, pkg.repo, "sequence 9: the first compose publishes the tree")

    temp_root = ocx.ocx_home / "temp"
    if temp_root.exists():
        shutil.rmtree(temp_root)
    temp_root.write_text("staging is impossible while this is a file\n")

    second = _run(ocx, project, "--format", "json", "env")

    assert second.returncode == EXIT_SUCCESS, (
        f"sequence 9: recomposing a published shim must not stage anything; "
        f"rc={second.returncode}\nstderr:\n{second.stderr}"
    )
    assert second.stdout == first.stdout, (
        f"sequence 9: the reused tree must compose identically\n"
        f"first:\n{first.stdout}\nsecond:\n{second.stdout}"
    )
    assert temp_root.is_file(), (
        "sequence 9: the sabotage must still be in place, or the assertion above proves nothing"
    )
