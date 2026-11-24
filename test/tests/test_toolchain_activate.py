# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for what `activate` puts on a live shell's ``PATH``.

Owned by WP-12b of the toolchain-activation plan. Everything here drives a
**real ``bash`` process** through ``src.shell_matrix`` and reads ``PATH`` back
out of it, because the contracts under test are about the string a shell ends
up holding, not about the vector the reconciler built:
``SessionPath::entries`` is documented as ordering the three session
directories *back to front*, so a test that read the vector would pin the
reverse of the contract and stay green through a swap.

Three habits are load-bearing in every case below, and each of them exists
because this branch produced the failure it prevents.

Identity, never spelling
------------------------
Every directory an assertion names is obtained **from the binary** —
:func:`resolved_toolchain_home` for a project's home, ``ocx --format json env``
for a composed package directory — and compared through
:func:`os.path.realpath`. A literal ``.ocx/toolchain`` join would assert the
default *spelling* of the home while the sentence around it names the home's
*identity*, and would red the moment ``toolchain_dir`` relocates it or a link
is repointed. :func:`locked_project` mints three disjoint spellings per tool
(repository, ``[tools]`` key, binary name) so an assertion about one of them
can never be satisfied by another.

A negative always carries a positive
------------------------------------
``matrix.probes()`` returns ``{}`` for a shell that never ran, so a bare
``X not in segments`` passes on a dead fixture just as well as on a correct
one. :func:`_segments` therefore fails when its probe is missing, and every
"absent" assertion sits beside an "and this one *is* here" assertion taken
from the same probe.

The stream is not the value
---------------------------
The emitted hook is shell source that the session ``eval``s, so its own
command text appears on stdout and a substring search over it can be satisfied
by the fixture's own bytes. Values are read through ``matrix.probe()``; raw
stream matching is reserved for the one diagnostic that is genuinely a
sentence — the C-061 hint, which the shell prints on **stderr**.

Bounded, and never through ``exec``
-----------------------------------
Every :func:`matrix.run_script` call passes an explicit ``timeout=``. A project
that renders a trampoline named ``ocx`` (:func:`test_a_bare_ocx_lookup_...`)
would re-exec itself inside ``/bin/sh``, so that case resolves the name with
``command -v`` and never runs it: its regression mode is a hang, not an exit
code, and only the timeout turns that back into a failure (RUL-110).
"""

from __future__ import annotations

import dataclasses
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src import shell_matrix as matrix
from src.exec_state import default_state, encode
from src.helpers import bash_executable, make_package, posix_shell_path, write_ocx_toml
from src.runner import OcxRunner
from src.shell_eval import git_bash_env
from src.toolchain_fixtures import (
    DEFAULT_GROUP,
    EXIT_SUCCESS,
    assert_key_and_binary_namespaces_stay_disjoint,
    bin_entries,
    copy_tree,
    entry_link,
    git,
    link_entries,
    locked_project,
    make_link,
    read_render_stamp,
    remove_link,
    resolved_toolchain_bin,
    resolved_toolchain_home,
    run_in,
    shell_bin,
    snapshot_tree,
    trampoline_body,
    trampoline_name,
    two_branch_checkout,
    write_toolchain_dir_config,
)

pytestmark = pytest.mark.command("toolchain_env")

# One arm. The nine-shell matrix is WP-12e's subject; what is under test here
# is the reconciler's answer, and running it through nine interpreters would
# multiply the runtime by nine to re-observe one PATH string.
SHELL = "bash"

# On Windows the session shell is Git Bash: its child gets a native `;` PATH, bash holds
# `:`-joined MSYS spellings, and every PATH probe is converted back with Git's own `cygpath -w -p`
# before Python compares directories through `matrix.path_identity`.
_WINDOWS = sys.platform == "win32"

# Printed by the Windows preamble when Git Bash's own tools are missing; `_session` fails on it.
_HARNESS_FAULT = "ocx-harness:"
_WINDOWS_PREAMBLE = (
    "command -v cygpath >/dev/null 2>&1 || "
    f"{{ printf '{_HARNESS_FAULT} cygpath is not on the session PATH (%s)\\n' \"$PATH\" >&2; exit 97; }}"
)

# Every shell session is bounded (RUL-110). The regression this guards is a
# trampoline re-entry loop, whose symptom is a process that never returns.
_TIMEOUT = 120

# The C-061 sentence, verbatim from `activation.rs::project_contribution`. Read
# off **stderr**: the reconciler emits it as shell source on stdout, and the
# session prints it when it evaluates that source.
_HINT = "its toolchain has not been rendered for this lock; run `ocx pull` here"

# Where `ocx self setup` puts the installed binary, and therefore what
# `SessionPath::install_bin` names. Same constant `test_shell_reconcile.py`
# seeds; `matrix.SESSION_BIN_DIRS[0]` is its directory half.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"


# ---------------------------------------------------------------------------
# The arena
# ---------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Arena:
    """One test's shell ``$HOME``, script directory and installed-ocx seed.

    Declared here rather than imported: ``test_shell_reconcile.py`` defines its
    own for the same reason (DAMP), and hoisting one into
    ``src/shell_matrix.py`` would put a project import into a module that is
    bind-mounted into the shell-zoo container without ``test/src/`` beside it.
    """

    home: Path
    scripts: Path
    ocx_home: Path
    binary: Path

    def env(self, shell_abs: str, **extra: str) -> dict[str, str]:
        """A clean child environment holding both session ``PATH`` directories.

        ``clean_env`` splices them in for the reason its own docstring gives: a
        shell born without them makes every prompt a PATH-changing prompt, and
        "the reconciler kept them" then cannot be told from "the fixture never
        had them".
        """
        return matrix.clean_env(self.home, shell_abs, ocx_home=self.ocx_home, **extra)


@pytest.fixture
def arena(ocx: OcxRunner, tmp_path: Path) -> Arena:
    """A shell arena sharing the runner's ``$OCX_HOME``.

    Sharing it is the point: the project is built with the registry-backed
    ``ocx`` fixture and observed from a registry-free shell, and the render
    stamp, the consent stamp and the two session directories all hang off
    ``$OCX_HOME``. The installed-ocx candidate is seeded so
    ``SessionPath::install_bin`` names a directory that actually holds an
    ``ocx``, which is what makes the ``command -v`` row discriminating.
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    # The binary's own name: `ocx.exe` on Windows, which is what `ocx self setup` installs and bash finds as `ocx`.
    candidate = ocx_home / _CANDIDATE_REL.parent / ocx.binary.name
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ocx.binary, candidate)
    candidate.chmod(0o755)

    home = tmp_path / "shell-home"
    home.mkdir()
    scripts = tmp_path / "shell-scripts"
    scripts.mkdir()
    return Arena(home=home, scripts=scripts, ocx_home=ocx_home, binary=ocx.binary)


def _bash() -> str:
    """Absolute ``bash``, or skip naming what is missing.

    A skip on a host that ships the interpreter is a pass with no evidence, so
    ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` turns it back into a failure wherever
    the tool is expected to exist. On Windows it is Git for Windows' bash,
    never the WSL launcher, and its absence is a failure.
    """
    if _WINDOWS:
        found = bash_executable()
        assert found is not None, (
            "Git for Windows' bash is required on Windows, and bash_executable() found none "
            "(Program Files\\Git, then PATH without System32)"
        )
        return found
    resolved = matrix.which_arm(SHELL)
    if resolved is None:
        assert not matrix.missing_arm_is_fatal(SHELL), (
            "bash is not installed, so this test asserted nothing — and "
            "__OCX_TESTING_REQUIRE_LIVE_SHELLS names it as an arm that must be live here"
        )
        pytest.skip("bash is not installed on this host (shutil.which returned None)")
    return resolved


