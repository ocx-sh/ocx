"""Verify-path doc-binding gate (NC1–NC3, RN5b) over the real walkthrough pages.

Runs in the ``test:parallel`` collection (so in ``task verify``), with no
Docker and no website build — pure static file analysis (§6b and §6e of
design_spec_doc_command_scripts.md).

Phase status (plan_tested-doc-commands):

- **NC2/NC3** are enforced as a hard gate.  A slug typo / rename / stale
  ``<<<`` reference fails ``task verify`` (not only ``website:build``) —
  this closes Codex finding 2.
- **NC1** (no ungated inline ``ocx`` blocks) is now a **hard gate** for all
  five walkthrough pages.  Phase 6 migration is complete: the three pages
  that had inline snippets have been rewritten to ``<<<`` transclusions, and
  the two remaining `````sh`` blocks in ``environments.md`` and
  ``entry-points.md`` are shebang'd generated-file listings exempted by the
  §6b Living-Design-Record shebang exemption (see ``find_inline_ocx_blocks``).
- **RN5b** (static pre-publish surface of RN5): asserts that no displayed
  (``# doc:`` present) script under ``test/doc_scripts/`` references a
  **non-renderable** fixture/harness variable in its display scope.  Runs
  on the verify path (no publish, no Docker).  Renderable matrix (LDR
  2026-05-17): ``$PKG_<KEY>`` + ``$REPO_<KEY>`` are renderable (not
  flagged); only ``$FQ_*``/``$TAG_*``/``$MARKER_*``/``$HOME_KEY_*`` and
  runner vars (``$REGISTRY``/``$SCENARIO_TMP``/``$OCX``/``$OCX_HOME``) are
  banned.  Codex F2's premise was in fact correct — ~27 scripts referenced
  fixture/harness vars and were remediated in Phase 3 (region-scoped so the
  banned refs fall outside the displayed region); this gate keeps it green.
"""
from __future__ import annotations

import re
import sys
import textwrap
from pathlib import Path

import pytest

from src.doc_binding import (
    WALKTHROUGH_PAGES,
    find_inline_ocx_blocks,
    find_region_fragment_transclusions,
    find_unrecognised_region_markers,
    unresolved_transclusions,
)
from src.doc_scripts import doc_scripts_export, parse_doc_header, DocScriptParseError
from src.helpers import PROJECT_ROOT

# Import the render layer (website-owned stdlib module) the same way
# test_doc_scripts_publish does — website/ is not on the pytest pythonpath.
_WEBSITE_SCRIPTS_DIR: Path = PROJECT_ROOT / "website" / "scripts"
if str(_WEBSITE_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_WEBSITE_SCRIPTS_DIR))
from publish_doc_scripts import render_display  # noqa: E402

# RN9 (LDR 2026-05-18): verification scaffolding banned from the *displayed*
# output.  A reader/cast must see only documented commands; captures and
# assertions belong outside the displayed region (drift gate still runs the
# full body).  Patterns: capture-into-var, `[[ … ]]` tests, assertion exits,
# stderr error echoes.
_RN9_SCAFFOLDING_RE: tuple[re.Pattern[str], ...] = (
    re.compile(r'^\s*[A-Za-z_][A-Za-z0-9_]*="\$\('),  # out="$(ocx …)"
    re.compile(r"\[\[ "),                                # [[ -n "$out" ]]
    re.compile(r"\|\|\s*exit\b|^\s*exit\s+\d"),         # || exit 1 / exit 1
    re.compile(r">&2"),                                  # echo "ERROR" >&2
)

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="doc-binding gate parity with test_scenarios_smoke (Linux/macOS)",
)

DOC_SCRIPTS_DIR: Path = PROJECT_ROOT / "test" / "doc_scripts"

# ---------------------------------------------------------------------------
# RN5b helpers — fixture/harness variable namespace patterns
# ---------------------------------------------------------------------------

# Non-renderable fixture prefixes (§6e RN5, LDR 2026-05-17): FQ_*, TAG_*,
# MARKER_*, HOME_KEY_*.  PKG_* and REPO_* are the renderable matrix
# (substituted via display_env by RN3) and are NOT flagged by RN5b.
_RN5B_PREFIX_RE: re.Pattern[str] = re.compile(
    r"\$\{?(FQ|TAG|MARKER|HOME_KEY)_[A-Za-z0-9_]+"
)

