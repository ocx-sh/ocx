"""Header parser, discovery, and drift-gate executor for doc scripts.

A *doc script* is a ``.sh`` file under ``test/doc_scripts/`` carrying a
unified metadata header (§1 of design_spec_doc_command_scripts.md).  Each
file is executed as a drift-gate acceptance case; scripts that also declare
``# cast: true`` and ``# doc: <slug>`` are candidates for publication to
``website/src/_scripts/`` and for cast recording during ``website:build``.

Design contract reference: design_spec_doc_command_scripts.md
§1 (schema / grammar / cast region), §2 (executor EX1–EX9, GO1–GO3,
discovery), §6 (drift gate DG1–DG3), §6b (NC1–NC3).

Import-time guarantee (SP0): importing this module performs **zero**
registry I/O.
"""
from __future__ import annotations

import difflib
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, TypedDict

if TYPE_CHECKING:
    from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Export entry type (PT6 / NC2)
# ---------------------------------------------------------------------------


class DocScriptExportEntry(TypedDict):
    """Typed schema for a single entry in ``doc_scripts_export`` output.

    Schema (§5 PT6 / DE1):
        path:         Absolute path to the script file.
        slug:         Value of ``# doc:`` or ``None`` (no publication slug).
        cast:         ``True`` if the script declares ``# cast: true``.
        expect:       Value of ``# expect:`` or ``None`` (no golden file check).
        display_env:  Static zero-I/O env projection ``{PKG_<KEY>: <short_ref>}``
                      from ``StateProvider.declared_display_env()`` for the
                      script's resolved ``# state:``.  Always present; ``{}``
                      for scripts whose state has no declared packages (DE1).
        state:        The resolved ``# state:`` value (family-qualified, e.g.
                      ``"setup:basic"``).  Surfaced so consumers can identify
                      the provider without reparsing the header.
        cast_region:  ``[start, end]`` (1-based inclusive line numbers) of the
                      ``# region cast`` block, or ``null`` when absent (DE1).
                      JSON-serialised as a two-element array; Python type is
                      ``tuple[int, int] | None``.
        title:        Value of ``# title:`` or the file stem when absent.
        description:  Value of ``# description:`` or ``None``.
    """

    path: str
    slug: str | None
    cast: bool
    expect: str | None
    # DE1 additions — always present, never null for display_env
    display_env: dict[str, str]
    state: str
    cast_region: tuple[int, int] | None
    title: str | None
    description: str | None


# ---------------------------------------------------------------------------
# Slug grammar (§1.1)
# ---------------------------------------------------------------------------

SLUG_RE: re.Pattern[str] = re.compile(r"^[a-z0-9]+(?:[-/][a-z0-9]+)*$")
"""Compiled regex for the ``# doc:`` slug grammar.

Valid: ``getting-started/install-select``, ``user-guide/env-compose``.
Invalid: uppercase letters, leading/trailing separators, double separators,
underscores, empty string.

Validated as a hard parse error on violation (§1.1).
"""

# ---------------------------------------------------------------------------
# Recognised header keys (§1.1)
# ---------------------------------------------------------------------------

_RECOGNISED_KEYS: frozenset[str] = frozenset(
    {"state", "doc", "cast", "title", "description", "expect"}
)

# ---------------------------------------------------------------------------
# Error type
# ---------------------------------------------------------------------------


class DocScriptParseError(Exception):
    """Raised when a doc-script header violates the grammar.

    Covers (design_spec §1.1 / §2):

    - **EX5**: Unknown (unrecognised) header key — prevents silent typos such
      as ``# scenrio:`` that would silently disable a feature.
    - **Slug grammar**: ``# doc:`` value that does not match ``SLUG_RE``.
    - **EX9**: ``# cast: true`` with zero *or* more-than-one ``# region cast``
      / ``# endregion cast`` block — the cast layer requires exactly one region.

    Note: unqualified / unknown ``# state:`` values are a **resolver** error
    (``ValueError`` from ``resolve_state``), not a parse error; the header
    parser accepts any string for ``state``.  A ``# expect:`` pointing at a
    missing file is a **runtime** (executor) error, not a parse error.
    """


