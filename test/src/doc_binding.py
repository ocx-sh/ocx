"""Static verify-path checks for walkthrough page doc bindings (NC1–NC4, RG3).

Checks that:

- NC1: Walkthrough pages contain zero inline fenced ``bash``/``sh`` blocks
  containing an ``ocx`` *invocation* that are NOT ``<<<`` transclusion
  references — i.e., every demonstrated ``ocx`` invocation is backed by a
  tested script.  Detection is structural (not a bare ``ocx `` substring),
  and fences with a VitePress code-group label (`` ```sh [label] ``) are
  treated as shell fences.  Two exemptions apply — see
  ``find_inline_ocx_blocks`` docstring and §6b of the design spec.
- NC2: Every ``<<< @/_scripts/<file>.sh`` transclusion reference in a
  walkthrough page resolves to a slug present in the publish export
  (``doc_scripts_export`` / ``task test:doc-scripts:list`` JSON).  Slug
  resolution supports both nested paths (``a/b``, ADR Decision D / LDR
  2026-05-17) and the legacy flat form (``a__b``, kept for
  backwards-compatibility detection).  A stem ``a/b`` resolves by direct slug
  lookup; a stem ``a__b`` resolves by converting to ``a/b`` first.
- NC3: All ``<<<`` references resolve (binding gated with execution).
- NC4: Any ``<<<`` that carries a ``#region`` fragment (e.g.
  ``@/_scripts/foo.sh#cast{sh}``) fails the gate — under ADR H-2 the
  published file is the pre-rendered region body, so a correct doc-author
  ``<<<`` never names a region.  A ``#region`` in a ``<<<`` is the exact
  shape that triggers the VitePress GH #4625 silent whole-file fallback.
- RG3: Any displayed (``# doc:`` present) script under ``test/doc_scripts/``
  that contains a ``# region <x>`` / ``# endregion <x>`` where ``<x>`` is
  not ``cast`` fails the gate — only ``cast`` regions are supported (ADR
  H-3), preventing a typo'd marker from silently shipping a raw comment line
  into a rendered page.

These checks run in the same ``test:parallel`` collection as the drift gate
(§6b of design_spec_doc_command_scripts.md), so a green ``task verify``
proves *both* that documented commands execute *and* that every page
actually binds to a real published tested script.

No Docker, no website build required — pure static file analysis.
"""
from __future__ import annotations

import re
from pathlib import Path

from src.doc_scripts import DocScriptExportEntry
from src.helpers import PROJECT_ROOT

# ---------------------------------------------------------------------------
# Walkthrough pages in scope (§7.2)
# ---------------------------------------------------------------------------

WALKTHROUGH_PAGES: tuple[Path, ...] = (
    PROJECT_ROOT / "website" / "src" / "docs" / "getting-started.md",
    PROJECT_ROOT / "website" / "src" / "docs" / "user-guide.md",
    PROJECT_ROOT / "website" / "src" / "docs" / "faq.md",
    PROJECT_ROOT / "website" / "src" / "docs" / "in-depth" / "environments.md",
    PROJECT_ROOT / "website" / "src" / "docs" / "in-depth" / "entry-points.md",
)
"""The five walkthrough pages subject to NC1–NC3 checks.

Relative to ``PROJECT_ROOT``; paths are resolved at module import time via
``src.helpers.PROJECT_ROOT``.  These are the prose pages whose inline code
blocks must all be ``<<<`` transclusions backed by tested doc scripts.
"""

# ---------------------------------------------------------------------------
# Regexes
# ---------------------------------------------------------------------------

# Matches a VitePress code-include line: <<< @/_scripts/<file>.sh{...}
# The file stem is captured (without the .sh suffix, optional #region fragment,
# and optional language spec).  Supports nested slug paths (``a/b``) and legacy
# flat forms (``a__b``).
_TRANSCLUSION_RE: re.Pattern[str] = re.compile(
    r"<<<\s+@/_scripts/([^.{}\s]+)\.sh"
)

# Matches a VitePress code-include line that also names a region fragment:
# <<< @/_scripts/<file>.sh#<region>{...}
# Used by NC4 to detect forbidden #region references.
# Group 1: the file stem; group 2: the region name (without the leading '#').
_TRANSCLUSION_REGION_RE: re.Pattern[str] = re.compile(
    r"<<<\s+@/_scripts/[^.{}\s]+\.sh#([^\s{]+)"
)

# Matches a fenced code block opening with bash or sh language specifier,
# optionally followed by a VitePress code-group label (e.g. ```sh [label]).
# Non-shell languages (python, bat, etc.) do NOT match.
_FENCE_OPEN_RE: re.Pattern[str] = re.compile(r"^```(?:bash|sh)(?:\s+\S.*)?\s*$")

