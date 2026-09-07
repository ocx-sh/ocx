# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the toolchain **trampoline** as an executable.

WP-12d of ``adr_toolchain_activation.md``: validation items 1, 16, 22, 33, 36,
38 and C-057 — everything that is only observable once a rendered
``<home>/bin/<name>`` is actually *run*, or once ``ocx exec`` is asked to
resolve a name on a ``PATH`` that carries one.

The exit code does not discriminate — assert the wording
-------------------------------------------------------
``CommandResolutionError``'s three variants all classify to ``DataError``
(``crates/ocx_lib/src/env.rs``, ``impl ClassifyExitCode``), on purpose: the
code names the *class*, and guard identity lives in the message. A case that
asserted only "exit 65" could not tell C-010's ``PATH`` exclusion from C-069's
trampoline refusal, and item 22 requires knowing which fired. So every
resolution case here asserts the sentence, and asserts the *other* guard's
sentence is absent:

* C-010 miss → :data:`NOT_FOUND_WORDING`
* C-069 refusal → :data:`TRAMPOLINE_WORDING`

Deleting either guard alone must flip a case from one wording to the other (or,
where the refusal is what stops the A → B → A re-entry, into a deadline).

Every subprocess is bounded
---------------------------
Several rows here fail by *hanging* rather than by exiting: a removed guard
turns two trampoline directories on one ``PATH`` into an unbounded
``A → B → A`` re-entry, one full compose per hop, with no depth counter
(D-V1). ``OcxRunner`` passes no ``timeout=`` and the suite configures no
``pytest-timeout``, so a reintroduced loop would hang the whole run instead of
failing one case. :data:`TIMEOUT` is therefore explicit on every call.

The Windows arm executes the **committed** blob — never a local build
--------------------------------------------------------------------
``OCX_SHIM_BINARY`` is read in exactly one place in this repository
(``test/tests/test_windows_shim.py``) and CI points it at
``target/debug/ocx-shim.exe``, so all twelve tests in that module execute a
**locally built substitute**: not one has ever executed a byte of
``crates/ocx_lib/src/shims/ocx-shim-*.exe``, which is what ``include_bytes!``
ships and what users actually get. The Windows cases below close that gap and
are the only tests in the tree that do.

**They must therefore never read ``OCX_SHIM_BINARY`` and never reuse
``test_windows_shim.py``'s ``shim_entrypoint`` fixture.** Both would silently
swap the committed artefact for a fresh local build and re-open the exact hole
these cases exist to close, while every assertion kept passing. The fixture
here (:func:`_committed_blob`, :func:`_install_trampoline_slot`) is deliberately
a second, non-shared one for that reason — do not fold them together.

