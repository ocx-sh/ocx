# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the rendered toolchain tree.

Two halves, both traced to ``.claude/state/plans/plan_toolchain_activation.md``:

* **WP-15's C-070** — every composing emitter heals the groups it is about to
  emit, not just the default one (C-065, C-067, C-070, S-006; ruling RUL-96).
* **WP-12a's render / prune / stamp / config-refusal suite** — ADR validation
  items 2, 12, 13, 14, 17, 18, 23, 24, 25, 31, 32, 33, 34, 36 and scenario
  S-004, at the *tree* level.

Where WP-8's ``test_toolchain_cli.py`` already covers the CLI half of an item
(does ``pull`` render, does ``remove`` prune, does a skip roll the commit back),
the case here observes the **tree**: which artefacts survive, what the stamp
records, and whether two renders are byte-identical. It is not a second copy of
the same assertion.

Why ``ocx exec -g ci`` is the discriminating invocation
-------------------------------------------------------
C-062 narrows the *non-composing* ``bin``-mode prompt path to the default
group, and that narrowing must not leak here. Without C-070, ``ocx exec -g ci``
in the following lane composes through a stale ``ci/<entry>`` link after a
branch switch and runs the previous package while the lock says otherwise —
S-006's stamp protection covers the default group only, so nothing else catches
it. **RED for the two C-070 cases: narrow the heal back to the default group.**

The unit half of C-067's per-entry degrade lives in
``crates/ocx_lib/src/package_manager/composer.rs``; these cases exist because
nothing there can prove the ``-g`` set actually reaches ``ToolchainLinks::groups``
through the CLI.

Identity, not spelling
----------------------
``bin/<name>`` is keyed on the exposed **binary name**; ``<group>/<entry>`` on
the ``[tools]`` **key**. ``src.toolchain_fixtures`` mints a distinct
UUID-prefixed spelling for each namespace, so no assertion about one can be
satisfied by the other — see
:func:`~src.toolchain_fixtures.assert_key_and_binary_namespaces_stay_disjoint`.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
from pathlib import Path
from uuid import uuid4

import pytest

from src.assertions import assert_symlink_exists
from src.helpers import make_package, push_managed_config, write_ocx_toml
from src.runner import OcxRunner
from src.toolchain_fixtures import (
    DEFAULT_GROUP,
    assert_key_and_binary_namespaces_stay_disjoint,
    back_reference_entries,
    bin_entries,
    git,
    link_entries,
    locked_project,
    read_render_stamp,
    render_stamp_path,
    resolved_toolchain_home,
    run_in,
    sha256_of,
    snapshot_tree,
    toolchain_home,
    two_branch_checkout,
    write_toolchain_dir_config,
)

EXIT_SUCCESS = 0
EXIT_CONFIG = 78  # every `toolchain-dir` refusal, and every name refusal


# ---------------------------------------------------------------------------
# C-070 — a composing emitter heals every group it selected (WP-15)
# ---------------------------------------------------------------------------


