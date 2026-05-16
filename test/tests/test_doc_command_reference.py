# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Phase 10 specification tests for ``website/src/docs/reference/command-line.md``.

Pure file-content checks — no subprocess, no registry. Validates that
every new command introduced in the project-toolchain plan has:

* a stable anchor at ``{#name}``,
* a ``**Usage**`` block,
* a ``**Options**`` block.

Plus command-specific assertions:

* ``lock`` — references ``ocx.lock`` and ``declaration_hash``, includes
  an exit-code table.
* ``shell hook`` — references the ``prompt-hook`` flow and the
  ``_OCX_APPLIED`` fingerprint.

Plan reference: ``.claude/state/plans/plan_project_toolchain.md`` lines
859–873 (Phase 10 deliverable 5 — reference page entries for the new
commands). ``pull`` already has a documented body (see lines 520–571 of
``command-line.md`` at the time of writing), so this file does not
re-validate it.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[2]
CLI_REF = PROJECT_ROOT / "website" / "src" / "docs" / "reference" / "command-line.md"
ENV_COMPOSITION = (
    PROJECT_ROOT / "website" / "src" / "docs" / "reference" / "env-composition.md"
)


# Live commands: each must have a ``**Usage**`` and ``**Options**`` block.
# Updated to the new taxonomy (handshake_toolchain_cli.md §2):
#   - ``{#shell-hook}`` and ``{#shell-init}`` are TOMBSTONES (> **REMOVED**),
#     not live commands — moved to TOMBSTONE_ANCHORS below.
#   - New live commands added: ``{#env-root}`` (toolchain-tier ocx env),
#     ``{#env}`` (ocx package env), ``{#package-env}`` (full package-tier entry).
NEW_COMMAND_ANCHORS = [
    ("{#lock}", "lock"),
    ("{#direnv}", "direnv"),
    ("{#direnv-init}", "direnv init"),
    ("{#direnv-export}", "direnv export"),
    ("{#env-root}", "env (toolchain-tier)"),
    ("{#env}", "env (package-tier alias)"),
    ("{#package-env}", "package env"),
]

# Removed/tombstone anchors: these commands were deleted in the
# handshake_toolchain_cli.md taxonomy refactor (plan_toolchain_cli.md C4).
# They must have a ``> **REMOVED**`` or ``> **Moved to ...`` tombstone marker
# (NOT a ``**Usage**`` block — they no longer exist as live commands).
TOMBSTONE_ANCHORS = [
    ("{#shell-hook}", "shell hook", "REMOVED"),
    ("{#shell-init}", "shell init", "REMOVED"),
    ("{#shell-env}", "shell env", "REMOVED"),
    ("{#ci}", "ci", "REMOVED"),
    ("{#ci-export}", "ci export", "REMOVED"),
    ("{#install}", "install", "Moved to"),
    ("{#select}", "select", "Moved to"),
    ("{#exec}", "exec", "Moved to"),
    ("{#deselect}", "deselect", "Moved to"),
    ("{#uninstall}", "uninstall", "Moved to"),
]


@pytest.fixture(scope="module")
def cli_ref_text() -> str:
    """Read command-line.md once per module."""
    assert CLI_REF.exists(), f"command-line.md missing at {CLI_REF}"
    return CLI_REF.read_text(encoding="utf-8")


def _slice_section_by_anchor(text: str, anchor: str) -> str:
    """Return text from `anchor` to the next H3/H4/H5 heading at the same
    or higher level (or EOF). Used to bound checks to a specific command's
    body so `**Usage**` in a sibling doesn't satisfy the assertion.

    `anchor` is the literal `{#xxx}` form. We match any heading line that
    contains the anchor, then stop at the next heading at level <= the
    starting level.
    """
    heading_re = re.compile(
        rf"^(#{{3,5}})\s.*{re.escape(anchor)}\s*$",
        re.MULTILINE,
    )
    start = heading_re.search(text)
    assert start is not None, f"anchor {anchor} not found"
    start_level = len(start.group(1))
    body_start = start.end()
    tail = text[body_start:]

    # Stop at the next heading whose level <= start_level.
    next_heading = re.search(
        rf"^#{{1,{start_level}}}\s",
        tail,
        re.MULTILINE,
    )
    if next_heading is None:
        return tail
    return tail[: next_heading.start()]


# ---------------------------------------------------------------------------
# Anchor presence
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("anchor,name", NEW_COMMAND_ANCHORS)
def test_new_command_anchor_present(
    cli_ref_text: str, anchor: str, name: str
) -> None:
    """Every new command must declare its stable anchor."""
    assert anchor in cli_ref_text, (
        f"command-line.md must declare the `{name}` command anchor `{anchor}` "
        "(plan Phase 10 deliverable 5)"
    )


# ---------------------------------------------------------------------------
# Body completeness — Usage + Options blocks
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("anchor,name", NEW_COMMAND_ANCHORS)
def test_new_command_has_usage_block(
    cli_ref_text: str, anchor: str, name: str
) -> None:
    """Every new command must declare a ``**Usage**`` block."""
    section = _slice_section_by_anchor(cli_ref_text, anchor)
    assert "**Usage**" in section, (
        f"`{name}` section ({anchor}) must contain a `**Usage**` block "
        "(plan Phase 10 deliverable 5 — reference page entries)"
    )


@pytest.mark.parametrize("anchor,name", NEW_COMMAND_ANCHORS)
def test_new_command_has_options_block(
    cli_ref_text: str, anchor: str, name: str
) -> None:
    """Every new command must declare an ``**Options**`` block (catches
    truncated doc bodies that stop at the usage line)."""
    section = _slice_section_by_anchor(cli_ref_text, anchor)
    assert "**Options**" in section, (
        f"`{name}` section ({anchor}) must contain an `**Options**` block "
        "(catches truncated doc bodies)"
    )


# ---------------------------------------------------------------------------
# Tombstone anchors — removed commands must NOT have a Usage block;
# they must have the tombstone marker (REMOVED / Moved to)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("anchor,name,marker", TOMBSTONE_ANCHORS)
def test_tombstone_anchor_present(
    cli_ref_text: str, anchor: str, name: str, marker: str
) -> None:
    """Every tombstone anchor must still be declared in the doc (for stable links
    from external content). The anchor must exist even though the command is gone."""
    assert anchor in cli_ref_text, (
        f"tombstone anchor `{anchor}` ({name}) must still exist in command-line.md "
        f"for link stability — even removed commands keep their anchor as a tombstone"
    )


@pytest.mark.parametrize("anchor,name,marker", TOMBSTONE_ANCHORS)
def test_tombstone_has_removed_marker(
    cli_ref_text: str, anchor: str, name: str, marker: str
) -> None:
    """Every tombstone section must contain the expected removal marker
    (``> **REMOVED**`` or ``> **Moved to``), proving the section is a tombstone,
    not a live command with a missing Usage block."""
    section = _slice_section_by_anchor(cli_ref_text, anchor)
    assert marker in section, (
        f"tombstone section `{name}` ({anchor}) must contain '{marker}' marker; "
        f"got section:\n{section[:200]!r}"
    )


@pytest.mark.parametrize("anchor,name,marker", [
    (a, n, m) for a, n, m in TOMBSTONE_ANCHORS if m == "REMOVED"
])
def test_removed_tombstone_has_no_usage_block(
    cli_ref_text: str, anchor: str, name: str, marker: str
) -> None:
    """Pure-REMOVED tombstone sections must NOT have a ``**Usage**`` block.

    Commands with ``> **REMOVED**`` are fully deleted and must not document
    any usage form (there is no new location to redirect to). This is distinct
    from ``> **Moved to ...`` tombstones, which legitimately show the new
    ``ocx package ...`` usage form at the redirect target.
    """
    section = _slice_section_by_anchor(cli_ref_text, anchor)
    assert "**Usage**" not in section, (
        f"REMOVED tombstone `{name}` ({anchor}) must NOT have a **Usage** block; "
        f"fully-deleted commands must only show the '> **REMOVED**' marker"
    )


# ---------------------------------------------------------------------------
# No remaining stub markers
# ---------------------------------------------------------------------------


def test_command_reference_has_no_phase10_todo_markers(
    cli_ref_text: str,
) -> None:
    """The implement phase must replace every Phase-10 TODO placeholder."""
    pattern = re.compile(r"<!--\s*TODO:?\s*Phase\s*10[^\n]*-->", re.IGNORECASE)
    leftover = pattern.findall(cli_ref_text)
    assert not leftover, (
        f"command-line.md still has {len(leftover)} unimplemented Phase 10 "
        f"TODO markers: {leftover[:3]} (showing first 3)"
    )


# ---------------------------------------------------------------------------
# Per-command body assertions
# ---------------------------------------------------------------------------


def test_lock_section_mentions_ocx_lock_filename(cli_ref_text: str) -> None:
    """`lock` must reference the file it writes."""
    section = _slice_section_by_anchor(cli_ref_text, "{#lock}")
    assert "ocx.lock" in section, (
        "`lock` section must reference `ocx.lock` (the file it writes)"
    )


def test_lock_section_mentions_declaration_hash(cli_ref_text: str) -> None:
    """`lock` must reference `declaration_hash` so users understand the
    staleness model that drives `pull`'s exit-code 65 (`DataError`)."""
    section = _slice_section_by_anchor(cli_ref_text, "{#lock}")
    assert "declaration_hash" in section, (
        "`lock` section must reference `declaration_hash` "
        "(see ocx pull --dry-run docs and exit-code 65 contract)"
    )


def test_lock_section_has_exit_code_table(cli_ref_text: str) -> None:
    """`lock` must declare its exit-code contract as a table — same
    convention `pull` already follows in command-line.md."""
    section = _slice_section_by_anchor(cli_ref_text, "{#lock}")
    assert "| Code | Meaning |" in section, (
        "`lock` section must include an exit-code table (`| Code | Meaning |` "
        "header) — convention from `pull` section"
    )


def test_shell_hook_section_references_prompt_hook(cli_ref_text: str) -> None:
    """`shell hook` must reference the `prompt-hook` flow it serves."""
    section = _slice_section_by_anchor(cli_ref_text, "{#shell-hook}")
    assert "prompt-hook" in section.lower() or "prompt hook" in section.lower() or "prompt cycle" in section.lower(), (
        "`shell hook` section must reference the `prompt-hook` flow "
        "(it's the prompt-side machinery that calls shell hook)"
    )


def test_shell_hook_section_references_applied_fingerprint(
    cli_ref_text: str,
) -> None:
    """The fingerprint env var is `_OCX_APPLIED`. This assertion verifies
    the reference page names it accurately."""
    section = _slice_section_by_anchor(cli_ref_text, "{#shell-hook}")
    assert "_OCX_APPLIED" in section, (
        "`shell hook` section must mention the applied-fingerprint env "
        "var `_OCX_APPLIED`"
    )


# ---------------------------------------------------------------------------
# C (doc accuracy) — plan §"Living Design — Review-Fix Amendments" C
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def env_composition_text() -> str:
    """Read env-composition.md once per module."""
    assert ENV_COMPOSITION.exists(), (
        f"env-composition.md missing at {ENV_COMPOSITION}"
    )
    return ENV_COMPOSITION.read_text(encoding="utf-8")


def test_env_composition_does_not_claim_ambient_path_not_forwarded(
    env_composition_text: str,
) -> None:
    """Plan C: the env-composition page's ``ocx run`` section currently
    states "Ambient PATH entries from the parent shell are not forwarded",
    which is FALSE for the default (non-``--clean``) ``ocx run`` — the
    default inherits the parent environment and merely *prepends* the
    composed tool ``bin/`` dirs to PATH; only ``--clean`` is hermetic.

    The false claim must be removed. This is a substring-absence assertion:
    it fails NOW (the false sentence is present) and passes once the page is
    corrected to describe the inherit-and-prepend default.
    """
    lowered = env_composition_text.lower()
    assert "ambient path entries from the parent shell are not forwarded" not in lowered, (
        "env-composition.md must NOT claim ambient PATH is not forwarded for "
        "the default `ocx run` — the default inherits the parent environment "
        "and prepends composed tool bin/ dirs; only `--clean` is hermetic "
        "(plan amendment C)."
    )


def test_env_composition_states_default_run_inherits_and_prepends(
    env_composition_text: str,
) -> None:
    """Plan C positive form: the corrected page must state that the default
    ``ocx run`` inherits the parent environment and prepends the composed
    tool bin dirs, and that only ``--clean`` is hermetic (matching
    ``exec --clean``). Substring presence — phrasing is the writer's call,
    but the load-bearing tokens must be there."""
    lowered = env_composition_text.lower()
    assert "--clean" in lowered, (
        "env-composition.md `ocx run` section must reference `--clean` as the "
        "hermetic opt-in (plan amendment C)"
    )
    assert ("inherit" in lowered and "prepend" in lowered), (
        "env-composition.md must state the default `ocx run` *inherits* the "
        "parent environment and *prepends* composed tool bin/ dirs (plan "
        "amendment C — the default is not hermetic)."
    )


@pytest.mark.parametrize("anchor,name", [
    ("{#pull}", "pull"),
    ("{#run}", "run"),
    ("{#upgrade}", "upgrade"),
])
def test_exit64_row_mentions_global_project_conflict(
    cli_ref_text: str, anchor: str, name: str
) -> None:
    """Plan C: ``command-line.md``'s exit-64 row for ``pull``/``run``/
    ``upgrade`` must mention the ``--global`` + ``--project`` conflict, for
    parity with ``add``/``lock``/``remove`` (which already say "`--global`
    combined with `--project`").

    Currently these three sections' exit-64 rows do NOT name ``--global`` at
    all, so this fails now and pins the doc gap. Bound to the command's own
    section so an `add`-section match cannot satisfy it.
    """
    section = _slice_section_by_anchor(cli_ref_text, anchor)
    # Find the exit-code-64 table row within this command's section.
    row_match = re.search(r"^\|\s*64\s*\|[^\n]*$", section, re.MULTILINE)
    assert row_match is not None, (
        f"`{name}` ({anchor}) section must have an exit-64 table row"
    )
    row = row_match.group(0)
    assert "--global" in row and "--project" in row, (
        f"`{name}` ({anchor}) exit-64 row must mention the `--global` + "
        f"`--project` conflict for parity with `add`/`lock`/`remove` "
        f"(plan amendment C); got row: {row!r}"
    )


def test_global_flag_section_links_strict_isolation(cli_ref_text: str) -> None:
    """Plan C: the ``--global`` flag section must carry the
    ``[env-composition-strict-isolation]`` reference link so readers reach
    the strict-isolation spec. Fails now (the link is absent)."""
    section = _slice_section_by_anchor(cli_ref_text, "{#global-flag}")
    assert "env-composition-strict-isolation" in section, (
        "the `--global` flag section ({#global-flag}) must reference "
        "`[env-composition-strict-isolation]` so users reach the "
        "strict-isolation spec (plan amendment C)"
    )