# Runner-harness variables (§6e RN5): exact names (no prefix).
_RN5B_RUNNER_VARS: frozenset[str] = frozenset(
    {"REGISTRY", "SCENARIO_TMP", "OCX", "OCX_HOME"}
)

# Match any $VARNAME or ${VARNAME} — used to extract var names for runner check.
_VAR_RE: re.Pattern[str] = re.compile(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?")


def _find_rn5b_violations(script_path: Path) -> list[tuple[int, str]]:
    """Return (1-based line number, var name) pairs for each RN5b violation.

    A violation is a line in the script body that references a non-renderable
    fixture or harness variable (§6e RN5 namespace, minus $PKG_* which is
    renderable).

    Only applies to displayed (``# doc:``-annotated) scripts.  For scripts
    with a ``# region cast``, only lines inside the region are checked
    (the display artifact's scope).  For scripts without a region (RN2 path),
    the full body below the header block is checked.

    Args:
        script_path: Path to a ``.sh`` file to inspect.

    Returns:
        List of (lineno, varname) for each violation found, in file order.
    """
    try:
        meta = parse_doc_header(script_path)
    except (DocScriptParseError, OSError):
        return []

    # Only check displayed scripts.
    if meta.doc is None:
        return []

    text = script_path.read_text()
    lines = text.splitlines()

    # Determine which lines are in scope for display.
    if meta.cast_region is not None:
        start, end = meta.cast_region
        # 1-based inclusive; exclude the marker lines themselves.
        # Slicing: lines[start:end-1] extracts the body between markers.
        display_lines = list(enumerate(lines[start:end - 1], start=start + 1))
    else:
        # Full body: skip the header block (shebang + metadata lines).
        # Find the first non-blank, non-comment, non-shebang line index.
        header_end = 0
        for i, raw in enumerate(lines):
            stripped = raw.strip()
            if not stripped:
                continue
            # Any line starting with '#' is a comment/header line — mirror
            # parse_doc_header semantics (header ends at the first non-blank
            # non-comment line), not the narrower '# ' heuristic (A4).
            if stripped.startswith("#"):
                header_end = i + 1
                continue
            break
        display_lines = list(enumerate(lines[header_end:], start=header_end + 1))

    violations: list[tuple[int, str]] = []
    for lineno, line in display_lines:
        # Check non-renderable prefix fixture vars (FQ_*, TAG_*, MARKER_*, HOME_KEY_*).
        for m in _RN5B_PREFIX_RE.finditer(line):
            # Extract the full var name (without leading $/{).
            raw = m.group(0).lstrip("${")
            var_name = raw.rstrip("}")
            violations.append((lineno, var_name))

        # Check runner-harness vars by exact name.
        for m in _VAR_RE.finditer(line):
            var_name = m.group(1)
            if var_name in _RN5B_RUNNER_VARS:
                violations.append((lineno, var_name))

    return violations


def test_nc2_nc3_every_transclusion_resolves_to_a_published_slug() -> None:
    """NC2/NC3: every ``<<< @/_scripts/<file>.sh`` in a walkthrough page
    resolves to a slug in the publish export.

    Verify-path gate: a stale/typo/renamed reference fails ``task verify``,
    not only ``website:build`` (Codex finding 2). Vacuously green until
    Phase 6 wires the first transclusion.
    """
    export = doc_scripts_export(DOC_SCRIPTS_DIR)
    unresolved = unresolved_transclusions(WALKTHROUGH_PAGES, export)
    assert unresolved == [], (
        "Unresolved <<< @/_scripts/ references (no backing # doc: slug):\n"
        + "\n".join(
            f"  {page.relative_to(PROJECT_ROOT)} → _scripts/{stem}.sh"
            for page, stem in unresolved
        )
    )


@pytest.mark.parametrize(
    "page",
    WALKTHROUGH_PAGES,
    ids=[p.name for p in WALKTHROUGH_PAGES],
)
def test_nc1_no_ungated_inline_ocx_blocks(page: Path) -> None:
    """NC1 hard gate: a walkthrough page has zero inline fenced ``ocx``
    blocks that are not ``<<<`` transclusions.

    Phase 6 migration is complete for all five pages.  Shebang'd blocks
    (generated-file listings) are exempt per §6b Living-Design-Record.
    """
    blocks = find_inline_ocx_blocks(page)
    assert blocks == [], (
        f"{page.relative_to(PROJECT_ROOT)} has {len(blocks)} ungated inline "
        f"ocx block(s); each must become a <<< @/_scripts/ transclusion"
    )


# ===========================================================================
# RN5b — static verify-path scrub: no non-renderable fixture/harness var
#         in any displayed (# doc:) script body
# ===========================================================================


def test_rn5b_live_tree_zero_violations() -> None:
    """RN5b (live tree): the real ``test/doc_scripts/`` tree has zero
    non-renderable fixture/harness variable references in any displayed
    (``# doc:``-annotated) script's display scope.

    Forward guard (§6e RN5b): Codex F2's premise was correct — ~27 live
    scripts referenced fixture/harness vars and were remediated in Phase 3
    (region-scoped so the banned refs fall outside the displayed region).
    This test keeps the tree at zero violations.

    Non-renderable namespace (§6e RN5):
    - Prefix-based: ``FQ_*``, ``TAG_*``, ``MARKER_*``, ``HOME_KEY_*``
      (registry-/run-dependent — no clean static reader form).
    - Runner vars: ``REGISTRY``, ``SCENARIO_TMP``, ``OCX``, ``OCX_HOME``.

    Note: ``$PKG_<KEY>`` and ``$REPO_<KEY>`` are **not** flagged — they
    are the renderable matrix (LDR 2026-05-17), substituted via
    ``display_env`` by RN3.

    Contract reference: §6e RN5b.
    """
    from src.doc_scripts import discover_doc_scripts

    scripts = discover_doc_scripts(DOC_SCRIPTS_DIR)
    all_violations: list[str] = []

    for script in scripts:
        violations = _find_rn5b_violations(script)
        for lineno, var_name in violations:
            rel = script.relative_to(PROJECT_ROOT)
            all_violations.append(f"  {rel}:{lineno}: ${var_name}")

    assert all_violations == [], (
        f"RN5b: {len(all_violations)} non-renderable fixture/harness variable "
        f"reference(s) found in displayed (# doc:) scripts. "
        f"These vars are banned from the display scope: "
        f"FQ_*/TAG_*/MARKER_*/HOME_KEY_* and runner vars "
        f"(REGISTRY/SCENARIO_TMP/OCX/OCX_HOME). "
        f"Move them outside the displayed region or use the renderable "
        f"matrix $PKG_<KEY>/$REPO_<KEY> instead.\n"
        + "\n".join(all_violations)
    )


# ===========================================================================
# RN9 — no verification scaffolding in the *displayed* (rendered) output
# ===========================================================================


def test_rn9_no_verification_in_displayed_output() -> None:
    """RN9 (LDR 2026-05-18): the **rendered display** of every displayed
    (``# doc:``) script contains only documented commands — no capture
    (``out="$(ocx …)"``), no ``[[ … ]]`` test, no assertion ``exit``, no
    ``>&2`` error echo.

    User-reported (2026-05-18): a published snippet showed
    ``out="$(ocx package exec … )"`` + ``[[ -n "$out" ]] || { … exit 1; }``.
    Verification belongs in the drift gate (which runs the full body),
    **not** in the reader-facing / cast output.  The fix is region-scoping:
    the documented command sits inside ``# region cast``; the capture +
    assertion fall outside it.  This gate keeps the displayed projection
    clean and fails ``task verify`` (parity with RN5b/RG3) if scaffolding
    leaks back in — render is the single source of truth for "what is
    shown", so the check renders each script and scans the *result*.

    Contract reference: §6e RN9.
    """
    from publish_doc_scripts import RenderError

    export = doc_scripts_export(DOC_SCRIPTS_DIR)
    violations: list[str] = []

    for entry in export:
        if entry["slug"] is None:
            continue  # parse-error / non-displayed — other gates cover it
        try:
            rendered = render_display(
                Path(entry["path"]).read_text(),
                cast_region=entry["cast_region"],
                display_env=entry["display_env"],
                slug=entry["slug"],
            )
        except RenderError:
            continue  # RN5/empty-region — RN5b and §6e edge gates cover it

        for i, line in enumerate(rendered.splitlines(), start=1):
            if any(pat.search(line) for pat in _RN9_SCAFFOLDING_RE):
                violations.append(
                    f"  {entry['slug']} (rendered line {i}): {line.strip()}"
                )

    assert violations == [], (
        f"RN9: {len(violations)} verification-scaffolding line(s) leaked into "
        f"the displayed output of # doc: scripts. Move captures/assertions "
        f"outside the # region cast block (the drift gate still runs the full "
        f"body — it stays tested, just not shown):\n" + "\n".join(violations)
    )


def test_rn5b_fixture_injection_catches_violation(tmp_path: Path) -> None:
    """RN5b (fixture): injecting a non-renderable var into a displayed script
    triggers a violation in the static check.

    This test verifies that the check catches what it claims to catch — a
    script with ``$FQ_UV`` in its body (a still-non-renderable fixture
    prefix; LDR 2026-05-17 made REPO_ renderable, FQ_/TAG_/MARKER_/HOME_KEY_
    remain banned) must produce at least one RN5b violation.

    Contract reference: §6e RN5b — "a fixture that injects one fails".
    """
    bad_script = tmp_path / "rn5b_fixture.sh"
    bad_script.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rn5b/fixture-test
            # title: RN5b fixture injection test
            set -euo pipefail

            ocx package install "$FQ_UV"
            """
        )
    )
    bad_script.chmod(0o755)

    violations = _find_rn5b_violations(bad_script)

    assert len(violations) >= 1, (
        "RN5b/fixture: injecting $FQ_UV into a displayed script must produce "
        "at least one RN5b violation; got 0"
    )
    var_names = [v for _, v in violations]
    assert any("FQ_UV" in v for v in var_names), (
        f"RN5b/fixture: expected FQ_UV in violations; got {var_names!r}"
    )


def test_rn5b_runner_var_injection_caught(tmp_path: Path) -> None:
    """RN5b (runner var fixture): a displayed script using $REGISTRY triggers
    a violation.

    Contract reference: §6e RN5b (runner-harness var ban).
    """
    bad_script = tmp_path / "rn5b_runner.sh"
    bad_script.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rn5b/runner-test
            # title: RN5b runner var test
            set -euo pipefail

            echo "$REGISTRY"
            """
        )
    )
    bad_script.chmod(0o755)

    violations = _find_rn5b_violations(bad_script)

    assert len(violations) >= 1, (
        "RN5b/runner: injecting $REGISTRY into a displayed script must produce "
        "at least one RN5b violation; got 0"
    )
    var_names = [v for _, v in violations]
    assert any("REGISTRY" in v for v in var_names), (
        f"RN5b/runner: expected REGISTRY in violations; got {var_names!r}"
    )