# ---------------------------------------------------------------------------
# DocScriptMeta
# ---------------------------------------------------------------------------


@dataclass(slots=True, frozen=True)
class DocScriptMeta:
    """Parsed metadata extracted from a doc-script header.

    All fields are derived from the contiguous comment block at the top of
    the script (shebang ignored; stops at first non-blank non-comment line).
    Keys are case-insensitive and lowercased on parse (§1.1).

    Attributes:
        state: Family-qualified state key (``setup:<name>`` or
            ``scenario:<Name>``).  Defaults to ``"setup:basic"`` when the
            ``# state:`` header is absent (EX6).  Unqualified or unknown
            values are rejected later by ``resolve_state`` (EX4), not by the
            parser.
        doc: Publication slug (must match ``SLUG_RE``).  ``None`` when absent
            — the script is tested-only, never published.
        cast: Whether the script opts in to cast recording
            (``# cast: true``).  Defaults to ``False``.
        title: Human-readable label shown in drift-failure output (DG1) and
            as the cast title.  Defaults to the file stem when absent.
        description: Optional human note surfaced in failure output (EX3/DG1).
        expect: Relative path (string) to the golden-output file.  ``None``
            when absent (assertion-only mode, GO1–GO3).
        cast_region: Line span ``(start, end)`` of the single
            ``# region cast`` / ``# endregion cast`` block (1-based,
            inclusive), or ``None`` when the file has no region markers
            (or has >1 and is not ``cast: true``).  RG0 (LDR 2026-05-17):
            populated whenever exactly one region block is present,
            **independent of ``# cast:``** — it is the display selector
            (ADR H-3).  For ``cast: true``, ≠1 region is a hard parse
            error (EX9); for display-only scripts, no region simply means
            full-body-minus-header rendering (RN2).
        path: Absolute path to the script file.
    """

    state: str = "setup:basic"
    doc: str | None = None
    cast: bool = False
    title: str = ""
    description: str | None = None
    expect: str | None = None
    cast_region: tuple[int, int] | None = None
    path: Path = field(default_factory=Path)


# ---------------------------------------------------------------------------
# Header parser
# ---------------------------------------------------------------------------