def test_exec_scoped_to_a_named_group_repoints_that_groups_stale_link(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-070 / S-006 — ``ocx exec -g ci`` heals ``ci`` before it composes.

    The branch-switch shape: the ``ci`` link names a package the lock no longer
    selects. C-067's per-entry degrade means the *command* still runs the right
    binary either way, so the discriminating observable is the link itself —
    a heal narrowed to the default group leaves it naming the other package.
    """
    project = locked_project(ocx, tmp_path)

    assert_symlink_exists(project.default_link)
    other_digest_root = os.readlink(project.default_link)

    ci_link = project.group_link
    ci_link.parent.mkdir(parents=True, exist_ok=True)
    if ci_link.is_symlink() or ci_link.exists():
        ci_link.unlink()
    os.symlink(other_digest_root, ci_link)
    assert os.readlink(ci_link) == other_digest_root, (
        "precondition: the ci link names a different real digest root"
    )

    result = run_in(
        ocx, project.directory, "exec", "-g", project.group, "--", project.group_binary
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"C-067: a stale link degrades, never fails the emission; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert project.group_package.marker in result.stdout, (
        f"S-006: the invocation must run the current lock's package; "
        f"marker={project.group_package.marker!r}\nstdout:\n{result.stdout!r}"
    )

    assert_symlink_exists(ci_link)
    assert os.readlink(ci_link) != other_digest_root, (
        "C-070: `ocx exec -g ci` must heal the ci group's link, not the default "
        f"group's; it still names {other_digest_root!r}"
    )


def test_exec_scoped_to_a_named_group_renders_that_group_when_no_tree_exists(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-070 / C-067 — with no rendered tree at all, ``ocx exec -g ci`` still
    succeeds (the degrade is never an error) **and** leaves the ci group's link
    behind.

    The tree is removed wholesale, which is the state both a never-pulled
    project and a render C-050 skipped leave the home in. A heal narrowed to
    the default group recreates ``default/`` and never ``ci/``.
    """
    project = locked_project(ocx, tmp_path)

    shutil.rmtree(project.home)
    assert not project.home.exists(), "precondition: nothing is rendered"

    result = run_in(
        ocx, project.directory, "exec", "-g", project.group, "--", project.group_binary
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"C-067: a home with no rendered tree degrades to digest paths, never "
        f"an error; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert project.group_package.marker in result.stdout, (
        f"the invocation must run the lock's package; "
        f"marker={project.group_package.marker!r}\nstdout:\n{result.stdout!r}"
    )

    assert_symlink_exists(project.group_link)


# ---------------------------------------------------------------------------
# The fixture's own invariant — identity, not spelling
# ---------------------------------------------------------------------------


def test_a_tools_key_is_never_a_bin_entry_and_a_binary_name_is_never_a_link(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-044 / C-045 — the two namespaces the render writes stay disjoint.

    ``bin/`` is keyed on the exposed binary name and ``<group>/`` on the
    ``[tools]`` key. Every other case in this module asserts a path in one of
    those namespaces; if a renderer confused them, those assertions would keep
    passing against the wrong one. This is the only case that says so.

    RED: key ``bin/`` on ``tool.name`` (the ``[tools]`` key) instead of on the
    exposed name.
    """
    project = locked_project(ocx, tmp_path)

    assert bin_entries(project.home) == [project.default_binary], (
        f"C-045 — `bin/` exposes the default group's binary names and nothing "
        f"else; got {bin_entries(project.home)}"
    )
    assert sorted(link_entries(project.home)) == sorted(
        [
            f"{DEFAULT_GROUP}/{project.default_key}",
            f"{project.group}/{project.group_key}",
        ]
    ), f"links are keyed on `[tools]` keys; got {sorted(link_entries(project.home))}"

    assert_key_and_binary_namespaces_stay_disjoint(project)


# ---------------------------------------------------------------------------
# Item 2 — trampoline body injection (C-030)
# ---------------------------------------------------------------------------


def test_a_hostile_project_path_is_single_quoted_into_the_body_and_never_runs(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 2 / C-030 — a project directory whose name carries ``$(...)`` and a
    backtick is baked **inside single quotes**, and nothing executes.

    D-9 makes the project root the only baked value, so the root is the whole
    injection surface. The invocation runs from a watched empty directory: had
    the substitution fired, ``touch pwned`` would have landed there.

    RED: emit the root inside double quotes — the marker file appears and the
    body no longer round-trips.
    """
    hostile = tmp_path / "he$(touch pwned)`id`re"
    hostile.mkdir()
    project = locked_project(ocx, tmp_path, directory=hostile)

    body = project.default_trampoline.read_text()
    assert f"'{hostile}'" in body, (
        f"C-030 — the baked root must appear inside single quotes; body:\n{body}"
    )
    assert f'"{hostile}"' not in body, (
        f"C-030 — the baked root must never be double-quoted; body:\n{body}"
    )

    watched = tmp_path / "watched"
    watched.mkdir()
    invoked = run_in(
        ocx, watched, "--project", str(hostile), "exec", "--", project.default_binary
    )
    assert invoked.returncode == EXIT_SUCCESS, invoked.stderr
    assert project.default_package.marker in invoked.stdout, (
        f"the control: the tool must actually run; stdout:\n{invoked.stdout!r}"
    )
    assert [entry.name for entry in watched.iterdir()] == [], (
        "C-030 — nothing in the hostile path may execute; the watched directory "
        f"gained {[entry.name for entry in watched.iterdir()]}"
    )


@pytest.mark.parametrize(
    ("name", "described"),
    [("quo'te", "'"), ("new\nline", "\\n")],
    ids=["apostrophe", "newline"],
)
def test_a_project_path_carrying_a_launcher_unsafe_character_is_not_rendered(
    ocx: OcxRunner, tmp_path: Path, name: str, described: str
) -> None:
    """Item 2, second half — a root ocx cannot quote is refused **at render**,
    naming the path and the offending character.

    The refusal is C-050's skip rather than a hard failure: a `pull` still exits
    0 and still composes, because DD6 forbids the renderer from blocking a
    pull. What must not happen is a trampoline written with an un-escapable root
    in it.

    RED: drop the launcher-safety check and observe a body emitted for a root
    containing an apostrophe.
    """
    project_dir = tmp_path / name
    project_dir.mkdir()
    project = locked_project(ocx, tmp_path, directory=project_dir, pull=False)

    result = run_in(ocx, project_dir, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"C-050 — an unrenderable home never blocks a pull; rc={result.returncode}\n"
        f"{result.stderr}"
    )
    assert "not rendered" in result.stderr, (
        f"item 2 — the skip must be reported; stderr:\n{result.stderr}"
    )
    assert (
        described in result.stderr
        and str(project_dir.name.split("\n")[0]) in result.stderr
    ), (
        f"item 2 — the refusal names the offending character and the path; "
        f"stderr:\n{result.stderr}"
    )
    assert bin_entries(project.home) == [], (
        f"item 2 — no trampoline may be written for an unquotable root; "
        f"{project.home} holds {bin_entries(project.home)}"
    )


# ---------------------------------------------------------------------------
# Item 12 — render idempotence (C-047), and the one control that discriminates
# ---------------------------------------------------------------------------


def test_two_consecutive_pulls_produce_a_byte_identical_tree(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 12 / C-047 — ``render(render(x)) == render(x)``, observed by walking
    the tree rather than by counting packages.

    R-W39: ``test_project_pull.py::test_pull_idempotent_second_run_no_changes``
    scopes its assertion to a package count under ``packages/`` and never walks
    ``.ocx/toolchain/``, so it passes vacuously for this contract. This case is
    what actually observes the tree.

    RED: rewrite every ``bin/`` entry unconditionally instead of comparing
    against the existing bytes — the tree still *looks* right but the second
    render is not a no-op, which the paired control below then cannot separate
    from a genuine change.
    """
    project = locked_project(ocx, tmp_path)
    first = snapshot_tree(project.home)
    assert first, "the control: the first pull must have rendered something"

    again = run_in(ocx, project.directory, "pull")
    assert again.returncode == EXIT_SUCCESS, again.stderr

    assert snapshot_tree(project.home) == first, (
        "C-047 — two consecutive `ocx pull` runs must leave a byte-identical tree"
    )


def test_rendering_the_same_project_from_a_different_directory_rewrites_every_body(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 12's positive control — the **baked selector** is the one input that
    rewrites a body.

    Explicitly *not* a ``pinned`` flip (C-046 asserts bodies stay identical on
    purpose) and *not* setting ``toolchain-dir`` (that moves the home, not the
    project, so the bodies come out identical). Without a control that can move
    the bytes, the byte-identity assertion above is indistinguishable from a
    snapshot function that always returns the same thing.
    """
    project = locked_project(ocx, tmp_path)
    original = project.default_trampoline.read_bytes()

    moved = tmp_path / "relocated"
    shutil.copytree(project.directory, moved, symlinks=True)
    shutil.rmtree(toolchain_home(moved), ignore_errors=True)
    result = run_in(ocx, moved, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    relocated_body = (
        toolchain_home(moved) / "bin" / project.default_binary
    ).read_bytes()
    assert relocated_body != original, (
        "item 12 — a body bakes the project root, so the same project rendered "
        "from a different absolute directory must produce different bytes; the "
        "byte-identity assertion above cannot discriminate otherwise"
    )
    assert str(moved).encode() in relocated_body, (
        f"C-028 — the relocated body must bake the relocated root; body:\n"
        f"{relocated_body!r}"
    )


# ---------------------------------------------------------------------------
# Item 13 / S-008 — `ocx remove`, the tree half
# ---------------------------------------------------------------------------


def test_remove_takes_both_halves_of_the_removed_tool_and_nothing_else(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 13 / C-044 / S-008 — the tree half beyond WP-8's
    ``test_remove_prunes_the_removed_tool_s_trampoline``.

    That case asserts one path is gone. This one asserts *which* artefacts
    vanish and *which* survive: the removed tool loses its ``bin/`` trampoline
    **and** its ``default/<key>`` link, the now-empty ``default/`` directory goes
    with them (class 3's non-recursive ``remove_dir``, reached here for real
    rather than by planting a stale directory), and the untouched group's link
    and the home's own ``.gitignore`` are byte-identical afterwards.

    RED: prune only ``bin/`` and leave the link (or the reverse) — the surviving
    half is then a link on disk that no lock entry backs.
    """
    project = locked_project(ocx, tmp_path)
    before = snapshot_tree(project.home)
    untouched = {
        key: value
        for key, value in before.items()
        if key
        in {".gitignore", "bin", project.group, f"{project.group}/{project.group_key}"}
    }
    assert len(untouched) == 4, (
        f"the control: the four artefacts that must survive; got {sorted(untouched)}"
    )

    result = run_in(ocx, project.directory, "remove", project.default_key)
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert bin_entries(project.home) == [], (
        f"S-008 — the removed tool's trampoline must be gone; `bin/` holds "
        f"{bin_entries(project.home)}"
    )
    assert list(link_entries(project.home)) == [
        f"{project.group}/{project.group_key}"
    ], (
        f"item 13 — its link must be gone too, and only it; links are "
        f"{sorted(link_entries(project.home))}"
    )
    assert snapshot_tree(project.home) == untouched, (
        "item 13 — every surviving artefact is byte-identical and nothing new "
        f"appears; expected {sorted(untouched)}, got {sorted(snapshot_tree(project.home))}"
    )


# ---------------------------------------------------------------------------
# Item 14 — heal after a `git pull`, and `--dry-run` performing no heal
# ---------------------------------------------------------------------------


def test_a_branch_switch_leaves_a_stale_link_that_the_next_composing_emit_heals(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 14 / C-051 / S-007 — the real ``git pull`` shape, on a checkout this
    test owns exclusively.

    Two branches lock two digests of one tool. Pulling on ``main`` and then
    checking out ``other`` leaves a link that names the *previous* branch's
    package — nothing has re-rendered. The next composing emit heals it, by lock
    arithmetic alone.

    RED: delete the heal from the composing path — the link stays stale and this
    assertion reds while every ``pull``-driven case stays green.
    """
    checkout = two_branch_checkout(ocx, tmp_path)
    pulled = run_in(ocx, checkout.directory, "pull")
    assert pulled.returncode == EXIT_SUCCESS, pulled.stderr

    on_main = os.readlink(checkout.link)
    switched = git(checkout.directory, "checkout", "-q", checkout.other_branch)
    assert switched.returncode == EXIT_SUCCESS, switched.stderr

    assert os.readlink(checkout.link) == on_main, (
        "the control: a branch switch writes nothing, so the link must still "
        "name the previous branch's package before anything composes"
    )

    emitted = run_in(ocx, checkout.directory, "--format", "json", "env")
    assert emitted.returncode == EXIT_SUCCESS, emitted.stderr
    assert os.readlink(checkout.link) != on_main, (
        f"C-051 — the composing emit must heal the link; it still names {on_main!r}"
    )


def test_pull_dry_run_performs_no_heal_on_a_poisoned_link(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 14 / C-049 / S-007 — ``--dry-run`` reports the delta and repairs
    nothing.

    WP-8's ``test_pull_dry_run_over_a_poisoned_tree_writes_nothing`` asserts the
    tree is unwritten. This asserts the narrower, easier-to-lose property: the
    **poisoned link is still poisoned afterwards**. A dry run that healed would
    report a delta it had itself already closed.

    RED: run the heal before the dry-run early return.
    """
    project = locked_project(ocx, tmp_path)
    poison = str(tmp_path)
    project.default_link.unlink()
    os.symlink(poison, project.default_link)

    result = run_in(ocx, project.directory, "pull", "--dry-run")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert os.readlink(project.default_link) == poison, (
        "C-049 — a dry run performs no heal; the poisoned link must survive it"
    )


# ---------------------------------------------------------------------------
# Item 17 — the `bin` reservation and the name charset (C-013, C-014)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("case", "body", "needle"),
    [
        (
            "group_bin",
            '[group.bin.tools]\n{key} = "{fq}"\n',
            "[group] name 'bin' is reserved",
        ),
        ("tools_bin", '[tools]\nbin = "{fq}"\n', "[tools] name 'bin' is reserved"),
        (
            "tools_BIN_case_folded",
            '[tools]\nBIN = "{fq}"\n',
            "[tools] name 'BIN' is reserved",
        ),
        (
            "group_tools_bin",
            '[group.ci.tools]\nbin = "{fq}"\n',
            "[group.ci.tools] name 'bin' is reserved",
        ),
        (
            "group_name_with_a_space",
            '[group."a b".tools]\n{key} = "{fq}"\n',
            "[group] name 'a b' must match",
        ),
        (
            "tool_key_with_a_space",
            '[tools]\n"a b" = "{fq}"\n',
            "[tools] name 'a b' must match",
        ),
    ],
)
def test_the_bin_reservation_and_the_name_charset_are_refused_at_every_site(
    ocx: OcxRunner, tmp_path: Path, case: str, body: str, needle: str
) -> None:
    """Item 17 / C-013 / C-014 — the acceptance half of the reservation.

    All four declaration sites plus both charset sites, each exiting **78** with
    a message naming the site and the offending name. ``BIN`` is the C-015 case
    fold: a reservation that only matched the lower-case spelling would be no
    reservation at all.

    RED: remove the validator and watch ``[tools] bin`` parse cleanly.
    """
    label = uuid4().hex[:8]
    package = make_package(
        ocx, f"t_{label}_resv", "1.0.0", tmp_path, cascade=False, bins=[f"rbin{label}"]
    )
    project = tmp_path / f"proj-{case}"
    project.mkdir()
    write_ocx_toml(project, body.format(key=f"rkey{label}", fq=package.fq))

    result = run_in(ocx, project, "status")
    assert result.returncode == EXIT_CONFIG, (
        f"item 17 — {case} must exit {EXIT_CONFIG}; got {result.returncode}\n"
        f"{result.stderr}"
    )
    assert needle in result.stderr, (
        f"item 17 — the refusal must name the site and the name ({needle!r}); "
        f"stderr:\n{result.stderr}"
    )


def test_a_valid_name_still_parses(ocx: OcxRunner, tmp_path: Path) -> None:
    """Item 17's positive control — a charset-valid, non-reserved name is
    accepted, so the parametrized refusals above cannot be a parser that refuses
    everything."""
    project = locked_project(ocx, tmp_path, pull=False)
    result = run_in(ocx, project.directory, "status")
    assert result.returncode == EXIT_SUCCESS, result.stderr


# ---------------------------------------------------------------------------
# Item 18 — `toolchain-dir` refusals (C-017 … C-019, S-011)
# ---------------------------------------------------------------------------


def _outside_both_anchors(ocx: OcxRunner) -> Path:
    """A directory outside ``$HOME`` and outside ``$OCX_HOME`` — S-011's
    ``/tmp/ocx-tc``, created owner-owned and mode 0700 so the refusal can only be
    containment."""
    candidate = Path("/tmp") / f"ocx-tc-{uuid4().hex[:12]}"
    home = Path(ocx.env["HOME"]).resolve()
    ocx_home = Path(ocx.env["OCX_HOME"]).resolve()
    assert home not in candidate.parents and ocx_home not in candidate.parents, (
        f"precondition: {candidate} must lie outside both containment anchors "
        f"({home}, {ocx_home}) or S-011 tests nothing"
    )
    return candidate


def test_a_containment_refusal_fires_even_for_an_owner_owned_private_directory(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 18 / S-011 / C-017 — ``toolchain-dir = "/tmp/ocx-tc"`` exits 78 even
    when the directory is owner-owned and mode 0700.

    **Containment, not permissions, is what refuses it.** A test that used a
    world-writable directory here would pass under C-019 alone and prove nothing
    about C-017.

    RED: drop the containment check and watch ``/tmp/ocx-tc`` pass.
    """
    project = locked_project(ocx, tmp_path, pull=False)
    root = _outside_both_anchors(ocx)
    root.mkdir(mode=0o700)
    try:
        assert stat.S_IMODE(root.stat().st_mode) == 0o700, (
            "the control: the refused directory must be owner-private, or the "
            "refusal could be C-019's rather than C-017's"
        )
        write_toolchain_dir_config(ocx, root)
        result = run_in(ocx, project.directory, "--format", "json", "shell", "state")
        assert result.returncode == EXIT_CONFIG, (
            f"S-011 — expected {EXIT_CONFIG}; got {result.returncode}\n{result.stderr}"
        )
        assert str(root) in result.stderr, (
            f"item 18 — the refusal names the path; stderr:\n{result.stderr}"
        )
        assert "outside both the home directory and $OCX_HOME" in result.stderr, (
            f"item 18 — …and the failing property; stderr:\n{result.stderr}"
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)


@pytest.mark.parametrize(
    ("case", "needle"),
    [
        ("relative", "is the relative path"),
        ("parent_component", "contains a '..' component"),
        ("is_the_anchor_itself", "which is the containment anchor"),
        ("inside_the_global_toolchain_home", "inside the global toolchain home"),
        ("system_prefix", "resolves to the system location"),
        ("filesystem_root", "resolves to the system location"),
        ("group_or_world_writable", "granting write to group or world"),
        ("not_a_directory", "is not a directory"),
    ],
)
def test_each_toolchain_dir_refusal_exits_78_and_names_its_failing_property(
    ocx: OcxRunner, tmp_path: Path, case: str, needle: str
) -> None:
    """Item 18 / C-017 … C-019, R-W1, R-W2, RUL-4 — several distinct refusal
    variants, not one.

    Per finding R-W32 this acceptance half only became writable once WP-8 gave
    ``ToolchainRoot::resolve`` a production caller: before that, every variant
    was unreachable from the CLI and a green here would have been
    indistinguishable from never having run.

    RED: delete the ``ToolchainRootError`` arm from ``cli::classify`` — every row
    then exits 1 instead of 78, which is the mutation that separates "classified"
    from "reachable".
    """
    project = locked_project(ocx, tmp_path, pull=False)
    ocx_home = Path(ocx.env["OCX_HOME"])

    if case == "relative":
        value: str | Path = "relative/toolchains"
    elif case == "parent_component":
        value = f"{ocx_home.as_posix()}/../escaped"
    elif case == "is_the_anchor_itself":
        value = ocx_home
    elif case == "inside_the_global_toolchain_home":
        value = ocx_home / "toolchain" / "nested"
    elif case == "system_prefix":
        value = "/usr"
    elif case == "filesystem_root":
        value = "/"
    elif case == "group_or_world_writable":
        loose = ocx_home / f"loose-{uuid4().hex[:8]}"
        loose.mkdir()
        os.chmod(loose, 0o777)
        value = loose
    else:
        regular = ocx_home / f"regular-{uuid4().hex[:8]}"
        regular.write_text("not a directory\n")
        value = regular

    write_toolchain_dir_config(ocx, value)
    result = run_in(ocx, project.directory, "--format", "json", "shell", "state")

    assert result.returncode == EXIT_CONFIG, (
        f"item 18 — {case} must exit {EXIT_CONFIG}; got {result.returncode}\n"
        f"{result.stderr}"
    )
    assert needle in result.stderr, (
        f"item 18 — {case} must name its failing property ({needle!r}); "
        f"stderr:\n{result.stderr}"
    )
    assert "toolchain-dir" in result.stderr, (
        f"item 18 — …and the tier that declared it; stderr:\n{result.stderr}"
    )


def test_a_refused_toolchain_dir_also_stops_a_pull_and_names_the_environment_tier(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 18 — the refusal is not a ``shell state`` peculiarity, and it names
    **which** tier declared the value.

    A refusal reported only by the read-only diagnostic would leave the writer
    rendering into a root every check had rejected. The environment tier is
    asserted separately because the message must send an operator to
    ``OCX_TOOLCHAIN_DIR``, not to a ``config.toml`` that says nothing.
    """
    project = locked_project(ocx, tmp_path, pull=False)

    write_toolchain_dir_config(ocx, "/usr")
    blocked = run_in(ocx, project.directory, "pull")
    assert blocked.returncode == EXIT_CONFIG, (
        f"item 18 — a refused root must stop the writer too; rc={blocked.returncode}\n"
        f"{blocked.stderr}"
    )
    assert "config.toml `toolchain-dir`" in blocked.stderr, (
        f"item 18 — the config tier must be named; stderr:\n{blocked.stderr}"
    )

    Path(ocx.env["OCX_HOME"], "config.toml").unlink()
    from_env = run_in(
        ocx,
        project.directory,
        "--format",
        "json",
        "shell",
        "state",
        env_extra={"OCX_TOOLCHAIN_DIR": "/usr"},
    )
    assert from_env.returncode == EXIT_CONFIG, from_env.stderr
    assert "OCX_TOOLCHAIN_DIR" in from_env.stderr, (
        f"item 18 — the environment tier must be named as itself; "
        f"stderr:\n{from_env.stderr}"
    )


# ---------------------------------------------------------------------------
# Item 23 — the registration invariant (C-052)
# ---------------------------------------------------------------------------


def _ledger(ocx: OcxRunner) -> set[str]:
    directory = Path(ocx.env["OCX_HOME"]) / "projects"
    return {p.name for p in directory.iterdir()} if directory.is_dir() else set()


def test_a_project_render_registers_and_a_global_render_registers_nothing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 23 / C-052 — both halves, because "registers nothing" is only a
    claim if the positive half is observed in the same run.

    Rendering ``$OCX_HOME/toolchain`` must add no ``projects/`` entry:
    ``register`` is a no-op for ``$OCX_HOME`` under the no-self-link invariant,
    and an entry there would make the ocx home its own GC root.

    RED: register unconditionally instead of only for ``RenderStampScope::Project``.
    """
    project = locked_project(ocx, tmp_path)
    after_project = _ledger(ocx)
    assert after_project, "C-052 — a project render must create a `projects/` entry"

    stamp = read_render_stamp(ocx, project.home)
    assert stamp is not None, "the control: a project render writes a stamp"
    assert stamp["scope"].get("project") == str(project.directory.resolve()), (
        f"D-V13 — the stamp carries the canonical project directory; got {stamp['scope']}"
    )

    write_ocx_toml(
        Path(ocx.env["OCX_HOME"]), f'[tools]\ngkey = "{project.default_package.fq}"\n'
    )
    assert run_in(ocx, tmp_path, "-g", "lock").returncode == EXIT_SUCCESS
    globally = run_in(ocx, tmp_path, "-g", "pull")
    assert globally.returncode == EXIT_SUCCESS, globally.stderr

    global_home = Path(ocx.env["OCX_HOME"]) / "toolchain"
    assert bin_entries(global_home), (
        "the control: the global render must have produced a tree, or the "
        "negative below is vacuous"
    )
    assert _ledger(ocx) == after_project, (
        f"C-052 — a global render registers nothing; the ledger grew from "
        f"{sorted(after_project)} to {sorted(_ledger(ocx))}"
    )
    assert (Path(ocx.env["OCX_HOME"]) / "state" / "render_stamp.json").exists(), (
        "D-V13 — the global stamp lives at `state/render_stamp.json`, never "
        "under `state/projects/<key>/`"
    )


def test_no_install_back_reference_is_taken_for_any_toolchain_link(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 23 / C-052 — a ``<group>/<entry>`` link takes no ``refs/symlinks/``
    back-reference.

    Writing links through ``ReferenceManager`` — the obvious "use the shipped
    helper" edit — would pin every rendered package forever, so no ``ocx clean``
    could ever collect one. The walker is shown able to report a non-empty
    answer first: a scan that could never find anything would report "clean" in
    every state, including the one this case exists to exclude.

    RED: publish links through ``ReferenceManager::create`` instead of
    ``symlink::replace_atomic``.

    Where the discriminating power actually lives, because it is not obvious
    from the shape: entirely in the negative assertion. The decoy below is a
    **walker control**, not a second contract — it proves the scan can report a
    non-empty answer, nothing more. No mutation of the render-side registration
    can red this case, because ``project/registry.rs:226`` registers the
    project independently of the render; that was measured, not assumed.
    So do not "strengthen" the decoy half into an assertion about the render.
    It would pass in every state and quietly replace the one half that
    discriminates.
    """
    project = locked_project(ocx, tmp_path)
    assert back_reference_entries(ocx) == [], (
        f"C-052 — the render must take no back-reference; found "
        f"{back_reference_entries(ocx)}"
    )

    # Non-vacuity: the same walker must be able to see one.
    target = os.readlink(project.default_link)
    decoy_dir = Path(target).parent / "refs" / "symlinks"
    decoy_dir.mkdir(parents=True, exist_ok=True)
    decoy = decoy_dir / "decoy"
    decoy.symlink_to(project.directory)
    try:
        assert back_reference_entries(ocx), (
            "the walker must be able to report a back-reference, or its empty "
            "answer above is indistinguishable from never having looked"
        )
    finally:
        decoy.unlink()


# ---------------------------------------------------------------------------
# Item 24 — a read-only checkout (C-050)
# ---------------------------------------------------------------------------


def test_a_render_that_cannot_write_warns_on_stderr_and_still_exits_zero(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 24 / C-050 — skip and continue: warn, compose digest paths for this
    run, exit 0.

    The parent directory is made read-only *after* the lock, so the render
    cannot create the home. ``ocx exec`` afterwards is what proves the second
    half — the run still composes, through C-067's per-entry digest fallback,
    rather than merely exiting 0 over a broken environment.

    RED: propagate the write failure — the pull exits non-zero and a read-only
    checkout becomes unusable.
    """
    project = locked_project(ocx, tmp_path, pull=False)
    parent = project.directory / ".ocx"
    shutil.rmtree(project.home, ignore_errors=True)
    os.chmod(parent, 0o555)
    try:
        result = run_in(ocx, project.directory, "pull")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-050 — a write failure never fails the pull; rc={result.returncode}\n"
            f"{result.stderr}"
        )
        assert "Skipping toolchain render" in result.stderr, (
            f"C-050 — the skip is warned on stderr; stderr:\n{result.stderr}"
        )
        assert str(project.home) in result.stderr, (
            f"C-050 — …naming the home it could not prepare; stderr:\n{result.stderr}"
        )
        assert "Skipping toolchain render" not in result.stdout, (
            f"the warning belongs on stderr only; stdout:\n{result.stdout}"
        )
        assert not project.home.exists(), (
            "the control: the home really must not have been created, or this "
            "case never exercised the skip"
        )

        composed = run_in(ocx, project.directory, "exec", "--", project.default_binary)
        assert composed.returncode == EXIT_SUCCESS, (
            f"C-050 — digest paths are composed for this run; rc={composed.returncode}\n"
            f"{composed.stderr}"
        )
        assert project.default_package.marker in composed.stdout, (
            f"…and they resolve the lock's package; stdout:\n{composed.stdout!r}"
        )
    finally:
        os.chmod(parent, 0o755)


# ---------------------------------------------------------------------------
# Item 25 — GC leaves the tree alone (C-001)
# ---------------------------------------------------------------------------


def test_clean_and_clean_force_leave_the_global_toolchain_tree_alone(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 25 / C-001 — ``$OCX_HOME/toolchain/`` sits outside the GC graph.

    Both flags, because ``--force`` is the one that bypasses the project
    registry: a tree retained only because a lock happened to reference it would
    survive plain ``clean`` and vanish under ``--force``.

    RED: walk ``$OCX_HOME/toolchain`` from the sweep root — the tree disappears
    on the first ``clean``.
    """
    project = locked_project(ocx, tmp_path, pull=False)
    write_ocx_toml(
        Path(ocx.env["OCX_HOME"]), f'[tools]\ngkey = "{project.default_package.fq}"\n'
    )
    assert run_in(ocx, tmp_path, "-g", "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, tmp_path, "-g", "pull").returncode == EXIT_SUCCESS

    global_home = Path(ocx.env["OCX_HOME"]) / "toolchain"
    before = snapshot_tree(global_home)
    assert before, "the control: the global tree must exist before `ocx clean` runs"

    for argv in (["clean"], ["clean", "--force"]):
        result = run_in(ocx, tmp_path, *argv)
        assert result.returncode == EXIT_SUCCESS, (
            f"`ocx {' '.join(argv)}`:\n{result.stderr}"
        )
        assert snapshot_tree(global_home) == before, (
            f"C-001 — `ocx {' '.join(argv)}` must leave the toolchain tree untouched"
        )


# ---------------------------------------------------------------------------
# Item 31 — prune exactness, all four orphan classes (C-044)
# ---------------------------------------------------------------------------


def test_an_orphan_trampoline_is_pruned_and_every_group_link_is_left_alone(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 31, class 1 / C-044 — a ``bin/`` name outside the computed set is
    removed, and the link half of the tree is untouched.

    This is the shape a vanished ``binaries`` claim leaves behind, and the shape
    S-003's force-committed hostile ``bin/`` entry leaves behind: a file on a
    directory that lands on PATH which no lock entry backs.

    RED: skip the ``bin/`` prune pass — the orphan survives every later render.
    """
    project = locked_project(ocx, tmp_path)
    links_before = link_entries(project.home)

    orphan = project.home / "bin" / f"ghost{uuid4().hex[:6]}"
    orphan.write_text("#!/bin/sh\necho pwned\n")
    os.chmod(orphan, 0o755)

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert not orphan.exists(), (
        f"C-044 — `bin/` is a whole-directory reconcile; {orphan.name} must be "
        f"pruned. `bin/` holds {bin_entries(project.home)}"
    )
    assert bin_entries(project.home) == [project.default_binary], (
        f"…and nothing inside the computed set may go with it; got "
        f"{bin_entries(project.home)}"
    )
    assert link_entries(project.home) == links_before, (
        "class 1 — an orphan trampoline takes no link with it"
    )


def test_an_entry_that_left_the_lock_loses_both_its_link_and_its_trampoline(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 31, class 2 / C-044 — dropping a tool from ``ocx.toml`` removes both
    halves in one render.

    Distinct from item 13's ``ocx remove`` row: the edit happens in the file and
    the re-render arrives through ``ocx lock``, so this covers the path a ``git
    pull`` takes rather than the path the CLI mutation command takes.

    RED: prune one half only — the surviving half is a live artefact for a tool
    the lock no longer names.
    """
    label = uuid4().hex[:8]
    keeper = make_package(
        ocx,
        f"t_{label}_keep",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"keepbin{label}"],
    )
    leaver = make_package(
        ocx,
        f"t_{label}_leave",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"leavebin{label}"],
    )
    project = tmp_path / f"proj-{label}"
    project.mkdir()
    both = f'[tools]\nkeepkey{label} = "{keeper.fq}"\nleavekey{label} = "{leaver.fq}"\n'
    write_ocx_toml(project, both)
    assert run_in(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, project, "pull").returncode == EXIT_SUCCESS

    home = toolchain_home(project)
    assert sorted(bin_entries(home)) == sorted([f"keepbin{label}", f"leavebin{label}"])

    write_ocx_toml(project, f'[tools]\nkeepkey{label} = "{keeper.fq}"\n')
    relocked = run_in(ocx, project, "lock")
    assert relocked.returncode == EXIT_SUCCESS, relocked.stderr

    assert bin_entries(home) == [f"keepbin{label}"], (
        f"class 2 — the departed entry's trampoline must be gone; got {bin_entries(home)}"
    )
    assert sorted(link_entries(home)) == [f"{DEFAULT_GROUP}/keepkey{label}"], (
        f"class 2 — …and its link with it; got {sorted(link_entries(home))}"
    )


def test_an_empty_orphan_group_directory_is_removed_and_a_populated_one_is_only_skipped(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 31, class 3 / C-044 / RUL-32 — the group-directory prune is a
    **non-recursive** ``remove_dir``.

    An emptied orphan group goes; one that still holds an entry is reported and
    left in place, because the only alternative is a recursive delete inside a
    directory the ADR itself calls attacker-writable. Both arms are asserted, so
    "the prune works" cannot be read as "the prune deletes whatever it finds".

    RED for the first arm: never scan the home root for orphan groups. RED for
    the second: swap ``remove_dir`` for ``remove_dir_all`` — the payload below
    disappears.
    """
    project = locked_project(ocx, tmp_path)

    empty = project.home / f"stale{uuid4().hex[:6]}"
    empty.mkdir()
    populated = project.home / f"held{uuid4().hex[:6]}"
    populated.mkdir()
    payload = populated / "payload"
    payload.write_text("keep me\n")

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"RUL-32 — a skip never aborts the render; rc={result.returncode}\n{result.stderr}"
    )

    assert not empty.exists(), (
        f"class 3 — an emptied orphan group directory is removed; {empty} survived"
    )
    assert payload.read_text() == "keep me\n", (
        "class 3 — a populated orphan group is never removed recursively"
    )
    assert populated.name in result.stderr, (
        f"…and the skip is reported, naming it; stderr:\n{result.stderr}"
    )
    assert bin_entries(project.home) == [project.default_binary], (
        "…and the render continued past the skip"
    )


def test_an_orphan_directory_in_bin_is_skipped_and_never_removed_recursively(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 31, class 4 / RUL-32 — an orphan that is itself a **directory**
    occupying a ``bin/`` name slot is reported ``Skipped`` and the render
    continues.

    Exit 0 with a warning on stderr only (``skipped_render_warnings``), never a
    recursive delete and never an aborted render — one hostile directory must
    not cost every later entry.

    RED: propagate the error instead of reporting a skip.
    """
    project = locked_project(ocx, tmp_path)
    hostile = project.home / "bin" / f"hostile{uuid4().hex[:6]}"
    hostile.mkdir()
    (hostile / "payload").write_text("keep me\n")

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"RUL-32 — the render continues; rc={result.returncode}\n{result.stderr}"
    )
    assert (hostile / "payload").read_text() == "keep me\n", (
        "class 4 — the directory is never removed recursively"
    )
    assert hostile.name in result.stderr, (
        f"class 4 — the skip names the path; stderr:\n{result.stderr}"
    )
    assert hostile.name not in result.stdout, (
        f"the skip warning belongs on stderr only; stdout:\n{result.stdout}"
    )
    assert project.default_binary in bin_entries(project.home), (
        "class 4 — later entries still render past the skip"
    )


# ---------------------------------------------------------------------------
# Item 32 — stamp coverage and mtime independence (C-003)
# ---------------------------------------------------------------------------


def test_the_render_stamp_covers_every_name_body_and_link_target(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 32, first half / C-003 — the fingerprint really describes the tree.

    ``names`` must equal the on-disk ``bin/`` entry set (the stamp's own stated
    invariant), every ``bin_fingerprint`` entry's ``content_hash`` must equal the
    file's bytes, and ``link_fingerprint`` must equal the default group's
    ``readlink`` values — default group only, since widening it would mismatch
    on every prompt for a group the prompt path cannot heal.

    RED: derive ``link_fingerprint`` from the lock rather than from the tree, or
    stamp a rolled-up digest instead of one entry per name.
    """
    project = locked_project(ocx, tmp_path)
    stamp = read_render_stamp(ocx, project.home)
    assert stamp is not None, "a project render writes a stamp"

    names = bin_entries(project.home)
    assert sorted(stamp["names"]) == names, (
        f"C-003 — `names` is the on-disk entry set; stamp {sorted(stamp['names'])} "
        f"vs disk {names}"
    )
    assert sorted(stamp["bin_fingerprint"]) == names, (
        "C-003 — `names` and `bin_fingerprint` encode one fact and must agree"
    )
    for name, entry in stamp["bin_fingerprint"].items():
        path = project.home / "bin" / name
        assert entry["content_hash"] == sha256_of(path), (
            f"C-003 — {name}'s content hash must be the file's bytes"
        )
        assert entry["size"] == path.stat().st_size, (
            f"C-003 — {name}'s stat half must match the file"
        )

    links = link_entries(project.home)
    default_links = {
        k: v for k, v in links.items() if k.startswith(f"{DEFAULT_GROUP}/")
    }
    assert stamp["link_fingerprint"] == default_links, (
        f"C-003 — `link_fingerprint` is the default group's targets, observed; "
        f"stamp {stamp['link_fingerprint']} vs disk {default_links}"
    )
    assert not any(
        k.startswith(f"{project.group}/") for k in stamp["link_fingerprint"]
    ), "C-003 — a non-default group must never enter `link_fingerprint`"


def test_shifting_a_trampolines_mtime_perturbs_neither_the_stamp_nor_the_tree(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 32, second half / C-003 — **mtime is not consulted**.

    No field of the stamp records one, a shifted-but-byte-identical trampoline
    re-renders to the same stamp, and the file is not rewritten (its inode
    survives). The paired positive control is the tamper below: a body whose
    *bytes* changed is noticed and restored, so "nothing happened" cannot be
    read as "the renderer never looks".

    RED: fold mtime into the fingerprint and watch a ``touch``-shifted but
    byte-identical tree invalidate.
    """
    project = locked_project(ocx, tmp_path)
    stamp_file = render_stamp_path(ocx, project.home)
    assert stamp_file is not None
    before = json.loads(stamp_file.read_text())
    assert "mtime" not in stamp_file.read_text(), (
        f"C-003 — no field of the stamp records an mtime:\n{stamp_file.read_text()}"
    )

    trampoline = project.default_trampoline
    inode = trampoline.lstat().st_ino
    os.utime(trampoline, (1_000_000, 1_000_000))

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert json.loads(stamp_file.read_text()) == before, (
        "C-003 — an mtime-only change must leave the stamp identical"
    )
    assert trampoline.lstat().st_ino == inode, (
        "…and must not cause a rewrite: the entry's identity survives"
    )
    assert trampoline.lstat().st_mtime == 1_000_000, (
        "the control: the shifted mtime must still be shifted, or nothing was "
        "actually perturbed"
    )

    # The positive control: a byte change *is* noticed.
    tampered = trampoline.read_text() + "# tamper\n"
    trampoline.write_text(tampered)
    assert run_in(ocx, project.directory, "pull").returncode == EXIT_SUCCESS
    assert not trampoline.read_text().endswith("# tamper\n"), (
        "C-003 — a changed body must be noticed and rewritten, or the mtime "
        "assertion above cannot tell 'unchanged' from 'never inspected'"
    )
    restored = json.loads(stamp_file.read_text())
    assert restored["names"] == before["names"]
    assert {
        name: entry["content_hash"]
        for name, entry in restored["bin_fingerprint"].items()
    } == {
        name: entry["content_hash"] for name, entry in before["bin_fingerprint"].items()
    }, "…and the restored tree stamps back to the same content fingerprint"
    assert (
        restored["bin_fingerprint"][project.default_binary]["file_id"]
        != before["bin_fingerprint"][project.default_binary]["file_id"]
    ), (
        "…while `file_id` moves, because the repair is an atomic replace — which "
        "is why the mtime assertion above checks the inode rather than the stamp "
        "alone"
    )


# ---------------------------------------------------------------------------
# Item 33 — the `pinned` flip, structural half (C-046, S-005, RUL-23)
# ---------------------------------------------------------------------------


def test_pinned_suppresses_the_whole_link_pass_and_leaves_bin_byte_identical(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 33, structural half / C-046 / RUL-23 — ``pinned = true`` suppresses
    the link pass **whole**: no writes and **no prunes**.

    Note this deliberately contradicts the ADR's item 33 wording ("all
    ``<group>/<entry>`` links are deleted"), which RUL-23 (D-V28) overturned:
    deleting them would make the flip asymmetric — switching ``pinned`` on would
    destroy live derived state and switching it back off would then *require* a
    re-render, against item 33's own "the flag takes effect with no re-render".

    The discriminating observable is therefore a **poisoned** link: a pinned
    render must neither repoint it nor remove it, and the very next unpinned
    render must repoint it. ``bin/`` is byte-identical throughout, because a body
    bakes no digest.

    RED: reconcile links against an empty computed set under ``pinned`` — the
    poisoned link is deleted and the "no re-render needed" property goes with it.
    """
    project = locked_project(ocx, tmp_path)
    bin_before = snapshot_tree(project.home / "bin")
    poison = str(tmp_path)
    project.default_link.unlink()
    os.symlink(poison, project.default_link)

    write_ocx_toml(project.directory, project.config_body(pinned=True))
    pinned = run_in(ocx, project.directory, "pull")
    assert pinned.returncode == EXIT_SUCCESS, pinned.stderr

    assert os.readlink(project.default_link) == poison, (
        "RUL-23 — a pinned render writes no link, so the poisoned target stands"
    )
    assert project.group_link.is_symlink(), (
        "RUL-23 — …and prunes none either, so every other link stays on disk"
    )
    assert snapshot_tree(project.home / "bin") == bin_before, (
        "C-046 — every trampoline body is byte-identical under `pinned`; a body "
        "bakes no digest"
    )

    stamp = read_render_stamp(ocx, project.home)
    assert stamp is not None
    assert stamp["link_fingerprint"] == {
        f"{DEFAULT_GROUP}/{project.default_key}": poison
    }, (
        "RUL-27 — the stamp describes the tree as it stands on disk, never the "
        f"tree the render intended; got {stamp['link_fingerprint']}"
    )

    write_ocx_toml(project.directory, project.config_body(pinned=False))
    unpinned = run_in(ocx, project.directory, "pull")
    assert unpinned.returncode == EXIT_SUCCESS, unpinned.stderr

    assert os.readlink(project.default_link) != poison, (
        "C-046 — flipping back re-renders the links"
    )
    assert snapshot_tree(project.home / "bin") == bin_before, (
        "C-046 — …and still leaves `bin/` byte-identical, in both directions"
    )


# ---------------------------------------------------------------------------
# Item 34 — an abandoned tree on a `toolchain-dir` change (C-053)
# ---------------------------------------------------------------------------


def test_relocating_the_home_renders_the_new_tree_and_abandons_the_old_one_in_place(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 34 / C-053 — render and prune act **only** inside the resolved home.

    A tree at a location ocx no longer resolves to is left in place, never
    deleted: a mistaken ``[managed]`` push must not destroy trees across a
    fleet. ``ocx shell state`` naming the new home is what makes the old one
    observably abandoned rather than merely stale.

    RED: prune the previous location on a config change and watch a tree deleted
    by a `config.toml` edit alone.
    """
    project = locked_project(ocx, tmp_path)
    abandoned = snapshot_tree(project.home)
    assert abandoned, "the control: the in-project tree must exist before the move"

    root = Path(ocx.env["OCX_HOME"]) / "relocated"
    root.mkdir()
    write_toolchain_dir_config(ocx, root)

    relocated = resolved_toolchain_home(ocx, project.directory)
    assert root in relocated.parents, (
        f"the control: the home must have moved under {root}; got {relocated}"
    )
    assert relocated != project.home

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert bin_entries(relocated) == [project.default_binary], (
        f"item 34 — the new home renders; it holds {bin_entries(relocated)}"
    )
    assert snapshot_tree(project.home) == abandoned, (
        "C-053 — the tree at the no-longer-resolved location is left in place, "
        "byte for byte"
    )


# ---------------------------------------------------------------------------
# Item 36 — a project deleted or moved, the prune half (C-052)
# ---------------------------------------------------------------------------


def test_clean_prunes_a_departed_projects_ledger_entry_and_stamp_only(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Item 36, prune half / C-052 — a departed project's GC-ledger entry and
    render stamp are swept, and a sibling's are untouched.

    The keyed **tree** is not swept, which is C-053's rule holding in the one
    place it is easiest to violate: ``ocx clean`` knows the ledger, not the
    ``toolchain-dir`` root, so a sweep that reached the tree would be reaching
    outside the home it was handed. (The exit-64 half of item 36 — a trampoline
    whose baked root no longer exists — is WP-12d's.)

    RED: sweep by ``toolchain-dir`` key instead of by ledger entry — the sibling
    goes with the departed project.
    """
    root = Path(ocx.env["OCX_HOME"]) / "fleet"
    root.mkdir()
    write_toolchain_dir_config(ocx, root)

    departing = locked_project(ocx, tmp_path, directory=tmp_path / "departing")
    sibling = locked_project(ocx, tmp_path, directory=tmp_path / "sibling")
    departing_home = resolved_toolchain_home(ocx, departing.directory)
    sibling_home = resolved_toolchain_home(ocx, sibling.directory)
    assert run_in(ocx, departing.directory, "pull").returncode == EXIT_SUCCESS
    assert run_in(ocx, sibling.directory, "pull").returncode == EXIT_SUCCESS
    assert bin_entries(departing_home) and bin_entries(sibling_home), (
        "the control: both keyed trees must exist before the sweep"
    )

    ledger_before = _ledger(ocx)
    assert len(ledger_before) >= 2, (
        f"both projects must be registered; got {ledger_before}"
    )
    stamps_before = {
        p.parent.name
        for p in (Path(ocx.env["OCX_HOME"]) / "state" / "projects").rglob(
            "render_stamp.json"
        )
    }
    abandoned = snapshot_tree(departing_home)

    shutil.rmtree(departing.directory)
    swept = run_in(ocx, sibling.directory, "clean")
    assert swept.returncode == EXIT_SUCCESS, swept.stderr

    ledger_after = _ledger(ocx)
    assert len(ledger_after) == len(ledger_before) - 1, (
        f"C-052 — exactly the departed project leaves the ledger; "
        f"{sorted(ledger_before)} -> {sorted(ledger_after)}"
    )
    stamps_after = {
        p.parent.name
        for p in (Path(ocx.env["OCX_HOME"]) / "state" / "projects").rglob(
            "render_stamp.json"
        )
    }
    assert len(stamps_after) == len(stamps_before) - 1, (
        f"…and its render stamp with it; {sorted(stamps_before)} -> {sorted(stamps_after)}"
    )
    assert bin_entries(sibling_home), (
        "item 36 — a sibling project's keyed tree is untouched"
    )
    assert snapshot_tree(departing_home) == abandoned, (
        "C-053 — `ocx clean` sweeps the ledger, never a tree outside the home it "
        "was handed"
    )


# ---------------------------------------------------------------------------
# S-004 — a fleet operator reaches the feature through `[managed]`
# ---------------------------------------------------------------------------


def test_a_managed_config_tier_relocates_every_project_tree_under_the_fleet_root(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-004 — the only user-facing path by which a fleet reaches this feature.

    A ``[managed]`` payload carrying ``toolchain-dir`` folds into the same
    ``config.toml`` chain, so project trees render under
    ``<root>/<project-key>/toolchain`` and ``ocx shell state --format json``
    names the live home — the supported way for a devcontainer feature or a CI
    step to discover it without re-deriving a 16-hex key.

    RED: read ``toolchain-dir`` from the home tier only — the managed payload is
    then ignored and the tree renders in-project.
    """
    label = uuid4().hex[:8]
    root = Path(ocx.env["OCX_HOME"]) / "fleet"
    root.mkdir()
    repo = f"t_{label}_fleetcfg"
    push_managed_config(
        ocx, repo, "1.0.0", f'toolchain-dir = "{root.as_posix()}"\n', tmp_path
    )
    managed = {"OCX_MANAGED_CONFIG": f"{ocx.registry}/{repo}:1.0.0"}

    updated = run_in(ocx, tmp_path, "config", "update", env_extra=managed)
    assert updated.returncode == EXIT_SUCCESS, updated.stderr

    project = locked_project(ocx, tmp_path, pull=False, label=label, env_extra=managed)

    reported = resolved_toolchain_home(ocx, project.directory, env_extra=managed)
    assert root in reported.parents, (
        f"S-004 — the fleet root must key every project home; got {reported}"
    )
    assert reported.name == "toolchain", (
        f"C-002 — `<root>/<project-key>/toolchain`, key first; got {reported}"
    )

    pulled = run_in(ocx, project.directory, "pull", env_extra=managed)
    assert pulled.returncode == EXIT_SUCCESS, pulled.stderr
    assert bin_entries(reported) == [project.default_binary], (
        f"S-004 — the project tree renders under the fleet root; {reported} holds "
        f"{bin_entries(reported)}"
    )
    assert not toolchain_home(project.directory).exists(), (
        "S-004 — …and not in-project, or the relocation did not take effect"
    )