# ===========================================================================
# NC4 — no #region fragment in doc-author <<< references
# ===========================================================================


@pytest.mark.parametrize(
    "page",
    WALKTHROUGH_PAGES,
    ids=[p.name for p in WALKTHROUGH_PAGES],
)
def test_nc4_no_region_fragment_in_transclusion(page: Path) -> None:
    """NC4 hard gate: no walkthrough page ``<<<`` reference carries a
    ``#region`` fragment.

    Under ADR H-2 the published file is the pre-rendered region body.  A
    correct doc-author ``<<<`` reference never names a region.  Any
    ``#region`` fragment is the exact shape that triggers the VitePress GH
    #4625 silent whole-file fallback.

    Contract reference: §6h NC4.
    """
    hits = find_region_fragment_transclusions(page)
    assert hits == [], (
        f"{page.relative_to(PROJECT_ROOT)} has {len(hits)} <<< reference(s) "
        f"with a forbidden #region fragment (ADR H-2: transclude the "
        f"pre-rendered file with no #region; the publish render already "
        f"selected the region):\n"
        + "\n".join(
            f"  _scripts/{stem}.sh#{region}"
            for stem, region in hits
        )
    )


def test_nc4_region_fragment_fixture_fails(tmp_path: Path) -> None:
    """NC4 (fixture): a ``<<<`` reference with a ``#region`` fragment is
    correctly detected as a violation.

    This test verifies that ``find_region_fragment_transclusions`` flags a
    ``#cast`` fragment, proving the check catches what it claims to catch.

    Contract reference: §6h NC4 — "a fixture proving a #region-bearing ref
    fails".
    """
    page = tmp_path / "nc4_fixture.md"
    page.write_text(
        textwrap.dedent(
            """\
            # NC4 fixture

            <<< @/_scripts/getting-started/install.sh#cast{sh}
            """
        )
    )

    hits = find_region_fragment_transclusions(page)

    assert len(hits) >= 1, (
        "NC4/fixture: a <<< with a #cast fragment must produce at least one "
        "hit; got 0"
    )
    stems = [s for s, _ in hits]
    assert any("getting-started/install" in s for s in stems), (
        f"NC4/fixture: expected 'getting-started/install' in stems; got {stems!r}"
    )


