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

Design record: ``.claude/artifacts/adr_project_toolchain_config.md`` —
reference page entries for the new commands. ``pull`` already has a
documented body (see lines 520–571 of ``command-line.md`` at the time of
writing), so this file does not re-validate it.
"""
from __future__ import annotations

import json
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
    ("{#exec}", "exec (toolchain-tier)"),
    ("{#status}", "status"),
    ("{#inspect}", "inspect (toolchain-tier)"),
]

# Removed/tombstone anchors: these commands were deleted in the
# handshake_toolchain_cli.md taxonomy refactor.
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
    ("{#deselect}", "deselect", "Moved to"),
    ("{#uninstall}", "uninstall", "Moved to"),
]


@pytest.fixture(scope="module")
def cli_ref_text() -> str:
    """Read command-line.md once per module."""
    assert CLI_REF.exists(), f"command-line.md missing at {CLI_REF}"
    return CLI_REF.read_text(encoding="utf-8")


_FENCE = re.compile(r"\s*```")
_HEADING_LINE = re.compile(r"(#{1,6})\s")


def _slice_section_by_anchor(text: str, anchor: str) -> str:
    """Return text from `anchor` to the next heading at the same or higher
    level (or EOF). Used to bound checks to a specific command's body so
    `**Usage**` in a sibling doesn't satisfy the assertion.

    `anchor` is the literal `{#xxx}` form. We match an H3-H5 heading line
    ending in the anchor, then stop at the next heading at level <= the
    starting level. Fenced blocks are skipped: a `# comment` line inside a
    shell fence is not a heading, and treating it as one cuts the section
    short at the first example.
    """
    in_fence = False
    start_level = 0
    body: list[str] = []
    for line in text.splitlines(keepends=True):
        if _FENCE.match(line):
            in_fence = not in_fence
        heading = None if in_fence else _HEADING_LINE.match(line)
        if not start_level:
            if heading and 3 <= len(heading.group(1)) <= 5 and line.rstrip().endswith(anchor):
                start_level = len(heading.group(1))
            continue
        if heading and len(heading.group(1)) <= start_level:
            break
        body.append(line)
    assert start_level, f"anchor {anchor} not found"
    return "".join(body)


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
    """Plan C: the env-composition page's ``ocx exec`` section currently
    states "Ambient PATH entries from the parent shell are not forwarded",
    which is FALSE for the default (non-``--clean``) ``ocx exec`` — the
    default inherits the parent environment and merely *prepends* the
    composed tool ``bin/`` dirs to PATH; only ``--clean`` is hermetic.

    The false claim must be removed. This is a substring-absence assertion:
    it fails NOW (the false sentence is present) and passes once the page is
    corrected to describe the inherit-and-prepend default.
    """
    lowered = env_composition_text.lower()
    assert "ambient path entries from the parent shell are not forwarded" not in lowered, (
        "env-composition.md must NOT claim ambient PATH is not forwarded for "
        "the default `ocx exec` — the default inherits the parent environment "
        "and prepends composed tool bin/ dirs; only `--clean` is hermetic "
        "(plan amendment C)."
    )


def test_env_composition_states_default_run_inherits_and_prepends(
    env_composition_text: str,
) -> None:
    """Plan C positive form: the corrected page must state that the default
    ``ocx exec`` inherits the parent environment and prepends the composed
    tool bin dirs, and that only ``--clean`` is hermetic (matching
    ``exec --clean``). Substring presence — phrasing is the writer's call,
    but the load-bearing tokens must be there."""
    lowered = env_composition_text.lower()
    assert "--clean" in lowered, (
        "env-composition.md `ocx exec` section must reference `--clean` as the "
        "hermetic opt-in (plan amendment C)"
    )
    assert ("inherit" in lowered and "prepend" in lowered), (
        "env-composition.md must state the default `ocx exec` *inherits* the "
        "parent environment and *prepends* composed tool bin/ dirs (plan "
        "amendment C — the default is not hermetic)."
    )


@pytest.mark.parametrize("anchor,name", [
    ("{#pull}", "pull"),
    ("{#exec}", "exec"),
    ("{#update}", "update"),
])
def test_exit64_row_mentions_global_project_conflict(
    cli_ref_text: str, anchor: str, name: str
) -> None:
    """Plan C: ``command-line.md``'s exit-64 row for ``pull``/``exec``/
    ``update`` must mention the ``--global`` + ``--project`` conflict, for
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