# ---------------------------------------------------------------------------
# OCX invocation detection (structural, not substring)
# ---------------------------------------------------------------------------

# Three token forms that represent an ocx binary:
#   1. "path/to/ocx" — double-quoted absolute or variable path ending in /ocx
#   2. ${VAR:-ocx} or "${VAR:-ocx}" — shell variable with ocx fallback
#   3. ocx — the bare word (word-boundary guarded so ".ocx/" directory is skipped)
_OCX_TOKEN: str = (
    r"(?:"
    r'"[^"\n]*?/ocx"'           # "…/ocx" double-quoted path-qualified binary
    r'|"?\$\{[^}]*:-ocx\}"?'   # ${VAR:-ocx} or "${VAR:-ocx}"
    r"|\bocx\b"                  # plain ocx word (not inside a path component)
    r")"
)

# An ocx *invocation* means the token appears in command position:
#   - Start of a (possibly indented) line or pipeline segment: ^, &&, ||, ;, |, (
#   - After exec (e.g. ``exec "${OCX_BINARY_PIN:-ocx}" launcher exec …``)
#   - After eval "$( … (the installer eval idiom)
_OCX_CMD_RE: re.Pattern[str] = re.compile(
    r"(?:(?:^|&&|\|\||;|\||\()\s*)"    # pipeline-start contexts (^ = start of line)
    + _OCX_TOKEN
    + r"|(?:exec\s+)"                   # exec COMMAND
    + _OCX_TOKEN
    + r"|(?:eval\s+\"?\$\(\s*)"        # eval "$( TOKEN …
    + _OCX_TOKEN,
    re.MULTILINE,
)

# ---------------------------------------------------------------------------
# NC1 — inline ocx blocks
# ---------------------------------------------------------------------------


def find_inline_ocx_blocks(page: Path) -> list[str]:
    """Return fenced shell blocks containing an ``ocx`` invocation that are
    NOT ``<<<`` transclusions (NC1).

    **What counts as a shell fence:**  the opening fence is `` ```bash `` or
    `` ```sh ``, optionally followed by a VitePress code-group label
    (e.g. `` ```sh [Single binary] ``).  Non-shell languages are ignored.

    **What counts as an ocx invocation (structural detection — §6b):**
    a token in command position that is:

    - the bare word ``ocx`` (word-boundary guarded; ``.ocx/`` directory
      references do NOT match),
    - a double-quoted path-qualified binary ending in ``/ocx``
      (e.g. ``"$HOME/.ocx/ocx"``), or
    - a shell variable with ocx fallback (e.g. ``${OCX_BINARY_PIN:-ocx}``).

    "Command position" means the token appears at the start of a line or
    pipeline segment (after ``^``, ``&&``, ``||``, ``;``, ``|``, ``(``) or
    after ``exec`` or ``eval "$(…"``.  The test-condition ``[ -x "…/ocx" ]``
    alone (without a following ``&&`` command) is NOT a match.

    **Exemptions (§6b Living-Design-Record, 2026-05-17, extended after Codex
    cross-model review):**

    (a) *Shebang exemption* — a fenced block whose first non-blank content
    line starts with ``#!`` is a *generated-file listing*, not a runnable
    invocation.  Covers the install-time launcher shown in
    ``entry-points.md`` / ``environments.md``
    (``#!/bin/sh … exec "${OCX_BINARY_PIN:-ocx}" launcher exec …``).

    (b) *Installer block-marker exemption* — a fenced block that contains
    both ``# BEGIN ocx`` and ``# END ocx`` is the OCX-installer-written
    shell-profile fragment (shown in ``user-guide.md``).  It is
    documentation of an on-disk artifact the installer manages, not a
    command a reader types.

    Both exemptions reflect NC1's intent: "every demonstrated ``ocx``
    *invocation* is backed by a tested script"; generated/installer-managed
    artifact listings are not invocations.

    Args:
        page: Absolute path to a Markdown file.

    Returns:
        List of raw block strings (fence-to-fence inclusive) for every
        non-exempt block containing an ocx invocation.  An empty list means
        NC1 is satisfied for this page.

    Raises:
        OSError: If the file cannot be read.
    """
    text = page.read_text()
    lines = text.splitlines(keepends=True)
    result: list[str] = []

    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.rstrip("\n\r")

        # Check if this line is a VitePress transclusion — skip it
        if _TRANSCLUSION_RE.search(stripped):
            i += 1
            continue

        # Check if this is a fenced code block opening (bash/sh, with or without label)
        if _FENCE_OPEN_RE.match(stripped):
            # Collect the block lines
            block_lines: list[str] = [line]
            i += 1
            while i < len(lines):
                block_line = lines[i]
                block_lines.append(block_line)
                i += 1
                if block_line.rstrip("\n\r") == "```":
                    break

            block_text = "".join(block_lines)

            # Structural ocx invocation detection (replaces bare "ocx " substring)
            if not _OCX_CMD_RE.search(block_text):
                continue

            # §6b exemption (a): shebang — a block whose first non-blank content
            # line (after the opening fence) starts with "#!" is a generated-file
            # listing, not a runnable invocation.
            content_lines = block_lines[1:]  # skip the opening fence line
            first_content = next(
                (ln.lstrip() for ln in content_lines if ln.strip() and ln.strip() != "```"),
                "",
            )
            if first_content.startswith("#!"):
                continue

            # §6b exemption (b): installer block markers — the OCX-installer-written
            # shell-profile fragment contains "# BEGIN ocx" and "# END ocx".
            if any("# BEGIN ocx" in ln for ln in block_lines) and any(
                "# END ocx" in ln for ln in block_lines
            ):
                continue

            result.append(block_text)
        else:
            i += 1

    return result