def parse_doc_header(path: Path) -> DocScriptMeta:
    """Parse the metadata header of a doc script.

    The header is a contiguous comment block at the top of the file.
    Shebang lines (``#!…``) are ignored.  Parsing stops at the first
    non-blank, non-comment line.  Key lookup is case-insensitive (lowercased
    internally).

    Recognised keys: ``state``, ``doc``, ``cast``, ``title``, ``description``,
    ``expect``.  Any other key raises ``DocScriptParseError`` (EX5) so typos
    like ``# scenrio:`` fail loudly.

    The ``# doc:`` value is validated against ``SLUG_RE``; violation raises
    ``DocScriptParseError``.

    When ``cast: true``, the file body is scanned for ``# region cast`` /
    ``# endregion cast`` markers.  Exactly one such block is required; zero
    or >1 blocks raises ``DocScriptParseError`` (EX9).

    Args:
        path: Absolute or relative path to the ``.sh`` file.

    Returns:
        A fully-populated, immutable ``DocScriptMeta`` instance.

    Raises:
        DocScriptParseError: On unknown header key (EX5), bad slug grammar,
            or ``cast: true`` with ≠1 cast region (EX9).
        OSError: If the file cannot be read.
    """
    text = path.read_text()
    lines = text.splitlines()

    # --- parse header ---
    raw_meta: dict[str, str] = {}
    for raw_line in lines:
        stripped = raw_line.strip()
        if not stripped:
            # blank line: stop header
            break
        if stripped.startswith("#!"):
            # shebang: skip silently
            continue
        if not stripped.startswith("#"):
            # first non-blank non-comment line: stop
            break
        # comment line: try to extract key: value
        rest = stripped[1:].strip()
        if ":" not in rest:
            # plain comment with no colon — not a metadata line, skip
            continue
        key, _, value = rest.partition(":")
        key_stripped = key.strip().lower()
        value_stripped = value.strip()
        if key_stripped not in _RECOGNISED_KEYS:
            raise DocScriptParseError(
                f"unknown metadata key {key_stripped!r} in {path}"
            )
        raw_meta[key_stripped] = value_stripped

    # --- extract typed fields ---
    state = raw_meta.get("state", "setup:basic")

    doc: str | None = raw_meta.get("doc", None)
    if doc is not None:
        if not SLUG_RE.fullmatch(doc):
            raise DocScriptParseError(
                f"invalid doc slug {doc!r} in {path}; must match {SLUG_RE.pattern}"
            )

    cast_raw = raw_meta.get("cast", "false").lower()
    cast = cast_raw == "true"

    title = raw_meta.get("title", path.stem)

    description: str | None = raw_meta.get("description", None)

    expect: str | None = raw_meta.get("expect", None)

    # --- cast/display region scan (RG0 + EX9) ---
    # RG0 (LDR 2026-05-17): the `# region cast` block is the *display*
    # selector, independent of `# cast:` (ADR H-3).  Scan for it on every
    # script; populate cast_region when exactly one block is present so a
    # display-only (`# doc:`, `# cast: false`) script can scope its clean
    # displayed commands.  The EX9 *arity* hard-error (≠1 region) still fires
    # only for `# cast: true` (cast PTY replay requires exactly one).  A
    # non-cast script with >1 region leaves cast_region=None; the publish
    # task / NC4b reject it (§6e edge-cases).
    cast_region: tuple[int, int] | None = None
    region_starts: list[int] = []
    region_ends: list[int] = []
    for i, raw_line in enumerate(lines, start=1):
        stripped_line = raw_line.strip()
        if stripped_line == "# region cast":
            region_starts.append(i)
        elif stripped_line == "# endregion cast":
            region_ends.append(i)

    if cast and (len(region_starts) != 1 or len(region_ends) != 1):
        raise DocScriptParseError(
            "cast script must have exactly one cast region"
        )
    # RG0 well-formedness (Codex F3): if region markers are present at all,
    # they must form exactly one *ordered* block (start strictly before
    # end).  A misordered/overlapping pair (e.g. `# endregion cast` before
    # `# region cast`) is a hard parse error rejected at verify time — not
    # deferred to an empty-region RenderError at publish or an empty cast.
    if region_starts or region_ends:
        if len(region_starts) == 1 and len(region_ends) == 1:
            start, end = region_starts[0], region_ends[0]
            if start >= end:
                raise DocScriptParseError(
                    f"misordered cast region: # region cast (line {start}) "
                    f"must precede # endregion cast (line {end})"
                )
            cast_region = (start, end)
        elif not cast:
            # >1 or unpaired markers on a non-cast script: not a valid
            # display region.  cast_region stays None (RN2 full-body); the
            # publish task / NC4b reject the malformed-region case.
            raise DocScriptParseError(
                "display region malformed: expected exactly one ordered "
                "# region cast / # endregion cast block "
                f"(found {len(region_starts)} start / {len(region_ends)} end)"
            )

    return DocScriptMeta(
        state=state,
        doc=doc,
        cast=cast,
        title=title,
        description=description,
        expect=expect,
        cast_region=cast_region,
        path=path,
    )


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------


def discover_doc_scripts(root: Path) -> list[Path]:
    """Return all ``.sh`` files under *root* recursively, sorted.

    Missing or empty root returns ``[]`` (not an error) — the drift-gate
    test module must collect zero cases cleanly when no doc scripts exist
    yet.

    Args:
        root: Directory to search.  May be missing or empty.

    Returns:
        Sorted list of ``Path`` objects for every ``*.sh`` file found.
    """
    if not root.exists():
        return []
    return sorted(root.glob("**/*.sh"))


# ---------------------------------------------------------------------------
# Discovery export seam (PT6 / NC2)
# ---------------------------------------------------------------------------


