# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Phase 10 specification tests for the user-guide ``Project Toolchain`` section.

Pure file-content checks against ``website/src/docs/user-guide.md``. These
tests pin the section structure required by the plan
(`.claude/state/plans/plan_project_toolchain.md` lines 859–873) and the
architect's binding constraints folded into Phase 10:

* Anchor renamed: ``{#project-toolchain-slsa}`` →
  ``{#project-toolchain-reproducibility}`` (architect warn-tier).
* The locking subsection MUST carry a callout warning users not to
  hand-author ``ocx.lock`` (architect path-1 finding) AND the
  ``ocx.lock merge=union`` ``.gitattributes`` guidance.
* The migration subsection MUST mention both the v1 deprecation note
  and the v2 removal of ``shell profile {add, remove, list, load}``,
  with ``generate`` surviving v2.
* The reproducibility subsection MUST mention SLSA in a forward-looking
  context only (no level claim), the v2 deferral of attestation, and the
  word ``v2`` so external readers know when richer provenance arrives.

No subprocess; no registry. The test reads the markdown directly and
asserts on substrings, anchors, and TODO markers. Failures here mean the
implement-phase doc body is incomplete or has drifted from the plan.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

# Project root is two directories above this test file:
# test/tests/test_doc_project_toolchain.py → repo root.
PROJECT_ROOT = Path(__file__).resolve().parents[2]
USER_GUIDE = PROJECT_ROOT / "website" / "src" / "docs" / "user-guide.md"


# Required anchors in their declared order. The slsa→reproducibility rename
# is the architect's warn-tier finding; both the old and new must NOT
# coexist (catches half-applied renames).
REQUIRED_ANCHORS_IN_ORDER = [
    "{#project-toolchain-toml}",
    "{#project-toolchain-lock}",
    "{#project-toolchain-pull-exec}",
    "{#project-toolchain-groups}",
    "{#project-toolchain-activation}",
    "{#project-toolchain-home-tier}",
    "{#project-toolchain-migration}",
    "{#project-toolchain-reproducibility}",
]


@pytest.fixture(scope="module")
def user_guide_text() -> str:
    """Read user-guide.md once per module."""
    assert USER_GUIDE.exists(), f"user-guide.md missing at {USER_GUIDE}"
    return USER_GUIDE.read_text(encoding="utf-8")


def _slice_section(text: str, anchor: str) -> str:
    """Return the text between ``anchor`` (exclusive) and the next H3 (or
    ``<!-- external -->`` link block / EOF), exclusive.

    Used to bound substring assertions to a specific subsection so a stray
    match in a sibling subsection doesn't satisfy a check.
    """
    start_match = re.search(rf"###[^\n]*{re.escape(anchor)}", text)
    assert start_match is not None, (
        f"anchor {anchor} not found while slicing section"
    )
    body_start = start_match.end()
    tail = text[body_start:]
    # Stop at the next H3 (`### `) at line start, the next H2 (`## `),
    # or the link-definition block at the bottom of the file.
    end_match = re.search(r"\n(###?\s|<!--\s*external\s*-->)", tail)
    if end_match is None:
        return tail
    return tail[: end_match.start()]


# ---------------------------------------------------------------------------
# H2 + anchor presence
# ---------------------------------------------------------------------------


def test_user_guide_has_project_toolchain_h2(user_guide_text: str) -> None:
    """The H2 anchor that the section as a whole hangs off."""
    assert "## Project Toolchain {#project-toolchain}" in user_guide_text, (
        "user-guide.md must declare the H2 "
        "`## Project Toolchain {#project-toolchain}` (plan Phase 10 deliverable)"
    )


def test_user_guide_has_all_required_subsection_anchors(
    user_guide_text: str,
) -> None:
    """Every documented subsection anchor must be present, in declared order."""
    cursor = 0
    for anchor in REQUIRED_ANCHORS_IN_ORDER:
        idx = user_guide_text.find(anchor, cursor)
        assert idx != -1, (
            f"required anchor {anchor} missing from user-guide.md "
            f"(or out of order — searched from offset {cursor})"
        )
        cursor = idx + len(anchor)


def test_user_guide_has_no_legacy_slsa_anchor(user_guide_text: str) -> None:
    """Architect rename: the old `-slsa` anchor must not coexist with the
    new `-reproducibility` one. Catches half-applied renames."""
    assert "{#project-toolchain-slsa}" not in user_guide_text, (
        "legacy anchor `{#project-toolchain-slsa}` must be removed; "
        "use `{#project-toolchain-reproducibility}` per architect finding"
    )


# ---------------------------------------------------------------------------
# Body completeness — TODO markers must all be resolved
# ---------------------------------------------------------------------------


def test_user_guide_has_no_phase10_todo_markers(user_guide_text: str) -> None:
    """The implement phase must replace every Phase-10 TODO placeholder."""
    pattern = re.compile(r"<!--\s*TODO:?\s*Phase\s*10[^\n]*-->", re.IGNORECASE)
    leftover = pattern.findall(user_guide_text)
    assert not leftover, (
        f"user-guide.md still has {len(leftover)} unimplemented Phase 10 "
        f"TODO markers: {leftover[:3]} (showing first 3)"
    )


# ---------------------------------------------------------------------------
# Locking subsection: hand-authoring callout + .gitattributes guidance
# ---------------------------------------------------------------------------


def test_locking_section_warns_against_hand_authoring(
    user_guide_text: str,
) -> None:
    """Architect path-1 finding: locking subsection must contain a warning
    or tip callout flagging that ``ocx.lock`` is machine-generated and must
    not be hand-edited. Mirrors the schema-side ``$comment`` enforced by
    crates/ocx_schema/tests/schema_outputs.rs."""
    section = _slice_section(user_guide_text, "{#project-toolchain-lock}")
    callout_match = re.search(r"^:::(warning|tip)", section, re.MULTILINE)
    assert callout_match is not None, (
        "locking subsection must contain a `:::warning` or `:::tip` callout "
        "(architect path-1 finding: warn against hand-authoring ocx.lock)"
    )


def test_locking_section_mentions_machine_generated_lockfile(
    user_guide_text: str,
) -> None:
    """The callout's content must communicate the machine-generated nature
    of the lock file. Substring-only — phrasing is the implementer's call."""
    section = _slice_section(user_guide_text, "{#project-toolchain-lock}")
    lower = section.lower()
    assert "machine" in lower or "do not edit" in lower or "do not hand" in lower, (
        "locking subsection callout must mention that `ocx.lock` is "
        "machine-generated (or otherwise warn against hand-editing). "
        "Searched for: 'machine', 'do not edit', 'do not hand'"
    )


def test_locking_section_includes_gitattributes_merge_union(
    user_guide_text: str,
) -> None:
    """Phase 10 deliverable bullet 4: `.gitattributes` guidance with the
    literal ``ocx.lock merge=union`` line. Substring is exact so users can
    copy-paste it without modification."""
    section = _slice_section(user_guide_text, "{#project-toolchain-lock}")
    assert "ocx.lock merge=union" in section, (
        "locking subsection must include the literal `ocx.lock merge=union` "
        "for `.gitattributes` (plan Phase 10 deliverable 4 — committing "
        "the lock file)"
    )


def test_locking_section_mentions_gitattributes(user_guide_text: str) -> None:
    """The merge=union line is meaningless without context — the prose
    must name `.gitattributes` so readers know where to put it."""
    section = _slice_section(user_guide_text, "{#project-toolchain-lock}")
    assert ".gitattributes" in section, (
        "locking subsection must reference `.gitattributes` so the "
        "`ocx.lock merge=union` line has installation context"
    )


# ---------------------------------------------------------------------------
# Migration subsection: v1 deprecation + v2 removal
# ---------------------------------------------------------------------------


def test_migration_section_covers_two_release_timeline(
    user_guide_text: str,
) -> None:
    """Architect's two-release-timeline finding: the migration subsection
    must explicitly call out v1 (deprecation note) and v2 (removal) so
    users know the schedule."""
    section = _slice_section(user_guide_text, "{#project-toolchain-migration}")
    assert "v1" in section, (
        "migration subsection must mention `v1` (deprecation phase). "
        "Architect: deprecation in v1, removal in v2."
    )
    assert "v2" in section, (
        "migration subsection must mention `v2` (removal phase). "
        "Architect: deprecation in v1, removal in v2."
    )


def test_migration_section_mentions_shell_profile_add_removal(
    user_guide_text: str,
) -> None:
    """Architect: ``shell profile add`` (along with ``remove``, ``list``,
    ``load``) is removed in v2; ``generate`` survives. The section must
    name at least the ``add`` subcommand explicitly so readers using it
    today know what's going away."""
    section = _slice_section(user_guide_text, "{#project-toolchain-migration}")
    assert "shell profile add" in section, (
        "migration subsection must name `shell profile add` as removed in "
        "v2 (architect's two-release-timeline finding)"
    )


def test_migration_section_names_all_removed_subcommands(
    user_guide_text: str,
) -> None:
    """Architect's binding constraint: the migration body must name all
    four removed subcommands (``add``, ``remove``, ``list``, ``load``) so
    every reader currently using one of them sees their flow called out
    explicitly. Substring-only — phrasing is the implementer's call."""
    section = _slice_section(user_guide_text, "{#project-toolchain-migration}")
    for subcommand in (
        "shell profile add",
        "shell profile remove",
        "shell profile list",
        "shell profile load",
    ):
        assert subcommand in section, (
            f"migration subsection must name `{subcommand}` as removed in "
            f"v2 (architect's binding constraint: enumerate the full "
            f"removal set so every existing user sees their flow listed)"
        )


def test_migration_section_preserves_shell_profile_generate(
    user_guide_text: str,
) -> None:
    """The migration narrative needs to make clear that
    ``shell profile generate`` is NOT removed (it's the boundary between
    interactive and one-shot generation flows)."""
    section = _slice_section(user_guide_text, "{#project-toolchain-migration}")
    assert "generate" in section, (
        "migration subsection must mention `generate` — it's the surviving "
        "subcommand after v2 removes add/remove/list/load"
    )


# ---------------------------------------------------------------------------
# Reproducibility subsection: SLSA framing (forward-looking, v2 attestation)
# ---------------------------------------------------------------------------


def test_reproducibility_section_links_slsa_for_v2_context_only(
    user_guide_text: str,
) -> None:
    """SLSA framing in the v1 doc is contextual / forward-looking only —
    no level claim. Per the SLSA v1.0 spec, levels L1–L3 cover producer-
    track build provenance, not consumer-side digest pinning, so the
    section must reference SLSA only as the destination concept that v2
    will deliver via signed attestations.

    Asserts: the substring ``SLSA`` is present (so external readers can
    follow the link to the spec), AND ``attestation`` is present (the
    capability deferred to v2), AND ``v2`` is present (the release that
    introduces it).
    """
    section = _slice_section(
        user_guide_text, "{#project-toolchain-reproducibility}"
    )
    assert "SLSA" in section, (
        "reproducibility subsection must mention `SLSA` as forward-looking "
        "context for the v2 attestation work (no level claim — see plan "
        "line 869: SLSA L1–L3 are producer-track build provenance, not "
        "consumer-side digest pinning)"
    )
    assert "attestation" in section.lower(), (
        "reproducibility subsection must mention `attestation` "
        "(deferred from v1 to v2 per plan deliverable 6)"
    )
    assert "v2" in section, (
        "reproducibility subsection must mention `v2` (the release that "
        "introduces signed attestations per plan deliverable 6)"
    )


def test_reproducibility_section_makes_no_slsa_level_claim(
    user_guide_text: str,
) -> None:
    """OCX v1 ships digest pinning, not SLSA build provenance. SLSA L1–L3
    are producer-track build provenance levels per the SLSA v1.0 spec —
    they do not cover consumer-side digest pinning. The subsection must
    not contain literal level tokens (``SLSA L1``, ``SLSA L2``,
    ``SLSA L3``) because external compliance graders grep for them and
    will misread their presence as a claim of conformance."""
    section = _slice_section(
        user_guide_text, "{#project-toolchain-reproducibility}"
    )
    for token in ("SLSA L1", "SLSA L2", "SLSA L3"):
        assert token not in section, (
            f"reproducibility subsection must not contain the literal "
            f"`{token}` — SLSA L1–L3 are producer-track build provenance "
            f"levels (v1.0 spec), not consumer-side digest pinning. v1 "
            f"makes no SLSA-level claim; reword to keep the framing "
            f"forward-looking only."
        )