# Root commands the taxonomy refactor moved under `ocx package`. The bare
# forms reach plugin dispatch and exit 64, so a runnable snippet naming one
# is a copy-paste trap.
#
# ponytail: shell fences only, deliberately — do NOT widen this to a plain
# string search over the docs. Reading fences is what makes the exemption
# free: `command-line.md`'s `> **Moved to `ocx package install`**` tombstones
# and the migration table in `user-guide.md` have to name the bare forms to
# document them, and they are prose, so they never match. A string ban needs
# an allowlist to stay green, and an allowlist is a thing people append to
# instead of fixing the doc. The ceiling is that prose still drifts (~80 such
# references today); fix that with a sweep, not by broadening the guard.
# `exec` is deliberately absent: unlike the others, the name was reused.
# The root `exec` that moved to `ocx package exec` is gone, and `ocx exec`
# is now the live toolchain-tier command renamed from `ocx run`.
MOVED_ROOT_COMMANDS = ("install", "uninstall", "select", "deselect", "which", "deps")
_MOVED_INVOCATION = re.compile(
    r"(?<![\w.-])ocx\s+(?:" + "|".join(MOVED_ROOT_COMMANDS) + r")(?![\w-])"
)
_SHELL_FENCE = re.compile(r"^```(?:sh|shell|bash|console)\b")


def test_no_moved_root_command_in_a_runnable_snippet() -> None:
    """No shell snippet under ``website/src/docs`` invokes a root command
    that now exits 64. Guards the whole docs tree, not just the CLI
    reference — the same trap has surfaced in the user guide, the storage
    page, and the environment reference."""
    docs = PROJECT_ROOT / "website" / "src" / "docs"
    offenders = []
    for page in sorted(docs.rglob("*.md")):
        in_shell = False
        for lineno, line in enumerate(page.read_text().splitlines(), 1):
            if line.startswith("```"):
                in_shell = bool(_SHELL_FENCE.match(line)) if not in_shell else False
            elif in_shell and _MOVED_INVOCATION.search(line):
                offenders.append(f"{page.relative_to(PROJECT_ROOT)}:{lineno}: {line.strip()}")
    assert not offenders, (
        "shell snippets invoke a root command that exits 64 — use the "
        "`ocx package <cmd>` form:\n  " + "\n  ".join(offenders)
    )


# ---------------------------------------------------------------------------
# Prose guard (issue #242) — separate mechanism from the fence guard above
# ---------------------------------------------------------------------------

# ponytail: an allowlist with per-site opt-in, deliberately NOT a wider version
# of the fence regex above. The fence guard reads fences because reading fences
# is what makes its exemption free; prose has the opposite property — a sentence
# that *documents* the removal must name the bare form, and no regex tells that
# apart from a sentence that assumes it still works. So the rule here is not
# "guess intent", it is "an unmarked prose occurrence is red". Two things turn
# it green, both explicit: the line is a `> **Moved to ...` tombstone (that
# shape IS the documentation of the removal), or it sits inside a
# `<!-- moved-command-ok -->` … `<!-- /moved-command-ok -->` region, which is a
# reviewable diff hunk rather than a suppression comment scattered per line.
# The ceiling: opening a region hides everything until the close marker, so a
# region kept small is on the author. If regions start spanning pages, split
# them — do not relax the guard.
_TOMBSTONE = "> **Moved to "
_OK_OPEN = "<!-- moved-command-ok"
_OK_CLOSE = "<!-- /moved-command-ok"


def test_no_unmarked_moved_root_command_in_prose() -> None:
    """No prose line under ``website/src/docs`` names a moved root command
    unless it is explicitly marked as documenting the move.

    Complements ``test_no_moved_root_command_in_a_runnable_snippet``: that one
    owns fenced snippets, this one owns everything outside a fence.
    """
    docs = PROJECT_ROOT / "website" / "src" / "docs"
    offenders = []
    for page in sorted(docs.rglob("*.md")):
        in_fence = False
        in_ok = False
        for lineno, line in enumerate(page.read_text().splitlines(), 1):
            if line.startswith("```"):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            if _OK_CLOSE in line:
                in_ok = False
                continue
            if _OK_OPEN in line:
                in_ok = True
                continue
            if in_ok or line.lstrip().startswith(_TOMBSTONE):
                continue
            if _MOVED_INVOCATION.search(line):
                offenders.append(
                    f"{page.relative_to(PROJECT_ROOT)}:{lineno}: {line.strip()}"
                )
    assert not offenders, (
        "prose names a root command that exits 64 — use the `ocx package <cmd>` "
        "form, or wrap the passage in `<!-- moved-command-ok -->` … "
        "`<!-- /moved-command-ok -->` if it documents the removal:\n  "
        + "\n  ".join(offenders)
    )