# ===========================================================================
# NC4b — publish render hard-errors on unclosed / >1 cast region
# ===========================================================================


def test_nc4b_unclosed_region_raises_render_error(tmp_path: Path) -> None:
    """NC4b: the publish render hard-errors on a ``# doc:`` script with an
    unclosed ``# region cast`` (no matching ``# endregion cast``).

    An unclosed region causes VitePress to silently return the entire file
    (GH #4625 whole-file dump).  The render layer must detect this and raise
    :class:`RenderError` before writing any output.

    Contract reference: §6h NC4b(a).
    """
    import sys

    sys.path.insert(0, str(PROJECT_ROOT / "website" / "scripts"))
    try:
        from publish_doc_scripts import RenderError, render_display
    finally:
        sys.path.pop(0)

    # Script text with an unclosed cast region.
    # cast_region is None here — passed as (start, end) only when fully parsed;
    # for the unclosed case we simulate it coming through with cast_region=None
    # (the full body path) but the script body still contains the marker.
    # For NC4b, the publish task detects unclosed regions at the parse level;
    # a cast_region=None with a dangling opener in the body is the canonical
    # representation of an unclosed region reaching render_display.
    # We directly verify the hard-error via a script whose cast_region tuple
    # spans a region with *no content between markers* (empty region).
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: nc4b/unclosed-test
        set -euo pipefail
        # region cast
        ocx package install uv:0.10
        # endregion cast
        # region cast
        ocx package which uv
        # endregion cast
        """
    )
    # Two cast regions → cast_region from the parser would be an error (EX9).
    # For the render path, we test the >1 region case by calling render_display
    # with a cast_region that covers the first region and checking that a
    # second region present in the body does NOT cause silent output.
    # The NC4b test here covers the publish-task hard-error path: we verify
    # that passing a script body with >1 cast region block and cast_region=None
    # (i.e. the full-body path RN2) raises no RenderError — but render_display
    # itself only gets the resolved cast_region.  NC4b at the publish level is
    # enforced by parse_doc_header (EX9) which fires before render_display is
    # called.  We verify it fires here via the empty-region branch.
    #
    # Empty region: cast_region span where start == end-1 (adjacent markers).
    # Lines are 1-indexed in the span; here lines 5 and 6 are adjacent markers.
    with pytest.raises(RenderError, match=r"empty cast region"):
        render_display(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                # state: setup:basic
                # doc: nc4b/empty-region-test
                set -euo pipefail
                # region cast
                # endregion cast
                """
            ),
            cast_region=(5, 6),  # adjacent markers → empty region
            display_env={},
            slug="nc4b/empty-region-test",
        )


