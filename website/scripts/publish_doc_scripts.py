"""Website-owned publish task implementation for doc scripts (PT1–PT7).

Behaviour contract (design_spec_doc_command_scripts.md §5):

- PT1: Scripts with no ``# doc:`` slug are not copied.
- PT2: ``# doc: a/b-c`` → ``website/src/_scripts/a/b-c.sh`` (nested path;
  slug ``/`` = directory separator; parent dirs created ``mkdir -p``).
  LDR 2026-05-17 (ADR Decision D): the ``/`` → ``__`` flattening is removed.
- PT3: Idempotent — writes only when content differs (mtime-stable on second
  identical run).  Concurrent invocations from parallel test workers are safe:
  the manifest is keyed by source root so each unique ``OCX_DOC_SCRIPTS_ROOT``
  manages its own section independently (no cross-worker orphan sweep).
- PT4: Duplicate slug across two scripts → exit non-zero with message
  containing ``duplicate doc slug '<slug>'`` and both filenames; write nothing.
- PT5: Maintains ``website/src/_scripts/.published.json`` manifest of
  task-owned published filenames.  Orphan sweep removes only task-owned ``*.sh``
  no longer backed by a current slug, then prunes slug directories it owns that
  became empty.  Never touches files absent from the manifest, non-``.sh``
  files, or any directory still containing foreign content.
- PT6: Discovers scripts exclusively via ``task test:doc-scripts:list``
  (subprocess → parse JSON); no ``test/`` path literal hardcoded here.

Usage (called by taskfile)::

    python publish_doc_scripts.py <scripts_out_dir> <manifest_path>

Environment:
    OCX_DOC_SCRIPTS_ROOT  Forwarded to ``task test:doc-scripts:list`` so the
                           taskfile tests can redirect discovery to a fixture
                           directory instead of the real doc_scripts tree.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import TypedDict

# ---------------------------------------------------------------------------
# Module-level compiled patterns (Perf F1).
#
# render_display is called once per published script inside the locked
# section; compiling these regexes per call (and re-importing ``re`` as a
# function-local) is pure overhead.  They are deterministic, stateless and
# safe to share across calls — hoisted to module scope so compilation happens
# exactly once at import time.
# ---------------------------------------------------------------------------

# RN2 metadata-header line: ``# <key>: <value>`` (key recognised below).
_META_LINE_RE: re.Pattern[str] = re.compile(r"^#\s*([a-zA-Z][a-zA-Z0-9_-]*):\s*")

# RN2 step (c): a single leading ``set -e`` / ``set -eu`` / ``set -euo pipefail``.
_SET_E_RE: re.Pattern[str] = re.compile(r"^set\s+-e(?:u(?:o\s+pipefail)?)?\s*$")

# RN2 recognised header keys (mirrors _RECOGNISED_KEYS in doc_scripts.py).
_RECOGNISED_KEYS: frozenset[str] = frozenset(
    {"state", "doc", "cast", "title", "description", "expect"}
)

# RN5 fixture/harness namespace: ^(PKG|REPO|FQ|TAG|MARKER|HOME_KEY)_ prefix.
_FIXTURE_PREFIX_RE: re.Pattern[str] = re.compile(
    r"^(?:PKG|REPO|FQ|TAG|MARKER|HOME_KEY)_"
)

# RN5 runner-harness exact names (not prefix-matched).
_HARNESS_EXACT: frozenset[str] = frozenset(
    {"REGISTRY", "SCENARIO_TMP", "OCX", "OCX_HOME"}
)

# RN3/RN5 variable-reference scanner: $NAME / ${NAME} / "$NAME" / "${NAME}".
# Digit/special forms ($1, $@, $$, …) deliberately do not match.
_VAR_REF_RE: re.Pattern[str] = re.compile(
    r'"?\$\{([A-Za-z_][A-Za-z0-9_]*)\}"?|"?\$([A-Za-z_][A-Za-z0-9_]*)"?'
)

# fcntl is POSIX-only (Linux/macOS).  Guard the import so this module can be
# imported on Windows without an opaque ModuleNotFoundError at import time.
# The task is Linux-CI only; the no-op branch is a safety net for tooling.
_HAS_FCNTL: bool
if sys.platform != "win32":
    import fcntl  # type: ignore[import]  # noqa: F401 – used below
    _HAS_FCNTL = True
else:
    _HAS_FCNTL = False


# ---------------------------------------------------------------------------
# Render-layer exception (Phase 2 — RN5 hard publish error).
# ---------------------------------------------------------------------------


class RenderError(Exception):
    """Raised by :func:`render_display` when a non-renderable fixture/harness
    variable is encountered (RN5) or when a region is empty/malformed.

    The publish task must treat any :class:`RenderError` as a hard publish
    error: write nothing, propagate the message to stderr, and exit non-zero
    (consistent with PT4 duplicate-slug behaviour).
    """


# ---------------------------------------------------------------------------
# Export entry type (canonical definition is test/src/doc_scripts.py
# DocScriptExportEntry — mirrored here to avoid a runtime test→website import
# dependency, per PT6 tenet #2 / design_spec §5).
# ---------------------------------------------------------------------------


class _DocScriptExportEntry(TypedDict):
    """Local mirror of the canonical DocScriptExportEntry TypedDict (PT6 / DE5).

    Canonical definition lives in the test subsystem's doc_scripts module.
    Mirrored here to avoid a runtime cross-boundary import (PT6 tenet #2:
    no website/ file imports from the test tree at runtime).

    Schema must stay byte-for-byte identical (key set + per-key types) to the
    canonical ``DocScriptExportEntry`` defined in the test subsystem's
    doc_scripts module.  A verify-path static gate (DE5) asserts parity via
    ``ast`` annotation extraction — drifting a field here without updating the
    canonical definition fails ``task verify``.

    Schema:
        path:         str
        slug:         str | None
        cast:         bool
        expect:       str | None
        display_env:  dict[str, str]
        state:        str
        cast_region:  tuple[int, int] | None
        title:        str | None
        description:  str | None
    """

    path: str
    slug: str | None
    cast: bool
    expect: str | None
    # DE1 additions — parity with DocScriptExportEntry
    display_env: dict[str, str]
    state: str
    cast_region: tuple[int, int] | None
    title: str | None
    description: str | None


def _slug_to_relpath(slug: str) -> str:
    """Return the relative path for a doc-script slug under ``_scripts/``.

    Contract (PT2 / ADR Decision D / LDR 2026-05-17 nested-path update):

    - Slug ``a/b-c`` → relative path ``a/b-c.sh``.
    - The slug ``/`` character is the **directory separator** — it maps 1:1 to
      a filesystem path component.  No escaping, no flattening.
    - Result is ``slug + ".sh"``; parent directories are created by the caller
      via ``mkdir -p`` before writing (PT2 / PT5).
    - The old ``/`` → ``__`` flattening (``_flat_slug``) is **removed** by this
      LDR; this function supersedes it.
    - Injective: distinct slugs produce distinct relative paths by construction
      (slug grammar ``^[a-z0-9]+(?:[-/][a-z0-9]+)*$`` forbids ambiguous forms).

    Args:
        slug: A validated doc-script slug (``^[a-z0-9]+(?:[-/][a-z0-9]+)*$``).

    Returns:
        The relative path string ``<slug>.sh`` with ``/`` preserved as the
        directory separator (e.g. ``getting-started/install-select.sh``).
    """
    return slug + ".sh"


def _substitute_renderable(text: str, display_env: dict[str, str]) -> str:
    """RN8 substitution — behaviour mirror of the canonical
    ``substitute_renderable`` in the test subsystem.

    Kept as a hand-mirror (not an import) because the website never imports
    the test subsystem (PT6).  The DE5-style parity gate
    (``test_*::test_substitute_renderable_parity``) asserts this stays
    byte-equivalent to the canonical implementation.  RN3 rule only:
    longest-name-first, word-boundary-correct replacement of the ``${NAME}``
    and bare ``$NAME`` forms (bare form guarded so ``$PKG_UVX`` /
    ``$PKG_UV_2`` are left verbatim when only ``PKG_UV`` is declared),
    surrounding double quotes preserved as adjacent literal text;
    non-renderable ``$VAR`` left verbatim; raises nothing.

    Publish-only path.  This function is **not** what the drift gate runs:
    the drift gate executes the *raw* script body under ``script_env`` with
    the SP7-prefixed ``$PKG_*`` refs (EX10).  Equivalence between the
    published artifact and the drift-gated command is **not** a substring or
    "the drift gate executes the rendered text" relationship — it is the
    DE6-gated canonical equivalence ``declared == canonical(provisioned)``
    (SP7 isolation prefix stripped).  This function only realises the
    *declared*-value rendering side of that gated equivalence.
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