# ---------------------------------------------------------------------------
# Reference coverage — every command and flag of the published CLI document
# ---------------------------------------------------------------------------

# `cli.json` is read as data; a hidden command is exempt with its subtree.
CLI_GOLDEN = PROJECT_ROOT / "crates" / "ocx_schema" / "tests" / "golden" / "cli.json"

# Commands whose anchor is not their space-joined path: the toolchain-tier `env`
# takes `{#env-root}`, and the package-tier `deps` and `which` kept the
# anchors they had as root commands.
ANCHOR_OVERRIDES = {("env",): "env-root", ("package", "deps"): "deps", ("package", "which"): "which"}

# Measured at 73 visible commands, 293 long flags on them and 13 global flags;
# the slack absorbs a removal, and a reader that stopped descending (commands
# or args) reds long before it.
MIN_COMMAND_NODES = 70
MIN_COMMAND_FLAGS = 280
MIN_GLOBAL_FLAGS = 12

_OPTIONS_END = re.compile(r"(\*\*|:::)")


def _visible_commands(node: dict) -> list[dict]:
    """Every non-hidden command node under `node`, `node` itself excluded."""
    found: list[dict] = []
    for child in node.get("commands") or []:
        if child["hidden"]:
            continue
        found.append(child)
        found.extend(_visible_commands(child))
    return found


def _options_block(section: str, command_anchors: set[str]) -> str:
    """The lines after `**Options**` up to the next bold label or callout.

    The section ends at the first heading that is itself a command, so a child
    command's Options block never counts toward its parent's flags. A heading
    that only subdivides one command's prose (`package create`'s build receipt)
    does not end it.
    """
    kept: list[str] = []
    inside = False
    in_fence = False
    for line in section.splitlines():
        if _FENCE.match(line):
            in_fence = not in_fence
        if not in_fence and _HEADING_LINE.match(line) and line.rstrip().endswith(tuple(command_anchors)):
            break
        if line.strip() == "**Options**":
            inside = True
        elif inside and _OPTIONS_END.match(line):
            inside = False
        elif inside:
            kept.append(line)
    return "\n".join(kept)


def _name_cells(block: str) -> list[str]:
    """The flag-name part of each Options row: a table's first cell, a bullet's text before `:`."""
    cells: list[str] = []
    for line in block.splitlines():
        if line.startswith("|"):
            cells.append(line.split("|")[1])
        elif line.startswith("- "):
            cells.append(line[2:].split(":")[0])
    return cells


def _names_flag(cells: list[str], long: str) -> bool:
    needle = re.compile(rf"(?<![\w-])--{re.escape(long)}(?![\w-])")
    return any(needle.search(cell) for cell in cells)


def _command_anchor(node: dict) -> str:
    path = tuple(node["path"])
    return "{#" + ANCHOR_OVERRIDES.get(path, "-".join(path)) + "}"


def command_reference_problems(cli_root: dict, page: str) -> list[str]:
    nodes = _visible_commands(cli_root)
    problems: list[str] = []
    if len(nodes) < MIN_COMMAND_NODES:
        problems.append(f"only {len(nodes)} visible commands read (floor {MIN_COMMAND_NODES})")
    command_flags = 0
    command_anchors = {_command_anchor(node) for node in nodes}
    for node in nodes:
        path = tuple(node["path"])
        anchor = _command_anchor(node)
        try:
            section = _slice_section_by_anchor(page, anchor)
        except AssertionError:
            problems.append(f"`ocx {' '.join(path)}` has no heading with `{anchor}`")
            continue
        cells = _name_cells(_options_block(section, command_anchors))
        flags = [arg["long"] for arg in node["args"] if arg.get("long") and not arg["hidden"]]
        command_flags += len(flags)
        problems += [
            f"`ocx {' '.join(path)}` Options block has no row for `--{long}`"
            for long in flags
            if not _names_flag(cells, long)
        ]
    if command_flags < MIN_COMMAND_FLAGS:
        problems.append(f"only {command_flags} command flags read (floor {MIN_COMMAND_FLAGS})")
    general = page[page.index("## General Options") : page.index("## Exit codes")]
    headings = [line for line in general.splitlines() if line.startswith("### ")]
    global_flags = [arg["long"] for arg in cli_root["args"] if arg.get("long") and not arg["hidden"]]
    problems += [
        f"General Options has no heading for the global flag `--{long}`"
        for long in global_flags
        if not any(f"`--{long}`" in h for h in headings)
    ]
    if len(global_flags) < MIN_GLOBAL_FLAGS:
        problems.append(f"only {len(global_flags)} global flags read (floor {MIN_GLOBAL_FLAGS})")
    return problems