# ---------------------------------------------------------------------------
# NC2 / NC3 — transclusion references
# ---------------------------------------------------------------------------


def find_script_transclusions(page: Path) -> list[str]:
    """Return the file stem for every ``<<< @/_scripts/<file>.sh`` reference.

    Scans the Markdown source for VitePress code-include syntax::

        <<< @/_scripts/<file>.sh{sh}

    and extracts the ``<file>`` stem (without the ``.sh`` suffix) for each
    occurrence.  The stem is matched against the publish export as a
    **nested slug** — ``a/b`` is looked up directly (ADR Decision D / LDR
    2026-05-17, the canonical convention; ``/`` is the slug's directory
    separator).  The legacy flat form ``a__b`` is still accepted by
    ``_stem_to_slug`` for backwards-compatibility detection only (converted
    once to ``a/b``); it is not the current authoring shape.

    Args:
        page: Absolute path to a Markdown file.

    Returns:
        List of stem strings (one per ``<<<`` reference found), in document
        order.  May contain duplicates if the same script is referenced more
        than once on the same page.

    Raises:
        OSError: If the file cannot be read.
    """
    text = page.read_text()
    return _TRANSCLUSION_RE.findall(text)


def _stem_to_slug(stem: str) -> str:
    """Normalise a ``<<<`` file stem to its canonical slug form.

    Handles both path forms used in doc-author ``<<<`` references:

    - **Nested** (ADR Decision D / LDR 2026-05-17, current convention):
      ``getting-started/install`` → ``getting-started/install`` (no change;
      ``/`` is already the slug's directory separator).
    - **Legacy flat** (``/`` → ``__`` flattening, pre-LDR):
      ``getting-started__install`` → ``getting-started/install``.

    The conversion is applied exactly once (the first ``__`` occurrence) to
    match the slug grammar ``^[a-z0-9]+(?:[-/][a-z0-9]+)*$``, which has a
    single ``/`` level.

    Args:
        stem: The raw stem extracted from a ``<<<`` reference by
            ``_TRANSCLUSION_RE`` (no ``.sh`` suffix).

    Returns:
        The slug string (``/``-separated) that should exist in the publish
        export for the reference to be considered resolved (NC2/NC3).
    """
    # Nested path: stem already uses / as separator → slug == stem
    if "/" in stem:
        return stem
    # Legacy flat form: first __ → /
    return stem.replace("__", "/", 1)


def unresolved_transclusions(
    pages: tuple[Path, ...],
    export: list[DocScriptExportEntry],
) -> list[tuple[Path, str]]:
    """Find ``<<<`` transclusion references that have no backing slug.

    For each page, calls ``find_script_transclusions`` and checks that every
    returned stem resolves to a slug in *export*.  Resolution supports both
    the current nested path convention (ADR Decision D / LDR 2026-05-17) and
    the legacy flat form:

    - A stem ``a/b-c`` (nested) is looked up directly as slug ``a/b-c``.
    - A stem ``a__b-c`` (legacy flat) is first converted to ``a/b-c`` via
      ``_stem_to_slug``, then looked up in the slug set.

    Both forms resolve correctly because the slug set is built from the
    canonical ``# doc:`` slugs in the export, which always use ``/``.

    The slug set is built from ``entry["slug"]`` for each entry in *export*
    where ``entry["slug"]`` is not ``None``.

    Args:
        pages: Tuple of walkthrough page paths (typically
            ``WALKTHROUGH_PAGES``).
        export: Output of ``doc_scripts_export`` — list of dicts with at
            least ``{"path": str, "slug": str | None, ...}``.

    Returns:
        List of ``(page_path, stem)`` pairs for every unresolved reference,
        in page order then document order within each page.  An empty list
        means NC2/NC3 are satisfied.
    """
    # Build slug set directly from canonical / -separated slugs.
    slugs: set[str] = {
        entry["slug"]
        for entry in export
        if entry.get("slug") is not None
    }

    result: list[tuple[Path, str]] = []
    for page in pages:
        for stem in find_script_transclusions(page):
            resolved_slug = _stem_to_slug(stem)
            if resolved_slug not in slugs:
                result.append((page, stem))

    return result