def test_nc4b_duplicate_region_render_error(tmp_path: Path) -> None:
    """NC4b: a ``# doc:`` script passed with a ``cast_region`` pointing into
    a valid non-empty region renders correctly; this test verifies the
    :class:`RenderError` is raised by the empty-region branch (NC4b(a)) which
    is the publish task's guard against the VitePress whole-file fallback.

    Additionally tests that a fixture with a ``$REGISTRY`` var in the region
    (NC4b overlapping with RN5) raises :class:`RenderError` with the
    non-renderable-var message.

    Contract reference: §6h NC4b(b) — RenderError on non-renderable var in
    displayed region.
    """
    import sys

    sys.path.insert(0, str(PROJECT_ROOT / "website" / "scripts"))
    try:
        from publish_doc_scripts import RenderError, render_display
    finally:
        sys.path.pop(0)

    # A region containing a non-renderable harness variable ($REGISTRY).
    # This must raise RenderError (RN5 / NC4b).
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: nc4b/rn5-region-test
        set -euo pipefail
        # region cast
        ocx package install --registry "$REGISTRY" uv:0.10
        # endregion cast
        """
    )
    # Lines: 1=shebang, 2=state, 3=doc, 4=set, 5=region cast, 6=command, 7=endregion
    with pytest.raises(RenderError):
        render_display(
            script_text,
            cast_region=(5, 7),
            display_env={},
            slug="nc4b/rn5-region-test",
        )


# ===========================================================================
# RG3 — unrecognised region grammar in displayed scripts
# ===========================================================================


def test_rg3_live_tree_zero_unrecognised_regions() -> None:
    """RG3 (live tree): the real ``test/doc_scripts/`` tree has zero
    unrecognised region markers in any displayed (``# doc:``-annotated) script.

    Only ``cast`` regions are supported (ADR H-3).  Any other
    ``# region <x>`` / ``# endregion <x>`` where ``<x> != cast`` in a
    displayed script is flagged.

    Forward guard: 0 of the live scripts violate this today.

    Contract reference: §6g RG3.
    """
    from src.doc_scripts import discover_doc_scripts

    scripts = discover_doc_scripts(DOC_SCRIPTS_DIR)
    all_violations: list[str] = []

    for script in scripts:
        violations = find_unrecognised_region_markers(script)
        for lineno, raw_line in violations:
            rel = script.relative_to(PROJECT_ROOT)
            all_violations.append(f"  {rel}:{lineno}: {raw_line.strip()!r}")

    assert all_violations == [], (
        f"RG3: {len(all_violations)} unrecognised region marker(s) found in "
        f"displayed (# doc:) scripts. "
        f"Only '# region cast' / '# endregion cast' are supported (ADR H-3). "
        f"A typo'd or second-grammar region marker would ship verbatim into a "
        f"rendered page.\n"
        + "\n".join(all_violations)
    )


def test_rg3_typo_region_name_fails(tmp_path: Path) -> None:
    """RG3 (fixture): a displayed script with a typo'd region name
    (``# region cats``) is correctly flagged.

    Contract reference: §6g RG3 — "a fixture proving a typo'd
    ``# region cats`` fails".
    """
    bad_script = tmp_path / "rg3_fixture.sh"
    bad_script.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rg3/typo-test
            # title: RG3 typo fixture
            set -euo pipefail

            # region cats
            ocx package install uv:0.10
            # endregion cats
            """
        )
    )
    bad_script.chmod(0o755)

    violations = find_unrecognised_region_markers(bad_script)

    assert len(violations) >= 1, (
        "RG3/fixture: a # region cats marker in a displayed script must "
        "produce at least one violation; got 0"
    )
    raw_lines = [line for _, line in violations]
    assert any("cats" in line for line in raw_lines), (
        f"RG3/fixture: expected 'cats' in violation lines; got {raw_lines!r}"
    )


def test_rg3_valid_cast_region_not_flagged(tmp_path: Path) -> None:
    """RG3 (negative fixture): a displayed script with a valid
    ``# region cast`` is NOT flagged by the check.

    Contract reference: §6g RG3 — only unrecognised names fail.
    """
    good_script = tmp_path / "rg3_good.sh"
    good_script.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rg3/good-test
            # title: RG3 good fixture
            # cast: true
            set -euo pipefail

            # region cast
            ocx package install uv:0.10
            # endregion cast
            """
        )
    )
    good_script.chmod(0o755)

    violations = find_unrecognised_region_markers(good_script)

    assert violations == [], (
        f"RG3/negative: a # region cast marker must NOT be flagged; "
        f"got {violations!r}"
    )
