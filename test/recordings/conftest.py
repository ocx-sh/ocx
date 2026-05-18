"""Fixtures for the recordings test suite.

Refactored (EQ3 — one-tree convergence, ADR H-4a) to discover cast scripts
from the single ``test/doc_scripts/`` tree.  ``collect_scripts()`` globs
``test/doc_scripts/**/*.sh`` directly, parses each header with
``src.doc_scripts.parse_doc_header``, and keeps only entries whose parsed
``DocScriptMeta.cast`` is ``True``.  This is the same one tree the publish
seam (``doc_scripts_export``) and the drift gate read; there is **no**
legacy ``recordings/scripts/`` glob and **no** second discovery path.

- Scripts are parsed with ``src.doc_scripts.parse_doc_header`` (unified
  header format: ``# state: setup:<name>`` / ``# scenario:<Name>``).
- State is resolved via ``src.state_providers.resolve_state``.
- The ``provider`` fixture replaces the legacy ``setup_env`` fixture; it
  exposes ``.display_map()`` (SP4) so ``test_recordings.py`` can build
  sanitize_map and repo_map from one source.
- Only lines inside the ``# region cast`` block are replayed (CA5); lines
  outside the region (``set -euo pipefail``, captures, assertions) are
  excluded so the cast shows only the documented commands.

**Publisher cd-hack:** The publisher setup function writes its inputs
(``build/``, ``metadata.json``, etc.) into the directory passed as
``tmp_path`` by ``SetupAdapter.provision()`` (which is ``_state/`` under the
pytest ``tmp_path``).  ``test_recordings.py`` reads ``provider.work_dir``
(SP8) to find the correct working directory and ``cd``s there before replaying
commands.  The sanitize map strips that directory from cast output.

Design contract reference: design_spec_doc_command_scripts.md §3 (SP4–SP5),
§6i (EQ1–EQ3, one-tree invariant).
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# Cast-script discovery errors recorded by collect_scripts() (Codex F2).
# Non-empty ⇒ the EQ3b orphan sweep is skipped (fail-closed: never delete a
# cast when discovery is incomplete) and recordings:build fails.
_DISCOVERY_ERRORS: list[tuple[Path, str]] = []

from src.doc_scripts import DocScriptMeta, parse_doc_header
from src.helpers import PROJECT_ROOT
from src.runner import OcxRunner
from src.state_providers import StateProvider, resolve_state

from recordings.cast_layer import _cast_path, _extract_region_lines
from recordings.cast_recorder import CastRecorder

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

_DOC_SCRIPTS_DIR = PROJECT_ROOT / "test" / "doc_scripts"

# PT8: casts output dir must NOT be hardcoded to a website/ path.
# Read from OCX_DOC_CASTS_DIR env (set by the website recordings taskfile),
# falling back to a neutral in-test/ directory so no test/ file hardcodes a
# website/ output path.
_CASTS_DIR_DEFAULT = Path(
    os.environ.get(
        "OCX_DOC_CASTS_DIR",
        str(PROJECT_ROOT / "test" / ".out" / "casts"),
    )
)
_CASTS_DIR = _CASTS_DIR_DEFAULT

# ---------------------------------------------------------------------------
# CLI options
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--cast-dir",
        default=str(_CASTS_DIR),
        help="Output directory for .cast files.",
    )


# ---------------------------------------------------------------------------
# Script parsing (new mechanism: parse_doc_header)
# ---------------------------------------------------------------------------


def _collect_script_fixture(path: Path) -> dict:
    """Parse a doc script and extract cast-region commands for recording.

    The script metadata is parsed via the unified header parser.  Only lines
    inside the ``# region cast`` block are collected for PTY replay (CA5) —
    setup lines (``set -euo pipefail``, ``cd "$SCENARIO_TMP"``, intermediate
    variable assignments) outside the region are excluded so the cast shows
    only the documented ``ocx`` commands.

    For cast scripts without a region (which should not exist in the
    converged tree but are tolerated here as a fallback), all non-comment
    non-blank body lines are collected.
    """
    meta = parse_doc_header(path)
    if meta.cast_region is not None:
        # CA5: only replay lines inside the cast region
        commands = _extract_region_lines(meta)
    else:
        # Fallback: collect all non-comment non-blank body lines
        commands = []
        in_header = True
        for raw_line in path.read_text().splitlines():
            stripped = raw_line.strip()
            if in_header:
                if not stripped or stripped.startswith("#"):
                    continue
                in_header = False
            if not stripped or stripped.startswith("#"):
                continue
            commands.append(stripped)
    return {"meta": meta, "commands": commands, "path": path}


def collect_scripts() -> list[dict]:
    """Discover cast scripts from the single ``test/doc_scripts/`` tree (EQ3).

    Returns one fixture dict per ``test/doc_scripts/`` script with
    ``cast == true``.  EQ3: there is exactly **one** script tree and no
    legacy ``recordings/scripts/`` glob — the same tree the publish seam
    (`doc_scripts_export`) and the drift gate read.  This recordings-only
    discovery globs that tree directly (it always operates on the real
    tree; the ``OCX_DOC_SCRIPTS_ROOT`` fixture override is a publish-task
    concern and does not apply to the website-build recordings step).
    """
    _DISCOVERY_ERRORS.clear()
    if not _DOC_SCRIPTS_DIR.exists():
        return []
    scripts = sorted(_DOC_SCRIPTS_DIR.glob("**/*.sh"))
    result = []
    for s in scripts:
        try:
            fixture = _collect_script_fixture(s)
            if fixture["meta"].cast:
                result.append(fixture)
        except Exception as exc:  # noqa: BLE001 — recorded, then fail-closed
            # Codex F2: do NOT silently swallow. A malformed cast script
            # that is dropped here would (a) generate no cast and (b) have
            # its previously-good cast deleted by the EQ3b orphan sweep,
            # silently shipping a broken docs page.  Record the error; the
            # orphan sweep is skipped and recordings:build fails when this
            # list is non-empty (fail-closed).
            _DISCOVERY_ERRORS.append((s, f"{type(exc).__name__}: {exc}"))
            print(
                f"ERROR: cast-script discovery failed for {s}: {exc}",
                file=sys.stderr,
            )
    return result


# ---------------------------------------------------------------------------
# Test generation
# ---------------------------------------------------------------------------


def pytest_generate_tests(metafunc: pytest.Metafunc) -> None:
    """Parametrise the ``script`` fixture from .sh files."""
    if "script" in metafunc.fixturenames:
        scripts = collect_scripts()
        ids = [s["path"].stem for s in scripts]
        metafunc.parametrize("script", scripts, ids=ids, indirect=True)


@pytest.fixture()
def script(request: pytest.FixtureRequest) -> dict:
    return request.param  # type: ignore[no-any-return]


# ---------------------------------------------------------------------------
# Recording-specific fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def cast_dir(request: pytest.FixtureRequest) -> Path:
    return Path(request.config.getoption("--cast-dir"))


@pytest.fixture()
def recorder(ocx: OcxRunner) -> CastRecorder:
    env = ocx.env.copy()
    env.setdefault("TERM", "xterm-256color")
    rec = CastRecorder(env=env)
    rec.open()
    yield rec  # type: ignore[misc]
    rec.close()


@pytest.fixture()
def provider(
    script: dict,
    ocx: OcxRunner,
    tmp_path: Path,
) -> StateProvider:
    """Resolve and provision the StateProvider for this recording script.

    Replaces the legacy ``setup_env`` fixture.  Calls
    ``provider.provision(ocx, tmp_path)`` so ``display_map()`` is populated
    before the test function runs.

    Resolves ``meta.state`` via ``resolve_state``; raises ``ValueError`` for
    unqualified or unknown states (same as the drift-gate executor, EX4).
    """
    meta: DocScriptMeta = script["meta"]
    p = resolve_state(meta.state)
    p.provision(ocx, tmp_path)
    return p


# ---------------------------------------------------------------------------
# EQ3b: cast-orphan sweep (post-session)
# ---------------------------------------------------------------------------


def sweep_orphan_casts(
    casts_dir: Path,
    scripts: list[dict],
    discovery_errors: list[tuple[Path, str]],
) -> tuple[list[Path], bool]:
    """Delete ``.cast`` files not backed by a current ``cast == true`` script.

    Pure, side-effecting-on-the-filesystem helper extracted from
    ``pytest_sessionfinish`` so the EQ3b contract is unit-testable without a
    full pytest session.  The sweep is:

    - **Manifest-scoped**: only files ending in ``.cast`` are candidates
      (foreign files and subdirectories are never touched).
    - **Idempotent**: deleting a non-existent file is silently ignored.
    - **Fail-closed**: if *discovery_errors* is non-empty the backing set is
      incomplete, so nothing is deleted and the caller is told to fail the
      run (returns ``([], False)``).

    Args:
        casts_dir: Directory holding generated ``.cast`` files.
        scripts: ``collect_scripts()`` output (one dict per backed script).
        discovery_errors: ``_DISCOVERY_ERRORS`` snapshot; non-empty ⇒ skip.

    Returns:
        ``(removed, ok)`` — ``removed`` is the list of deleted cast paths;
        ``ok`` is ``False`` when the sweep was skipped fail-closed (the
        caller must mark the session failed) and ``True`` otherwise.
    """
    if discovery_errors:
        print(
            f"\n[EQ3b] SKIPPED orphan sweep — {len(discovery_errors)} "
            f"cast-script discovery error(s); not deleting any cast on an "
            f"incomplete backing set:",
            file=sys.stderr,
        )
        for path, err in discovery_errors:
            print(f"  {path}: {err}", file=sys.stderr)
        return [], False

    if not casts_dir.exists():
        return [], True

    # Build the set of currently-backed cast paths using the SAME
    # derivation as the writer (`_cast_path`, CA2 / LDR 2026-05-17 nested
    # scheme) so the sweep and generation always agree.  Resolve to absolute
    # paths for a robust membership test against the recursive glob.
    casts_root = casts_dir.resolve()
    backed: set[Path] = set()
    for script_data in scripts:
        meta = script_data["meta"]
        backed.add(_cast_path(meta, casts_dir).resolve())

    # Remove any *.cast (at any nesting depth) not in the backed set.
    # Guard: never touch a path that resolves outside casts_dir, never
    # follow symlinks (CWE-22 defense-in-depth — slugs are SLUG_RE-validated
    # upstream, this is belt-and-suspenders).
    removed: list[Path] = []
    for cast_file in casts_dir.glob("**/*.cast"):
        if cast_file.is_symlink():
            continue
        rp = cast_file.resolve()
        if not rp.is_relative_to(casts_root):
            continue
        if rp not in backed:
            cast_file.unlink(missing_ok=True)
            removed.append(cast_file)

    # Prune now-empty slug directories the sweep created orphans in (never
    # the casts root itself, never a dir still holding foreign content).
    for d in sorted(
        {p.parent for p in removed}, key=lambda x: len(x.parts), reverse=True
    ):
        cur = d
        while (
            cur.is_relative_to(casts_root)
            and cur != casts_root
            and cur.is_dir()
            and not any(cur.iterdir())
        ):
            cur.rmdir()
            cur = cur.parent

    if removed:
        names = ", ".join(str(f.relative_to(casts_dir)) for f in removed)
        print(f"\n[EQ3b] Removed {len(removed)} orphan cast(s): {names}")
    return removed, True


def pytest_sessionfinish(session: pytest.Session, exitstatus: object) -> None:
    """Remove orphan ``.cast`` files after the recording session (EQ3b).

    After all recordings complete, any ``*.cast`` file in the casts directory
    that is NOT backed by a current ``cast == true`` script in ``doc_scripts/``
    is deleted.  Delegates to :func:`sweep_orphan_casts`; runs regardless of
    test exit status so a partial run (some tests failed) still sweeps
    orphans from a previous complete run.  Expected backing set is derived
    from ``collect_scripts()`` — the same seam used for discovery — so the
    sweep and generation always agree on which casts are current.
    """
    cast_dir_opt = session.config.getoption("--cast-dir", default=None)
    if cast_dir_opt is None:
        return
    casts_dir = Path(cast_dir_opt)

    # Codex F2 fail-closed: if discovery hit ANY error the backing set is
    # incomplete — deleting "orphans" against it could remove a cast whose
    # source script merely failed to parse, silently shipping a broken docs
    # page.  sweep_orphan_casts() returns ok=False in that case; force a
    # non-zero recordings:build so the malformed script is fixed rather than
    # silently dropped.
    _removed, ok = sweep_orphan_casts(
        casts_dir, collect_scripts(), _DISCOVERY_ERRORS
    )
    if not ok:
        session.exitstatus = pytest.ExitCode.TESTS_FAILED