def _windows_base(bash: str) -> list[str]:
    """The ambient native ``PATH`` a Windows session starts from: Git's ``usr/bin``, then System32.

    ``usr/bin`` carries ``cygpath``, the coreutils and ``msys-2.0.dll``, by which the binary
    detects the MSYS runtime. Git's ``bin`` stays out: MSYS mounts ``/bin`` onto ``usr/bin``, so
    ``cygpath -w`` would hand it back as a different directory.
    """
    system32 = str(Path(dict(os.environ).get("SYSTEMROOT", r"C:\Windows")) / "System32")
    base = git_bash_env({"PATH": system32}, bash)["PATH"].split(os.pathsep)
    assert any((Path(directory) / "cygpath.exe").is_file() for directory in base), (
        f"Git Bash found at {bash!r}, but no usr/bin holding cygpath.exe beside it, so no session "
        f"PATH can be converted back to native spellings; base={base}"
    )
    return base


def _windows_env(
    arena: Arena, bash: str, extra_env: dict[str, str], path: str | None, session_pair: bool
) -> dict[str, str]:
    """``clean_env``'s Windows counterpart: a native ``;`` PATH of ``path``, the session pair, then the base.

    ``USERPROFILE`` follows ``HOME`` because it is the home directory a Windows ``ocx`` reads.
    """
    ambient = dict(os.environ)
    env = {key: ambient[key] for key in ("SYSTEMROOT", "TEMP", "TMP", "PATHEXT") if key in ambient}
    env.update(HOME=str(arena.home), USERPROFILE=str(arena.home), OCX_HOME=str(arena.ocx_home))
    env.update(extra_env)
    planted = [path] if path is not None else []
    session = matrix.session_path_dirs(arena.ocx_home) if session_pair else []
    env["PATH"] = ";".join([*planted, *session, *_windows_base(bash)])
    return env


# ---------------------------------------------------------------------------
# Running a session, and reading PATH back out of it
# ---------------------------------------------------------------------------