They need no registry and no ``ocx`` binary (a ``.exec`` sidecar carries no
containment path, so the shim never consults ``OCX_HOME``), which is what lets
them run on the one Windows job that exists — ``build-windows-shims.yml``,
whose pytest invocation names this module alongside ``test_windows_shim.py``.
"""

from __future__ import annotations

import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, make_package_with_entrypoints, write_ocx_toml
from src.runner import OcxRunner
from src.toolchain_fixtures import (
    DEFAULT_GROUP,
    ToolchainProject,
    bin_entries,
    locked_project,
    run_in,
    sha256_of,
)

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_DATA = 65
EXIT_CONFIG = 78

#: Every subprocess in this module is bounded — see the module docstring.
TIMEOUT = 90

#: C-010's miss (`CommandResolutionError::NotFound`).
NOT_FOUND_WORDING = "does not resolve in the composed environment; searched:"

#: C-069's refusal (`CommandResolutionError::TrampolineRefused`).
TRAMPOLINE_WORDING = "resolves to an ocx launcher trampoline at"

#: `crate::env::TRAMPOLINE_MARKER` — line **two** of every rendered trampoline.
TRAMPOLINE_MARKER = "# ocx-toolchain-trampoline"

#: `crate::env::TRAMPOLINE_PROBE_BYTES` — how much of a candidate is read.
TRAMPOLINE_PROBE_BYTES = 256


# ---------------------------------------------------------------------------
# Running things
# ---------------------------------------------------------------------------


def run_file(
    executable: Path,
    *args: str,
    cwd: Path,
    env: dict[str, str],
) -> subprocess.CompletedProcess[str]:
    """Invoke a file directly, bounded.

    Not ``run_in``: these rows run a *trampoline*, not ``ocx``, and the whole
    point of several of them is that the file decides its own project — so the
    working directory is deliberately somewhere unrelated.
    """
    return subprocess.run(
        [str(executable), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=TIMEOUT,
    )


def composed_path(ocx: OcxRunner, project: Path, **kwargs: object) -> list[str]:
    """The ``PATH`` the child of ``ocx exec`` actually receives, split.

    Read through the composition itself rather than re-derived, so a row that
    asserts "this directory was excluded from the *lookup*" can first establish
    the directory is on the child's ``PATH`` at all. Without that, the negative
    passes just as well against a drifted spelling.
    """
    result = run_in(ocx, project, "exec", "--", "sh", "-c", "printenv PATH", **kwargs)  # type: ignore[arg-type]
    assert result.returncode == EXIT_SUCCESS, (
        f"composing the child PATH must succeed; rc={result.returncode}\n{result.stderr}"
    )
    return result.stdout.strip().split(os.pathsep)


def write_observer(path: Path, log: Path) -> Path:
    """A ``#!/bin/sh`` stand-in for ``ocx``, recording one line per invocation.

    Pinned through ``OCX_BINARY_PIN``, which every trampoline body honours
    (``exec "${OCX_BINARY_PIN:-$__ocx_binary}" …``). Two jobs: it makes the
    wire line the trampoline emits observable, and it makes a *spawn* itself
    observable — the log staying absent is how "nothing was spawned" is
    asserted rather than assumed. It never runs ocx, so it also bounds any
    re-entry a removed guard would otherwise turn into a loop.
    """
    path.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{log}"\nexit 0\n')
    path.chmod(0o755)
    return path


def stale_trampoline(project: ToolchainProject, name: str) -> Path:
    """A rendered trampoline copied into ``<home>/bin/`` under ``name``.

    A byte copy is a *faithful* stale trampoline, not an approximation: the
    body bakes the project root and nothing else, and derives the tool name at
    run time from ``${0##*/}`` (C-028). Copying it under a new name therefore
    produces exactly what a rendered tree holds after a tool is dropped from
    ``ocx.toml`` without a re-render — the state D-V1's two-home loop needs,
    and the one thing the composition itself will not provide.
    """
    target = project.home / "bin" / name
    shutil.copy(project.default_trampoline, target)
    return target


def assert_names_one_guard(stderr: str, expected: str, other: str) -> None:
    """The refusal names ``expected`` and not ``other``.

    Both halves matter. All three `CommandResolutionError` variants exit 65, so
    a case asserting only the code — or only the presence of one sentence —
    cannot tell a guard that fired from a sibling that fired in its place.
    """
    assert expected in stderr, f"the refusal must name this guard: {stderr!r}"
    assert other not in stderr, f"a different guard fired than the one under test: {stderr!r}"


# ---------------------------------------------------------------------------
# Item 22 — no self-referential loop (the mise v2026.3.18 class)
# ---------------------------------------------------------------------------


def test_item22_a_name_the_composition_does_not_provide_exits_65_naming_the_search(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-010: both trampoline directories are dropped from the **lookup** PATH.

    The name exists as a stale trampoline in the project's own ``bin/`` and
    nowhere else, so the two guards give visibly different answers: with the
    exclusion in force the lookup never sees the file (**NotFound**, and the
    reported ``searched:`` list omits both trampoline directories); without it
    the file is found and C-069 refuses it (**TrampolineRefused**).

    The child's own ``PATH`` is asserted to still carry both directories — the
    exclusion is scoped to the lookup copy, and a `searched:` list that omitted
    them because their spelling drifted would satisfy the negative just as well.

    RED: make ``trampoline_lookup_exclusions``
    (``crates/ocx_cli/src/command/toolchain_exec.rs``) return an empty vector —
    the message flips to :data:`TRAMPOLINE_WORDING`.
    """
    project = locked_project(ocx, tmp_path)
    ghost = f"ghost{project.default_binary}"
    stale_trampoline(project, ghost)

    project_bin = str(project.home / "bin")
    global_bin = str(Path(ocx.env["OCX_HOME"]) / "toolchain" / "bin")
    Path(global_bin).mkdir(parents=True, exist_ok=True)
    # `bin` mode is what puts these two on an interactive shell's PATH, and it
    # is the arrangement C-010 exists for — `ocx exec` does not add them itself.
    activated = {"PATH": os.pathsep.join([project_bin, global_bin, ocx.env["PATH"]])}

    child_path = composed_path(ocx, project.directory, env_extra=activated)
    assert {project_bin, global_bin} <= set(child_path), (
        "the child's own PATH must carry both trampoline directories — C-010 "
        f"excludes the lookup copy only; got {child_path}"
    )

    result = run_in(ocx, project.directory, "exec", "--", ghost, env_extra=activated)

    assert result.returncode == EXIT_DATA, (
        f"a name the composition does not provide is exit 65; rc={result.returncode}\n"
        f"{result.stderr}"
    )
    assert_names_one_guard(result.stderr, NOT_FOUND_WORDING, TRAMPOLINE_WORDING)
    searched = result.stderr.split("searched:", 1)[1]
    for excluded in (project_bin, global_bin):
        assert excluded not in searched, (
            f"{excluded!r} is a trampoline directory and must not be searched; got {searched!r}"
        )


def test_item22_nothing_is_spawned_on_the_refusal(ocx: OcxRunner, tmp_path: Path) -> None:
    """The refusal is terminal: no child process is created on the way out.

    ``ocx exec`` resolves once and hands the answer to the launch seam, so a
    guard that resolved-then-refused — or one that let the stale trampoline
    through — would have started something first. The observer pinned here is
    what the trampoline would exec; its log file therefore exists **iff**
    something was spawned, and it never appears.

    The observer also bounds the failure: without it, a build missing both
    guards re-enters ocx per hop forever rather than failing.

    RED: return an empty exclusion set from ``trampoline_lookup_exclusions``
    *and* make ``is_ocx_trampoline`` answer ``false`` — the stale trampoline
    runs, the observer records the wire line, and this file appears.
    """
    project = locked_project(ocx, tmp_path)
    ghost = f"ghost{project.default_binary}"
    stale_trampoline(project, ghost)
    log = tmp_path / "spawned.log"
    observer = write_observer(tmp_path / "observer.sh", log)

    result = run_in(
        ocx,
        project.directory,
        "exec",
        "--",
        ghost,
        env_extra={
            "OCX_BINARY_PIN": str(observer),
            "PATH": f"{project.home / 'bin'}{os.pathsep}{ocx.env['PATH']}",
        },
    )

    assert result.returncode == EXIT_DATA, (
        f"the refusal is exit 65; rc={result.returncode}\n{result.stderr}"
    )
    assert_names_one_guard(result.stderr, NOT_FOUND_WORDING, TRAMPOLINE_WORDING)
    assert not log.exists(), (
        f"nothing may be spawned on the refusal; the observer ran and recorded "
        f"{log.read_text()!r}"
    )


def test_item22_a_foreign_home_trampoline_on_path_is_refused_by_identity(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-069: a *second* project's trampoline is refused by identity, not by list.

    C-010's exclusion can only ever name this invocation's own two trees. The
    defect D-V1 records is two project homes on one ``PATH``, each holding a
    stale trampoline for the same name: A's lookup skips A's own directory,
    finds B's, and executes it; B's lookup skips B's, finds A's, and executes
    that — one full compose per hop, forever, with no error and no depth
    counter. Only a predicate over the resolved *file* stops it.

    RED: make ``is_ocx_trampoline`` (``crates/ocx_lib/src/env.rs``) answer
    ``false``. The re-entry becomes the A → B → A loop, so this case reds as
    a :data:`TIMEOUT` rather than as a wrong exit code.
    """
    project_a = locked_project(ocx, tmp_path, label=f"a{uuid4().hex[:6]}")
    project_b = locked_project(ocx, tmp_path, label=f"b{uuid4().hex[:6]}")
    ghost = f"ghost{uuid4().hex[:8]}"
    stale_trampoline(project_a, ghost)
    foreign = stale_trampoline(project_b, ghost)

    result = run_in(
        ocx,
        project_a.directory,
        "exec",
        "--",
        ghost,
        env_extra={
            # Both homes on one PATH — the arrangement D-V1 names. A's own
            # directory is dropped from A's lookup by C-010; B's is not, and
            # only the file-level predicate stops the hop from being taken.
            "PATH": os.pathsep.join(
                [
                    str(project_a.home / "bin"),
                    str(project_b.home / "bin"),
                    ocx.env["PATH"],
                ]
            )
        },
    )

    assert result.returncode == EXIT_DATA, (
        f"a foreign home's trampoline is refused with exit 65; rc={result.returncode}\n"
        f"{result.stderr}"
    )
    assert_names_one_guard(result.stderr, TRAMPOLINE_WORDING, NOT_FOUND_WORDING)
    assert str(foreign) in result.stderr, (
        f"the refusal must name the foreign trampoline it resolved to; got {result.stderr!r}"
    )


def test_item22_a_marker_bearing_project_path_is_not_refused(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """E-21: the marker counts as **line two**, never "somewhere in the head".

    Two halves of the same anchor, both built here.

    A project root that literally spells the marker is baked into every
    trampoline body this project renders — inside single quotes, on line five.
    It must render, and the trampoline must run.

    The discriminating half is the tool: its own body carries the marker text
    on line three, well inside the probe window. That models what WP-6 makes
    reachable — an operator-controlled path interpolated into the probed head of
    an ordinary generated launcher — and it must resolve. A ``contains()`` over
    the 256-byte prefix would refuse it, break ``ocx exec`` for that install,
    and be indistinguishable from a correct refusal by exit code alone.

    The precondition asserts are what stop the case passing vacuously: a marker
    that fell outside the window, or that landed on line two after all, would
    make the loosened predicate answer ``false`` too.

    RED: replace the ``nth(1)``/whole-line comparison in ``trampoline_signal``
    (``crates/ocx_lib/src/env.rs``) with ``prefix.windows(..).any(..)`` over the
    marker bytes — this case exits 65 with :data:`TRAMPOLINE_WORDING`.
    """
    label = uuid4().hex[:8]
    name = f"mk{label}"
    key = f"mkkey{label}"
    package = make_package(
        ocx,
        f"t_{label}_marker",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[name],
        bin_scripts={name: f"#!/bin/sh\n# an ordinary comment\n{TRAMPOLINE_MARKER}\necho ran-{label}\n"},
    )
    directory = tmp_path / TRAMPOLINE_MARKER / f"proj-{label}"
    directory.mkdir(parents=True)
    write_ocx_toml(directory, f'[tools]\n{key} = "{package.fq}"\n')
    assert run_in(ocx, directory, "lock").returncode == EXIT_SUCCESS
    pulled = run_in(ocx, directory, "pull")
    assert pulled.returncode == EXIT_SUCCESS, (
        f"a project root spelling the marker must still render; {pulled.stderr}"
    )

    home = directory / ".ocx" / "toolchain"
    resolved = (home / DEFAULT_GROUP / key).resolve() / "content" / "bin" / name
    prefix = resolved.read_bytes()[:TRAMPOLINE_PROBE_BYTES]
    assert TRAMPOLINE_MARKER.encode() in prefix, (
        "precondition: the marker must sit inside the probe window, or the "
        "loosened predicate answers `false` here for the wrong reason"
    )
    assert prefix.split(b"\n")[1] != TRAMPOLINE_MARKER.encode(), (
        "precondition: the marker must NOT be line two, or the anchored "
        "predicate would refuse this file too"
    )

    result = run_in(ocx, directory, "exec", "--", name)
    assert result.returncode == EXIT_SUCCESS, (
        f"a tool merely containing the marker below line two must resolve; "
        f"rc={result.returncode}\n{result.stderr}"
    )
    assert f"ran-{label}" in result.stdout, result.stdout

    trampoline = home / "bin" / name
    body = trampoline.read_text()
    assert f"'{directory}'" in body, (
        f"the marker-spelling root is baked inside single quotes; got {body!r}"
    )
    invoked = run_file(trampoline, cwd=tmp_path, env=dict(ocx.env))
    assert invoked.returncode == EXIT_SUCCESS, (
        f"the trampoline for a marker-spelling root must run; rc={invoked.returncode}\n"
        f"{invoked.stderr}"
    )
    assert f"ran-{label}" in invoked.stdout, invoked.stdout


def test_c057_launcher_exec_propagates_65_for_a_name_that_resolves_to_a_trampoline(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-057 at the shipped launcher's re-entry: a refusal is not a miss.

    ``ocx launcher exec`` resolves through ``Env::resolve_test_command``, whose
    total-miss fall-through to the bare name is **deliberately retained**
    (C-011: four production callers, the Starlark host among them, treat a miss
    as "let the OS answer"). That fall-through is scoped to ``NotFound`` and to
    nothing else — a name that resolves to a trampoline must propagate C-069's
    refusal as **65**, because handing the bare name to ``execvp`` repeats the
    lookup against the ambient ``PATH`` and finds the same trampoline again.

    A shipped launcher prunes nothing and inherits the ambient ``PATH`` a
    ``bin``-mode toolchain puts its trampolines on, so this is the reachable
    path, not a constructed one.

    Not asserted here, and correct: the same invocation with a name that
    resolves *nowhere* exits **1**, through that retained fall-through. The two
    answers are the contract; collapsing them is the regression.

    RED: broaden ``resolve_test_command``'s fall-through arm back to a blanket
    ``Err(_) => Ok(PathBuf::from(command))``. Note what that red looks like —
    the exit code stays **65** and only the sentence changes, to
    :data:`NOT_FOUND_WORDING`, because the swallowed refusal re-surfaces one
    frame out as a miss. A case asserting the code alone would have passed.
    """
    label = uuid4().hex[:8]
    package = make_package_with_entrypoints(ocx, f"t_{label}_launcher", tmp_path, [f"ep{label}"])
    installed = run_in(ocx, tmp_path, "package", "install", package.fq)
    assert installed.returncode == EXIT_SUCCESS, installed.stderr
    roots = sorted((Path(ocx.env["OCX_HOME"]) / "packages").rglob("metadata.json"))
    assert len(roots) == 1, f"exactly one package root is expected; got {roots}"
    package_root = roots[0].parent

    project = locked_project(ocx, tmp_path, label=f"l{label}")
    ghost = f"ghost{label}"
    trampoline = stale_trampoline(project, ghost)

    result = subprocess.run(
        [str(ocx.binary), "launcher", "exec", str(package_root), "--", ghost],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
        timeout=TIMEOUT,
        env={**ocx.env, "PATH": f"{project.home / 'bin'}{os.pathsep}{ocx.env['PATH']}"},
    )

    assert result.returncode == EXIT_DATA, (
        f"the launcher re-entry propagates the refusal as 65, not 1 and not 64; "
        f"rc={result.returncode}\n{result.stderr}"
    )
    assert_names_one_guard(result.stderr, TRAMPOLINE_WORDING, NOT_FOUND_WORDING)
    assert str(trampoline) in result.stderr, result.stderr


# ---------------------------------------------------------------------------
# Item 1 — the baked selector is the only selector
# ---------------------------------------------------------------------------


def test_item1_a_trampoline_invoked_through_a_symlink_elsewhere_uses_the_baked_home(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 1: a trampoline symlinked anywhere composes the home it bakes.

    Two independent observations, because either alone is satisfiable by the
    wrong implementation. The wire line the trampoline emits must name the
    **bare stem** (``${0##*/}``) and the baked root — a body that passed ``$0``
    would send the symlink's whole path as the tool name, and one that walked
    the working directory would name a different project or none. And the same
    invocation without the observer must actually run the baked project's tool,
    from a working directory that is neither project nor link.

    RED: replace ``${0##*/}`` with ``$0`` in ``unix_trampoline_body``
    (``crates/ocx_lib/src/package_manager/launcher/body.rs``) — the recorded
    line names the symlink's absolute path, and the unpinned half re-executes
    the symlink instead of the tool, which is a loop caught by :data:`TIMEOUT`.
    """
    project = locked_project(ocx, tmp_path)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    link = elsewhere / project.default_binary
    link.symlink_to(project.default_trampoline)
    unrelated = tmp_path / "unrelated"
    unrelated.mkdir()

    log = tmp_path / "wire.log"
    observer = write_observer(tmp_path / "observer.sh", log)
    pinned = run_file(
        link,
        cwd=unrelated,
        env={**ocx.env, "OCX_BINARY_PIN": str(observer)},
    )
    assert pinned.returncode == EXIT_SUCCESS, pinned.stderr
    assert log.read_text().strip() == (
        f"--project {project.directory} exec -- {project.default_binary}"
    ), (
        f"the trampoline must re-enter with the baked root and the bare stem; "
        f"got {log.read_text()!r}"
    )

    ran = run_file(link, cwd=unrelated, env=dict(ocx.env))
    assert ran.returncode == EXIT_SUCCESS, (
        f"invoking through the symlink must run the baked project's tool; "
        f"rc={ran.returncode}\n{ran.stderr}"
    )
    assert project.default_package.marker in ran.stdout, ran.stdout


# ---------------------------------------------------------------------------
# Item 38 — an exported tier selector never shadows the baked one
# ---------------------------------------------------------------------------


def test_item38_an_exported_ocx_project_does_not_override_the_baked_selector(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 38 / S-009: the body's ``unset`` is what keeps trampolines usable.

    ``--project`` is baked, so an ambient ``OCX_GLOBAL`` or ``OCX_PROJECT``
    would reach ``check_global_project_exclusivity`` as a second, conflicting
    tier selector and refuse a flag the user never typed — for *every*
    trampoline on that ``PATH``, from one exported variable. Both are exported
    here, and ``OCX_PROJECT`` names a different real project so a body that
    merely tolerated them would still be caught composing the wrong home.

    RED: drop ``unset OCX_GLOBAL OCX_PROJECT`` from ``unix_trampoline_body`` —
    exit 64.
    """
    project = locked_project(ocx, tmp_path, label=f"s{uuid4().hex[:6]}")
    other = locked_project(ocx, tmp_path, label=f"o{uuid4().hex[:6]}")

    result = run_file(
        project.default_trampoline,
        cwd=tmp_path,
        env={**ocx.env, "OCX_GLOBAL": "1", "OCX_PROJECT": str(other.directory)},
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"the baked selector is the only selector; rc={result.returncode}\n{result.stderr}"
    )
    assert project.default_package.marker in result.stdout, (
        f"the baked project's tool must run, not the exported project's; got {result.stdout!r}"
    )
    assert other.default_package.marker not in result.stdout, result.stdout


# ---------------------------------------------------------------------------
# Item 36 — a trampoline whose baked home went bad
# ---------------------------------------------------------------------------


def test_item36_a_trampoline_whose_home_lost_its_ocx_toml_exits_64_naming_the_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 36 / C-068: a missing baked project is **64**, naming the baked root.

    A trampoline re-enters as ``ocx --project '<baked root>' exec`` from
    whatever directory the user happened to be in. The shipped ``--project``
    path answers about the wrong tier for that shape — an explicit selection
    that is absent is ``FileNotFound`` (79) — so ``attribute_to_selected_project``
    re-attributes it to the project the invocation *selected*, giving the same
    three-way contract ``ocx exec`` already states, and names the baked path
    rather than the working directory.

    RED: neutralize ``attribute_to_selected_project``'s ``NoProject`` arm — the
    exit code stays 64 and the message names ``tmp_path``, the directory the
    user happened to be in, instead of the baked root. Which is why the path is
    asserted and not only the code: deleting the sibling ``FileNotFound`` arm
    reds nothing here, because the loader answers ``NoProject`` for a
    ``--project`` directory whose ``ocx.toml`` was removed.
    """
    project = locked_project(ocx, tmp_path)
    (project.directory / "ocx.toml").unlink()

    result = run_file(project.default_trampoline, cwd=tmp_path, env=dict(ocx.env))

    assert result.returncode == EXIT_USAGE, (
        f"a baked project that is gone is exit 64; rc={result.returncode}\n{result.stderr}"
    )
    assert str(project.directory) in result.stderr, (
        f"the refusal must name the baked root, not the working directory "
        f"({tmp_path}); got {result.stderr!r}"
    )


def test_item36_a_trampoline_whose_home_lost_its_lock_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 36 / C-068: a missing ``ocx.lock`` is **78**, not 64 and not 65.

    The three-way contract is the point: a caller must be able to tell "there
    is no project here" from "the project is unlocked" from "the lock is out of
    date" by exit code alone, because each has a different repair.

    RED: swap the two arms of ``LockCurrency::classify``
    (``crates/ocx_lib/src/project/lock.rs``) — this case and its stale sibling
    trade answers, and neither can be caught by the other's assertion.
    """
    project = locked_project(ocx, tmp_path)
    (project.directory / "ocx.lock").unlink()

    result = run_file(project.default_trampoline, cwd=tmp_path, env=dict(ocx.env))

    assert result.returncode == EXIT_CONFIG, (
        f"a baked project with no lock is exit 78; rc={result.returncode}\n{result.stderr}"
    )
    assert str(project.directory / "ocx.lock") in result.stderr, result.stderr


def test_item36_a_trampoline_whose_home_carries_a_stale_lock_exits_65(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 36 / C-068: a lock that no longer describes ``ocx.toml`` is **65**.

    The third arm of the same contract. The mutation renames the declared group
    without re-locking, which is the cheapest edit that moves the declaration
    hash without changing what any tool resolves to.

    RED: as the sibling above — swap the two arms of ``LockCurrency::classify``.
    """
    project = locked_project(ocx, tmp_path)
    write_ocx_toml(project.directory, project.config_body(group=f"{project.group}x"))

    result = run_file(project.default_trampoline, cwd=tmp_path, env=dict(ocx.env))

    assert result.returncode == EXIT_DATA, (
        f"a baked project with a stale lock is exit 65; rc={result.returncode}\n{result.stderr}"
    )
    assert "stale" in result.stderr, result.stderr


# ---------------------------------------------------------------------------
# Item 16 — name collisions never refuse
# ---------------------------------------------------------------------------


def test_item16_a_package_claiming_the_name_ocx_renders_a_trampoline_and_runs(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 16 / D-4: ``ocx`` is an ordinary name — no refusal, no warning.

    The `ShimNameShadowsOcx` refusal was removed, so a project may pin its own
    ``ocx``. The rendered ``bin/ocx`` must also *work*, which is a stronger
    claim than its existence: the trampoline re-enters ocx and asks it to
    resolve the name ``ocx``, and C-010's exclusion is what keeps that
    resolution from answering with the trampoline itself.

    RED: reinstate the refusal — ``ocx pull`` exits 65 and ``bin/ocx`` is
    absent. RED for the second half: drop the exclusion, and the re-entry
    resolves back to this same file — caught by :data:`TIMEOUT`.
    """
    label = uuid4().hex[:8]
    package = make_package(ocx, f"t_{label}_ocxname", "1.0.0", tmp_path, cascade=False, bins=["ocx"])
    directory = tmp_path / f"proj-{label}"
    directory.mkdir()
    write_ocx_toml(directory, f'[tools]\nkey{label} = "{package.fq}"\n')
    assert run_in(ocx, directory, "lock").returncode == EXIT_SUCCESS

    pulled = run_in(ocx, directory, "pull")
    assert pulled.returncode == EXIT_SUCCESS, (
        f"claiming the name `ocx` must not refuse; rc={pulled.returncode}\n{pulled.stderr}"
    )
    assert "WARN" not in pulled.stderr, (
        f"a claimed `ocx` warns nobody — the surface is one debug line; got {pulled.stderr!r}"
    )

    home = directory / ".ocx" / "toolchain"
    assert bin_entries(home) == ["ocx"], bin_entries(home)

    # `bin` mode: the trampoline directory is PATH-front when this runs, which
    # is what makes the re-entry's own lookup for the name `ocx` a live question.
    ran = run_file(
        home / "bin" / "ocx",
        cwd=tmp_path,
        env={**ocx.env, "PATH": f"{home / 'bin'}{os.pathsep}{ocx.env['PATH']}"},
    )
    assert ran.returncode == EXIT_SUCCESS, (
        f"the rendered `ocx` trampoline must run the package's tool; rc={ran.returncode}\n"
        f"{ran.stderr}"
    )
    assert package.marker in ran.stdout, ran.stdout


def test_item16_two_packages_claiming_one_name_render_one_trampoline_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 16 / C-024: two claims on one name — last walked wins, silently.

    One file, not two and not a refusal, and the loser is not lost: the debug
    line names the winning identifier *and* the claim it shadowed. That line is
    the whole of the surface D-4 allows (``RenderReport`` carries no collision
    field, RUL-34), so it is what this asserts — an `ocx inspect` row for the
    shadowed rival does not exist today.

    RED: add a collision refusal or a warning to
    ``toolchain_names::record_claim`` — the pull stops being silent, or stops
    succeeding.
    """
    label = uuid4().hex[:8]
    shared = f"dup{label}"
    first = make_package(ocx, f"t_{label}_first", "1.0.0", tmp_path, cascade=False, bins=[shared])
    second = make_package(ocx, f"t_{label}_second", "1.0.0", tmp_path, cascade=False, bins=[shared])
    directory = tmp_path / f"proj-{label}"
    directory.mkdir()
    write_ocx_toml(
        directory,
        f'[tools]\nk1{label} = "{first.fq}"\nk2{label} = "{second.fq}"\n',
    )
    assert run_in(ocx, directory, "lock").returncode == EXIT_SUCCESS

    pulled = run_in(ocx, directory, "pull")
    assert pulled.returncode == EXIT_SUCCESS, (
        f"a name collision never refuses; rc={pulled.returncode}\n{pulled.stderr}"
    )
    assert "WARN" not in pulled.stderr, (
        f"a name collision never warns either; got {pulled.stderr!r}"
    )

    home = directory / ".ocx" / "toolchain"
    assert bin_entries(home) == [shared], (
        f"two claims on one name render exactly one trampoline; got {bin_entries(home)}"
    )

    ran = run_file(home / "bin" / shared, cwd=tmp_path, env=dict(ocx.env))
    assert ran.returncode == EXIT_SUCCESS, ran.stderr
    assert second.marker in ran.stdout, (
        f"the last-walked claim owns the name; got {ran.stdout!r}"
    )
    assert first.marker not in ran.stdout, ran.stdout

    debug = run_in(ocx, directory, "--log-level", "debug", "pull")
    assert debug.returncode == EXIT_SUCCESS, debug.stderr
    claims = [line for line in debug.stderr.splitlines() if f"'{shared}' claimed by" in line]
    assert len(claims) == 1, f"exactly one collision line per collision; got {claims}"
    assert second.repo in claims[0] and first.repo in claims[0], (
        f"the debug line names the winner and the claim it shadowed; got {claims[0]!r}"
    )


# ---------------------------------------------------------------------------
# Item 33 — the body bakes no composition flag
# ---------------------------------------------------------------------------


def test_item33_flipping_pinned_changes_the_composed_path_without_re_rendering(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 33 / C-046: ``pinned`` takes effect at re-entry, not at render.

    A trampoline bakes the project *selector* and nothing else — no digest, no
    lane, no composition flag — so the ladder is re-read by the ocx the
    trampoline re-enters. Flipping ``pinned`` in ``ocx.toml`` with **no
    ``ocx pull`` between** must therefore change what composes while leaving
    ``bin/`` untouched, and flipping back must restore it.

    Both halves are asserted every time, because either alone is satisfiable by
    the wrong implementation: a body that baked the flag would keep the old
    lane (and the bytes would still match), and a render triggered by the flip
    would change the lane (and the bytes with it).

    RED: bake ``--pinned`` into ``unix_trampoline_body`` — the second
    composition still emits link paths.
    """
    project = locked_project(ocx, tmp_path)
    link_lane = str(project.home / DEFAULT_GROUP / project.default_key)
    digest_lane = str(Path(ocx.env["OCX_HOME"]) / "packages")
    rendered = sha256_of(project.default_trampoline)

    following = composed_path(ocx, project.directory)[0]
    assert following.startswith(link_lane), (
        f"the default lane composes through the `<group>/<entry>` link; got {following}"
    )

    write_ocx_toml(project.directory, project.config_body(pinned=True))
    pinned = composed_path(ocx, project.directory)[0]
    assert pinned.startswith(digest_lane), (
        f"`pinned = true` composes digest paths with no re-render; got {pinned}"
    )
    assert sha256_of(project.default_trampoline) == rendered, (
        "no trampoline byte may change: the flip is read at re-entry, not baked"
    )

    write_ocx_toml(project.directory, project.config_body(pinned=False))
    restored = composed_path(ocx, project.directory)[0]
    assert restored == following, (
        f"flipping back restores the link lane; got {restored}, expected {following}"
    )
    assert sha256_of(project.default_trampoline) == rendered, (
        "no trampoline byte may change on the way back either"
    )


# ---------------------------------------------------------------------------
# Windows — the COMMITTED shim blob, dispatching a `.exec` sidecar
# ---------------------------------------------------------------------------
#
# Read the module docstring before touching anything below: these are the only
# tests in the tree that execute a committed shim byte, and folding them onto
# `test_windows_shim.py`'s `OCX_SHIM_BINARY` fixture silently un-tests it.

windows_only = pytest.mark.skipif(
    sys.platform != "win32",
    reason="the committed shim blob is a Windows PE; it can only be executed on Windows",
)

#: Two arches ship; `include_bytes!` picks by target, this picks by host.
_BLOB_BY_MACHINE = {
    "amd64": "ocx-shim-x86_64.exe",
    "x86_64": "ocx-shim-x86_64.exe",
    "arm64": "ocx-shim-aarch64.exe",
    "aarch64": "ocx-shim-aarch64.exe",
}


def _committed_blob() -> Path:
    """``crates/ocx_lib/src/shims/ocx-shim-<arch>.exe`` — the shipped artefact.

    Deliberately **not** ``OCX_SHIM_BINARY`` and not a ``target/`` build. This
    is the file ``include_bytes!`` embeds and a user's trampoline slot is a
    hardlink of; a locally built substitute is a different binary that happens
    to be built from the same source, and every existing Windows shim test
    already covers that one.

    Asserts rather than skips: a missing blob is a broken checkout, and a whole
    case skipping itself away is indistinguishable from a pass.
    """
    machine = platform.machine().lower()
    name = _BLOB_BY_MACHINE.get(machine)
    assert name, f"no committed shim blob for host machine {machine!r}"
    blob = Path(__file__).resolve().parents[2] / "crates" / "ocx_lib" / "src" / "shims" / name
    assert blob.is_file(), f"the committed shim blob is missing: {blob}"
    return blob


def _install_trampoline_slot(bin_dir: Path, stem: str, selector: str, baked_ocx: Path | None = None) -> Path:
    """``<stem>.exe`` (a byte copy of the committed blob) + ``<stem>.exec``.

    The registry-free equivalent of what ``render_toolchain`` writes into
    ``<home>/bin/``. The sidecar grammar: line one is an absolute project root
    or the literal ``global``; line two, **optional**, is the absolute ``ocx``
    the rendering ocx resolved (V-9). Each line ends in a single LF, UTF-8,
    no BOM.

    ``baked_ocx=None`` writes the one-line form, which stays valid — that is
    the degraded arm, and the rows that predate V-9 keep exercising it.
    """
    bin_dir.mkdir(parents=True, exist_ok=True)
    exe = bin_dir / f"{stem}.exe"
    exe.write_bytes(_committed_blob().read_bytes())
    body = f"{selector}\n" if baked_ocx is None else f"{selector}\n{baked_ocx}\n"
    (bin_dir / f"{stem}.exec").write_bytes(body.encode())
    return exe


def _install_shimref_slot(bin_dir: Path, stem: str, identifier: str) -> Path:
    """``<stem>.exe`` (a byte copy of the committed blob) + ``<stem>.shimref``.

    The deferred-tool grammar (C-017): one line, an OCI pinned identifier.
    ``core::strips_tier_selectors`` answers **false** for it (RUL-14 / D-V20),
    which is the whole reason this slot exists here — it is the same shipped
    blob dispatching through the one sidecar shape that must NOT delete the
    tier selectors, and therefore the observable RED for C-033's case above.
    """
    bin_dir.mkdir(parents=True, exist_ok=True)
    exe = bin_dir / f"{stem}.exe"
    exe.write_bytes(_committed_blob().read_bytes())
    (bin_dir / f"{stem}.shimref").write_bytes(f"{identifier}\n".encode())
    return exe


def _pin_recorder(directory: Path, log: Path, *, echo_env: bool = False) -> Path:
    """An ``ocx.cmd`` that records the argv the shim built, then exits 0.

    Pinned through ``OCX_BINARY_PIN``, which the shim resolves via an explicit
    ``lpApplicationName`` — the same branch ``test_windows_shim.py`` uses to
    observe forwarded argv, and the only way to see the wire line without an
    ``ocx`` on the machine.

    V-9 gave it a second role: the ``.exec`` sidecar's **baked** second line
    reaches the same explicit ``lpApplicationName`` branch, so this recorder is
    equally what a trampoline spawns with no pin defined at all.

    With ``echo_env``, it also dumps every ``OCX_*`` name the child inherited
    (``SET OCX_``), which prints one ``NAME=value`` line per **defined**
    variable and nothing at all for an undefined one. That is the only spelling
    that separates C-033's three candidate states — deleted, inherited, or set
    to the empty string — because ``SET`` prints ``NAME=`` for a defined-empty
    variable and omits the name entirely for a deleted one.

    Deliberately not ``ECHO [%OCX_GLOBAL%]``: an undefined variable is echoed
    as its own literal ``%NAME%`` only at cmd's **interactive prompt**. Inside
    a batch file — which is how this recorder is always run — percent expansion
    substitutes the empty string instead, so the literal never appears and
    ``[]`` cannot be told from a defined-but-empty variable.
    """
    directory.mkdir(parents=True, exist_ok=True)
    script = directory / "ocx.cmd"
    body = "@ECHO off\r\n" + f'>"{log}" ECHO %*\r\n'
    if echo_env:
        body += f'>>"{log}" SET OCX_\r\n'
    script.write_text(body + "EXIT /B 0\r\n", encoding="utf-8")
    return script


def _dispatch(exe: Path, *args: str, env: dict[str, str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(exe), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=TIMEOUT,
    )


@windows_only
def test_item15_dispatch_the_committed_blob_builds_the_project_wire_line(
    tmp_path: Path,
) -> None:
    """C-032, on the committed blob: the root flag comes **first**, then the verb.

    ``--project`` and ``--global`` are root-level flags on ``ocx`` itself, not
    arguments of ``exec``, so the trampoline's wire line breaks the shape of
    its two siblings: ``<program> --project "<root>" exec -- "<stem>" <argv…>``.
    User arguments follow the stem verbatim.

    This executes ``crates/ocx_lib/src/shims/ocx-shim-*.exe`` — the shipped
    bytes — and must never be re-pointed at ``OCX_SHIM_BINARY`` or at
    ``test_windows_shim.py``'s ``shim_entrypoint``; see the module docstring.

    RED: emit the flag after the verb in ``build_child_child_command_line``'s
    ``Sidecar::ToolchainHome`` arm (``crates/ocx_shim/src/core.rs``).
    """
    stem = "tcbin"
    root = tmp_path / "project"
    root.mkdir()
    exe = _install_trampoline_slot(tmp_path / "bin", stem, str(root))
    log = tmp_path / "argv.txt"
    pin = _pin_recorder(tmp_path / "fake_ocx", log)

    result = _dispatch(
        exe,
        "--flag",
        "value with space",
        env={**os.environ, "OCX_BINARY_PIN": str(pin)},
        cwd=tmp_path,
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"the pinned recorder must run; rc={result.returncode} stderr={result.stderr!r}"
    )
    recorded = log.read_text(encoding="utf-8").strip()
    # Ordered, not anchored: `append_quoted_arg` quotes only what needs it, so
    # what is contractual is the token ORDER — root flag, root, verb, `--`,
    # stem, then the user's argv verbatim — never a fixed quoting spelling.
    assert re.search(
        rf'--project\s+"?{re.escape(str(root))}"?\s+exec\s+--\s+"?{stem}"?\s+--flag\s+"value with space"',
        recorded,
    ), f"C-032's project wire line; got {recorded!r}"


@windows_only
def test_item15_dispatch_the_committed_blob_builds_the_global_wire_line(
    tmp_path: Path,
) -> None:
    """C-032's global arm: ``--global`` carries **no value token**.

    An empty string in the value slot would make ``ocx`` read ``exec`` as the
    flag's argument, so the arm cannot be written as "the project arm with an
    empty root" — it is a separate branch, and this is what proves it.

    Executes the committed blob; see the module docstring for why it must not
    share ``test_windows_shim.py``'s fixture.

    RED: emit ``--global <value>`` in ``build_child_command_line``.
    """
    stem = "gtool"
    exe = _install_trampoline_slot(tmp_path / "bin", stem, "global")
    log = tmp_path / "argv.txt"
    pin = _pin_recorder(tmp_path / "fake_ocx", log)

    result = _dispatch(exe, "--version", env={**os.environ, "OCX_BINARY_PIN": str(pin)}, cwd=tmp_path)

    assert result.returncode == EXIT_SUCCESS, (
        f"the pinned recorder must run; rc={result.returncode} stderr={result.stderr!r}"
    )
    recorded = log.read_text(encoding="utf-8").strip()
    # `exec` follows `--global` directly: nothing — not even `""` — sits in the
    # value slot. That adjacency is the whole assertion.
    assert re.search(rf'--global\s+exec\s+--\s+"?{stem}"?\s+--version', recorded), (
        f"`--global` takes no value token; got {recorded!r}"
    )
    assert "global global" not in recorded and '--global ""' not in recorded, (
        f"`--global` must emit no value token at all; got {recorded!r}"
    )


@windows_only
def test_item38_the_committed_blob_strips_ocx_global_and_ocx_project_before_the_spawn(
    tmp_path: Path,
) -> None:
    """Item 38 / C-033 on Windows: the shim deletes both tier selectors.

    The Windows counterpart of the POSIX body's ``unset`` — two
    ``SetEnvironmentVariableW(name, NULL)`` calls against the shim's *own*
    environment block, which is what the child inherits because
    ``CreateProcessW`` runs with ``lpEnvironment = NULL``. Without them one
    exported ``OCX_GLOBAL`` makes every trampoline on that ``PATH`` exit 64 for
    a flag nobody typed.

    The recorder dumps ``SET OCX_``, which lists one ``NAME=value`` line per
    **defined** variable: ``OCX_GLOBAL=`` is absent exactly when the name is
    gone, present (as ``OCX_GLOBAL=`` alone) if the shim had merely emptied it,
    and present with its value if the child inherited it. ``OCX_BINARY_PIN`` is
    defined in this very invocation and is asserted present, so the dump's
    ability to *show* a live ``OCX_*`` name is demonstrated in-band rather than
    assumed — the two absences below are not a dump that silently did nothing.

    Executes the committed blob; see the module docstring.

    RED: delete the two ``SetEnvironmentVariableW`` calls in
    ``crates/ocx_shim/src/main.rs``, or narrow ``core::strips_tier_selectors``
    away from the ``.exec`` sidecar. Neither can be run against the *committed*
    blob without re-cutting it, so the RED is shown instead by
    :func:`test_v20_a_shimref_sidecar_does_not_strip_the_tier_selectors`: the
    same blob, the same recorder, the same assertions, over the sidecar
    ``strips_tier_selectors`` answers ``false`` for — which is byte-for-byte
    the output a shim with those two calls deleted would produce here.
    """
    stem = "stripped"
    root = tmp_path / "project"
    root.mkdir()
    exe = _install_trampoline_slot(tmp_path / "bin", stem, str(root))
    log = tmp_path / "argv.txt"
    pin = _pin_recorder(tmp_path / "fake_ocx", log, echo_env=True)

    result = _dispatch(
        exe,
        env={
            **os.environ,
            "OCX_BINARY_PIN": str(pin),
            "OCX_GLOBAL": "leaked-global",
            "OCX_PROJECT": r"C:\leaked\project",
        },
        cwd=tmp_path,
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"the pinned recorder must run; rc={result.returncode} stderr={result.stderr!r}"
    )
    recorded = log.read_text(encoding="utf-8")
    assert "OCX_BINARY_PIN=" in recorded, (
        f"the SET OCX_ dump must have run and must show a defined OCX_* name, "
        f"or the two absences below prove nothing; got {recorded!r}"
    )
    assert "OCX_GLOBAL=" not in recorded, (
        f"OCX_GLOBAL must be deleted, not emptied, from the child's block; got {recorded!r}"
    )
    assert "OCX_PROJECT=" not in recorded, (
        f"OCX_PROJECT must be deleted, not emptied, from the child's block; got {recorded!r}"
    )
    assert "leaked" not in recorded, (
        f"an exported selector must not reach the child; got {recorded!r}"
    )


@windows_only
def test_v20_a_shimref_sidecar_does_not_strip_the_tier_selectors(tmp_path: Path) -> None:
    """RUL-14 / D-V20 on the committed blob — and the RED for the case above.

    ``core::strips_tier_selectors`` is true for ``.exec`` and **only** for
    ``.exec``: the two positional grammars bake no tier selector, so they have
    nothing to shadow, and deleting there would change what ``launcher shim``
    resolves for a caller who deliberately exported a tier.

    Two jobs, and the second is why it sits here rather than beside its
    grammar siblings. The case above cannot be shown red by mutating the shim,
    because it executes the *committed* blob and a source mutation would
    require re-cutting it. This row supplies the red instead: same blob, same
    recorder, same ``SET OCX_`` dump, same exported pair — and both names come
    back **defined, with their exported values**. That is exactly the output
    the case above would record if the two ``SetEnvironmentVariableW`` calls
    were deleted, so a dump that could not observe a live selector would fail
    here rather than pass silently there.

    RED: widen ``strips_tier_selectors`` to every sidecar (``|matches!(..)| ->
    true``) — this row loses both selectors and the case above stays green.
    """
    stem = "deferred"
    identifier = "example.test/pkg@sha256:" + "0" * 64
    exe = _install_shimref_slot(tmp_path / "bin", stem, identifier)
    log = tmp_path / "argv.txt"
    pin = _pin_recorder(tmp_path / "fake_ocx", log, echo_env=True)

    result = _dispatch(
        exe,
        env={
            **os.environ,
            "OCX_BINARY_PIN": str(pin),
            "OCX_GLOBAL": "leaked-global",
            "OCX_PROJECT": r"C:\leaked\project",
        },
        cwd=tmp_path,
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"the pinned recorder must run; rc={result.returncode} stderr={result.stderr!r}"
    )
    recorded = log.read_text(encoding="utf-8")
    assert "launcher shim" in recorded, (
        f"the dispatch must have taken the .shimref route, not another grammar; got {recorded!r}"
    )
    assert "OCX_GLOBAL=leaked-global" in recorded, (
        f"a .shimref dispatch must pass an exported OCX_GLOBAL through untouched; got {recorded!r}"
    )
    assert "OCX_PROJECT=C:\\leaked\\project" in recorded, (
        f"a .shimref dispatch must pass an exported OCX_PROJECT through untouched; got {recorded!r}"
    )


@windows_only
def test_v9_a_co_resident_ocx_exe_cannot_capture_a_trampoline_spawn(tmp_path: Path) -> None:
    """V-9: a trampoline spawns the **baked** ocx, never ``bin\\ocx.exe``.

    ``CreateProcessW`` with a NULL ``lpApplicationName`` begins its search at
    *the directory the calling image loaded from* — ``<home>/toolchain/bin``
    itself — before the working directory, the system directories or ``PATH``.
    A package claiming the name ``ocx`` is admitted by design (item 16 / ADR
    D-4 removed ``ShimNameShadowsOcx``), and ``ocx pull`` then renders
    ``bin\\ocx.exe`` + ``bin\\ocx.exec`` beside every other trampoline. With no
    baked program every one of them resolves the literal ``ocx`` to that
    co-resident sibling — unbounded re-entry, and PATH-independent, so C-010's
    ambient-``PATH`` exclusion and C-069's trampoline refusal (both inside a
    *running* ocx) never execute.

    POSIX has had the narrowing from the start — ``unix_trampoline_body`` bakes
    ``__ocx_binary`` as an absolute path. This is its Windows half: the
    ``.exec`` sidecar's second line, passed as an explicit
    ``lpApplicationName`` so no search happens at all.

    The layout is the failing one verbatim: ``tcbin`` and ``ocx`` are both
    rendered trampolines in one ``bin/``. The decoy carries its own baked
    recorder so a regression is *observed* rather than inferred — and so it
    terminates instead of fork-bombing the runner.

    ``OCX_BINARY_PIN`` is deliberately absent: with a pin defined the baked
    line is never consulted and the case would pass against the defect.

    RED: drop the second line from ``exec_sidecar_body``
    (``crates/ocx_lib/src/package_manager/launcher/body.rs``), stop parsing it
    in ``parse_exec_sidecar``, or let ``resolve_program`` prefer its literal
    over the baked path (``crates/ocx_shim/src/core.rs``) — the baked log stays
    absent in all three.
    """
    root = tmp_path / "project"
    root.mkdir()
    decoy_root = tmp_path / "decoy-project"
    decoy_root.mkdir()

    baked_log = tmp_path / "baked-argv.txt"
    decoy_log = tmp_path / "decoy-argv.txt"
    baked = _pin_recorder(tmp_path / "real_ocx", baked_log)
    decoy = _pin_recorder(tmp_path / "decoy_ocx", decoy_log)

    bin_dir = tmp_path / "bin"
    exe = _install_trampoline_slot(bin_dir, "tcbin", str(root), baked_ocx=baked)
    # The package that claims the name `ocx`, rendered exactly where the ADR
    # says it may be. Its own baked recorder is the tripwire.
    _install_trampoline_slot(bin_dir, "ocx", str(decoy_root), baked_ocx=decoy)

    environment = {key: value for key, value in os.environ.items() if key != "OCX_BINARY_PIN"}

    result = _dispatch(exe, "--version", env=environment, cwd=tmp_path)

    assert result.returncode == EXIT_SUCCESS, (
        f"the baked recorder must run; rc={result.returncode} stderr={result.stderr!r}"
    )
    assert baked_log.is_file(), f"the trampoline must spawn the baked ocx at {baked}; stderr={result.stderr!r}"
    assert not decoy_log.exists(), (
        "the co-resident bin\\ocx.exe captured the spawn — R-W12's self-resolution loop: "
        f"{decoy_log.read_text(encoding='utf-8')!r}"
    )
    recorded = baked_log.read_text(encoding="utf-8").strip()
    assert re.search(rf'--project\s+"?{re.escape(str(root))}"?\s+exec\s+--\s+"?tcbin"?\s+--version', recorded), (
        f"and it is still C-032's wire line, unchanged by the second sidecar line; got {recorded!r}"
    )


@windows_only
def test_v9_a_one_line_exec_sidecar_still_dispatches(tmp_path: Path) -> None:
    """The degraded arm stays valid: a sidecar with no baked line still runs.

    The positive control for the row above. Without it a reader could satisfy
    V-9 by making the second line *mandatory* — which would break every
    trampoline rendered by an ocx that could not resolve its own path, the
    population ``generate.rs``'s rung ladder calls a rounding error and does
    not refuse. There the literal ``ocx`` is the answer and the
    self-resolution loop is the accepted narrow risk, exactly as on POSIX.

    ``OCX_BINARY_PIN`` supplies the program here because the one-line form
    names none — that is the whole of the arm.

    RED: make the baked line required in ``parse_exec_sidecar``.
    """
    root = tmp_path / "project"
    root.mkdir()
    exe = _install_trampoline_slot(tmp_path / "bin", "onlyline", str(root))
    log = tmp_path / "argv.txt"
    pin = _pin_recorder(tmp_path / "fake_ocx", log)

    result = _dispatch(exe, env={**os.environ, "OCX_BINARY_PIN": str(pin)}, cwd=tmp_path)

    assert result.returncode == EXIT_SUCCESS, (
        f"a one-line sidecar is still the frozen grammar; rc={result.returncode} stderr={result.stderr!r}"
    )
    assert log.is_file(), f"the pinned recorder must run; stderr={result.stderr!r}"