def render_display(
    script_text: str,
    *,
    cast_region: tuple[int, int] | None,
    display_env: dict[str, str],
    slug: str,
) -> str:
    """Render a doc-script source text to its display artifact.

    Pure, deterministic, side-effect-free (RN7).  Called by the publish task
    **before** the PT3 content-hash compare so an unchanged
    ``script_text + display_env`` produces no write.

    Render rule contract (design_spec_doc_command_scripts.md §6e):

    RN1 — Region present (``cast_region`` is not ``None``):
        Output = the lines **strictly between** the ``# region cast`` /
        ``# endregion cast`` markers (markers excluded), in file order, with
        leading and trailing fully-blank lines trimmed.  Lines outside the
        region (shebang, header, ``set -euo pipefail``, capture, assertions)
        are **absent** from the output.  ``cast_region`` is the ``(start, end)``
        line-number span already parsed from the script header (0- or 1-based
        per the caller's convention — see :class:`_DocScriptExportEntry`
        ``cast_region`` field; consumers treat it positionally as
        ``region[0]`` / ``region[1]`` because the JSON wire format carries a
        2-element array, not a tuple instance per DE1 wire-format note).

    RN2 — No region (``cast_region`` is ``None``):
        Output = full body **minus**:
        (a) a leading shebang line (``#!…``),
        (b) the contiguous ``# <key>: <value>`` metadata header block — the
            same span ``parse_doc_header`` consumes; plain non-metadata
            comments outside the header are **kept**,
        (c) a single leading ``set -euo pipefail`` line (and the shorter
            variants ``set -e`` / ``set -eu``) if it is the first non-blank
            line after the header.
        Nothing else is removed.

    RN3 — Variable expansion:
        In the post-RN1/RN2 text, every occurrence of ``$NAME``, ``${NAME}``,
        ``"$NAME"``, ``"${NAME}"`` where ``NAME`` is a key in ``display_env``
        is replaced by ``display_env[NAME]``.  Replacement is **literal text
        substitution** on the post-strip text; surrounding quotes (if any) are
        preserved verbatim (e.g. ``"$PKG_UV"`` → ``"uv:0.10"``; ``$PKG_UV``
        → ``uv:0.10``).

        Edge case: ``$NAME`` inside a single-quoted string (``'$PKG_UV'``) is
        still substituted — RN3 is text substitution, not shell-semantics
        emulation.  Documented so testers assert the literal-text behaviour.

    RN4 — Ambient / shell-special variables left verbatim:
        ``$(…)``, `` `…` ``, ``$$``, ``$@``, ``$#``, ``$?``, ``$!``, ``$0``,
        positional ``$1``..``$9`` / ``${1}``..., **and** any ``$NAME`` /
        ``${NAME}`` that is an ambient shell variable the author legitimately
        displays (``$HOME``, ``$PATH``, ``$PWD``, ``$USER``, ``$SHELL``,
        ``$XDG_*``, …) — i.e. anything **not** matching the RN5 fixture
        namespace — are **left verbatim**.  No expansion, no error.

    RN5 — Fixture / harness variable namespace = hard publish error:
        A ``$NAME`` / ``${NAME}`` whose ``NAME`` matches the fixture namespace
        ``^(FQ|TAG|MARKER|HOME_KEY)_`` **or** is a runner-harness
        var (``REGISTRY``, ``SCENARIO_TMP``, ``OCX``, ``OCX_HOME``) — AND is
        **not** a key in ``display_env`` — raises :class:`RenderError`
        (hard publish error; no file is written).  Error message includes
        the variable name + slug + canonical guidance.

        The SP0-safe **renderable matrix** is ``$PKG_<KEY>`` (→ short ref)
        and ``$REPO_<KEY>`` (→ bare repo name), both in ``display_env``
        (LDR 2026-05-17, DE1/DE2).  A var whose name is a key in
        ``display_env`` is substituted by RN3 regardless of prefix.
        ``$FQ_*`` / ``$TAG_*`` / ``$MARKER_*`` / ``$HOME_KEY_*`` and runner
        vars are never in ``display_env`` and are banned from displayed
        regions; the ~27 scripts that referenced fixture vars in their
        would-be-displayed body were remediated in Phase 3 (region-scoped).
        Note: the implementation pattern still lists ``PKG``/``REPO`` for
        symmetry but the ``in display_env`` pre-check renders them first, so
        only an *undeclared* fixture-namespace var ever reaches RN5.

    RN5b (verify-path surface, referenced here for completeness):
        The static pre-publish scrub of RN5 lives in the verify-path test
        collection (``test:parallel``), not in this function.  This function
        implements the **runtime** hard error only (RN5); the static check
        (RN5b) is implemented in the test subsystem's doc-binding gate.

    RN6 — No disclaimer header (LDR 2026-05-18, EX10/DE6 framing):
        **No** generated-marker / "not the tested source" line is prepended.
        The honest tested relationship is **DE6-canonical equivalence**, not
        a substring claim: the drift gate executes the *raw* script body
        under ``script_env`` with the SP7-prefixed ``$PKG_*`` refs (EX10) —
        it does **not** execute ``substitute_renderable``-rendered text.
        This output is therefore not a literal substring of what the drift
        gate ran.  Instead DE6 gates ``declared == canonical(provisioned)``
        (the SP7 isolation prefix canonicalised away), so the displayed
        artifact is this exact source rendered with the *declared* values
        and is equivalent to the drift-gated command modulo only that
        parallel-isolation prefix.  The old "not the tested source"
        disclaimer and the weaker "drift gate executes the rendered text"
        substring claim are both removed in favour of this precise, gated
        relationship.  The artifact is still not required to be
        standalone-valid bash (a region slice may omit its enclosing
        ``if``/``fi``); the runnable form is the full body the drift gate
        runs, of which this is the declared-value rendering.  Leading/
        trailing fully-blank lines are trimmed on both the RN1 and RN2 paths.

    RN8 — Single substitution source:
        Substitution is delegated to :func:`_substitute_renderable` (the
        behaviour mirror of the one canonical ``substitute_renderable`` in
        the test subsystem).  ``substitute_renderable`` is **publish-only**;
        the drift gate runs the raw body, not its output.  Sharing one
        implementation guarantees the *declared*-value rendering is the same
        bytes on both the publish path and the canonical reference used by
        the DE6 equivalence gate — it does not imply the drift gate executes
        these bytes.

    RN7 — Pure / idempotent:
        Calling this function twice with identical arguments produces byte-for-
        byte identical output.  This property feeds PT3 (idempotency): the
        render runs **before** the PT3 content-hash compare so an unchanged
        source + unchanged ``display_env`` produces no disk write.

    Edge cases (§6e):

    - Empty region (markers adjacent, ``cast_region`` span covers zero lines):
      raises :class:`RenderError` with message
      ``empty cast region in <slug> (slug <slug>)``.
    - ``# region cast`` on a ``# cast: false`` / ``# doc:``-only script:
      RN1 still applies (the region is the *display* selector independent of
      cast opt-in — ADR H-3).  The publish task enforces ≤1-region arity and
      raises :class:`RenderError` ``display script has >1 cast region`` on >1.
    - ``$NAME`` inside single-quoted strings: substituted (RN3 is text
      substitution, not shell-semantics).

    Args:
        script_text: Raw UTF-8 source text of the script under ``test/``.
        cast_region: ``(start_line, end_line)`` 1-based inclusive line-number
            span of the ``# region cast`` and ``# endregion cast`` marker lines
            themselves (same convention as ``parse_doc_header``), or ``None``
            when the script has no ``# region cast`` block.  Consumers treat
            this positionally (``region[0]``, ``region[1]``) as the JSON wire
            format delivers a 2-element array, not a Python ``tuple`` (DE1
            wire-format note).
        display_env: Mapping of bare env-var name (no ``$``) → canonical
            display value for the script's resolved state (e.g.
            ``{"PKG_UV": "uv:0.10", "REPO_UV": "uv"}``).  Keys are the
            renderable matrix ``PKG_<KEY>`` (→ short ref) and ``REPO_<KEY>``
            (→ bare repo name) per DE1/DE2 (LDR 2026-05-17).  Any key
            present here is renderable via RN3; ``$FQ_*``/``$TAG_*``/
            ``$MARKER_*``/``$HOME_KEY_*`` and runner vars are never present
            and (when referenced in a displayed region) fire RN5.  Always
            present (possibly ``{}``) — never ``None``.
        slug: The validated ``# doc:`` slug for this script (used in error
            messages; e.g. ``getting-started/install-select``).

    Returns:
        The fully-rendered display text (UTF-8 string) — blank-trimmed, one
        trailing newline, **no** disclaimer header (RN6, LDR 2026-05-18).
        Ready for byte-for-byte comparison (PT3) and writing to
        ``website/src/_scripts/<slug>.sh`` via :func:`_slug_to_relpath`.

    Raises:
        RenderError: When RN5 (non-renderable fixture var) or the empty-region
            edge case is detected.  Caller must **not** write any output file
            on this exception.
    """
    all_lines = script_text.splitlines()

    # ------------------------------------------------------------------
    # RN1 / RN2: extract the body lines to display
    # ------------------------------------------------------------------
    if cast_region is not None:
        # RN1: output = lines strictly between the two markers.
        # cast_region is (start, end), 1-based inclusive line numbers of the
        # marker lines themselves.  Reuse _extract_region_lines semantics:
        #   all_lines[start : end-1]  (0-indexed: lines after start marker,
        #   before end marker)
        start, end = cast_region[0], cast_region[1]
        region_body = all_lines[start : end - 1]
        # Trim leading/trailing fully-blank lines
        while region_body and not region_body[0].strip():
            region_body = region_body[1:]
        while region_body and not region_body[-1].strip():
            region_body = region_body[:-1]
        if not region_body:
            raise RenderError(
                f"empty cast region in {slug!r} (slug {slug}): "
                "the displayed region has no content — authoring bug"
            )
        body_lines = region_body
    else:
        # RN2: full body minus (a) shebang, (b) metadata header block, (c) set -e.
        #
        # The metadata header block is the contiguous set of lines at the top
        # parsed by parse_doc_header: shebang lines, and lines matching
        # ``# <recognized-key>: <value>`` (case-insensitive, same keys as
        # _RECOGNISED_KEYS in doc_scripts.py).  Plain comment lines that do not
        # carry a recognized key:value pair are kept (RN2 contract).
        #
        # The header scan mirrors parse_doc_header's loop precisely:
        # - shebang → strip (step a, part of header span)
        # - blank line → stops the header span
        # - non-comment non-blank line → stops the header span
        # - comment with recognized key:value → strip (step b)
        # - comment with unknown key or no colon → keep (plain comment, kept)
        #
        # _RECOGNISED_KEYS / _META_LINE_RE are module-level constants (Perf F1).
        stripped_lines: list[str] = []
        in_header = True
        for line in all_lines:
            stripped_line = line.strip()
            if in_header:
                if not stripped_line:
                    # blank line terminates header span; keep blank line in output
                    in_header = False
                    stripped_lines.append(line)
                    continue
                if stripped_line.startswith("#!"):
                    # shebang — always strip (step a)
                    continue
                if not stripped_line.startswith("#"):
                    # first non-blank non-comment line → header over; process below
                    in_header = False
                    stripped_lines.append(line)
                    continue
                # comment line in header span
                m = _META_LINE_RE.match(stripped_line)
                if m:
                    key = m.group(1).lower()
                    if key in _RECOGNISED_KEYS:
                        # metadata line → strip (step b)
                        continue
                # plain comment (no recognized key or no colon) → keep
                stripped_lines.append(line)
            else:
                stripped_lines.append(line)

        # step (c): strip a single leading set -e / set -eu / set -euo pipefail
        # if it is the first non-blank line after the header (_SET_E_RE is a
        # module-level constant — Perf F1).
        first_nonblank_idx: int | None = None
        for i, line in enumerate(stripped_lines):
            if line.strip():
                first_nonblank_idx = i
                break
        if first_nonblank_idx is not None and _SET_E_RE.match(
            stripped_lines[first_nonblank_idx].strip()
        ):
            stripped_lines.pop(first_nonblank_idx)

        body_lines = stripped_lines

    # Join to text for variable substitution
    body_text = "\n".join(body_lines)
    if body_lines:
        body_text += "\n"

    # ------------------------------------------------------------------
    # RN3 + RN5: variable substitution and fixture-namespace error check
    # ------------------------------------------------------------------
    # Fixture/harness namespace patterns (RN5) are module-level constants
    # (_FIXTURE_PREFIX_RE / _HARNESS_EXACT — Perf F1):
    # - NAME matching ^(PKG|REPO|FQ|TAG|MARKER|HOME_KEY)_
    # - OR exactly: REGISTRY, SCENARIO_TMP, OCX, OCX_HOME
    #
    # First pass: collect all variable names referenced in body_text
    # to detect RN5 errors before doing any substitution.
    # We need to find NAME from $NAME / ${NAME} / "$NAME" / "${NAME}"
    # but NOT from $(…) (subshell), $$ (pid), $@, $#, $?, $!, $0, $1-$9, ${1}, etc.
    # Those are handled by _VAR_REF_RE not matching them (digits, @, #, etc.)
    for m in _VAR_REF_RE.finditer(body_text):
        name = m.group(1) or m.group(2)
        # Skip if in display_env (will be substituted by RN3)
        if name in display_env:
            continue
        # Check if in fixture namespace (RN5 error)
        if _FIXTURE_PREFIX_RE.match(name) or name in _HARNESS_EXACT:
            raise RenderError(
                f"non-renderable fixture/harness variable '${name}' in "
                f"displayed region of script (slug {slug!r}): only the "
                f"renderable matrix ($PKG_<KEY>, $REPO_<KEY>) is allowed "
                f"in a displayed region (DE1/DE2) — move it outside the "
                f"region or rewrite to a $PKG_/$REPO_ ref"
            )
        # RN4: anything else (ambient shell vars like HOME, PATH, etc.) → verbatim, no error

    # Second pass: RN3/RN8 substitution (the single shared impl).
    result_text = _substitute_renderable(body_text, display_env)

    # ------------------------------------------------------------------
    # RN6 (LDR 2026-05-18, EX10/DE6): NO disclaimer header.  The display
    # artifact is NOT a substring of what the drift gate runs — the drift
    # gate executes the RAW body under script_env (SP7-prefixed $PKG_*),
    # not substitute_renderable's output.  The tested relationship is the
    # DE6-canonical equivalence (declared == canonical(provisioned), SP7
    # prefix stripped); the old "# Rendered for display ... not the tested
    # source." line and the weaker substring claim are both removed in
    # favour of that gated relationship.  Final blank-trim: strip leading/
    # trailing fully-blank
    # lines on BOTH the RN1 and RN2 paths (RN2's header-terminating blank
    # previously leaked as a spurious first line), normalise to exactly one
    # trailing newline.
    # ------------------------------------------------------------------
    out_lines = result_text.split("\n")
    while out_lines and not out_lines[0].strip():
        out_lines.pop(0)
    while out_lines and not out_lines[-1].strip():
        out_lines.pop()
    if not out_lines:
        return ""
    return "\n".join(out_lines) + "\n"