def _session(
    arena: Arena,
    *steps: str,
    cwd: Path,
    extra_env: dict[str, str] | None = None,
    path: str | None = None,
    session_pair: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run one bash session built from ``steps``, bounded by ``_TIMEOUT``.

    ``path`` plants a ``PATH`` prefix before ``clean_env`` splices the session
    pair in, which is how a foreign segment gets somewhere the repair could
    delete it from.

    ``session_pair=False`` takes the two session directories back out
    afterwards. ``clean_env`` puts them in unconditionally and is right to —
    every shell in the field is born holding them — but exactly one row needs a
    shell that does not, because "the reconciler contributed them" and "the
    fixture already had them" are otherwise the same observation.
    """
    shell_abs = _bash()
    if _WINDOWS:
        env = _windows_env(arena, shell_abs, dict(extra_env or {}), path, session_pair)
        steps = (_WINDOWS_PREAMBLE, *steps)
    else:
        overrides = dict(extra_env or {})
        if path is not None:
            overrides["PATH"] = path + os.pathsep + matrix.BASE_PATH
        env = arena.env(shell_abs, **overrides)
        if not session_pair:
            registered = set(matrix.session_path_dirs(arena.ocx_home))
            env["PATH"] = os.pathsep.join(
                segment for segment in env["PATH"].split(os.pathsep) if segment not in registered
            )
    body = "\n".join([matrix.header(SHELL, arena.binary), *steps])
    result = matrix.run_script(
        SHELL,
        shell_abs,
        body,
        cwd=cwd,
        env=env,
        script_dir=arena.scripts,
        name=f"session-{uuid4().hex[:8]}",
        timeout=_TIMEOUT,
    )
    assert _HARNESS_FAULT not in result.stderr, (
        f"the Git Bash at {shell_abs!r} could not run the harness preamble (child PATH {env['PATH']!r}):\n"
        f"{result.stderr}"
    )
    return result


def _cd(path: Path | str) -> str:
    """``cd`` into ``path``, spelled the way bash holds it on this host (``/c/x`` under Git Bash)."""
    return matrix.cd_to(SHELL, posix_shell_path(path))


def _path_probe(label: str) -> str:
    """Probe ``PATH`` under ``label``, in native spellings.

    Under Git Bash the raw value is probed as ``<label>_raw`` and ``label`` carries Git's own
    ``cygpath -w -p`` conversion of it, so no Python mirror of the MSYS mount table decides a match.
    """
    if not _WINDOWS:
        return matrix.probe(SHELL, label, "PATH")
    return "\n".join(
        [
            matrix.probe(SHELL, f"{label}_raw", "PATH"),
            '__ocx_native="$(cygpath -w -p "$PATH")"',
            matrix.probe(SHELL, label, "__ocx_native"),
        ]
    )


def _lookup_probe(name: str) -> str:
    """Resolve ``name`` with ``command -v`` (never run it) and probe it as ``lookup``, native on Windows."""
    steps = [f'__ocx_lookup="$(command -v {name})"']
    if _WINDOWS:
        steps.append('[ -n "$__ocx_lookup" ] && __ocx_lookup="$(cygpath -w "$__ocx_lookup")"')
    steps.append(matrix.probe(SHELL, "lookup", "__ocx_lookup"))
    return "\n".join(steps)


def _lookup(result: subprocess.CompletedProcess[str], name: str) -> str:
    """The file ``command -v name`` named, with the ``.exe`` the MSYS runtime leaves off restored."""
    lookup = matrix.probes(result.stdout).get("lookup")
    assert lookup and lookup != matrix.ABSENT, f"`command -v {name}` resolved nothing:\n{result.stdout}"
    return matrix.executable_identity(lookup)


class _Segments(list[str]):
    """Native ``PATH`` identities that print the raw bash ``PATH`` beside them, so a failure shows both spellings."""

    def __init__(self, segments: list[str], raw: str) -> None:
        super().__init__(segments)
        self.raw = raw

    def __repr__(self) -> str:
        return f"{list.__repr__(self)} (bash PATH: {self.raw!r})"

    __str__ = __repr__


class _WindowsIdentity(str):
    """A Windows identity key whose repr adds its MSYS spelling, so a failed comparison names both."""

    def __repr__(self) -> str:
        return f"{str.__repr__(self)} (bash: {posix_shell_path(str(self))!r})"


def _segments(result: subprocess.CompletedProcess[str], label: str = "path") -> list[str]:
    """The session's ``PATH``, realpath'd, or a failure naming the dead shell.

    The assertion is the non-vacuity half: ``matrix.probes()`` answers ``{}``
    for a shell that never ran, so without it every "X is absent" row below
    would pass against a fixture that produced nothing at all.
    """
    found = matrix.probes(result.stdout)
    assert label in found, (
        f"the shell produced no {label!r} probe, so nothing below was observed "
        f"(rc={result.returncode})\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert found[label] != matrix.ABSENT, f"PATH was unset in the session (probe {label!r})"
    segments = [_real(segment) for segment in matrix.native_path_list(found[label])]
    if not _WINDOWS:
        return segments
    raw = found.get(f"{label}_raw", "")
    fault = matrix.posix_path_list_fault(raw)
    assert fault is None, (
        f"session PATH separator: Git Bash must hold PATH as ':'-joined POSIX spellings, but {fault}: {raw!r}"
    )
    return _Segments(segments, raw)


def _real(path: Path | str) -> str:
    """The identity of a directory, not one of its spellings."""
    resolved = os.path.realpath(path)
    return _WindowsIdentity(matrix.path_identity(resolved)) if _WINDOWS else resolved


def _hint_dirs(stderr: str) -> list[str]:
    """The project directory of every stale-window hint line on ``stderr``, as the binary spelled it."""
    prefix, suffix = "ocx: ", f": {_HINT}"
    return [
        line[len(prefix) : -len(suffix)]
        for line in stderr.splitlines()
        if line.startswith(prefix) and line.endswith(suffix)
    ]


def _session_dirs(ocx: OcxRunner, arena: Arena, project_free: Path) -> tuple[str, str]:
    """``(install_bin, global_bin)``, realpath'd, both pinned against the binary.

    ``matrix.SESSION_BIN_DIRS`` is WP-12e's mirror of
    ``setup::session_path_directories``; it is what the fixture's ``PATH``
    carries, so it has to be what an assertion names. Both halves are checked
    against something the binary produced rather than trusted: the global
    directory against the ``toolchain_bin`` ``ocx shell state`` reports with no project in
    scope, and the install directory against the candidate this arena seeded.
    A rename on either side then reds here, once, instead of silently making
    every ordering row below compare two spellings of nothing.
    """
    global_bin, install_bin = matrix.session_path_dirs(arena.ocx_home)
    reported_global = resolved_toolchain_bin(ocx, project_free)
    assert _real(global_bin) == _real(reported_global), (
        "the global session directory the fixture puts on PATH must be the one "
        f"`ocx shell state` reports: {global_bin!r} vs {reported_global!r}"
    )
    assert (Path(install_bin) / arena.binary.name).is_file(), (
        f"the arena must seed an installed ocx in {install_bin!r}, or `command -v ocx` proves nothing"
    )
    return _real(install_bin), _real(global_bin)


def _composed_path_dirs(ocx: OcxRunner, cwd: Path, *args: str) -> list[str]:
    """The ``PATH`` directories ``ocx env`` composes for ``cwd``, **as spelled**.

    Taken from the structured report rather than from the emitted shell source:
    the two agree by construction (one composer), and the report cannot be
    satisfied by the command text a stream match would also see.

    Returned unresolved, and that is the whole point of returning them
    separately from :func:`_real`. ``<home>/links/<group>/<entry>`` *is a symlink into
    the package store*, so the following lane and the pinned lane realpath to
    the **same** directory — realpathing here would erase the only difference
    between them and make a lane assertion pass in both states. Which lane a
    composition took is a question about the spelling; whether it reached
    ``PATH`` is a question about the identity, and the two are asked with
    different operators throughout this module.
    """
    result = run_in(ocx, cwd, "--format", "json", "env", *args)
    assert result.returncode == EXIT_SUCCESS, f"`ocx env` failed:\n{result.stderr}"
    payload = json.loads(result.stdout)
    return [entry["value"] for entry in payload["items"] if entry["key"] == "PATH"]


def _write_home_config(arena: Arena, body: str) -> Path:
    """Write ``$OCX_HOME/config.toml`` — the tier a ``paths`` grant lives in."""
    path = arena.ocx_home / "config.toml"
    path.write_text(body, encoding="utf-8")
    return path


def _clone(project: Path, destination: Path) -> Path:
    """A byte copy of a rendered project at a new path.

    The consent stamp and the render stamp are both keyed on the *canonical
    project directory*, so the copy inherits the committed tree and neither
    stamp — which is exactly the hostile-clone shape S-003 names.
    """
    if _WINDOWS:
        # `shutil.copytree` walks into a junction; `copy_tree` recreates each one, aimed inside the copy.
        copy_tree(project, destination)
    else:
        shutil.copytree(project, destination, symlinks=True)
    return destination


# ---------------------------------------------------------------------------
# A — the three session directories on a live PATH
# ---------------------------------------------------------------------------


def test_bin_mode_path_leads_project_then_global_then_install(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-060: project ▸ global ▸ install, read off a real shell's ``PATH``.

    The most specific tier that pinned a name answers for it, ``ocx`` included:
    the installed binary is the floor, never a lid over a toolchain that pins
    one (D-4).

    Pinned on the *emitted* order, never on ``SessionPath``'s field order —
    the struct's own documentation says the vector is the reverse of the PATH
    order, so a test written against the vector would pass through a swap.
    """
    project = locked_project(ocx, tmp_path, label="a1")
    assert_key_and_binary_namespaces_stay_disjoint(project)
    home = resolved_toolchain_home(ocx, project.directory)
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)
    project_bin = _real(shell_bin(home))

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)

    for name, directory in (("install", install_bin), ("project", project_bin), ("global", global_bin)):
        assert directory in segments, f"the {name} session directory must be on PATH: {directory!r} not in {segments}"
    assert segments.index(project_bin) < segments.index(global_bin) < segments.index(install_bin), (
        "C-060 orders the three session directories project ▸ global ▸ install on PATH; got "
        f"{[s for s in segments if s in {install_bin, project_bin, global_bin}]}"
    )