def doc_scripts_export(root: Path) -> list[DocScriptExportEntry]:
    """Return the discovery export consumed by the publish task (PT6) and NC2.

    Produces one entry per ``.sh`` file under *root* (same set as
    ``discover_doc_scripts``).  Each entry is a ``DocScriptExportEntry``
    TypedDict, suitable for JSON serialisation.

    Schema::

        [
            {
                "path": str,          # Absolute path to the script file
                "slug": str | None,   # Value of ``# doc:`` or null
                "cast": bool,         # True if ``# cast: true``
                "expect": str | None, # Value of ``# expect:`` or null
            },
            ...
        ]

    Parse errors in individual scripts are surfaced as entries with
    ``"slug": null`` and ``"cast": false`` rather than aborting the whole
    export — the publish task handles per-entry validation separately.
    A warning is printed to stderr so errors are observable without
    aborting the whole export.

    Args:
        root: Directory root passed to ``discover_doc_scripts``.

    Returns:
        List of ``DocScriptExportEntry`` dicts, one per discovered script,
        in sorted path order.
    """
    result: list[DocScriptExportEntry] = []
    for script_path in discover_doc_scripts(root):
        try:
            meta = parse_doc_header(script_path)
            from src.state_providers import resolve_state

            # A bad/unqualified ``# state:`` (EX4) raises ValueError from
            # resolve_state.  Do NOT abort the whole seam (D2): emit the
            # entry with the real slug/cast (NC2 binding still resolves)
            # but display_env={} and a stderr warning.  The per-script
            # drift gate (test_doc_scripts) independently fails that
            # script via EX4 — the seam is not the place to hard-fail.
            try:
                display_env = resolve_state(meta.state).declared_display_env()
            except ValueError as state_exc:
                print(
                    f"WARNING: doc-script {script_path} has invalid "
                    f"# state: {meta.state!r} ({state_exc}); display_env={{}}",
                    file=sys.stderr,
                )
                display_env = {}

            result.append(
                DocScriptExportEntry(
                    path=str(script_path),
                    slug=meta.doc,
                    cast=meta.cast,
                    expect=meta.expect,
                    display_env=display_env,
                    state=meta.state,
                    cast_region=meta.cast_region,
                    title=meta.title or None,
                    description=meta.description,
                )
            )
        except (DocScriptParseError, OSError) as exc:
            print(
                f"WARNING: doc-script parse error in {script_path}: {exc}",
                file=sys.stderr,
            )
            result.append(
                DocScriptExportEntry(
                    path=str(script_path),
                    slug=None,
                    cast=False,
                    expect=None,
                    # DE1 fields — safe fallbacks for parse-error entries
                    display_env={},
                    state="setup:basic",
                    cast_region=None,
                    title=None,
                    description=None,
                )
            )
    return result


# ---------------------------------------------------------------------------
# ANSI stripping utility
# ---------------------------------------------------------------------------

_ANSI_RE: re.Pattern[str] = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")


def strip_ansi(text: str) -> str:
    """Remove ANSI escape sequences from *text*.

    Used to produce clean, CI-readable failure output and for golden-file
    diffing (GO1–GO3, EX3, DG1–DG2).  The regex covers the common CSI
    (Control Sequence Introducer) sequences including colours, cursor
    movement, and erase codes, as well as standalone ESC codes.

    Args:
        text: Raw terminal output that may contain ANSI escapes.

    Returns:
        *text* with all ANSI escape sequences removed.
    """
    return _ANSI_RE.sub("", text)


# ---------------------------------------------------------------------------
# Renderable-variable substitution (RN8 — canonical impl)
# ---------------------------------------------------------------------------


