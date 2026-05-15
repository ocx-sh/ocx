# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Documentation contract tests for the Project Toolchain pages.

Pure file-content checks. After main's user-guide redesign
(``docs(website): redesign user guide as use-case-driven walkthrough``),
the canonical home for project-toolchain depth moved from
``website/src/docs/user-guide.md`` (single deep section) to
``website/src/docs/in-depth/project.md`` (dedicated In Depth page) with a
short use-case stub in ``user-guide.md`` linking into it.

These tests therefore enforce two contracts:

* **In Depth page** — required H2 anchors in declared order, locking
  callout (machine-generated lockfile, ``ocx.lock merge=union``
  ``.gitattributes`` guidance), and SLSA framing that is forward-looking
  only (no level claim, ``v2``-tagged attestation deferral).
* **User guide** — has a use-case section that mentions project tooling
  and links to the In Depth page so readers can find the depth.

The original Phase-10 anchor scheme used ``project-toolchain-*`` prefixes
because the section was nested under an H2 in ``user-guide.md``. Now that
the deep content lives in a standalone page, anchors are short
(``#toml``, ``#lock``) per the convention used by the other In Depth
pages (``storage.md`` uses ``#packages``, not ``#storage-packages``).

No subprocess; no registry. The test reads markdown directly and asserts
on substrings, anchors, and TODO markers.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

# Project root is two directories above this test file:
# test/tests/test_doc_project_toolchain.py → repo root.
PROJECT_ROOT = Path(__file__).resolve().parents[2]
USER_GUIDE = PROJECT_ROOT / "website" / "src" / "docs" / "user-guide.md"
IN_DEPTH = PROJECT_ROOT / "website" / "src" / "docs" / "in-depth" / "project.md"


# Required anchors in their declared order on the In Depth page. The
# old slsa→reproducibility rename remains a hard constraint.
#
# Living Design — anchor rename by ``adr_global_toolchain_tier.md``
# §Decision 1: the implicit ``$OCX_HOME/ocx.toml`` home-tier fallback was
# DELETED and replaced by the explicit ``--global`` toolchain tier. The
# In Depth page's ``## Home tier {#home-tier}`` section was rewritten to
# ``## Global toolchain {#global-toolchain}`` accordingly (the page now
# states verbatim "Unlike the old home-tier fallback, the global
# toolchain is never discovered implicitly"). This is a sanctioned
# rename, not a dropped anchor — it occupies the same ordinal slot the
# home-tier section did and every other required anchor is unchanged and
# still in order. Updating the contract list to the new structure (not
# weakening it).
REQUIRED_ANCHORS_IN_ORDER = [
    "{#toml}",
    "{#lock}",
    "{#pull-exec}",
    "{#groups}",
    "{#activation}",
    "{#global-toolchain}",
    "{#reproducibility}",
]


@pytest.fixture(scope="module")
def in_depth_text() -> str:
    """Read in-depth/project.md once per module."""
    assert IN_DEPTH.exists(), f"in-depth/project.md missing at {IN_DEPTH}"
    return IN_DEPTH.read_text(encoding="utf-8")


@pytest.fixture(scope="module")
def user_guide_text() -> str:
    """Read user-guide.md once per module."""
    assert USER_GUIDE.exists(), f"user-guide.md missing at {USER_GUIDE}"
    return USER_GUIDE.read_text(encoding="utf-8")


def _slice_section(text: str, anchor: str) -> str:
    """Return the text between an H2 ``anchor`` (exclusive) and the next
    H2 (or ``<!-- external -->`` link block / EOF), exclusive.

    Used to bound substring assertions to a specific subsection so a stray
    match in a sibling subsection doesn't satisfy a check.
    """
    start_match = re.search(rf"^##[^\n]*{re.escape(anchor)}", text, re.MULTILINE)
    assert start_match is not None, (
        f"anchor {anchor} not found while slicing section"
    )
    body_start = start_match.end()
    tail = text[body_start:]
    # Stop at the next H2 (`## `) at line start or the link-definition
    # block at the bottom of the file.
    end_match = re.search(r"\n(##\s|<!--\s*external\s*-->)", tail)
    if end_match is None:
        return tail
    return tail[: end_match.start()]


# ---------------------------------------------------------------------------
# In Depth: H1 + anchor presence
# ---------------------------------------------------------------------------


def test_in_depth_has_project_toolchain_h1(in_depth_text: str) -> None:
    """The dedicated In Depth page is titled `# Project Toolchain`."""
    assert "# Project Toolchain" in in_depth_text, (
        "in-depth/project.md must declare the H1 `# Project Toolchain`"
    )


def test_in_depth_has_all_required_subsection_anchors(
    in_depth_text: str,
) -> None:
    """Every documented subsection anchor must be present, in declared order."""
    cursor = 0
    for anchor in REQUIRED_ANCHORS_IN_ORDER:
        idx = in_depth_text.find(anchor, cursor)
        assert idx != -1, (
            f"required anchor {anchor} missing from in-depth/project.md "
            f"(or out of order — searched from offset {cursor})"
        )
        cursor = idx + len(anchor)


def test_in_depth_has_no_legacy_slsa_anchor(in_depth_text: str) -> None:
    """Architect rename: the old `-slsa` anchor must not coexist with the
    new `-reproducibility` one. Catches half-applied renames."""
    assert "{#slsa}" not in in_depth_text, (
        "legacy anchor `{#slsa}` must be removed; use `{#reproducibility}` "
        "per architect finding"
    )
    assert "{#project-toolchain-slsa}" not in in_depth_text, (
        "legacy anchor `{#project-toolchain-slsa}` must be removed"
    )


# ---------------------------------------------------------------------------
# Body completeness — TODO markers must all be resolved
# ---------------------------------------------------------------------------


def test_in_depth_has_no_phase10_todo_markers(in_depth_text: str) -> None:
    """The implement phase must replace every Phase-10 TODO placeholder."""
    pattern = re.compile(r"<!--\s*TODO:?\s*Phase\s*10[^\n]*-->", re.IGNORECASE)
    leftover = pattern.findall(in_depth_text)
    assert not leftover, (
        f"in-depth/project.md still has {len(leftover)} unimplemented "
        f"Phase 10 TODO markers: {leftover[:3]} (showing first 3)"
    )


# ---------------------------------------------------------------------------
# Locking subsection: hand-authoring callout + .gitattributes guidance
# ---------------------------------------------------------------------------


def test_locking_section_warns_against_hand_authoring(
    in_depth_text: str,
) -> None:
    """Architect path-1 finding: locking subsection must contain a warning
    or tip callout flagging that ``ocx.lock`` is machine-generated and must
    not be hand-edited. Mirrors the schema-side ``$comment`` enforced by
    crates/ocx_schema/tests/schema_outputs.rs."""
    section = _slice_section(in_depth_text, "{#lock}")
    callout_match = re.search(r"^:::\s*(warning|tip)", section, re.MULTILINE)
    assert callout_match is not None, (
        "locking subsection must contain a `:::warning` or `:::tip` callout "
        "(architect path-1 finding: warn against hand-authoring ocx.lock)"
    )


def test_locking_section_mentions_machine_generated_lockfile(
    in_depth_text: str,
) -> None:
    """The callout's content must communicate the machine-generated nature
    of the lock file. Substring-only — phrasing is the implementer's call."""
    section = _slice_section(in_depth_text, "{#lock}")
    lower = section.lower()
    assert "machine" in lower or "do not edit" in lower or "do not hand" in lower, (
        "locking subsection callout must mention that `ocx.lock` is "
        "machine-generated (or otherwise warn against hand-editing). "
        "Searched for: 'machine', 'do not edit', 'do not hand'"
    )


def test_locking_section_includes_gitattributes_merge_union(
    in_depth_text: str,
) -> None:
    """`.gitattributes` guidance with the literal ``ocx.lock merge=union``
    line. Substring is exact so users can copy-paste it without modification."""
    section = _slice_section(in_depth_text, "{#lock}")
    assert "ocx.lock merge=union" in section, (
        "locking subsection must include the literal `ocx.lock merge=union` "
        "for `.gitattributes` (committing the lock file)"
    )


def test_locking_section_mentions_gitattributes(in_depth_text: str) -> None:
    """The merge=union line is meaningless without context — the prose
    must name `.gitattributes` so readers know where to put it."""
    section = _slice_section(in_depth_text, "{#lock}")
    assert ".gitattributes" in section, (
        "locking subsection must reference `.gitattributes` so the "
        "`ocx.lock merge=union` line has installation context"
    )


# ---------------------------------------------------------------------------
# Reproducibility subsection: SLSA framing (forward-looking, v2 attestation)
# ---------------------------------------------------------------------------


def test_reproducibility_section_links_slsa_for_v2_context_only(
    in_depth_text: str,
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
    section = _slice_section(in_depth_text, "{#reproducibility}")
    assert "SLSA" in section, (
        "reproducibility subsection must mention `SLSA` as forward-looking "
        "context for the v2 attestation work (no level claim — SLSA L1–L3 "
        "are producer-track build provenance, not consumer-side digest "
        "pinning)"
    )
    assert "attestation" in section.lower(), (
        "reproducibility subsection must mention `attestation` "
        "(deferred from v1 to v2)"
    )
    assert "v2" in section, (
        "reproducibility subsection must mention `v2` (the release that "
        "introduces signed attestations)"
    )


def test_reproducibility_section_makes_no_slsa_level_claim(
    in_depth_text: str,
) -> None:
    """OCX v1 ships digest pinning, not SLSA build provenance. SLSA L1–L3
    are producer-track build provenance levels per the SLSA v1.0 spec —
    they do not cover consumer-side digest pinning. The subsection must
    not contain literal level tokens (``SLSA L1``, ``SLSA L2``,
    ``SLSA L3``) because external compliance graders grep for them and
    will misread their presence as a claim of conformance."""
    section = _slice_section(in_depth_text, "{#reproducibility}")
    for token in ("SLSA L1", "SLSA L2", "SLSA L3"):
        assert token not in section, (
            f"reproducibility subsection must not contain the literal "
            f"`{token}` — SLSA L1–L3 are producer-track build provenance "
            f"levels (v1.0 spec), not consumer-side digest pinning. v1 "
            f"makes no SLSA-level claim; reword to keep the framing "
            f"forward-looking only."
        )


# ---------------------------------------------------------------------------
# User guide: has a stub linking into the In Depth page
# ---------------------------------------------------------------------------


def test_user_guide_links_to_in_depth_project(user_guide_text: str) -> None:
    """The use-case section in the user guide must link into the In Depth
    page so readers find the depth. The ref-style label
    ``in-depth-project`` resolves to ``./in-depth/project.md`` per the
    user-guide link block."""
    assert "[in-depth-project]: ./in-depth/project.md" in user_guide_text, (
        "user-guide.md must define the `in-depth-project` reference link "
        "pointing at `./in-depth/project.md` so the use-case section can "
        "deep-link into the dedicated In Depth page"
    )
    assert "[in-depth-project]" in user_guide_text or "[Project Toolchain In Depth][in-depth-project]" in user_guide_text, (
        "user-guide.md must reference the in-depth project page via the "
        "`in-depth-project` ref-style link in a `Learn more` callout"
    )


def test_user_guide_mentions_project_toolchain(user_guide_text: str) -> None:
    """The user guide must surface the project toolchain concept so users
    can find it from the use-case-driven entry point. Phrasing is
    flexible; the literal `ocx.toml` is the most reliable signal."""
    assert "ocx.toml" in user_guide_text, (
        "user-guide.md must mention `ocx.toml` (the project toolchain "
        "manifest) somewhere in the use-case sections so readers can "
        "discover it"
    )