def test_a_bare_ocx_lookup_resolves_the_projects_ocx_pin(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-060's reason: a toolchain that pins ``ocx`` is one that can win.

    D-4 removed the ``ShimNameShadowsOcx`` refusal so a project may pin its own
    ``ocx``. An ordering that kept the installed binary in front made that pin
    render and stay permanently unreachable, which is the defect this row now
    guards against — the inverse of what it asserted before the tiers were
    re-ordered.

    ``command -v`` only, and the name is **never run** (RUL-110): a project
    trampoline called ``ocx`` re-execs itself inside ``/bin/sh``, so a
    regression here hangs rather than exiting, and the ``timeout=`` on the
    session is what turns that back into a failure.
    """
    label = uuid4().hex[:8]
    package = make_package(ocx, f"t_{label}_shadow", "1.0.0", tmp_path, cascade=False, bins=["ocx"])
    project = tmp_path / f"shadow-{label}"
    project.mkdir()
    write_ocx_toml(project, f'[tools]\nshadowkey{label} = "{package.fq}"\n')
    assert run_in(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, project, "pull", "-g", "all").returncode == EXIT_SUCCESS

    home = resolved_toolchain_home(ocx, project)
    assert trampoline_name("ocx") in bin_entries(home), (
        "the fixture must render a trampoline named `ocx`, or nothing is being shadowed; "
        f"got {bin_entries(home)}"
    )
    install_bin, _ = _session_dirs(ocx, arena, tmp_path)

    result = _session(
        arena,
        _cd(project),
        matrix.prompt(SHELL),
        # Lookup, not execution. `command -v` writes a path and returns.
        _lookup_probe("ocx"),
        _path_probe("path"),
        cwd=project,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) in segments, (
        "the project's trampoline directory must be on PATH, or the resolution this row "
        f"asserts never had a candidate: {segments}"
    )
    assert _real(install_bin) in segments, (
        "the install directory must also be on PATH, or `ocx` resolving the pin proves "
        f"nothing about precedence: {segments}"
    )
    lookup, pin = _lookup(result, "ocx"), shell_bin(home) / trampoline_name("ocx")
    assert _real(lookup) == _real(pin), (
        f"a bare `ocx` must resolve the project's pin, not the installed binary; got {_real(lookup)!r}, "
        f"want {_real(pin)!r}"
    )


def test_a_bare_ocx_lookup_resolves_the_global_ocx_pin(ocx: OcxRunner, arena: Arena, tmp_path: Path) -> None:
    """The owner-reported case: pin ``ocx`` globally and a bare ``ocx`` finds it.

    No project anywhere — this is a plain shell in a plain directory, which is
    where ``$OCX_HOME/toolchain/active/bin`` versus the install directory is the whole
    of the question. Before the tiers were re-ordered the pin rendered and was
    unreachable for the life of every shell.

    ``command -v`` only, never an execution (RUL-110): a trampoline named
    ``ocx`` re-enters ocx, so running it would hang rather than fail.
    """
    label = uuid4().hex[:8]
    package = make_package(ocx, f"t_{label}_gpin", "1.0.0", tmp_path, cascade=False, bins=["ocx"])
    (arena.ocx_home / "ocx.toml").write_text(f'[tools]\ngpin{label} = "{package.fq}"\n', encoding="utf-8")
    assert run_in(ocx, tmp_path, "--global", "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, tmp_path, "--global", "pull", "-g", "all").returncode == EXIT_SUCCESS

    project_free = tmp_path / f"plain-{label}"
    project_free.mkdir()
    global_home = resolved_toolchain_home(ocx, project_free)
    assert trampoline_name("ocx") in bin_entries(global_home), (
        f"the global tier must render a trampoline named `ocx`; got {bin_entries(global_home)}"
    )
    install_bin, global_bin = _session_dirs(ocx, arena, project_free)

    result = _session(
        arena,
        _cd(project_free),
        matrix.prompt(SHELL),
        _lookup_probe("ocx"),
        _path_probe("path"),
        cwd=project_free,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    for name, directory in (("install", install_bin), ("global", global_bin)):
        assert directory in segments, (
            f"the {name} session directory must be on PATH, or the precedence this row "
            f"asserts was never contested: {directory!r} not in {segments}"
        )
    assert segments.index(global_bin) < segments.index(install_bin), (
        f"the global toolchain bin must precede the install bin; got {segments}"
    )
    lookup, pin = _lookup(result, "ocx"), Path(global_bin) / trampoline_name("ocx")
    assert _real(lookup) == _real(pin), (
        f"a bare `ocx` must resolve the global toolchain's pin, not the installed binary; got {_real(lookup)!r}, "
        f"want {_real(pin)!r}"
    )


def test_a_tool_in_both_toolchains_resolves_to_the_projects_trampoline(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-018's tier order: the project's trampoline shadows the global one."""
    project = locked_project(ocx, tmp_path, label="a3")
    # The same package in the global tier, so both trees expose the same
    # binary name and only the PATH order can decide the lookup.
    (arena.ocx_home / "ocx.toml").write_text(
        f'[tools]\nglobalkeya3 = "{project.default_package.fq}"\n', encoding="utf-8"
    )
    assert run_in(ocx, tmp_path, "--global", "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, tmp_path, "--global", "pull", "-g", "all").returncode == EXIT_SUCCESS

    home = resolved_toolchain_home(ocx, project.directory)
    global_home = resolved_toolchain_home(ocx, tmp_path)
    binary = project.default_binary
    exposed = trampoline_name(binary)
    assert exposed in bin_entries(home) and exposed in bin_entries(global_home), (
        "both tiers must expose the same binary name, or the shadowing is not exercised: "
        f"project={bin_entries(home)} global={bin_entries(global_home)}"
    )

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _lookup_probe(binary),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(global_home)) in segments, (
        f"the global trampoline directory must be on PATH for this row to mean anything: {segments}"
    )
    lookup, winner = _lookup(result, binary), shell_bin(home) / exposed
    assert _real(lookup) == _real(winner), (
        f"the project's trampoline must win over the global tier's; got {_real(lookup)!r}, want {_real(winner)!r}"
    )


def test_env_mode_never_puts_the_project_trampoline_dir_on_path(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-005: ``shells/default/bin`` is ``bin`` mode's, and ``env`` mode composes instead.

    Both halves in one probe: the trampoline directory is **rendered on disk**
    and still absent from ``PATH``, while the composed package directory *is*
    there. Without the second half the row would pass on a prompt that emitted
    nothing at all.
    """
    project = locked_project(ocx, tmp_path, label="a4")
    home = resolved_toolchain_home(ocx, project.directory)
    assert bin_entries(home), f"the tree must be rendered for this row to be a negative: {home}"
    composed = _composed_path_dirs(ocx, project.directory)
    assert composed, "`ocx env` must compose at least one PATH directory for the positive half"

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "env"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) not in segments, (
        f"`env` mode must never put the trampoline directory on PATH: {segments}"
    )
    for directory in composed:
        assert _real(directory) in segments, (
            f"`env` mode must contribute the composed package directory {directory!r}: {segments}"
        )


@pytest.mark.parametrize("mode", ["env", "bin", "none"])
def test_a_fresh_shell_gains_both_session_directories_in_every_mode(
    ocx: OcxRunner, arena: Arena, tmp_path: Path, mode: str
) -> None:
    """C-059: the two session directories are contributed on **every** prompt.

    The shell starts without them, and that is the whole design of the row.
    ``clean_env`` normally splices them in — a shell in the field is born
    holding them, because ``ocx self setup`` registered them at session level —
    but a fixture that starts that way cannot tell "the reconciler contributed
    them" from "the fixture already had them". Worse, the mutation this row
    exists to catch (dropping the pair from the desired set) is **invisible**
    to it: with no project in scope and no session entries, ``desired`` is
    empty, ``repair_owned_segments`` derives no keys at all, and nothing is
    removed — so a shell that started with the pair still ends with it. The
    only observation that separates the two states is a shell that starts
    without them.

    B1's transition rows cover the other half — that a shell which *has* them
    keeps them across a mode change — and those do red on the same mutation,
    because a live project keeps ``desired`` non-empty.
    """
    project_free = tmp_path / "no-project"
    project_free.mkdir()
    install_bin, global_bin = _session_dirs(ocx, arena, project_free)
    result = _session(
        arena,
        _cd(project_free),
        _path_probe("before"),
        matrix.prompt(SHELL),
        _path_probe("after"),
        cwd=project_free,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": mode},
        session_pair=False,
    )
    before = _segments(result, "before")
    assert install_bin not in before and global_bin not in before, (
        f"the fixture must start without the session pair, or `{mode}` proves nothing: {before}"
    )
    after = _segments(result, "after")
    assert install_bin in after and global_bin in after, (
        f"one prompt must contribute both session directories in `{mode}` mode: {after}"
    )


# ---------------------------------------------------------------------------
# B — the six ordered activate transitions
# ---------------------------------------------------------------------------

_TRANSITIONS = [
    ("env", "bin"),
    ("env", "none"),
    ("bin", "env"),
    ("bin", "none"),
    ("none", "env"),
    ("none", "bin"),
]


@pytest.mark.parametrize(("first", "second"), _TRANSITIONS, ids=lambda value: value)
def test_every_ordered_activate_transition_keeps_both_session_directories(
    ocx: OcxRunner, arena: Arena, tmp_path: Path, first: str, second: str
) -> None:
    """One session, two prompts, three independent properties.

    The mode is moved with ``OCX_TOOLCHAIN_ACTIVATE`` **inside** the running
    shell, so both prompts share one process and one ledger — which is the only
    arrangement in which a transition exists at all. The project therefore
    carries no ``activate`` key: C-007 makes the file tier win, and a fixture
    that stated one would make the lever inert and every row below vacuous.

    Three things are asserted per prompt, and they fail independently:

    1. both session directories survive (C-059);
    2. the project's ``shells/default/bin`` is on ``PATH`` in ``bin`` mode and
       **gone** in the
       other two — it lives outside ``$OCX_HOME``, so only ``owned_home``
       authorises removing it, and a ``None`` there strands it for the shell's
       whole life (C-063, RUL-91);
    3. a planted foreign segment survives both prompts — ``owned_prefixes`` is
       a deletion authority over a live shell's ``PATH``, and widening it past
       ``$OCX_HOME`` plus this project's home deletes a stranger's directory.
    """
    project = locked_project(ocx, tmp_path, label="b1")
    assert "activate" not in (project.directory / "ocx.toml").read_text(), (
        "the fixture must state no `activate` key, or C-007 makes the file tier win "
        "and OCX_TOOLCHAIN_ACTIVATE is inert"
    )
    home = resolved_toolchain_home(ocx, project.directory)
    project_bin = _real(shell_bin(home))
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)
    foreign = tmp_path / "someone-elses-bin"
    foreign.mkdir()

    result = _session(
        arena,
        _cd(project.directory),
        matrix.set_var(SHELL, "OCX_TOOLCHAIN_ACTIVATE", first),
        matrix.prompt(SHELL),
        _path_probe("first"),
        matrix.set_var(SHELL, "OCX_TOOLCHAIN_ACTIVATE", second),
        matrix.prompt(SHELL),
        _path_probe("second"),
        cwd=project.directory,
        path=str(foreign),
    )

    for label, mode in (("first", first), ("second", second)):
        segments = _segments(result, label)
        assert install_bin in segments and global_bin in segments, (
            f"both session directories must survive the `{mode}` half of {first}→{second}: {segments}"
        )
        assert _real(foreign) in segments, (
            f"a foreign PATH segment must survive the `{mode}` half of {first}→{second}; "
            f"`owned_prefixes` may only name $OCX_HOME and this project's home: {segments}"
        )
        if mode == "bin":
            assert project_bin in segments, (
                f"`bin` mode must contribute the project's trampoline directory: {segments}"
            )
        else:
            assert project_bin not in segments, (
                f"`{mode}` mode must not leave the project's trampoline directory stranded on PATH "
                f"after {first}→{second}: {segments}"
            )


# ---------------------------------------------------------------------------
# C — the stale window
# ---------------------------------------------------------------------------


def _poison_trampoline(home: Path, name: str) -> bytes:
    """Append bytes to a rendered trampoline and return its new content.

    Appending rather than overwriting in place: ``BinEntryStamp``'s cheap half
    is ``(size, file id)``, and a same-size same-inode overwrite is a
    documented accepted residual (R-W4). Growing the file moves the size, so
    the gate reaches the content hash it is really about.
    """
    trampoline = shell_bin(home) / trampoline_name(name)
    poisoned = trampoline.read_bytes() + b"\n# not what the stamp recorded\n"
    trampoline.write_bytes(poisoned)
    return poisoned


def test_a_lock_change_without_pull_withholds_the_project_dir_and_names_ocx_pull(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-061: a tree that no longer matches its stamp is withheld, once.

    The hint is matched on **stderr** and as the whole sentence, naming this
    project's own directory — the emitted stdout carries the ``printf`` that
    produces it, so a substring search there would be satisfied by the
    fixture's own command text rather than by the shell having printed
    anything. ``count == 1``: A-21 defers diagnostics through the ledger's
    ``messages_fp`` precisely so a steady state does not repeat them.
    """
    project = locked_project(ocx, tmp_path, label="c1")
    home = resolved_toolchain_home(ocx, project.directory)
    _poison_trampoline(home, project.default_binary)
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) not in segments, (
        f"a tree that does not match its render stamp must be withheld from PATH: {segments}"
    )
    assert install_bin in segments and global_bin in segments, (
        f"withholding the project must not cost the two session directories: {segments}"
    )
    if _WINDOWS:
        # The binary spells the directory as it canonicalised it, so the match is on identity.
        hints = _hint_dirs(result.stderr)
        assert len(hints) == 1, f"the C-061 hint must be printed exactly once; stderr:\n{result.stderr}"
        assert _real(hints[0]) == _real(project.directory), (
            f"the C-061 hint must name this project: {_real(hints[0])!r} vs {_real(project.directory)!r}"
        )
    else:
        sentence = f"ocx: {project.directory}: {_HINT}"
        assert result.stderr.count(sentence) == 1, (
            f"the C-061 hint must be printed exactly once, naming this project; stderr:\n{result.stderr}"
        )


def test_the_prompt_path_never_prunes_the_stale_trampolines(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-064: the prompt path has no delete, and none may be added.

    A prompt that pruned would be a whole-directory delete inside an
    attacker-writable tree, running before every command the user types.
    Byte-identity over the whole home, not just over ``shells/default/bin``.
    """
    project = locked_project(ocx, tmp_path, label="c2")
    home = resolved_toolchain_home(ocx, project.directory)
    poisoned = _poison_trampoline(home, project.default_binary)
    before = snapshot_tree(home)

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    _segments(result)  # the prompt ran; without this the comparison is of two untouched trees

    assert (shell_bin(home) / trampoline_name(project.default_binary)).read_bytes() == poisoned, (
        "the prompt path must leave a stale trampoline exactly as it found it"
    )
    assert snapshot_tree(home) == before, "the prompt path must not write anywhere under the home"


def test_the_stale_window_closes_at_the_next_pull(ocx: OcxRunner, arena: Arena, tmp_path: Path) -> None:
    """C-061's positive half: ``ocx pull`` re-renders, re-stamps, and the hint stops."""
    project = locked_project(ocx, tmp_path, label="c3")
    home = resolved_toolchain_home(ocx, project.directory)
    poisoned = _poison_trampoline(home, project.default_binary)

    assert run_in(ocx, project.directory, "pull", "-g", "all").returncode == EXIT_SUCCESS
    assert (shell_bin(home) / trampoline_name(project.default_binary)).read_bytes() != poisoned, (
        "`ocx pull` must re-render the trampoline the prompt refused to touch"
    )

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) in segments, f"a freshly pulled tree must reach PATH again: {segments}"
    assert _HINT not in result.stderr, f"the stale-window hint must be gone after a pull:\n{result.stderr}"


# ---------------------------------------------------------------------------
# D — the ladders, and the deletion authority
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", ["env", "bin", "none"])
@pytest.mark.parametrize("pinned", [False, True])
def test_the_activate_pinned_matrix_emits_the_contracted_set(
    ocx: OcxRunner, arena: Arena, tmp_path: Path, mode: str, pinned: bool
) -> None:
    """Six cells, and the ``(bin, pinned=true)`` one is why this exists.

    ``pinned`` decides which *content* the composed lane names; it must not
    decide whether the ``bin`` lane works at all. A render under ``pinned``
    that stamped an empty ``link_fingerprint`` would leave the trampolines
    unhealable, and the cheapest way for that to look correct is for nobody to
    check that ``bin`` mode still emits after a pinned render — so the tree is
    re-rendered under each cell's ``pinned`` value rather than once up front.
    """
    project = locked_project(ocx, tmp_path, label="d1")
    write_ocx_toml(project.directory, project.config_body(pinned=pinned))
    assert run_in(ocx, project.directory, "pull", "-g", "all").returncode == EXIT_SUCCESS

    home = resolved_toolchain_home(ocx, project.directory)
    store = str(arena.ocx_home / "packages")
    composed = _composed_path_dirs(ocx, project.directory)

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": mode},
    )
    segments = _segments(result)
    project_bin = _real(shell_bin(home))

    if mode == "bin":
        assert project_bin in segments, (
            f"`bin` mode must emit the trampoline directory under pinned={pinned}: {segments}"
        )
        stamp = read_render_stamp(ocx, home)
        assert stamp and stamp["link_fingerprint"], (
            f"a render under pinned={pinned} must still stamp a link fingerprint, or the "
            f"trampolines it exposes can never be healed: {stamp}"
        )
    else:
        assert project_bin not in segments, f"only `bin` mode contributes the trampoline directory: {segments}"

    if mode == "env":
        assert composed, "`env` mode has nothing to assert without a composed directory"
        for directory in composed:
            assert _real(directory) in segments, f"`env` mode must contribute {directory!r}: {segments}"
        # Lane on the spelling, membership on the identity — see
        # `_composed_path_dirs`: both lanes realpath to the same store directory.
        under_store = [directory for directory in composed if directory.startswith(store + os.sep)]
        under_home = [directory for directory in composed if directory.startswith(str(home) + os.sep)]
        if pinned:
            assert under_store and not under_home, (
                f"the pinned lane names digest paths in the package store: {composed}"
            )
        else:
            assert under_home and not under_store, (
                f"the following lane names paths under the toolchain home: {composed}"
            )
    else:
        for directory in composed:
            assert _real(directory) not in segments, (
                f"`{mode}` mode must compose nothing onto PATH, but {directory!r} is there: {segments}"
            )