def substitute_renderable(text: str, display_env: dict[str, str]) -> str:
    """Substitute the renderable matrix (`$PKG_<KEY>`/`$REPO_<KEY>`) in *text*.

    The **single** substitution implementation (RN8) — but a **publish-time**
    transform, not part of execution.  EX10: the drift-gate executor runs the
    *raw* script body under ``script_env`` (SP7-prefixed ``$PKG_*``); it does
    **not** call this function before ``bash -c``.  This is mirrored — for the
    PT6 no-import boundary — by ``publish_doc_scripts._substitute_renderable``
    after the RN1/RN2 strip, and it is the canonical *declared*-value
    rendering against which DE6 gates equivalence
    (``declared == canonical(provisioned)``, SP7 prefix stripped).  The
    published artifact is therefore not a substring of the executed text; the
    tested guarantee is the DE6 canonical equivalence, not byte-identity or a
    "drift gate executes the rendered text" substring claim.  Behaviour is
    the RN3 rule only:

    - For every ``name`` in *display_env* (longest first, so ``PKG_FOO_BAR``
      is replaced before ``PKG_FOO``), replace ``${name}`` (brace form,
      unambiguous — ``${PKG_UV}x`` → ``uv:0.10x``) and the **word-boundary**
      bare form ``$name`` *not* followed by ``[A-Za-z0-9_]`` with the value.
      Surrounding double quotes are adjacent literal text and are preserved
      verbatim (``"$PKG_UV"`` → ``"uv:0.10"``; ``$PKG_UV`` → ``uv:0.10``).
      The bare form is word-boundary correct: with only ``PKG_UV`` declared,
      ``$PKG_UVX`` and ``$PKG_UV_2`` are left verbatim (no prefix-substring
      false substitution).  Literal text substitution, not shell-semantics.
    - Non-renderable ``$VAR`` (anything not a *display_env* key) is left
      **verbatim** — it resolves from ``provider.script_env()`` at drift-gate
      runtime, so the executed text stays valid.  This function raises
      **nothing**; RN5/RN5b remain the *display-region* gate (a separate
      verify-path check), not this runtime path.

    Idempotent and pure (RN7): values never contain a ``$`` form that would
    re-match, so a second pass is a no-op.

    Args:
        text: Script text (full body for EX10; post-RN1/RN2 body for display).
        display_env: ``{PKG_<KEY>: short_ref, REPO_<KEY>: bare_repo}`` for the
            script's resolved state (possibly ``{}``).

    Returns:
        *text* with renderable matrix vars substituted; everything else
        unchanged.
    """
    if not display_env:
        return text
    # Longest-name-first so ``PKG_FOO_BAR`` is tried before ``PKG_FOO``; the
    # brace form consumes ``${...}`` exactly while the bare form is guarded by
    # a trailing ``(?![A-Za-z0-9_])`` negative lookahead so ``$PKG_UV`` does
    # not match the ``PKG_UV`` prefix inside ``$PKG_UVX`` / ``$PKG_UV_2``.
    names = sorted(display_env, key=len, reverse=True)
    alternation = "|".join(re.escape(n) for n in names)
    pattern = re.compile(
        rf"\$\{{({alternation})\}}|\$({alternation})(?![A-Za-z0-9_])"
    )

    def _replace(m: re.Match[str]) -> str:
        return display_env[m.group(1) or m.group(2)]

    return pattern.sub(_replace, text)


# ---------------------------------------------------------------------------
# Drift-gate executor
# ---------------------------------------------------------------------------


