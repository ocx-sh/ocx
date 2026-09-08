# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the rendered toolchain tree.

Two halves, both traced to ``.claude/state/plans/plan_toolchain_activation.md``:

* **WP-15's C-070** — every composing emitter heals the groups it is about to
  emit, not just the default one (C-065, C-067, C-070, S-006; ruling RUL-96).
* **WP-12a's render / prune / stamp / config-refusal suite** — ADR validation
  items 2, 12, 13, 14, 17, 18, 23, 24, 25, 31, 32, 33, 34, 36 and scenario
  S-004 (toolchain_activation), at the *tree* level. This file now cites two
  scenario catalogs, so every ``S-NNN`` in it names its record.

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
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src.assertions import assert_symlink_exists
from src.helpers import make_package, push_managed_config, write_ocx_toml
from src.runner import OcxRunner
from src.toolchain_fixtures import (
    ACTIVE_LINK,
    DEFAULT_GROUP,
    DEFAULT_SHELL,
    LINKS_DIR,
    SHELLS_DIR,
    active_link,
    assert_key_and_binary_namespaces_stay_disjoint,
    back_reference_entries,
    bin_entries,
    entry_link,
    git,
    link_entries,
    locked_project,
    path_facing_bin,
    read_render_stamp,
    render_stamp_path,
    resolved_toolchain_bin,
    resolved_toolchain_home,
    run_in,
    sha256_of,
    shell_bin,
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
        shell_bin(toolchain_home(moved)) / project.default_binary
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
    vanish and *which* survive: the removed tool loses its trampoline **and**
    its ``links/default/<key>`` link, the now-empty ``links/default/`` directory
    goes with them (class 3's non-recursive ``remove_dir``, reached here for real
    rather than by planting a stale directory), and the untouched group's link,
    the tree's own depth-1 names and the home's ``.gitignore`` are
    byte-identical afterwards.

    RED: prune only the trampoline and leave the link (or the reverse) — the
    surviving half is then a link on disk that no lock entry backs.
    """
    project = locked_project(ocx, tmp_path)
    before = snapshot_tree(project.home)
    survivors = {
        ".gitignore",
        ACTIVE_LINK,
        LINKS_DIR,
        f"{LINKS_DIR}/{project.group}",
        f"{LINKS_DIR}/{project.group}/{project.group_key}",
        SHELLS_DIR,
        f"{SHELLS_DIR}/{DEFAULT_SHELL}",
        f"{SHELLS_DIR}/{DEFAULT_SHELL}/bin",
    }
    untouched = {key: value for key, value in before.items() if key in survivors}
    assert len(untouched) == len(survivors), (
        f"the control: every artefact that must survive is present first; "
        f"expected {sorted(survivors)}, snapshot holds {sorted(before)}"
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
# C-082 / S-002 — a dereferencing copy leaves a real directory where a link
# belongs
# ---------------------------------------------------------------------------


def test_a_dereferenced_copy_of_an_entry_link_is_skipped_with_its_remedy_and_an_empty_one_heals(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-082 / S-002 — the ``<group>/<entry>`` publish dispatches on the
    **observed kind** before it renames.

    ``cp -rL``, ``unzip``, ``rsync`` without ``-l`` and Docker ``COPY`` all
    dereference symlinks, so a copied checkout carries a full *directory copy of
    the package* at every entry name. ``rename(2)`` fails ``EISDIR`` against a
    real directory — measured on an **empty** one too — so before C-082 neither
    arm healed and the skip said only ``internal file error for '<path>'``: no
    cause, no remedy, forever, with the composing lane degraded to digest paths
    (C-067) and no route back.

    Both arms in one case, for the reason
    ``test_an_empty_orphan_group_directory_is_removed_and_a_populated_one_is_only_skipped``
    states: "the publish heals a directory" must not be readable as "the publish
    deletes whatever it finds". The removal is ``remove_dir``, non-recursive,
    because both path components come from ``ocx.lock`` (RUL-32) — so the empty
    arm heals and the populated arm is *named*, not repaired.

    RED, one mutation per arm, both run: drop the kind dispatch from
    ``publish_link_within`` and the empty directory survives every later render;
    key the ``Skipped`` reason on anything but the re-observed kind and the
    populated one falls back to ``Is a directory (os error 21)`` with no remedy.
    """
    label = uuid4().hex[:8]
    copied_package = make_package(
        ocx, f"t_{label}_copy", "1.0.0", tmp_path, cascade=False, bins=[f"cpbin{label}"]
    )
    empty_package = make_package(
        ocx, f"t_{label}_mt", "1.0.0", tmp_path, cascade=False, bins=[f"mtbin{label}"]
    )
    project = tmp_path / f"proj-{label}"
    project.mkdir()
    write_ocx_toml(
        project,
        f"[tools]\n"
        f'cpkey{label} = "{copied_package.fq}"\n'
        f'mtkey{label} = "{empty_package.fq}"\n',
    )
    assert run_in(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert run_in(ocx, project, "pull").returncode == EXIT_SUCCESS

    home = toolchain_home(project)
    copied = entry_link(home, DEFAULT_GROUP, f"cpkey{label}")
    emptied = entry_link(home, DEFAULT_GROUP, f"mtkey{label}")
    assert copied.is_symlink() and emptied.is_symlink(), (
        f"the control: both entries render as links first; got {link_entries(home)}"
    )

    # Arm 1 — the `cp -rL` outcome, reproduced exactly: the link is replaced by
    # a real directory copy of the package it named.
    package_root = copied.resolve()
    copied.unlink()
    shutil.copytree(package_root, copied, symlinks=False)
    payload = sorted(p for p in copied.rglob("*") if p.is_file())
    assert payload, "the control: a dereferenced copy must carry real files"
    contents_before = {p: p.read_bytes() for p in payload}

    # Arm 2 — the same kind, with nothing in it.
    emptied.unlink()
    emptied.mkdir()

    result = run_in(ocx, project, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"C-050 — one unhealable entry never fails the render; "
        f"rc={result.returncode}\n{result.stderr}"
    )

    assert {p: p.read_bytes() for p in payload} == contents_before, (
        "RUL-32 — the populated copy is never removed recursively"
    )
    assert copied.is_dir() and not copied.is_symlink(), (
        "…and it is not removed at all: WP-0 names this state, it does not heal it"
    )
    assert str(copied) in result.stderr, (
        f"C-082 — the skip names the exact path to delete; stderr:\n{result.stderr}"
    )
    assert "cp -rL" in result.stderr, (
        f"C-082 — …and names what put it there; stderr:\n{result.stderr}"
    )
    assert "ocx pull" in result.stderr, (
        f"C-082 — …and what to run once it is gone; stderr:\n{result.stderr}"
    )
    assert str(copied) not in result.stdout, (
        f"the skip warning belongs on stderr only; stdout:\n{result.stdout}"
    )

    assert emptied.is_symlink(), (
        f"C-082 — an empty directory is removed with a non-recursive `remove_dir` "
        f"and the link written; `{emptied}` is still "
        f"{'a directory' if emptied.is_dir() else 'absent'}"
    )
    assert Path(os.readlink(emptied)).is_dir(), (
        "…and the healed link names a real digest root, not a dangling path"
    )
    assert sorted(bin_entries(home)) == sorted([f"cpbin{label}", f"mtbin{label}"]), (
        f"…and the render continued past the skip; bin/ holds {bin_entries(home)}"
    )


def test_a_regular_file_where_an_entry_link_belongs_is_still_replaced(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-082 — the kind dispatch is kind-*specific*: only a real directory is
    treated as unhealable.

    ``rename(2)`` already replaces an existing regular file, so this arm needs
    no removal and must not acquire one — an explicit ``remove_file`` would open
    a window in which the entry is absent, which is the whole reason this
    publish uses ``replace_atomic`` rather than ``symlink::update``.

    RED: refuse on the observation alone, widened to every pre-existing
    non-link — the file is reported ``Skipped`` and the entry never recovers.
    The stale-link half of the same property is held by
    ``test_a_branch_switch_leaves_a_stale_link_that_the_next_composing_emit_heals``.
    """
    project = locked_project(ocx, tmp_path)
    entry = project.default_link
    target_before = os.readlink(entry)
    entry.unlink()
    entry.write_text("not a link\n")

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert entry.is_symlink(), (
        f"C-082 — a regular file at an entry name is replaced, not refused; "
        f"stderr:\n{result.stderr}"
    )
    assert os.readlink(entry) == target_before, (
        "…by the link the lock derives, unchanged"
    )
    assert "cp -rL" not in result.stderr, (
        f"…and no directory remedy is printed for it; stderr:\n{result.stderr}"
    )


def test_an_entry_that_cannot_be_written_for_any_other_reason_names_its_cause(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """C-082 / C-050 — the arm that is *not* a directory still says why.

    ``Error::InternalFile`` carries its io cause by ``#[source]`` alone and its
    ``Display`` names only the path, so ``error.to_string()`` in a ``reason``
    field produced ``internal file error for '<path>'`` and stopped there — the
    defect C-082 describes, in its non-directory half. ``error::render_chain``
    exists for exactly this ("a warn line, a machine-readable ``reason`` field")
    and is what the skip now walks.

    A read-only group directory is the cheapest reachable instance: the staging
    symlink cannot be created inside it.

    RED: put ``error.to_string()`` back — the errno vanishes from the warning
    and the user is told a path they can already see.
    """
    project = locked_project(ocx, tmp_path)
    entry = project.default_link
    entry.unlink()
    group = entry.parent
    os.chmod(group, 0o555)
    try:
        result = run_in(ocx, project.directory, "pull")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-050 — the render continues; rc={result.returncode}\n{result.stderr}"
        )
        assert not entry.exists(), (
            "the control: the entry really must have stayed unwritten, or this "
            "case never exercised the skip"
        )
        assert "Permission denied" in result.stderr, (
            f"C-082 — a skip that is not a directory names its own cause; "
            f"stderr:\n{result.stderr}"
        )
        assert "cp -rL" not in result.stderr, (
            f"…and never borrows the directory remedy; stderr:\n{result.stderr}"
        )
    finally:
        os.chmod(group, 0o755)


# ---------------------------------------------------------------------------
# Item 17 / S-003 — `bin` is an ordinary name, and the charset still refuses
# (C-014, C-071, C-073)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("case", "body"),
    [
        ("group_bin", '[group.bin.tools]\n{key} = "{fq}"\n'),
        ("tools_bin", '[tools]\nbin = "{fq}"\n'),
        ("tools_BIN_case_folded", '[tools]\nBIN = "{fq}"\n'),
        ("group_tools_bin", '[group.ci.tools]\nbin = "{fq}"\n'),
    ],
)
def test_the_bin_name_is_accepted_at_every_declaration_site(
    ocx: OcxRunner, tmp_path: Path, case: str, body: str
) -> None:
    """S-003 / C-073 — the acceptance half of the reservation's **deletion**.

    These four rows asserted exit 78 and the message ``name 'bin' is reserved``
    until the rendered tree moved every user-supplied name one level down, under
    ``links/`` (C-071). ``bin`` was held for a per-group launcher directory that
    would have sat beside each ``<group>/``; the launcher directory is now
    ``shells/<shell>/bin`` and no user-supplied name reaches depth 1 at all, so
    ``[group.bin]`` renders at ``links/bin/<entry>`` and collides with nothing.
    The refusal is deleted with its error variant rather than reworded — no rule
    is left for its message to describe.

    ``BIN`` stays in the set because the old refusal was ASCII-case-folded: an
    acceptance test that only covered the lower-case spelling could not tell a
    deleted reservation from a reservation that merely stopped folding.

    Asserted through ``ocx status``, the same invocation the refusal rows used —
    it reads ``ocx.toml`` and reports, resolving nothing and rendering nothing,
    so the exit code answers exactly one question: does the reader admit this
    name?

    RED: restore the ``is_reserved_toolchain_name(name, "bin")`` arm in
    ``validate_toolchain_name`` and all four rows exit 78 again.
    """
    project = tmp_path / f"proj-{case}"
    project.mkdir()
    write_ocx_toml(
        project,
        body.format(key=f"rkey{uuid4().hex[:8]}", fq="ocx.sh/acme/tool:1.0"),
    )

    result = run_in(ocx, project, "status")
    assert result.returncode == EXIT_SUCCESS, (
        f"S-003 — {case} must be accepted (exit {EXIT_SUCCESS}); "
        f"got {result.returncode}\n{result.stderr}"
    )
    assert "is reserved" not in result.stderr, (
        f"C-073 — the reservation's message must be gone, not demoted to a "
        f"warning; stderr:\n{result.stderr}"
    )


@pytest.mark.parametrize(
    ("case", "body", "needle"),
    [
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
def test_the_name_charset_is_still_refused_at_every_site(
    ocx: OcxRunner, tmp_path: Path, case: str, body: str, needle: str
) -> None:
    """Item 17 / C-014 — the charset refusal is untouched by C-073.

    Both sites still exit **78** with a message naming the site and the
    offending name. Kept as its own case rather than merged into the acceptance
    rows above: deleting the whole `bin` family together would have taken the
    charset's own acceptance-level coverage with it.

    RED: remove the validator and watch ``[tools] "a b"`` parse cleanly.
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

    orphan = shell_bin(project.home) / f"ghost{uuid4().hex[:6]}"
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
    """Item 31, class 3 / C-044 / RUL-32 — the depth-1 orphan prune is a
    **non-recursive** ``remove_dir``.

    Both plants are at the **home root**, which under the closed depth-1 set
    mints ``RenderedArtifact::RootEntry``, not ``GroupDirectory``. The two share
    one ``prune_within`` arm, so the behaviour is covered either way — but the
    title said ``GroupDirectory`` because it predates ``links/``. The
    ``GroupDirectory`` path proper is covered under ``links/``.

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
    hostile = shell_bin(project.home) / f"hostile{uuid4().hex[:6]}"
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
        path = shell_bin(project.home) / name
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
    bin_before = snapshot_tree(shell_bin(project.home))
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
    assert snapshot_tree(shell_bin(project.home)) == bin_before, (
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
    assert snapshot_tree(shell_bin(project.home)) == bin_before, (
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


# ---------------------------------------------------------------------------
# S-001 — a checkout swaps `ocx.toml` and `ocx.lock` (WP-5)
# ---------------------------------------------------------------------------


#: The env that puts the session on C-005's ``bin`` rung, where
#: ``bin_mode_entry``'s ladder — and therefore C-080's ``active_is_valid`` gate —
#: is the thing that decides whether a project reaches ``PATH``.
_BIN_MODE = {"OCX_TOOLCHAIN_ACTIVATE": "bin"}


def _path_segments(ocx: OcxRunner, cwd: Path) -> tuple[list[str], str]:
    """The ``PATH`` segments a prompt would apply for ``cwd``, and the whole
    stream they came out of.

    ``ocx self activate --reconcile`` is the real emitter, so this observes the
    answer a shell actually receives rather than a diagnostic's opinion of it.
    ``ocx shell state`` cannot stand in: it reports ``toolchain_bin``
    unconditionally (RUL-51), by design, so a withhold is invisible there.

    ``--shell=bash`` is pinned, and it is load-bearing twice over. Without it
    the arm is chosen by ``Shell::detect``, which walks the **process tree** for
    a recognised shell and falls back to ``$SHELL`` — a variable ``OcxRunner``
    deliberately does not set. A suite run detached from its launching shell
    (``nohup … &``, a CI step, anything reparented to init) therefore has no
    shell anywhere in the chain, and the emitter then prints *nothing at all*
    and exits 0. Measured: 2372 bytes and two segments with a shell ancestor,
    0 bytes and none without, same binary and same environment. That is not a
    hypothetical — it is what made three rows in this module fail in the full
    suite while passing file-scoped. The second reason is that ``__ocx_p='…'``
    is the bash/zsh spelling, not a universal one: fish emits
    ``set __ocx_p "…"``, which this pattern would silently read as zero
    segments.

    The empty-set assertion below is the guard that keeps the pin honest. Every
    caller that asserts a path is **absent** from the result is satisfied by an
    emitter that said nothing, so a silent no-emission arm would turn every
    withhold row in this module vacuously green. The two ``$OCX_HOME`` segments
    are contributed unconditionally, so an empty list means the emitter did not
    speak, never that it withheld.

    The second element is the emitted stream itself, because a prompt's
    diagnostics travel inside it as ``printf … >&2`` rather than on the binary's
    own stderr, which the hook discards unconditionally (A-21).
    """
    result = run_in(
        ocx, cwd, "self", "activate", "--reconcile", "--shell=bash", env_extra=_BIN_MODE
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"the emitter must succeed before its output means anything; "
        f"rc={result.returncode}\n{result.stderr}"
    )
    segments = re.findall(r"__ocx_p='([^']*)'", result.stdout)
    assert segments, (
        f"the emitter contributed no `PATH` segment at all, not even the two "
        f"`$OCX_HOME` ones every arm carries — so this result cannot tell a "
        f"withhold from an emitter that never spoke, and no absence assertion "
        f"built on it would mean anything. Emitted:\n{result.stdout}"
    )
    return segments, result.stdout


def test_a_lock_swap_renders_the_new_locks_tools_groups_and_stamp(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-001 — a branch switch swaps ``ocx.toml`` *and* ``ocx.lock``; the next
    ``ocx pull`` renders the new lock.

    The two branches differ on three axes at once (see
    :class:`~src.toolchain_fixtures.TwoBranchCheckout`), because each one is a
    different prune loop in ``reconcile_links`` / ``reconcile_bin`` and a single
    axis leaves the other two vacuous:

    * a **shared** key whose digest moved — the repoint;
    * a default-group tool that **departed** and one that **arrived** — the
      trampoline prune and the entry prune;
    * a **group** that departed and one that arrived — the group pass.

    The expectations are the fixture's declaration of what each branch's
    ``ocx.toml`` said, never a re-reading of the tree: an expectation derived
    from the tree is satisfied by every tree.

    RED, one mutation per axis, each run and each proven to land:

    * drop the ``reconcile_bin`` prune loop — ``mobin…`` survives into the
      ``other`` tree and the ``bin_entries`` assertion reds;
    * drop the per-group entry prune loop — ``default/mokey…`` survives and the
      ``link_entries`` assertion reds;
    * make ``publish_link_within`` return ``Unchanged`` unconditionally — the
      shared key still names ``main``'s digest root and the repoint assertion
      reds.

    The departed **group** directory is deliberately not asserted absent here;
    ``test_a_departed_group_directory_converges_instead_of_skipping_forever``
    owns that clause exactly, and
    ``test_a_departing_group_holding_a_foreign_file_is_reported_every_render``
    owns the RUL-32 case where it must *not* go.
    """
    checkout = two_branch_checkout(ocx, tmp_path)

    first = run_in(ocx, checkout.directory, "pull")
    assert first.returncode == EXIT_SUCCESS, first.stderr
    assert set(link_entries(checkout.home)) == checkout.expected_links(
        checkout.main_branch
    ), (
        f"the control: `main`'s lock renders exactly its own three links; got "
        f"{sorted(link_entries(checkout.home))}"
    )
    assert bin_entries(checkout.home) == checkout.expected_bin(checkout.main_branch), (
        f"the control: and exactly its two trampolines; got "
        f"{bin_entries(checkout.home)}"
    )
    on_main = os.readlink(checkout.link)

    # The checkout, exactly as a user makes it: git swaps both files at once.
    git(checkout.directory, "checkout", "-q", checkout.other_branch)
    second = run_in(ocx, checkout.directory, "pull")
    assert second.returncode == EXIT_SUCCESS, second.stderr

    links = link_entries(checkout.home)
    assert checkout.expected_links(checkout.other_branch) <= set(links), (
        f"S-001 — every link the new lock declares is rendered; missing "
        f"{sorted(checkout.expected_links(checkout.other_branch) - set(links))}"
    )
    assert f"{DEFAULT_GROUP}/{checkout.main_only_key}" not in links, (
        f"S-001 — no stale link: the departed default-group tool's link is gone; "
        f"links are {sorted(links)}"
    )
    assert links[f"{DEFAULT_GROUP}/{checkout.key}"] != on_main, (
        "S-001 — the shared key is repointed at the new lock's digest root, not "
        "left naming the branch that was pulled"
    )
    assert bin_entries(checkout.home) == checkout.expected_bin(checkout.other_branch), (
        f"S-001 — no trampoline for a departed tool and one for every arrived "
        f"tool; `bin/` holds {bin_entries(checkout.home)}"
    )

    stamp = read_render_stamp(ocx, checkout.home)
    assert stamp is not None, "S-001 — the render stamp is current, so it exists"
    assert stamp["names"] == checkout.expected_bin(checkout.other_branch), (
        f"S-001 — …and describes the tree the new lock produced, not the old "
        f"one; stamp names {stamp['names']}"
    )
    assert set(stamp["link_fingerprint"]) == {
        key for key in links if key.startswith(f"{DEFAULT_GROUP}/")
    }, (
        f"RUL-27 — the stamped link set is the default group as it stands on "
        f"disk; stamp {sorted(stamp['link_fingerprint'])} vs disk {sorted(links)}"
    )


def test_a_departing_group_holding_a_foreign_file_is_reported_every_render(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-001's error clause, and the asymmetry the convergence half rests on.

    The links under a departed group are ocx's own and go — that is the sibling
    below. A file ocx did **not** write is a different thing entirely: the
    prune sweep skips it, the non-recursive ``remove_dir`` then fails
    ``ENOTEMPTY``, and the directory is reported — never silently deleted
    (RUL-32) — on **every** render, not once. ``skipped_render_warnings`` puts
    one line on stderr per render; the remedy half of that line is
    ``test_a_skipped_group_directory_says_why_and_what_to_do``.

    The planted file is what makes a skipped group directory reachable at all
    once a departed group converges. It is planted **before** the first
    post-switch render, because "reported forever" and "reported once" are the
    same tree after one render and different trees after two.

    RED: change the ``GroupDirectory`` arm's directory branch from
    ``std::fs::remove_dir`` to ``remove_dir_all`` — the planted file vanishes
    and the survival assertion reds. (Second red, for the "reported" half:
    delete the ``Skipped`` arm of ``skipped_render_warnings`` — the stderr
    assertion reds while the survival one still passes; the two are
    independent.) Third: drop ``prune_within``'s ``is_symlink`` refusal on the
    ``RenderedArtifact::Link`` arm and the same survival assertion reds,
    because ``crate::symlink::remove`` is ``std::fs::remove_file`` on Unix.
    """
    checkout = two_branch_checkout(ocx, tmp_path)
    assert run_in(ocx, checkout.directory, "pull").returncode == EXIT_SUCCESS
    assert checkout.main_group_link_present(), (
        "the control: `main`'s group link is on disk before the switch"
    )

    git(checkout.directory, "checkout", "-q", checkout.other_branch)
    planted = checkout.main_group_dir / "not-ocxs.txt"
    planted.write_bytes(b"planted\n")

    for run in (1, 2):
        result = run_in(ocx, checkout.directory, "pull")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-050 — a skip never fails the render (run {run}); "
            f"rc={result.returncode}\n{result.stderr}"
        )
        assert planted.read_bytes() == b"planted\n", (
            f"RUL-32 — the departed group's directory is never removed "
            f"recursively (run {run}); {checkout.main_group_dir} holds "
            f"{sorted(p.name for p in checkout.main_group_dir.iterdir())}"
        )
        assert not checkout.main_group_link_present(), (
            f"…while the link ocx itself wrote is pruned (run {run}) — that "
            f"asymmetry is the whole rule"
        )
        assert str(checkout.main_group_dir) in result.stderr, (
            f"S-001 — …and the skip names the path, on every render, not once "
            f"(run {run}); stderr:\n{result.stderr}"
        )
        assert str(checkout.main_group_dir) not in result.stdout, (
            f"the skip warning belongs on stderr only (run {run}); stdout:\n"
            f"{result.stdout}"
        )
        assert bin_entries(checkout.home) == checkout.expected_bin(
            checkout.other_branch
        ), f"…and the render continued past the skip (run {run})"


def test_a_foreign_file_in_a_live_group_directory_is_reported_never_removed(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """RUL-32 / C-082 on the path that runs every render — a group the lock
    **still declares**.

    The prune sweep removes every name in a group directory the computed set no
    longer holds, and it removed them with ``crate::symlink::remove``, which is
    ``std::fs::remove_file`` on Unix: a file ocx never wrote was deleted, in
    silence, on an ordinary ``ocx pull``. The departed-group sibling proved the
    shape; this is the same defect on the branch that runs far more often.

    ocx removes what ocx wrote. Anything else survives byte-for-byte and is
    named, so the user can see what is there rather than discovering it gone.

    RED: drop ``prune_within``'s ``is_symlink`` refusal on the
    ``RenderedArtifact::Link`` arm — the planted file is deleted and both the
    survival and the report assertions red.
    """
    project = locked_project(ocx, tmp_path)
    planted = entry_link(project.home, DEFAULT_GROUP, "not-ocxs.txt")
    planted.write_bytes(b"planted\n")

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"C-050 — a skip never fails the render; rc={result.returncode}\n"
        f"{result.stderr}"
    )
    assert planted.read_bytes() == b"planted\n", (
        f"RUL-32 — a file ocx did not write survives the prune byte-for-byte; "
        f"{planted.parent} holds {sorted(p.name for p in planted.parent.iterdir())}"
    )
    assert str(planted) in result.stderr, (
        f"C-050 — …and the skip names it, so the user is not left guessing; "
        f"stderr:\n{result.stderr}"
    )


def test_a_departed_group_directory_converges_instead_of_skipping_forever(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-001 *Expected* — "the rendered tree matches the new lock exactly — no
    orphan group directory".

    A departed group holds nothing but links the renderer itself wrote, so
    emptying it is not the recursive delete RUL-32 forbids — it is the same
    per-entry ``symlink::remove`` the surviving groups already get. RUL-32 still
    binds whatever ocx did **not** write, which is the case the sibling above
    pins.

    RED: drop the entry sweep from ``reconcile_links``'s ``links/``-orphan loop
    (the ``for entry in read_dir_utf8_names(&group_directory)`` block) — the
    departed group's link survives, ``link_entries`` gains a key the new lock
    never declared, and the directory is still on disk.
    """
    checkout = two_branch_checkout(ocx, tmp_path)
    assert run_in(ocx, checkout.directory, "pull").returncode == EXIT_SUCCESS
    git(checkout.directory, "checkout", "-q", checkout.other_branch)
    assert run_in(ocx, checkout.directory, "pull").returncode == EXIT_SUCCESS

    assert set(link_entries(checkout.home)) == checkout.expected_links(
        checkout.other_branch
    ), (
        f"S-001 — the tree matches the new lock **exactly**; got "
        f"{sorted(link_entries(checkout.home))}"
    )
    assert not checkout.main_group_dir.exists(), (
        f"S-001 — no orphan group directory; {checkout.main_group_dir} survived"
    )


def test_a_skipped_group_directory_says_why_and_what_to_do(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-001 *Errors* — "reported ``Skipped`` with **a remedy** naming the path".

    ``prune_outcome`` built its reason with ``error.to_string()``, and
    ``Error::InternalFile`` carries its cause by ``#[source]`` alone — so the
    reason was ``internal file error for '<path>'`` and nothing else: the path,
    twice, with no cause and no action. ``publish_link_within`` had already
    solved this at the sibling site, and its own doc comment says why
    (``to_string`` there "would name the path and nothing else — a refusal the
    user cannot act on"); one producer of that contract was fixed and the other
    was not.

    RED: revert ``prune_outcome``'s reason to ``error.to_string()`` — the cause
    assertion and the remedy assertion both red while the path assertion above
    them still passes, which is what makes them the two halves this row adds.
    """
    checkout = two_branch_checkout(ocx, tmp_path)
    assert run_in(ocx, checkout.directory, "pull").returncode == EXIT_SUCCESS
    git(checkout.directory, "checkout", "-q", checkout.other_branch)
    # A departed group whose contents are all ocx's own now converges, so a file
    # ocx did not write is what keeps the directory unremovable — the only state
    # in which there is a skip to read at all.
    (checkout.main_group_dir / "not-ocxs.txt").write_bytes(b"planted\n")
    result = run_in(ocx, checkout.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert str(checkout.main_group_dir) in result.stderr, (
        "the control: the skip names the path"
    )
    assert "not empty" in result.stderr.lower(), (
        f"S-001 — …and says why it could not be removed; stderr:\n{result.stderr}"
    )
    assert "ocx pull" in result.stderr, (
        f"S-001 — …and what to run once it is gone; stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# §C.1-C.4 — the nine pytest rows of the corrupt-state matrix (WP-5)
# ---------------------------------------------------------------------------


def test_a_dereferenced_toolchain_copy_reports_the_same_entry_on_every_pull(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 2 / §0 / S-002 — a whole project copied by something that
    dereferences symlinks reports each affected entry on **every** pull.

    The sibling
    ``test_a_dereferenced_copy_of_an_entry_link_is_skipped_with_its_remedy_and_an_empty_one_heals``
    plants the shape by hand and pulls once. This one reproduces how a user
    actually reaches it — ``copytree(..., symlinks=False)``, the shape ``cp
    -rL`` / ``unzip`` / Docker ``COPY`` all produce — and pulls **twice**,
    because "reported forever" and "reported once and then silently ignored"
    are the same tree after one render and different trees after two.

    The matrix spells the setup ``copytree(home, alt)``; a home outside a
    project is not something ``ocx pull`` can be pointed at, so the copy is of
    the project — which is what dereferences the entry links in the first place.

    RED: see the report's mutation table — the discriminating mutation is the
    one that makes the second run silent while the first still reports.
    """
    project = locked_project(ocx, tmp_path)
    alt = tmp_path / "dereferenced"
    shutil.copytree(project.directory, alt, symlinks=False)
    alt_home = toolchain_home(alt)
    entry = entry_link(alt_home, DEFAULT_GROUP, project.default_key)

    assert entry.is_dir() and not entry.is_symlink(), (
        f"the control: the copy dereferenced the entry link into a real "
        f"directory; {entry} is {'a link' if entry.is_symlink() else 'absent'}"
    )
    payload = sorted(p for p in entry.rglob("*") if p.is_file())
    assert payload, "the control: a dereferenced copy carries real files"
    before = {p: p.read_bytes() for p in payload}

    for run in (1, 2):
        result = run_in(ocx, alt, "pull")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-050 — one unhealable entry never fails the render (run {run}); "
            f"rc={result.returncode}\n{result.stderr}"
        )
        assert str(entry) in result.stderr, (
            f"row 2 — the entry is reported on run {run}, not only on run 1; "
            f"stderr:\n{result.stderr}"
        )
        assert f"{DEFAULT_GROUP}/{project.default_key}" not in link_entries(alt_home), (
            f"row 2 — …and it never becomes a link again (run {run}); links are "
            f"{sorted(link_entries(alt_home))}"
        )
        assert {p: p.read_bytes() for p in payload} == before, (
            f"RUL-32 — …and the copy is never removed recursively (run {run})"
        )
        # ADR item 44 asks the `active` row to be driven from a *real*
        # dereferencing copy rather than a hand-built fixture. The only test
        # that did so was win32-only and runs on no CI leg (#419); this is the
        # same assertion on the leg that actually executes.
        assert_symlink_exists(active_link(alt_home))
        assert os.readlink(active_link(alt_home)) == f"{SHELLS_DIR}/{DEFAULT_SHELL}", (
            f"item 47(b) — a copy that dereferenced `active` into a directory is "
            f"healed back into a link on run {run}, not left as the copy made it"
        )


def test_an_emptied_shells_directory_is_repopulated_by_the_next_pull(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 7 / A13 — ``shells/`` deleted wholesale comes back on the next pull.

    The state a ``git clean -xdf`` inside a checked-in tree leaves, and the one
    ``create_bin_owner_only``'s non-recursive ``mkdir`` cannot recover from on
    its own: after the move, ``bin``'s parent is ``shells/<shell>``, two levels
    below the root that ``ensure_home_root`` creates, so the shell tree needs a
    creation step of its own.

    RED: delete the ensure-present step — ``create_bin_owner_only`` fails
    ``ENOENT`` and every trampoline is ``Skipped``, so the set comes back empty.
    """
    project = locked_project(ocx, tmp_path)
    before = bin_entries(project.home)
    assert before, "the control: the first render produced trampolines"

    shutil.rmtree(project.home / SHELLS_DIR)
    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert bin_entries(project.home) == before, (
        f"row 7 — the next pull repopulates the shell tree; `bin/` holds "
        f"{bin_entries(project.home)}, expected {before}"
    )
    assert_symlink_exists(active_link(project.home))
    assert os.readlink(active_link(project.home)) == f"{SHELLS_DIR}/{DEFAULT_SHELL}", (
        "…and `active` still names the shell the render re-created (C-083's "
        "order: the directory before the link that points at it)"
    )


def test_a_project_copied_to_a_new_path_does_not_expose_the_sources_trampolines(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 9 / A15 — a project copied **with** its ``.ocx`` and never pulled is
    withheld from ``PATH``.

    Every trampoline body bakes the project root it was rendered for (C-064), so
    a copy's trampolines run the *source* project. The gate that catches it is
    the render stamp's key, which is derived from the project directory: the
    copy resolves a different key, finds no stamp for it, and withholds.

    Two things are removed first, or the withhold would be satisfied by an
    answer that has nothing to do with identity:

    * **consent** is granted in the copy and asserted granted, or the reason is
      ``no_stamp_no_grant``;
    * the source's **render stamp** is planted under the copy's own project key,
      or the withhold is "no stamp exists for this key at all" — true of the
      copy, but true of a never-pulled project too, and no mutation to the
      identity gate can red it.

    With both removed, the only thing still withholding is D-V13's identity
    gate: the stamp records the home and the project directory it was written
    for, and both name the source. That is what a copy defeats and what this row
    is about — "identity before content, both halves".

    RED: drop the ``stamp.home != home.root() || stamp.scope != scope`` guard —
    the planted stamp is accepted, the copy is exposed through trampolines that
    run the source project, and the withhold assertion reds.
    """
    project = locked_project(ocx, tmp_path)
    exposed, _ = _path_segments(ocx, project.directory)
    assert str(path_facing_bin(project.home)) in exposed, (
        f"the control: the source project is exposed before anything is copied; "
        f"segments {exposed}"
    )

    moved = tmp_path / "moved"
    shutil.copytree(project.directory, moved, symlinks=True)
    allowed = run_in(ocx, moved, "shell", "allow")
    assert allowed.returncode == EXIT_SUCCESS, allowed.stderr
    state = json.loads(run_in(ocx, moved, "--format", "json", "shell", "state").stdout)
    assert state["grant"] == "stamp", (
        f"the control: consent is granted in the copy, so a withhold below is "
        f"about identity and not about consent; grant={state['grant']!r}, "
        f"reason={state.get('inert_reason')!r}"
    )

    source_stamp = render_stamp_path(ocx, project.home)
    assert source_stamp is not None, "the control: the source rendered a stamp"
    planted = (
        Path(ocx.env["OCX_HOME"])
        / "state"
        / "projects"
        / state["project_key"]
        / source_stamp.name
    )
    planted.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(source_stamp, planted)
    assert json.loads(planted.read_text())["home"] == str(project.home), (
        "the control: the planted stamp records the **source** home, so only "
        "the identity gate can still refuse it"
    )

    segments, _ = _path_segments(ocx, moved)
    assert str(path_facing_bin(toolchain_home(moved))) not in segments, (
        f"row 9 — a copied tree is withheld until a render runs for its own "
        f"path, even with a stamp sitting under its own key; segments {segments}"
    )


def test_a_copied_tree_still_bakes_the_source_root_until_a_render_runs(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 10 / A15 — and the reason the withhold above matters: the copied
    trampolines still name the **source** project.

    The prompt path never rewrites a body and never prunes one (C-064): it is a
    read-only observer, so the stale bodies stay exactly as copied until an
    ``ocx pull`` runs in the copy. Asserting that is what makes row 9 a
    security property rather than a tidiness one.

    Consent is granted in the copy so the prompt reaches ``bin_mode_entry`` at
    all; without it the session is inert before C-064 is ever consulted, and the
    bytes would be unchanged for a reason that has nothing to do with this row.

    RED: make the prompt path rewrite stale bodies — the byte assertion reds
    (and C-064 is violated).
    """
    project = locked_project(ocx, tmp_path)
    moved = tmp_path / "moved"
    shutil.copytree(project.directory, moved, symlinks=True)
    assert run_in(ocx, moved, "shell", "allow").returncode == EXIT_SUCCESS
    body = (shell_bin(toolchain_home(moved)) / project.default_binary).read_bytes()
    assert str(project.directory).encode() in body, (
        "the control: a copied trampoline body names the source project root"
    )

    _path_segments(ocx, moved)

    assert (
        shell_bin(toolchain_home(moved)) / project.default_binary
    ).read_bytes() == body, (
        "row 10 — a prompt neither rewrites nor prunes a stale body; the copy is "
        "byte-identical after the prompt ran"
    )


def test_a_home_restored_at_the_same_absolute_path_passes_the_gate(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 11 / A16 — a home restored from a backup at the **same** absolute
    path is still exposed, even though every inode changed.

    The state a ``tar``-based backup restore, a devcontainer rebuild that
    re-hydrates a volume, or a `git stash` round trip leaves. C-064's identity
    is a path and a content hash, deliberately not an inode: ``file_id`` is the
    cheap first half of ``bin_matches_recorded`` and the content hash is the
    answer it falls through to. Requiring the cheap half to match would withhold
    every restored tree forever, with no user action that could ever fix it.

    RED: make ``bin_matches_recorded`` require ``file_id`` equality instead of
    falling through to the hash — the exposure assertion reds.
    """
    project = locked_project(ocx, tmp_path)
    home = project.home
    state = Path(ocx.env["OCX_HOME"]) / "state"
    ids_before = {
        p.name: p.stat().st_ino for p in shell_bin(home).iterdir() if p.is_file()
    }
    assert ids_before, "the control: there are trampolines to churn"

    home_backup, state_backup = tmp_path / "home.bak", tmp_path / "state.bak"
    shutil.copytree(home, home_backup, symlinks=True)
    shutil.copytree(state, state_backup, symlinks=True)
    shutil.rmtree(home)
    shutil.rmtree(state)
    shutil.copytree(home_backup, home, symlinks=True)
    shutil.copytree(state_backup, state, symlinks=True)

    ids_after = {
        p.name: p.stat().st_ino for p in shell_bin(home).iterdir() if p.is_file()
    }
    assert ids_after and ids_after != ids_before, (
        f"the control: the restore minted new inodes, or this row observes "
        f"nothing; before {ids_before}, after {ids_after}"
    )

    segments, _ = _path_segments(ocx, project.directory)
    assert str(path_facing_bin(home)) in segments, (
        f"row 11 — inode churn alone must not withhold a restored tree; "
        f"segments {segments}"
    )


def test_a_read_only_home_skips_rather_than_fails(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 13 / A18 — a home the user cannot write is a C-050 skip, never a
    failed command.

    A root-owned or read-only checkout is an ordinary state on a shared CI
    runner, and ``ocx pull``'s job — fetching and installing packages — has
    already succeeded by the time the render runs. Failing the command would
    make an unrenderable tree cost the install.

    RED: make any tree-own-name refusal a hard error instead of a C-050 skip —
    the exit code becomes non-zero and the first assertion reds.
    """
    project = locked_project(ocx, tmp_path)
    os.chmod(project.home, stat.S_IRUSR | stat.S_IXUSR)
    try:
        result = run_in(ocx, project.directory, "pull")
    finally:
        os.chmod(project.home, stat.S_IRWXU)

    assert result.returncode == EXIT_SUCCESS, (
        f"row 13 — an unwritable home skips; rc={result.returncode}\n{result.stderr}"
    )
    assert "WARN" in result.stderr, f"…and says so on stderr; stderr:\n{result.stderr}"
    assert str(project.home) in result.stderr, (
        f"…naming the home it could not write; stderr:\n{result.stderr}"
    )
    assert "panicked" not in result.stderr and "RUST_BACKTRACE" not in result.stderr, (
        f"…as a diagnostic, never a traceback; stderr:\n{result.stderr}"
    )
    assert "WARN" not in result.stdout and str(project.home) not in result.stdout, (
        f"…and stdout stays the command's own output; stdout:\n{result.stdout}"
    )


def test_two_concurrent_pulls_leave_one_consistent_tree(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 16 / A21 — two ``ocx pull`` processes racing one project leave one
    tree, and a stamp that describes it.

    Convergence only, never an interleaving: which process wins the render lock
    is not a contract, and asserting an order here would be a flake under
    ``pytest -n auto``. What *is* a contract is that the tree and the stamp
    agree afterwards, whichever won.

    The race runs against a **changed** lock, not a steady state. Two renders of
    an already-correct tree write nothing and agree under almost any mutation —
    a green that cannot go red. After a branch switch both processes have real
    work to do: two links to repoint, one trampoline to prune, one to write, one
    group to add. That is what gives the round-trip assertion teeth.

    RED: write the stamp before ``reconcile_bin`` instead of after — the stamp
    then describes the pre-switch tree while the disk holds the post-switch one,
    and both round-trip assertions red.
    """
    checkout = two_branch_checkout(ocx, tmp_path)
    assert run_in(ocx, checkout.directory, "pull").returncode == EXIT_SUCCESS
    git(checkout.directory, "checkout", "-q", checkout.other_branch)

    processes = [
        subprocess.Popen(
            [str(ocx.binary), "pull"],
            cwd=checkout.directory,
            env=dict(ocx.env),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(2)
    ]
    outcomes = [(proc.wait(timeout=180), *proc.communicate()) for proc in processes]
    for code, _, err in outcomes:
        assert code == EXIT_SUCCESS, (
            f"a concurrent pull must not fail; rc={code}\n{err}"
        )

    stamp = read_render_stamp(ocx, checkout.home)
    assert stamp is not None, "row 16 — a stamp describes the tree that survived"
    assert sorted(stamp["names"]) == bin_entries(checkout.home), (
        f"row 16 — the stamp and the tree name the same set, in both directions; "
        f"stamp {sorted(stamp['names'])} vs disk {bin_entries(checkout.home)}"
    )
    assert bin_entries(checkout.home) == checkout.expected_bin(checkout.other_branch), (
        f"…and that set is the new lock's, so the race did have work to do; "
        f"`bin/` holds {bin_entries(checkout.home)}"
    )
    assert set(stamp["link_fingerprint"]) == {
        key
        for key in link_entries(checkout.home)
        if key.startswith(f"{DEFAULT_GROUP}/")
    }, (
        f"…and so do the stamped and rendered link sets; stamp "
        f"{sorted(stamp['link_fingerprint'])} vs disk "
        f"{sorted(link_entries(checkout.home))}"
    )


def test_a_legacy_bin_directory_beside_the_new_names_is_pruned_when_empty_and_reported_when_not(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 17 / A20 — the pre-``shells/`` ``<home>/bin`` is an ordinary depth-1
    orphan now, and the two states it can be in get different, deliberate
    answers.

    The matrix asked for one policy asserted "explicitly and **silently** (no
    WARN)". Silence is unreachable for the populated state: a non-empty
    directory makes ``remove_dir`` fail, ``prune_outcome`` turns that into
    ``Skipped``, and ``skipped_render_warnings`` renders every ``Skipped`` as a
    warning — so the row is split into the two states rather than asserting one
    policy for both.

    ``bin`` is no longer a tree-own depth-1 name (``TREE_OWN_DEPTH1_NAMES`` is
    ``.gitignore``/``active``/``links``/``shells``), which is exactly what lets
    a leftover from the old layout be cleaned up instead of kept forever.

    RED: put ``bin`` back in the closed depth-1 set — the empty directory is
    kept, the ``not exists`` assertion reds, and the populated one stops being
    reported.
    """
    project = locked_project(ocx, tmp_path)
    legacy = project.home / "bin"

    # Arm 1 — empty: pruned, and pruning is not an event worth a warning.
    legacy.mkdir()
    first = run_in(ocx, project.directory, "pull")
    assert first.returncode == EXIT_SUCCESS, first.stderr
    assert not legacy.exists(), (
        f"row 17 — an empty legacy `bin/` is removed by the depth-1 scan; "
        f"{legacy} survived"
    )
    assert str(legacy) not in first.stderr, (
        f"…silently: a `Pruned` outcome emits no warning; stderr:\n{first.stderr}"
    )

    # Arm 2 — populated: never removed recursively, and named every render.
    legacy.mkdir()
    (legacy / "oldtool").write_text("#!/bin/sh\necho legacy\n")
    second = run_in(ocx, project.directory, "pull")
    assert second.returncode == EXIT_SUCCESS, second.stderr
    assert (legacy / "oldtool").read_text() == "#!/bin/sh\necho legacy\n", (
        "RUL-32 — a populated legacy `bin/` is never removed recursively"
    )
    assert str(legacy) in second.stderr, (
        f"…and the skip names it; stderr:\n{second.stderr}"
    )
    assert bin_entries(project.home) == [project.default_binary], (
        f"…and the real trampoline directory is untouched by either arm; it "
        f"holds {bin_entries(project.home)}"
    )


def test_a_legacy_group_directory_at_the_home_root_is_reported_and_never_deleted(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 18 / A20 / C-076 — a whole pre-``links/`` tree at the home root is
    **reported on every render** and never deleted.

    The matrix's original wording asserted the opposite — "the second run does
    not report ``oldgroup`` again" — and named as its red the very behaviour the
    plan settled on. "Reported once" needs somewhere to remember that it
    reported, and the render stamp deliberately describes the tree rather than
    the diagnostics; the plan's 2026-09-07 log dropped it. The row is rewritten
    here to the behaviour that ships and that C-076 states.

    RED, the one that matters (RUL-32): change the ``GroupDirectory`` /
    ``RootEntry`` prune arm's directory branch from ``std::fs::remove_dir`` to
    ``remove_dir_all`` — the payload assertions red on the first run.
    Second red, for the "reported" half: delete the ``Skipped`` arm of
    ``skipped_render_warnings`` — the stderr assertion reds while the payload
    assertions still pass, so the two halves are independent and both are
    needed.
    """
    project = locked_project(ocx, tmp_path)
    legacy = project.home / "oldgroup" / "oldentry"
    legacy.mkdir(parents=True)
    (legacy / "payload").write_bytes(b"legacy bytes\n")

    for run in (1, 2):
        result = run_in(ocx, project.directory, "pull")
        assert result.returncode == EXIT_SUCCESS, (
            f"C-050 — a render skip is never `pull`'s exit code (run {run}); "
            f"rc={result.returncode}\n{result.stderr}"
        )
        assert (legacy / "payload").read_bytes() == b"legacy bytes\n", (
            f"C-076 / RUL-32 — a legacy tree is never deleted (run {run})"
        )
        assert str(project.home / "oldgroup") in result.stderr, (
            f"C-076 — …and is reported per render, naming the directory to "
            f"remove (run {run}); stderr:\n{result.stderr}"
        )
        assert str(project.home / "oldgroup") not in result.stdout, (
            f"…on stderr only (run {run}); stdout:\n{result.stdout}"
        )

    assert set(link_entries(project.home)) == {
        f"{DEFAULT_GROUP}/{project.default_key}",
        f"{project.group}/{project.group_key}",
    }, (
        f"C-076 — …and the new tree renders under `links/` regardless; links are "
        f"{sorted(link_entries(project.home))}"
    )


# ---------------------------------------------------------------------------
# §C.5 — the `[active-only]` rows this package owns (WP-5)
# ---------------------------------------------------------------------------
#
# Rows 21, 22, 26, 27, 28, 31 and 33 are WP-2's, at the unit layer. Row 29 is
# **void**: it asserts a `RenderStamp` field C-079's own blockquote strikes
# ("no new `RenderStamp` field, no wire-format change"), so its red — "derive
# the field from config instead of `read_link`" — is unreachable against a field
# that does not exist. Row 30 is the behaviour row 29 was meant to protect and
# is sufficient. That leaves this package **four** runnable rows plus one
# Windows-only label, not six.


def test_an_escaping_active_link_is_repointed_and_nothing_is_written_through_it(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 23 / A4 — a hostile clone that force-commits ``active`` as a link to
    an attacker-chosen directory gets it **repointed**, and nothing lands there.

    ``active/bin`` is what goes on ``PATH``, so an escaping ``active`` is an
    arbitrary-directory-on-``PATH`` primitive (CWE-426). The matrix predicted
    ``rc == 78`` and a stderr naming ``active``; **that is not what ships and
    could not be**. C-081 makes the render *heal* the link — measured: rc 0, no
    diagnostic, ``active`` naming ``shells/default`` again. The security
    property the row is really about is the second half, and it survives the
    correction intact: the attacker's directory is still empty afterwards,
    because every write goes to the physical ``shells/<shell>/bin`` and never
    through the link.

    RED: revert any one of the four physical anchors (the write, the prune, the
    fingerprint or the guard) to ``home.bin()`` — the render resolves through
    the hostile link and ``evil/`` stops being empty.
    """
    project = locked_project(ocx, tmp_path)
    evil = tmp_path / "evil"
    evil.mkdir()
    active = active_link(project.home)
    os.unlink(active)
    os.symlink(str(evil), active)

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"C-081 — an escaping `active` is healed, not refused; "
        f"rc={result.returncode}\n{result.stderr}"
    )
    assert sorted(evil.rglob("*")) == [], (
        f"row 23 — nothing is ever written through `active`; {evil} holds "
        f"{sorted(p.name for p in evil.rglob('*'))}"
    )
    assert_symlink_exists(active)
    assert os.readlink(active) == f"{SHELLS_DIR}/{DEFAULT_SHELL}", (
        f"C-081 — …and the link is back at its one legal target; it names "
        f"{os.readlink(active)!r}"
    )
    assert bin_entries(project.home) == [project.default_binary], (
        f"…with the trampolines in the physical directory where they belong; "
        f"`bin/` holds {bin_entries(project.home)}"
    )


def test_an_absent_active_is_recreated_silently(ocx: OcxRunner, tmp_path: Path) -> None:
    """Row 24 / A2 — a deleted ``active`` is re-created, and its re-creation is
    not news.

    ``git clean``, a partial restore, or a user tidying a directory they do not
    recognise all reach this state, and none of them is a fault. Both halves are
    load-bearing and neither implies the other, so both are asserted and both
    have their own red.

    RED: delete ``heal_active``'s create arm — the presence half reds. Make the
    heal ``warn!`` instead of ``debug!`` — the silence half reds while the
    presence half still passes.
    """
    project = locked_project(ocx, tmp_path)
    os.unlink(active_link(project.home))
    assert not active_link(project.home).is_symlink(), (
        "the control: `active` is genuinely gone before the render runs"
    )

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert_symlink_exists(active_link(project.home))
    assert os.readlink(active_link(project.home)) == f"{SHELLS_DIR}/{DEFAULT_SHELL}", (
        f"row 24 — …at its derived target; it names "
        f"{os.readlink(active_link(project.home))!r}"
    )
    assert "WARN" not in result.stderr, (
        f"row 24 — an ordinary heal is not a warning; stderr:\n{result.stderr}"
    )
    assert str(active_link(project.home)) not in result.stdout, (
        f"…and never reaches stdout either; stdout:\n{result.stdout}"
    )


def test_a_dangling_active_is_repointed_not_pruned(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 25 / A3 — ``active`` pointing at a shell that does not exist is
    **repointed**, never removed.

    The distinction matters because the two outcomes are one ``if`` apart in the
    prune arm: a depth-1 orphan that is a symlink is removed with
    ``symlink::remove``, and the only thing keeping ``active`` out of that arm is
    its membership in the closed depth-1 set (C-074). A removed ``active``
    leaves a tree whose ``PATH`` spelling does not resolve at all.

    ``snapshot_tree`` records ``os.readlink`` rather than the resolved target,
    so it observes the repoint itself — an absence and a repoint are different
    entries, not two spellings of one.

    RED: drop ``active`` from ``TREE_OWN_DEPTH1_NAMES`` — the depth-1 scan mints
    a ``RootEntry("active")``, the ``is_symlink`` branch calls
    ``symlink::remove``, and the presence assertion reds.
    """
    project = locked_project(ocx, tmp_path)
    active = active_link(project.home)
    os.unlink(active)
    os.symlink(f"{SHELLS_DIR}/ghost", active)
    assert os.readlink(active) == f"{SHELLS_DIR}/ghost", "the control: it dangles"

    result = run_in(ocx, project.directory, "pull")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    assert snapshot_tree(project.home)[ACTIVE_LINK] == (
        "symlink",
        f"{SHELLS_DIR}/{DEFAULT_SHELL}",
    ), (
        f"row 25 — a dangling `active` is repointed, not pruned; the snapshot "
        f"holds {snapshot_tree(project.home).get(ACTIVE_LINK)!r}"
    )
    assert bin_entries(project.home) == [project.default_binary], (
        "…and the render it gated went on to write the trampolines"
    )


def test_a_repointed_active_withholds_the_project_from_the_prompt(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 30 / A7 — ``active`` swung at a second shell tree is refused by the
    prompt gate.

    Anyone who can write the home can swing a symlink between the render and the
    next prompt; the render lock is ocx's own convention and binds no one else.
    C-080's predicate is therefore evaluated by the **prompt** as well as by the
    render, read-only, and a mismatch withholds rather than heals — a prompt
    must never write.

    The observable is the emitter's own output, not ``ocx shell state``: the
    report carries ``toolchain_bin`` unconditionally (RUL-51) and would say the
    same thing in both states.

    RED: omit the ``active_is_valid`` call from ``bin_mode_entry``'s ladder —
    the project is exposed through a link it does not own and the assertion reds.
    """
    project = locked_project(ocx, tmp_path)
    exposed, _ = _path_segments(ocx, project.directory)
    assert str(path_facing_bin(project.home)) in exposed, (
        f"the control: a correct `active` exposes the project; segments {exposed}"
    )

    shell_bin(project.home, "other").mkdir(parents=True)
    os.unlink(active_link(project.home))
    os.symlink(f"{SHELLS_DIR}/other", active_link(project.home))

    segments, emitted = _path_segments(ocx, project.directory)
    assert str(path_facing_bin(project.home)) not in segments, (
        f"row 30 — a repointed `active` withholds the project; segments {segments}"
    )
    assert os.readlink(active_link(project.home)) == f"{SHELLS_DIR}/other", (
        "C-080 — …and the prompt healed nothing on its way there: a prompt is "
        "read-only, and the render is what repairs this"
    )
    assert f"ocx: {project.directory}: " in emitted, (
        f"…and the user is told which project went quiet. The hint travels as a "
        f"`printf … >&2` inside the emitted stream, not on the binary's own "
        f"stderr — the hook discards that (A-21). Emitted:\n{emitted}"
    )


@pytest.mark.skipif(
    sys.platform != "win32",
    reason=(
        "row 34 — Explorer / junction drag-copy dereference. The pytest "
        "acceptance job is Linux-only (#419), so this runs on no CI leg today: "
        "a labelled gap, never counted as coverage."
    ),
)
def test_a_junction_dereferencing_copy_leaves_a_real_directory_at_active(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Row 34 / A1 — Windows Explorer copies a junction by copying what it
    points at, so a drag-copied home carries a real directory at ``active``.

    Restated the same way WP-2 restated row 26, and for the same reason: C-081's
    table makes a real directory at ``active`` **healed** — ``remove_dir_all``
    then re-created — not refused. The matrix's "is refused" wording would red
    the specified behaviour.
    """
    project = locked_project(ocx, tmp_path)
    copied = tmp_path / "explorer-copy"
    shutil.copytree(project.directory, copied, symlinks=False)
    copied_home = toolchain_home(copied)
    assert copied_home.joinpath(ACTIVE_LINK).is_dir(), (
        "the control: the dereferencing copy left a real directory at `active`"
    )

    assert run_in(ocx, copied, "pull").returncode == EXIT_SUCCESS
    assert_symlink_exists(active_link(copied_home))
    assert os.readlink(active_link(copied_home)) == f"{SHELLS_DIR}\\{DEFAULT_SHELL}"


@pytest.mark.parametrize(
    "state",
    ["absent", "dangling", "escaping", "regular-file", "real-directory", "self-link"],
)
def test_a_prompt_writes_nothing_however_corrupt_the_tree_is(
    ocx: OcxRunner, tmp_path: Path, state: str
) -> None:
    """C-080's read-only half, which no matrix row asks for and which the
    ``[active-only]`` rows all depend on.

    Every one of those rows is "the render heals X, the prompt withholds on X",
    and the second clause is only meaningful if a prompt provably writes
    nothing. A prompt that healed would make the render's heal untestable — the
    corrupt state would be gone before the render saw it — and would put a write
    on the per-prompt path, which runs on every keystroke-to-newline in every
    interactive shell.

    ``snapshot_tree`` is ``lstat``-shaped and records a link's **raw** target, so
    a repoint is a difference here and not an accident of resolution.

    RED: make the prompt path call ``heal_active`` — every row where the state
    is repairable (all but ``escaping``, which the render also merely repoints)
    reds, because the snapshot changes.
    """
    project = locked_project(ocx, tmp_path)
    active = active_link(project.home)
    outside = tmp_path / "outside"
    outside.mkdir()
    os.unlink(active)
    if state == "absent":
        pass
    elif state == "dangling":
        os.symlink(f"{SHELLS_DIR}/ghost", active)
    elif state == "escaping":
        os.symlink(str(outside), active)
    elif state == "regular-file":
        active.write_bytes(b"not a link\n")
    elif state == "real-directory":
        active.mkdir()
        (active / "payload").write_bytes(b"foreign\n")
    else:
        os.symlink(ACTIVE_LINK, active)

    before = snapshot_tree(project.home)
    _path_segments(ocx, project.directory)
    run_in(ocx, project.directory, "--format", "json", "shell", "state")

    assert snapshot_tree(project.home) == before, (
        f"C-080 — a prompt is read-only; the {state!r} tree changed under it"
    )
    assert sorted(outside.rglob("*")) == [], (
        f"…and nothing reached {outside} through the link either"
    )


def test_shell_state_reports_the_launcher_directory_and_not_a_concatenation(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """G-1 / S-004 (toolchain_tree_layout) — ``toolchain_bin`` names the directory a consumer puts on
    ``PATH``, and it is not ``toolchain_home + "/bin"``.

    The end-to-end half of WP-3's field: its unit tests pin the wire contract,
    but the one expression that derives the value takes a ``&Context`` and is
    unreachable from any unit test (RUL-69), so the only check that can
    discriminate a wrong derivation is this one — a real pull, then reading the
    reported path and listing it.

    The negative is the whole point: every published recipe built the launcher
    directory by concatenating ``/bin`` onto ``toolchain_home``, and that path
    has not existed since the tree moved. ``jq`` exits 0 whatever it produces,
    so the failure surfaced steps later in someone else's automation.

    RED: derive the field as ``home.root().join("bin")`` — the reported path
    stops existing, the listing assertion reds, and so does the explicit
    "not the concatenation" one.
    """
    project = locked_project(ocx, tmp_path)
    reported = resolved_toolchain_bin(ocx, project.directory)
    home = resolved_toolchain_home(ocx, project.directory)

    assert reported == path_facing_bin(home), (
        f"G-1 — the field names `<home>/{ACTIVE_LINK}/bin`, the one spelling that "
        f"survives a repoint of the shell tree; got {reported}"
    )
    assert reported != home / "bin", (
        "G-1 — …and explicitly not the concatenation the old recipes built"
    )
    assert not (home / "bin").exists(), (
        f"…which names nothing at all under this layout, so a recipe that built "
        f"it exported a directory that does not exist; {home / 'bin'} exists"
    )
    assert reported.is_dir(), (
        f"G-1 — after a pull the reported directory is real; {reported} is not"
    )
    assert sorted(p.name for p in reported.iterdir()) == bin_entries(home), (
        f"…and resolves to the same trampolines the render physically wrote; "
        f"through `{ACTIVE_LINK}` it holds "
        f"{sorted(p.name for p in reported.iterdir())}, physically "
        f"{bin_entries(home)}"
    )