def test_activate_in_ocx_toml_beats_the_environment_variable(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-006: the environment tier is the weakest, below the file tier."""
    project = locked_project(ocx, tmp_path, label="d2")
    write_ocx_toml(project.directory, 'activate = "none"\n\n' + project.config_body())
    home = resolved_toolchain_home(ocx, project.directory)
    composed = _composed_path_dirs(ocx, project.directory)
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert install_bin in segments and global_bin in segments, (
        f"`none` still carries the two session directories (C-059): {segments}"
    )
    assert _real(shell_bin(home)) not in segments, (
        f"`activate = \"none\"` in ocx.toml must beat OCX_TOOLCHAIN_ACTIVATE=bin: {segments}"
    )
    for directory in composed:
        assert _real(directory) not in segments, f"`none` composes nothing either: {segments}"


def test_the_environment_tier_speaks_when_the_file_tier_is_absent(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-006's other half: with no file tier the environment decides."""
    project = locked_project(ocx, tmp_path, label="d3")
    assert "activate" not in (project.directory / "ocx.toml").read_text(), (
        "this row needs the file tier genuinely absent"
    )
    home = resolved_toolchain_home(ocx, project.directory)

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) in segments, (
        f"with no `activate` key, OCX_TOOLCHAIN_ACTIVATE=bin must reach the tree: {segments}"
    )