def run_doc_script(
    path: Path,
    ocx: "OcxRunner",
    tmp_path: Path,
) -> None:
    """Parse, provision, and execute a doc script as a drift-gate case.

    This is the single entry point for the drift-gate acceptance test
    (§2, EX1–EX9, GO1–GO3, DG1–DG3).  Execution is fully equivalent to the
    ``test_scenario_script`` harness: the script body is run via
    ``Scenario.run_file`` / ``script_env``, not via a custom subprocess call.

    Steps:

    1. Parse the header via ``parse_doc_header`` (raises
       ``DocScriptParseError`` on unknown keys, bad slug, or cast-arity
       violation).
    2. Resolve the state via ``resolve_state(meta.state)`` (raises
       ``ValueError`` on unqualified / unknown state — EX4).
    3. Call ``provider.provision(ocx, tmp_path)`` to push packages.
    4. Run the **full raw** body with ``provider.script_env()`` (SP7-prefixed
       ``$PKG_*``).  EX10: NOT renderable-substituted — SP7 parallel
       isolation requires the prefixed ref to resolve.  The displayed
       artifact is the same source rendered with *declared* values; DE6
       gates `declared == canonical(provisioned)`, so displayed == drift-
       gated command with the isolation prefix canonicalised away.
    5. On non-zero exit, raise ``AssertionError`` whose message includes
       (EX3 / DG1 / DG2):

       - ``meta.title`` (or path stem if absent)
       - ``str(path)``
       - ``meta.description`` (when present)
       - ``meta.doc`` slug (when present) — so the failing doc page is
         named in CI output without opening the script (DG2)
       - ANSI-stripped combined stdout+stderr

    6. If ``meta.expect`` is set:

       - Resolve the golden file relative to the script's parent directory.
       - If the file does not exist, raise ``AssertionError`` with
         ``"golden file not found: <path>"`` (GO3).
       - ANSI-strip both golden content and captured output.
       - On mismatch, raise ``AssertionError`` with a unified diff (GO2).
       - On match, return normally (GO1).

    Note: no cast is produced on the verify path (EX8 / CA3).

    Args:
        path: Absolute path to the ``.sh`` script.
        ocx: ``OcxRunner`` instance (test-isolated binary + env).
        tmp_path: Per-test temporary directory (used for provisioning and as
            script working directory).

    Raises:
        DocScriptParseError: Header grammar violation (EX5, EX9).
        ValueError: Unqualified or unknown ``# state:`` value (EX4).
        AssertionError: Script exits non-zero (EX3/DG1/DG2) or golden
            mismatch / missing golden (GO2/GO3).
    """
    from src.state_providers import resolve_state

    # Step 1: parse header (raises DocScriptParseError on grammar violations)
    meta = parse_doc_header(path)

    # Step 2: resolve state (raises ValueError on unknown/unqualified state — EX4)
    provider = resolve_state(meta.state)

    # Step 3: provision packages
    provider.provision(ocx, tmp_path)

    # Step 4: run the full body with the provider's env.
    #
    # EX10 (LDR 2026-05-18): the body is executed **raw** under script_env,
    # NOT renderable-substituted.  Substituting `$PKG_*`/`$REPO_*` to the
    # clean display short before `bash -c` is incompatible with SP7 parallel
    # isolation: provision() pushes to an SP7-prefixed repo
    # (`t_<8hex>_<repo>`), and only the SP7-prefixed `$PKG_*` value in
    # script_env resolves against the registry.  The honest tested guarantee
    # is therefore **DE6-canonical equivalence**, not byte-identity: the
    # displayed artifact is this exact source rendered with the *declared*
    # values, and DE6 gates `declared == canonical(provisioned)` (SP7 prefix
    # stripped) — so the displayed command is the drift-gated command with
    # only the parallel-isolation prefix canonicalised away.  That replaces
    # the removed RN6 "not the tested source" disclaimer with a precise,
    # gated relationship rather than a weaker byte-identity that SP7 forbids.
    script_env = provider.script_env()
    body = path.read_text()

    result = subprocess.run(
        ["bash", "-c", body],
        env=script_env,
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
    )

    combined_output = result.stdout + result.stderr
    stripped_output = strip_ansi(combined_output)

    # Step 5: on non-zero exit, raise with rich diagnostic (EX3/DG1/DG2)
    if result.returncode != 0:
        parts = [
            f"doc script failed (rc={result.returncode})",
            f"  script: {path}",
            f"  title: {meta.title}",
        ]
        if meta.description:
            parts.append(f"  description: {meta.description}")
        if meta.doc:
            parts.append(f"  doc: {meta.doc}")
        parts.append("--- output ---")
        parts.append(stripped_output)
        raise AssertionError("\n".join(parts))

    # Step 6: golden-output diffing (GO1–GO3)
    if meta.expect is not None:
        golden_path = path.parent / meta.expect
        if not golden_path.exists():
            raise AssertionError(f"golden file not found: {golden_path}")

        golden_text = strip_ansi(golden_path.read_text())
        actual_text = strip_ansi(combined_output)

        if golden_text != actual_text:
            diff = "\n".join(
                difflib.unified_diff(
                    golden_text.splitlines(),
                    actual_text.splitlines(),
                    fromfile="golden",
                    tofile="actual",
                    lineterm="",
                )
            )
            raise AssertionError(
                f"golden output mismatch for {path}\n{diff}"
            )


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

__all__ = [
    "SLUG_RE",
    "DocScriptExportEntry",
    "DocScriptMeta",
    "DocScriptParseError",
    "discover_doc_scripts",
    "doc_scripts_export",
    "parse_doc_header",
    "run_doc_script",
    "strip_ansi",
    "substitute_renderable",
]