# ---------------------------------------------------------------------------
# NC4 — region-fragment transclusion guard
# ---------------------------------------------------------------------------

# Regex to detect a ``# region <x>`` / ``# endregion <x>`` line in a script,
# capturing the region name ``<x>``.  Matches the recognised VitePress region
# marker pattern ``#\s*#?(end)?region`` followed by an optional name token.
# Used by RG3 to flag any region name other than ``cast``.
_REGION_MARKER_RE: re.Pattern[str] = re.compile(
    r"^#\s*#?(?:end)?region(?:\s+(\S+))?\s*$"
)


def find_region_fragment_transclusions(page: Path) -> list[tuple[str, str]]:
    """Return ``(stem, region_name)`` for every ``<<<`` reference carrying a
    ``#region`` fragment (NC4).

    Scans the Markdown source for VitePress code-include syntax that includes
    a region selector::

        <<< @/_scripts/<file>.sh#<region>{sh}

    Under ADR H-2 the published file is already the pre-rendered region body,
    so a correct doc-author ``<<<`` reference never names a region.  Any
    ``#region`` fragment is by definition wrong and is the exact shape that
    triggers the VitePress GH #4625 silent whole-file fallback.

    Args:
        page: Absolute path to a Markdown file.

    Returns:
        List of ``(stem, region_name)`` pairs (one per ``<<<`` reference that
        carries a ``#region``), in document order.  An empty list means NC4
        is satisfied for this page.

    Raises:
        OSError: If the file cannot be read.
    """
    text = page.read_text()
    return [
        # Extract stem using the standard _TRANSCLUSION_RE, region from the
        # dedicated region regex.  Both regexes must match the same line.
        (stem_m.group(1), region_m.group(1))
        for line in text.splitlines()
        for stem_m in [_TRANSCLUSION_RE.search(line)]
        if stem_m
        for region_m in [_TRANSCLUSION_REGION_RE.search(line)]
        if region_m
    ]


# ---------------------------------------------------------------------------
# RG3 — unrecognised region grammar in displayed scripts
# ---------------------------------------------------------------------------


def find_unrecognised_region_markers(script_path: Path) -> list[tuple[int, str]]:
    """Return ``(1-based line number, raw line)`` for every unrecognised
    ``# region <x>`` / ``# endregion <x>`` marker in a displayed script.

    A displayed script is one whose parsed header contains a non-``None``
    ``# doc:`` slug.  Only the ``cast`` region name is supported (ADR H-3);
    any other name — e.g. ``# region cats`` (typo) — is flagged by RG3.

    Lines inside the script that match the VitePress region-marker pattern
    (``^#\\s*#?(end)?region``) but whose region name is not ``cast`` are
    returned.  The ``# region cast`` / ``# endregion cast`` lines themselves
    are **not** flagged.

    Args:
        script_path: Absolute path to a ``.sh`` file.

    Returns:
        List of ``(lineno, raw_line)`` pairs for each unrecognised marker,
        in file order.  An empty list means RG3 is satisfied for this script.
    """
    try:
        from src.doc_scripts import DocScriptParseError, parse_doc_header

        meta = parse_doc_header(script_path)
    except (DocScriptParseError, OSError, ImportError):
        return []

    # Only check displayed scripts (# doc: present).
    if meta.doc is None:
        return []

    violations: list[tuple[int, str]] = []
    try:
        lines = script_path.read_text().splitlines()
    except OSError:
        return []

    for lineno, line in enumerate(lines, start=1):
        m = _REGION_MARKER_RE.match(line.strip())
        if m is None:
            continue
        region_name = m.group(1)  # may be None if no name follows
        if region_name != "cast":
            violations.append((lineno, line))

    return violations


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

__all__ = [
    "WALKTHROUGH_PAGES",
    "find_inline_ocx_blocks",
    "find_region_fragment_transclusions",
    "find_script_transclusions",
    "find_unrecognised_region_markers",
    "unresolved_transclusions",
]