def test_an_unrecognised_activate_value_warns_once_and_leaves_the_floor(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-006: an unreadable value is reported, not silently accepted.

    Only the warning is asserted. Per RUL-108 the *mode* half is
    acceptance-unobservable: the environment tier sits directly on the floor,
    so "unrecognised = absent" and "unrecognised = short-circuit to the floor"
    both resolve to ``Env`` in every tier arrangement, and a case asserting the
    outcome would be a green that cannot red. It stays pinned at the unit
    layer.

    ``ocx shell state`` is the emitter, and deliberately: it resolves the
    ladder exactly once for the scope it reports, so "exactly one" is a
    property of the contract. The per-prompt path resolves it twice — once per
    tier — and counting there would pin an implementation detail.
    """
    project = locked_project(ocx, tmp_path, label="d4")
    result = run_in(
        ocx,
        project.directory,
        "--format",
        "json",
        "shell",
        "state",
        env_extra={"OCX_TOOLCHAIN_ACTIVATE": "not-a-mode"},
    )
    assert result.returncode == EXIT_SUCCESS, f"an unreadable value must not fail the command:\n{result.stderr}"
    warnings = [line for line in result.stderr.splitlines() if "OCX_TOOLCHAIN_ACTIVATE" in line]
    assert len(warnings) == 1, f"exactly one warning per resolution; got {warnings}"
    assert "not-a-mode" in warnings[0], f"the warning must name the value it refused: {warnings[0]!r}"


def test_pinned_cli_beats_the_file_which_beats_the_environment(ocx: OcxRunner, tmp_path: Path) -> None:
    """The ``pinned`` ladder, one rung at a time: ``cli ▸ file ▸ environment``.

    Read off the composed directories rather than off a mode name: "pinned"
    *means* the digest path in the package store and "following" means the path
    under the toolchain home, and the two realpath to the same content — so
    only the raw spelling can tell the lanes apart, which is exactly what this
    row is for.
    """
    project = locked_project(ocx, tmp_path, label="d5")
    # Raw spellings on both sides: `<home>/<group>/<entry>` is a symlink into the
    # store, so realpathing either operand would collapse the two lanes into one
    # and this row would pass in both states.
    home = str(resolved_toolchain_home(ocx, project.directory))
    store = str(Path(ocx.env["OCX_HOME"]) / "packages")

    def lane(*args: str, env: dict[str, str] | None = None) -> str:
        result = run_in(ocx, project.directory, "--format", "json", "env", *args, env_extra=env)
        assert result.returncode == EXIT_SUCCESS, f"`ocx env` failed:\n{result.stderr}"
        values = [entry["value"] for entry in json.loads(result.stdout)["items"] if entry["key"] == "PATH"]
        assert values, "the project must compose at least one PATH directory"
        if all(value.startswith(store + os.sep) for value in values):
            return "pinned"
        if all(value.startswith(home + os.sep) for value in values):
            return "following"
        raise AssertionError(f"composed directories are in neither lane: {values}")

    write_ocx_toml(project.directory, project.config_body(pinned=True))
    assert lane() == "pinned", "the file tier decides when nothing above it does"
    assert lane("--no-pinned") == "following", "`--no-pinned` on the command line beats `pinned = true`"
    assert lane(env={"OCX_TOOLCHAIN_PINNED": "0"}) == "pinned", "the file tier beats the environment tier"

    write_ocx_toml(project.directory, project.config_body(pinned=False))
    assert lane(env={"OCX_TOOLCHAIN_PINNED": "1"}) == "following", "and it beats it in both directions"
    assert lane("--pinned", env={"OCX_TOOLCHAIN_PINNED": "0"}) == "pinned", "the command line beats both"


def test_a_committed_hostile_bin_never_reaches_path_before_a_render(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """S-003: consent is not a render. A clone's committed trampoline directory
    is not trusted.

    The clone inherits the tree and neither stamp — both are keyed on the
    canonical project directory — so ``bin_mode_entry`` finds no stamp for it
    and withholds. Consent is granted explicitly through ``[shell.consent]
    paths``, which is the point: a user who *did* authorise the directory still
    must not get an unrendered ``shells/default/bin`` on ``PATH``.
    """
    project = locked_project(ocx, tmp_path, label="d6")
    clone = _clone(project.directory, tmp_path / "hostile-clone")
    hostile = shell_bin(resolved_toolchain_home(ocx, clone)) / f"hostile{uuid4().hex[:8]}"
    hostile.write_text("#!/bin/sh\necho owned\n", encoding="utf-8")
    hostile.chmod(0o755)
    _write_home_config(arena, f"[shell.consent]\npaths = [{json.dumps(str(clone))}]\n")

    state = json.loads(run_in(ocx, clone, "--format", "json", "shell", "state").stdout)
    assert state["grant"] == "path", f"the fixture must actually be consented, or this proves nothing: {state}"
    assert read_render_stamp(ocx, Path(state["toolchain_home"])) is None, (
        "the clone must carry no render stamp, or the gate has nothing to refuse"
    )
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)

    result = _session(
        arena,
        _cd(clone),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=clone,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(Path(state["toolchain_home"]))) not in segments, (
        f"a committed, unstamped shells/default/bin must never reach PATH: {segments}"
    )
    assert install_bin in segments and global_bin in segments, (
        f"refusing the clone must not cost the two session directories: {segments}"
    )


def test_the_hostile_entry_is_replaced_and_the_dir_appears_after_pull(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """The positive half of the row above: a render is what unlocks the directory."""
    project = locked_project(ocx, tmp_path, label="d7")
    clone = _clone(project.directory, tmp_path / "clone-then-pull")
    home = resolved_toolchain_home(ocx, clone)
    hostile_name = f"hostile{uuid4().hex[:8]}"
    hostile = shell_bin(home) / hostile_name
    hostile.write_text("#!/bin/sh\necho owned\n", encoding="utf-8")
    hostile.chmod(0o755)
    _write_home_config(arena, f"[shell.consent]\npaths = [{json.dumps(str(clone))}]\n")

    assert run_in(ocx, clone, "pull", "-g", "all").returncode == EXIT_SUCCESS
    # `bin_entries` answers `[]` for a missing directory, so the negative below
    # is satisfied by an unrendered tree unless something pins the set non-empty.
    assert trampoline_name(project.default_binary) in bin_entries(home), (
        f"the witness: the clone rendered its own trampolines; got {bin_entries(home)}"
    )
    assert hostile_name not in bin_entries(home), (
        f"a render must prune an entry it did not write: {bin_entries(home)}"
    )

    result = _session(
        arena,
        _cd(clone),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=clone,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    assert _real(shell_bin(home)) in segments, f"a rendered, stamped tree reaches PATH: {segments}"


def test_owned_prefixes_exclude_a_sibling_project_and_the_toolchain_dir_root(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-063, RUL-68: the owned set is ``$OCX_HOME`` plus **this** project's home.

    Under ``toolchain_dir`` every project's home is a child of one root, so
    owning the bare root would make one project's prompt a deletion authority
    over every other project's trampolines. Both a sibling's directory and the
    root itself are planted on ``PATH`` and must survive — while this project's
    own stale segment, which *is* under its own home, is removed.

    The repair removes only for a session whose records the writer dropped at
    rung 2 with no inherited scope (``adr_exec_env_model.md`` "Owned-prefix
    repair"), so the session starts from a state marked ``all_applied_dropped``;
    the control run without it keeps the stale segment, proving the mark arms it.
    """
    # Under the arena's `$HOME`, not `tmp_path` or `$OCX_HOME` (already owned, so
    # the row would prove nothing): C-017 refuses a root under neither. The
    # runner moves onto that home too: C-017 judges the process `$HOME`, else
    # `package create` exits 78 under `--basetemp=/tmp/…`.
    root = arena.home / "toolchain-root"
    root.mkdir()
    root.chmod(0o755)
    ocx.env["HOME"] = str(arena.home)
    write_toolchain_dir_config(ocx, root)

    project = locked_project(ocx, tmp_path, label="d8a")
    sibling = locked_project(ocx, tmp_path, label="d8b")
    home = resolved_toolchain_home(ocx, project.directory)
    sibling_home = resolved_toolchain_home(ocx, sibling.directory)
    assert _real(home).startswith(_real(root) + os.sep) and _real(sibling_home).startswith(_real(root) + os.sep), (
        f"`toolchain_dir` must have relocated both homes under {root}: {home}, {sibling_home}"
    )
    assert _real(home) != _real(sibling_home), "the two projects must not share a home"

    stale = _real(shell_bin(home))
    planted = os.pathsep.join([str(root), str(shell_bin(sibling_home)), os.path.realpath(shell_bin(home))])

    def session_segments(extra_env: dict[str, str]) -> list[str]:
        return _segments(
            _session(
                arena,
                _cd(project.directory),
                matrix.prompt(SHELL),
                _path_probe("path"),
                cwd=project.directory,
                extra_env={"OCX_TOOLCHAIN_ACTIVATE": "env", **extra_env},
                path=planted,
            )
        )

    control = session_segments({})
    assert stale in control, f"a fresh session has no authority to repair an unrecorded segment: {control}"

    armed = encode({**default_state(), "truncated": "all_applied_dropped"})
    segments = session_segments({"__OCX_ENV_STATE": armed})
    assert _real(root) in segments, (
        f"the bare `toolchain_dir` root holds other projects' homes and may never be owned: {segments}"
    )
    assert _real(shell_bin(sibling_home)) in segments, (
        f"a sibling project's trampoline directory may never be owned: {segments}"
    )
    assert stale not in segments, (
        f"this project's own stale segment is under its own home and must be repaired away: {segments}"
    )


def test_no_project_prefix_is_owned_before_consent(ocx: OcxRunner, arena: Arena, tmp_path: Path) -> None:
    """C-028: the widening is strictly after consent, not merely usually after it.

    An unconsented project's home is not an owned prefix, so a segment under it
    survives untouched — stranding a stale directory is the deliberate cost of
    never deleting on a repository's say-so.
    """
    project = locked_project(ocx, tmp_path, label="d9")
    clone = _clone(project.directory, tmp_path / "unconsented-clone")
    state = json.loads(run_in(ocx, clone, "--format", "json", "shell", "state").stdout)
    assert state.get("grant") is None, f"this row needs a genuinely unconsented project: {state}"
    home = Path(state["toolchain_home"])
    planted = os.path.realpath(shell_bin(home))

    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)
    result = _session(
        arena,
        _cd(clone),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=clone,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
        path=planted,
    )
    segments = _segments(result)
    assert _real(planted) in segments, (
        f"an unconsented project's home is not an owned prefix; its segment must survive: {segments}"
    )
    assert install_bin in segments and global_bin in segments, (
        f"and the session pair is still contributed for an unconsented project: {segments}"
    )