@pytest.fixture(scope="module")
def cli_root() -> dict:
    return json.loads(CLI_GOLDEN.read_text(encoding="utf-8"))["root"]


def test_command_reference_covers_every_command_and_flag(cli_root: dict, cli_ref_text: str) -> None:
    assert command_reference_problems(cli_root, cli_ref_text) == []


def test_a_flag_missing_from_its_options_block_is_red(cli_root: dict, cli_ref_text: str) -> None:
    row = re.compile(r"^\| `--no-pull` .*\n", re.MULTILINE)
    mutated = row.sub("", cli_ref_text, count=1)
    assert mutated != cli_ref_text, "mutation did not land"
    assert command_reference_problems(cli_root, mutated) == [
        "`ocx add` Options block has no row for `--no-pull`"
    ]


def test_a_flag_named_only_in_another_rows_description_is_red(cli_root: dict, cli_ref_text: str) -> None:
    """`package test`'s `--output` row says "exclusive with `--keep`"; that is not a row for `--keep`."""
    row = re.compile(r"^\| `--keep` .*\n", re.MULTILINE)
    mutated = row.sub("", cli_ref_text, count=1)
    assert mutated != cli_ref_text, "mutation did not land"
    assert "`--keep`" in mutated, "the surviving mention is the point of the case"
    assert command_reference_problems(cli_root, mutated) == [
        "`ocx package test` Options block has no row for `--keep`"
    ]


def test_a_command_without_its_anchor_is_red(cli_root: dict, cli_ref_text: str) -> None:
    mutated = cli_ref_text.replace("{#package-cascade}", "{#package-cascade-gone}")
    assert mutated != cli_ref_text, "mutation did not land"
    assert command_reference_problems(cli_root, mutated) == [
        "`ocx package cascade` has no heading with `{#package-cascade}`"
    ]


def test_a_global_flag_missing_from_general_options_is_red(cli_root: dict, cli_ref_text: str) -> None:
    mutated = cli_ref_text.replace("### `--jobs` {#arg-jobs}", "### Parallelism {#arg-jobs}")
    assert mutated != cli_ref_text, "mutation did not land"
    assert command_reference_problems(cli_root, mutated) == [
        "General Options has no heading for the global flag `--jobs`"
    ]


def test_a_short_command_walk_is_red(cli_root: dict, cli_ref_text: str) -> None:
    truncated = {**cli_root, "commands": cli_root["commands"][:5]}
    problems = command_reference_problems(truncated, cli_ref_text)
    assert any("visible commands read" in problem for problem in problems), problems


def _without_command_args(node: dict) -> dict:
    return {**node, "args": [], "commands": [_without_command_args(c) for c in node.get("commands") or []]}


def test_a_walk_that_read_no_command_flags_is_red(cli_root: dict, cli_ref_text: str) -> None:
    stripped = {**_without_command_args(cli_root), "args": cli_root["args"]}
    assert any(arg.get("long") for node in _visible_commands(cli_root) for arg in node["args"]), "nothing to strip"
    assert command_reference_problems(stripped, cli_ref_text) == [
        f"only 0 command flags read (floor {MIN_COMMAND_FLAGS})"
    ]


def test_a_walk_that_read_no_global_flags_is_red(cli_root: dict, cli_ref_text: str) -> None:
    assert any(arg.get("long") for arg in cli_root["args"]), "nothing to strip"
    assert command_reference_problems({**cli_root, "args": []}, cli_ref_text) == [
        f"only 0 global flags read (floor {MIN_GLOBAL_FLAGS})"
    ]


def test_a_childs_options_block_does_not_count_for_its_parent() -> None:
    anchors = {"{#parent-child}"}
    section = "Prose only.\n\n#### `child` {#parent-child}\n\n**Options**\n\n- `--only-child`: x\n"
    assert _options_block(section, anchors) == ""
    own = "##### Receipt {#parent-receipt}\n\n**Options**\n\n- `--own`: x\n\n#### `child` {#parent-child}\n\n- `--c`: y\n"
    assert _options_block(own, anchors) == "\n- `--own`: x\n"


def test_a_hidden_command_is_not_required(cli_root: dict, cli_ref_text: str) -> None:
    hidden = [c for c in cli_root["commands"] if c["hidden"]]
    assert hidden, "the golden carries no hidden command, so this case proves nothing"
    subtree = {" ".join(child["path"]) for c in hidden for child in c.get("commands") or []}
    assert "launcher exec" in subtree, "the golden no longer carries the hidden `launcher exec` this case names"
    assert "launcher exec" not in {" ".join(c["path"]) for c in _visible_commands(cli_root)}
