# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Docs guards that survive the generated command reference.

``reference/command-line/`` is generated from the clap tree, and the coverage floors
that compared it with ``cli.json`` live in the generator's own tests
(``crates/ocx_schema/tests/docs.rs``). What stays here reads authored prose:

* no runnable snippet or unmarked prose line names a root command that now exits 64,
* ``env-composition.md`` describes the default ``ocx exec`` as inherit-and-prepend.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[2]
ENV_COMPOSITION = (
    PROJECT_ROOT / "website" / "src" / "content" / "docs" / "reference" / "env-composition.md"
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
    assert "inherit" in lowered and "prepend" in lowered, (
        "env-composition.md must state the default `ocx exec` *inherits* the "
        "parent environment and *prepends* composed tool bin/ dirs (plan "
        "amendment C — the default is not hermetic)."
    )


# Root commands the taxonomy refactor moved under `ocx package`. The bare
# forms reach plugin dispatch and exit 64, so a runnable snippet naming one
# is a copy-paste trap.
#
# ponytail: shell fences only, deliberately — do NOT widen this to a plain
# string search over the docs. Reading fences is what makes the exemption
# free: the `> **Moved to `ocx package install`**` tombstones
# have to name the bare forms to document them, and they are prose, so they
# never match. A string ban needs
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
    """No shell snippet under ``website/src/content/docs`` invokes a root command
    that now exits 64. Guards the whole docs tree, not just the CLI
    reference — the same trap has surfaced in the user guide, the storage
    page, and the environment reference."""
    docs = PROJECT_ROOT / "website" / "src" / "content" / "docs"
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
    """No prose line under ``website/src/content/docs`` names a moved root command
    unless it is explicitly marked as documenting the move.

    Complements ``test_no_moved_root_command_in_a_runnable_snippet``: that one
    owns fenced snippets, this one owns everything outside a fence.
    """
    docs = PROJECT_ROOT / "website" / "src" / "content" / "docs"
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