def test_a_symlinked_project_home_is_refused_from_the_owned_set(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """The read path takes the same symlink guard the write path takes.

    ``resolve_toolchain_home`` builds the home lexically and ``plan``
    canonicalises each owned prefix, so a repository committing
    ``.ocx/toolchain`` as a link to ``/`` once put ``/`` in the owned set — and
    ``repair_owned_segments`` then removed every ambient ``PATH`` segment, on
    every prompt. ``ocx pull`` refused to *render* through that link the whole
    time; nothing refused to *own* it.
    """
    project = locked_project(ocx, tmp_path, label="d10")
    clone = _clone(project.directory, tmp_path / "symlinked-home")
    committed = clone / ".ocx" / "toolchain"
    shutil.rmtree(committed)
    if _WINDOWS:
        # A junction to the drive root: the link kind ocx writes there, and one that needs no privilege.
        make_link(committed.anchor, committed)
    else:
        committed.symlink_to("/")
    _write_home_config(arena, f"[shell.consent]\npaths = [{json.dumps(str(clone))}]\n")
    state = json.loads(run_in(ocx, clone, "--format", "json", "shell", "state").stdout)
    assert state["grant"] == "path", f"the clone must be consented, or the refusal is not the reason: {state}"

    foreign = tmp_path / "ambient-bin"
    foreign.mkdir()
    install_bin, global_bin = _session_dirs(ocx, arena, tmp_path)

    result = _session(
        arena,
        _cd(clone),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=clone,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
        path=str(foreign),
    )
    segments = _segments(result)
    assert _real(foreign) in segments, (
        f"a home symlinked to `/` must not become a deletion authority over the ambient PATH: {segments}"
    )
    for ambient in _windows_base(_bash()) if _WINDOWS else ("/usr/bin", "/bin"):
        assert _real(ambient) in segments, f"{ambient} must survive: {segments}"
    assert install_bin in segments and global_bin in segments, (
        f"and the prompt still contributes the session pair: {segments}"
    )


def test_a_repointed_link_with_bin_untouched_is_healed_or_withheld(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """C-062: ``shells/default/bin`` matching its stamp is not enough — the links
    must resolve.

    The repoint leaves ``shells/default/bin`` byte-identical, so a gate that fingerprinted
    only the trampolines would pass it through and expose a trampoline that
    dereferences to the wrong package. Either outcome the design allows is
    accepted (heal, or withhold); what is refused is exposing the directory
    while the link still points at the other package.
    """
    project = locked_project(ocx, tmp_path, label="d11")
    home = resolved_toolchain_home(ocx, project.directory)
    before = snapshot_tree(shell_bin(home))
    correct = link_entries(home)[f"default/{project.default_key}"]
    other = link_entries(home)[f"{project.group}/{project.group_key}"]
    assert correct != other, "the two groups must point at different digests for this row to discriminate"

    link = entry_link(home, DEFAULT_GROUP, project.default_key)
    remove_link(link)
    make_link(other, link)
    assert snapshot_tree(shell_bin(home)) == before, (
        "the repoint must leave shells/default/bin byte-untouched"
    )

    result = _session(
        arena,
        _cd(project.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=project.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)
    healed = link_entries(home)[f"default/{project.default_key}"] == correct
    exposed = _real(shell_bin(home)) in segments
    # Stated as the refusal rather than as a disjunction of outcomes. A review
    # pass proposed collapsing this to `assert not exposed` on the premise that
    # C-064 makes `healed` unreachable — measured, it is reachable and this row
    # reds, so the premise is wrong and the docstring's "either outcome" is the
    # contract. What is never allowed is the conjunction below.
    #
    # Not vacuous: `correct != other` and the byte-identical `shells/default/bin`
    # snapshot above prove the repoint really happened before the prompt ran.
    assert not (exposed and not healed), (
        "refused: `shells/default/bin` is on PATH while `default/"
        f"{project.default_key}` still points at the other package; "
        f"link={link_entries(home)[f'default/{project.default_key}']!r} segments={segments}"
    )


def test_a_branch_switch_at_the_same_path_never_dereferences_the_other_branch(
    ocx: OcxRunner, arena: Arena, tmp_path: Path
) -> None:
    """S-006: the tree on disk still describes the branch that was pulled.

    Same directory, same ``[tools]`` key, same binary name, two digests — so
    nothing about the *spelling* changes across the switch and only the link's
    target can be wrong. The trampoline is executed here (it is an ordinary
    package binary, not ``ocx``) because running it is the only observation
    that distinguishes "the link was healed" from "the link looks plausible".
    """
    checkout = two_branch_checkout(ocx, tmp_path, label="d12")
    assert run_in(ocx, checkout.directory, "pull", "-g", "all").returncode == EXIT_SUCCESS
    trampoline = shell_bin(checkout.home) / trampoline_name(checkout.binary)
    # The `.exec` sidecar on Windows: the `.exe` is a launcher blob shared by every project.
    before = trampoline_body(trampoline).read_bytes()
    stale_target = link_entries(checkout.home)[f"default/{checkout.key}"]

    first = subprocess.run(
        [str(trampoline)], capture_output=True, text=True, env=dict(ocx.env), check=False, timeout=_TIMEOUT
    )
    assert checkout.package_main.marker in first.stdout, (
        f"the rendered trampoline must run the branch that was pulled: {first.stdout!r}"
    )

    assert git(checkout.directory, "checkout", "-q", checkout.other_branch).returncode == EXIT_SUCCESS
    assert trampoline_body(trampoline).read_bytes() == before, (
        "the switch must leave shells/default/bin byte-identical, or the stamp — not the link — is what notices"
    )

    result = _session(
        arena,
        _cd(checkout.directory),
        matrix.prompt(SHELL),
        _path_probe("path"),
        cwd=checkout.directory,
        extra_env={"OCX_TOOLCHAIN_ACTIVATE": "bin"},
    )
    segments = _segments(result)

    after = subprocess.run(
        [str(trampoline)], capture_output=True, text=True, env=dict(ocx.env), check=False, timeout=_TIMEOUT
    )
    exposed = _real(shell_bin(checkout.home)) in segments
    healed = link_entries(checkout.home)[f"default/{checkout.key}"] != stale_target
    # The link half is asserted separately from the run half **because the run
    # half is defended twice**: a trampoline re-resolves `ocx.lock` at run time
    # and bakes a digest root, so it names the checked-out branch even when the
    # `<group>/<entry>` link still points at the branch that was pulled. Only
    # the link assertion can tell "the prompt healed the tree" from "the
    # trampoline happens not to consult it" — and the composing emitters *do*
    # consult it.
    # The refusal, not a disjunction of outcomes — see the sibling row above for
    # why `healed` is reachable and a review pass's contrary premise was wrong.
    assert not (exposed and not healed), (
        "refused: the trampoline directory is on PATH while the link still names "
        f"the branch that was pulled ({stale_target!r}); segments={segments}"
    )
    if exposed:
        assert checkout.package_other.marker in after.stdout, (
            "a trampoline on PATH after a branch switch must dereference the branch that is checked out "
            f"now, never the one that was pulled; got {after.stdout!r}"
        )
        assert checkout.package_main.marker not in after.stdout, (
            f"the previous branch's package must not still be reachable: {after.stdout!r}"
        )
    else:
        assert _HINT in result.stderr, (
            "withholding is the other allowed outcome, and it owes the user the hint; "
            f"stderr:\n{result.stderr}"
        )