def _fetch_export(project_root: Path) -> list[_DocScriptExportEntry]:
    """Shell out to ``task test:doc-scripts:list`` and parse the JSON result.

    The ``OCX_DOC_SCRIPTS_ROOT`` env var (if set) is forwarded so fixture
    directories can be injected by tests (PT6).
    """
    env = os.environ.copy()
    result = subprocess.run(
        ["task", "test:doc-scripts:list"],
        cwd=str(project_root),
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(  # noqa: T201
            f"ERROR: task test:doc-scripts:list failed (rc={result.returncode}):\n"
            f"{result.stderr}",
            file=sys.stderr,
        )
        sys.exit(1)

    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        print(  # noqa: T201
            f"ERROR: task test:doc-scripts:list produced invalid JSON: {exc}\n"
            f"stdout={result.stdout!r}",
            file=sys.stderr,
        )
        sys.exit(1)

    return _validate_export(parsed)


# Required key set for the discovery-seam wire schema (mirror of
# _DocScriptExportEntry).  Kept as a module constant so the structural
# validation below stays a single source of truth.
_EXPORT_ENTRY_KEYS: frozenset[str] = frozenset(_DocScriptExportEntry.__annotations__)


def _validate_export(parsed: object) -> list[_DocScriptExportEntry]:
    """Validate the parsed seam payload against ``_DocScriptExportEntry``.

    W5: ``json.loads`` returns ``Any``; a malformed seam (non-list, or an
    entry that is not an object / is missing required keys) must fail with a
    precise per-entry stderr ``ERROR`` and a non-zero exit — consistent with
    the existing ``JSONDecodeError`` handling — rather than being silenced by
    a blanket ``# type: ignore`` and exploding later with an opaque
    ``KeyError`` / ``AttributeError`` deep inside the publish path.
    """
    if not isinstance(parsed, list):
        print(  # noqa: T201
            "ERROR: task test:doc-scripts:list produced a non-list payload "
            f"(got {type(parsed).__name__}); expected a JSON array of "
            "doc-script export entries",
            file=sys.stderr,
        )
        sys.exit(1)

    errors: list[str] = []
    for idx, entry in enumerate(parsed):
        if not isinstance(entry, dict):
            errors.append(
                f"entry[{idx}] is {type(entry).__name__}, expected an object"
            )
            continue
        keys = set(entry.keys())
        missing = _EXPORT_ENTRY_KEYS - keys
        unknown = keys - _EXPORT_ENTRY_KEYS
        if missing:
            errors.append(
                f"entry[{idx}] missing required keys: "
                f"{sorted(missing)} (path={entry.get('path')!r})"
            )
        if unknown:
            errors.append(
                f"entry[{idx}] has unknown keys: "
                f"{sorted(unknown)} (path={entry.get('path')!r})"
            )

    if errors:
        for msg in errors:
            print(f"ERROR: malformed doc-script export {msg}", file=sys.stderr)  # noqa: T201
        sys.exit(1)

    # Structurally validated above — the cast to the TypedDict is now sound.
    return parsed  # type: ignore[return-value]


def _load_manifest(manifest_path: Path) -> dict[str, list[str]]:
    """Load the manifest of task-owned filenames keyed by source root (PT5).

    The manifest is a dict mapping ``OCX_DOC_SCRIPTS_ROOT`` → list of
    published filenames.  Keying by source root makes concurrent invocations
    from different test workers independently manage their own file sets
    without interfering via cross-worker orphan sweeps (PT3 parallel safety).

    Backwards compat: a legacy flat list is treated as the empty-string key.
    """
    if not manifest_path.exists():
        return {}
    try:
        data = json.loads(manifest_path.read_text())
    except (json.JSONDecodeError, OSError):
        return {}
    if isinstance(data, list):
        # Legacy flat-list format → promote to keyed format under "" key
        return {"": [str(item) for item in data]}
    if isinstance(data, dict):
        return {k: [str(f) for f in v] for k, v in data.items() if isinstance(v, list)}
    return {}


def _save_manifest(manifest_path: Path, manifest: dict[str, list[str]]) -> None:
    """Persist the manifest dict (PT5).

    Skips the write when the content is already identical so that concurrent
    invocations do not needlessly invalidate each other's cached outputs
    (PT3 idempotency contract).
    """
    # Normalise: sort each section's file list and sort top-level keys
    normalised = {k: sorted(v) for k, v in sorted(manifest.items())}
    new_content = json.dumps(normalised, indent=2) + "\n"
    if manifest_path.exists():
        try:
            if manifest_path.read_text() == new_content:
                return
        except OSError:
            pass
    manifest_path.write_text(new_content)


def _publish_locked(
    scripts_out: Path,
    manifest_path: Path,
    to_publish: list[tuple[str, Path, dict[str, str], tuple[int, int] | None]],
    source_key: str,
) -> None:
    """Execute the manifest-aware render + copy + orphan sweep under an exclusive lock.

    Called only when the caller holds the fcntl lock on the manifest lock file,
    ensuring serialised access from concurrent parallel test workers (PT3).

    ``source_key`` is the value of ``OCX_DOC_SCRIPTS_ROOT`` (or ``""`` for the
    production case).  Each source root manages its own manifest section so
    parallel workers with different source roots do not orphan each other's
    published files.

    Phase-2 implementation (LDR 2026-05-17):

    - ``to_publish`` carries ``(slug, src_path, display_env, cast_region)``
      to thread :func:`render_display` inputs from the seam entry into the
      write step (RN1–RN7).
    - Orphan sweep semantics use nested slug paths (PT2 LDR): manifest tracks
      nested relative paths (``getting-started/install-select.sh``) instead of
      flat names; the sweep also prunes now-empty slug directories that are
      fully owned (PT5 LDR 2026-05-17).
    - Write path: destination is ``scripts_out / _slug_to_relpath(slug)``
      (nested); parent directories are created ``mkdir -p`` before writing.
    - Rendered content: each script is passed through :func:`render_display`
      before the PT3 hash compare.  A :class:`RenderError` from any entry
      aborts the entire locked section (no partial writes — consistent with
      PT4 duplicate-slug behaviour).

    """
    # --- PT5: load prior manifest ---
    manifest = _load_manifest(manifest_path)
    prior_section: set[str] = set(manifest.get(source_key, []))

    # --- compute current set of owned relative paths for this source root ---
    # PT2 LDR 2026-05-17: use nested _slug_to_relpath (no flattening).
    current_owned: set[str] = set()
    for slug, _src_path, _display_env, _cast_region in to_publish:
        current_owned.add(_slug_to_relpath(slug))

    # --- RN1–RN7: render all entries first; abort on any RenderError ---
    rendered_entries: list[tuple[str, str]] = []  # (relpath, rendered_text)
    for slug, src_path, display_env, cast_region in to_publish:
        rendered = render_display(
            src_path.read_text(),
            cast_region=cast_region,
            display_env=display_env,
            slug=slug,
        )
        rendered_entries.append((_slug_to_relpath(slug), rendered))

    # --- PT5: orphan sweep — remove only this source root's orphans ---
    # CWE-22 defense-in-depth: refuse any path that resolves outside
    # scripts_out (slugs are SLUG_RE-validated upstream so this only
    # triggers on a tampered .published.json — belt-and-suspenders), and
    # never follow symlinks.
    scripts_root = scripts_out.resolve()

    def _inside(p: Path) -> bool:
        try:
            return p.resolve().is_relative_to(scripts_root)
        except (OSError, ValueError):
            return False

    for orphan_relpath in prior_section - current_owned:
        orphan_path = scripts_out / orphan_relpath
        if (
            orphan_path.exists()
            and orphan_path.is_file()
            and not orphan_path.is_symlink()
            and _inside(orphan_path)
        ):
            orphan_path.unlink()

    # --- PT5 LDR: prune owned slug directories that became fully empty ---
    # A directory is pruned only if it was an ancestor of a now-removed
    # orphan, is fully empty, and stays strictly inside scripts_out.
    dirs_to_check: set[Path] = set()
    for orphan_relpath in prior_section - current_owned:
        parent = (scripts_out / orphan_relpath).parent
        while parent != scripts_out and parent.is_relative_to(scripts_out):
            dirs_to_check.add(parent)
            parent = parent.parent

    for dir_path in sorted(dirs_to_check, key=lambda p: len(p.parts), reverse=True):
        if (
            dir_path.is_dir()
            and not dir_path.is_symlink()
            and _inside(dir_path)
            and not any(dir_path.iterdir())
        ):
            dir_path.rmdir()

    # --- PT2 + PT3: write rendered content (only if different — idempotency) ---
    # CWE-22 (Codex F1): the nested-slug scheme creates intermediate
    # directories, so a symlinked ancestor anywhere under scripts_out
    # (stale worktree / tampering) would let write_text escape the docs
    # tree.  Before creating parents or writing, reject any destination
    # whose existing ancestor chain contains a symlink and verify the
    # resolved parent stays inside scripts_out — same containment posture
    # as the orphan-sweep guard.
    for relpath, rendered_text in rendered_entries:
        dest = scripts_out / relpath
        # Walk ancestors from dest.parent up to (and excluding) scripts_out;
        # any existing symlink in that chain is a hard publish error.
        anc = dest.parent
        while anc != scripts_out and anc.is_relative_to(scripts_out):
            if anc.is_symlink():
                raise RenderError(
                    f"refusing to publish '{relpath}': ancestor directory "
                    f"{anc} is a symlink (CWE-22 — write path must stay "
                    f"inside {scripts_out})"
                )
            anc = anc.parent
        dest.parent.mkdir(parents=True, exist_ok=True)
        if not _inside(dest):
            raise RenderError(
                f"refusing to publish '{relpath}': resolved destination "
                f"{dest} escapes {scripts_out} (CWE-22)"
            )
        if (
            dest.exists()
            and not dest.is_symlink()
            and dest.read_text() == rendered_text
        ):
            # unchanged — skip to preserve mtime (PT3 idempotency)
            continue
        if dest.is_symlink():
            raise RenderError(
                f"refusing to publish '{relpath}': destination {dest} is a "
                f"symlink (CWE-22 — would write through it)"
            )
        dest.write_text(rendered_text)

    # --- PT5: update manifest section for this source root ---
    manifest[source_key] = sorted(current_owned)
    # Remove empty sections (clean up after sweeps that removed all files)
    manifest = {k: v for k, v in manifest.items() if v}
    _save_manifest(manifest_path, manifest)


def main() -> None:
    # OCX_SCRIPTS_OUT_DIR overrides the output directory when set.  Tests use
    # this to redirect all writes to a per-test tmp_path so they never pollute
    # the real website/src/_scripts/ tree.  Manifest and lock files are placed
    # inside the override directory.  Production callers (taskfile) leave this
    # env var unset, so the CLI positional args remain the source of truth.
    out_dir_override = os.environ.get("OCX_SCRIPTS_OUT_DIR")
    if out_dir_override:
        scripts_out = Path(out_dir_override)
        manifest_path = scripts_out / ".published.json"
    else:
        if len(sys.argv) != 3:
            print(  # noqa: T201
                f"Usage: {sys.argv[0]} <scripts_out_dir> <manifest_path>",
                file=sys.stderr,
            )
            sys.exit(1)
        scripts_out = Path(sys.argv[1])
        manifest_path = Path(sys.argv[2])

    # Resolve project root: this script lives at website/scripts/publish_doc_scripts.py
    _here = Path(__file__).resolve()
    project_root = _here.parent.parent.parent

    # --- fetch the discovery export via the seam (PT6) ---
    export = _fetch_export(project_root)

    # --- D2: publish-mode hard-fail on degraded seam entries ---
    #
    # The discovery seam degrades on purpose (D2 "degrade-don't-abort"):
    # parse-error → slug=None, invalid ``# state:`` → display_env={}.  That
    # tolerance is correct for the *seam* (discovery must not abort), but the
    # *publisher* must NOT inherit it: a degraded entry that would still be
    # published yields a silently wrong (or absent) doc artifact — a
    # fail-open seam.  In publish mode we re-classify those entries as fatal.
    #
    # PT1 (clean tested-only script, no ``# doc:``) must still skip silently:
    # its body has no fixture/harness var, so the guard below leaves it
    # alone.  A degraded entry that *was* meant to publish always references
    # the renderable/fixture namespace in its body, so this catches exactly
    # the fail-open cases without regressing PT1.
    fatal: list[str] = []
    for entry in export:
        slug = entry.get("slug")
        src = entry.get("path")
        display_env_raw: dict[str, str] = entry.get("display_env") or {}
        if src is None:
            continue
        degraded_signal = slug is None or not display_env_raw
        if not degraded_signal:
            continue
        try:
            body = Path(src).read_text()
        except OSError:
            # Unreadable source backing a degraded entry is itself fatal in
            # publish mode (the seam would have warned, not aborted).
            fatal.append(
                f"degraded export entry for {src!r} is unreadable "
                f"(slug={slug!r}, display_env empty={not display_env_raw})"
            )
            continue
        referenced_fixture_vars = sorted(
            {
                m.group(1) or m.group(2)
                for m in _VAR_REF_RE.finditer(body)
                if _FIXTURE_PREFIX_RE.match(m.group(1) or m.group(2))
            }
        )
        if referenced_fixture_vars:
            fatal.append(
                f"degraded doc-script export entry for {src!r} "
                f"(slug={slug!r}, display_env empty={not display_env_raw}) "
                f"references the fixture/renderable namespace "
                f"{referenced_fixture_vars} but the seam degraded it — "
                "this would publish a silently wrong or missing artifact "
                "(fail-open). Fix the script's # doc:/# state: header so the "
                "seam resolves it cleanly."
            )

    if fatal:
        for msg in fatal:
            print(f"ERROR: {msg}", file=sys.stderr)  # noqa: T201
        sys.exit(1)

    # --- filter to entries with a slug ---
    # Phase-2 extension: carry display_env and cast_region alongside each entry
    # so _publish_locked can thread them into render_display (RN1–RN7 / DE1).
    to_publish: list[tuple[str, Path, dict[str, str], tuple[int, int] | None]] = []
    for entry in export:
        slug = entry.get("slug")
        src = entry.get("path")
        if slug is None or src is None:
            continue
        display_env: dict[str, str] = entry.get("display_env") or {}
        # cast_region is a 2-element list in JSON (wire format); normalise to
        # tuple[int,int] or None.  Consumers treat it positionally (DE1 note).
        raw_region = entry.get("cast_region")
        cast_region: tuple[int, int] | None = (
            (int(raw_region[0]), int(raw_region[1])) if raw_region is not None else None
        )
        to_publish.append((slug, Path(src), display_env, cast_region))

    # --- PT4: duplicate slug detection --- write nothing if collision ---
    seen: dict[str, Path] = {}
    duplicates: list[str] = []
    for slug, src_path, _display_env, _cast_region in to_publish:
        if slug in seen:
            duplicates.append(
                f"duplicate doc slug '{slug}' ({seen[slug].name}, {src_path.name})"
            )
        else:
            seen[slug] = src_path

    if duplicates:
        for msg in duplicates:
            print(f"ERROR: {msg}", file=sys.stderr)  # noqa: T201
        sys.exit(1)

    # --- ensure output directory exists ---
    scripts_out.mkdir(parents=True, exist_ok=True)

    # The manifest is keyed by OCX_DOC_SCRIPTS_ROOT so that parallel test
    # workers using different fixture directories do not interfere.
    source_key = os.environ.get("OCX_DOC_SCRIPTS_ROOT", "")

    # Use an exclusive lock on the manifest lock file to serialise concurrent
    # publish invocations and prevent TOCTOU races on manifest reads/writes.
    # On non-POSIX platforms (win32) fcntl is unavailable; the task is
    # Linux-CI only so we skip the lock — correctness unchanged on the
    # supported platform, no import-time failure on unsupported ones.
    lock_path = manifest_path.with_suffix(".lock")
    with lock_path.open("a") as _lock_fh:
        if _HAS_FCNTL:
            fcntl.flock(_lock_fh, fcntl.LOCK_EX)  # type: ignore[name-defined]
        try:
            _publish_locked(scripts_out, manifest_path, to_publish, source_key)
        except RenderError as exc:
            print(f"ERROR: render error — {exc}", file=sys.stderr)  # noqa: T201
            sys.exit(1)
        finally:
            if _HAS_FCNTL:
                fcntl.flock(_lock_fh, fcntl.LOCK_UN)  # type: ignore[name-defined]


if __name__ == "__main__":
    main()
