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


# Each entry: (anchor, human-readable command name used in error messages)
NEW_COMMAND_ANCHORS = [
    ("{#lock}", "lock"),
    ("{#shell-hook}", "shell hook"),
    ("{#shell-direnv}", "shell direnv"),
    ("{#shell-init}", "shell init"),
    ("{#generate}", "generate"),
    ("{#generate-direnv}", "generate direnv"),
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
