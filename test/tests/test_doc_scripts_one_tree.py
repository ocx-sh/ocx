"""One-tree invariant guard for the doc-script / recordings convergence.

These are **verify-path** structural tests (no registry I/O, no ``ocx``
binary, no opt-in marker) — collected by the standard ``tests/`` suite and
``task test:parallel`` on every platform the rest of the suite runs on.

They are the *permanent* guard for the one-tree convergence (ADR H-4a,
design_spec_doc_command_scripts.md §6i / §7.0a):

- **EQ1** — the legacy ``test/recordings/scripts/`` tree stays gone.
- **EQ2** — no slug is backed by >1 file; no ``cast: true`` script lives
  outside the single ``test/doc_scripts/`` tree.
- **EQ3** — no second discovery path: no ``recordings/scripts`` glob
  literal in the recordings conftest, the recordings test module, or the
  website recordings taskfile; recordings discovery globs the one tree.
- **EQ3b** — the cast-orphan sweep is manifest-scoped, foreign-safe, and
  fail-closed on incomplete discovery.

The transitional fold-equivalence gate (EQ-T) is **not** here — per the
ADR it is a one-shot Hat-1 safety net, not a standing equivalence tax.  Its
outcome is recorded in the worker report; only a single targeted regression
for the one slug that diverged is retained, below.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from src.doc_scripts import doc_scripts_export, parse_doc_header
from src.helpers import PROJECT_ROOT

from recordings.cast_layer import _extract_region_lines
from recordings.conftest import sweep_orphan_casts

_DOC_SCRIPTS_DIR = PROJECT_ROOT / "test" / "doc_scripts"
_LEGACY_SCRIPTS_DIR = PROJECT_ROOT / "test" / "recordings" / "scripts"


# ---------------------------------------------------------------------------
# EQ1 — the legacy recordings/scripts/ tree is gone
# ---------------------------------------------------------------------------


def test_eq1_no_legacy_recordings_scripts_tree() -> None:
    """``test/recordings/scripts/`` holds no ``*.sh`` (absent or empty)."""
    if not _LEGACY_SCRIPTS_DIR.exists():
        return
    stray = sorted(_LEGACY_SCRIPTS_DIR.glob("**/*.sh"))
    assert not stray, (
        "EQ1 violated: legacy recordings/scripts tree reappeared with "
        f"{len(stray)} .sh file(s): {[str(p) for p in stray]}"
    )


# ---------------------------------------------------------------------------
# EQ2 — one tree, no slug backed by >1 file, no cast script outside it
# ---------------------------------------------------------------------------


def test_eq2_no_slug_backed_by_more_than_one_file() -> None:
    """Every ``# doc:`` slug maps to exactly one source script."""
    entries = doc_scripts_export(_DOC_SCRIPTS_DIR)
    slug_to_paths: dict[str, list[str]] = {}
    for entry in entries:
        slug = entry["slug"]
        if slug is None:
            continue
        slug_to_paths.setdefault(slug, []).append(entry["path"])

    multi = {s: p for s, p in slug_to_paths.items() if len(p) > 1}
    assert not multi, (
        f"EQ2 violated: {len(multi)} slug(s) backed by >1 file: {multi}"
    )


def test_eq2_no_cast_script_outside_doc_scripts_tree() -> None:
    """No ``cast: true`` script lives outside ``test/doc_scripts/``.

    Walks the whole ``test/`` subtree for ``*.sh`` files whose unified
    header parses with ``cast == true`` and asserts every one is rooted in
    ``test/doc_scripts/``.  Unparseable / non-doc shell scripts elsewhere
    (scenarios, helpers) are ignored — only a *cast* script outside the one
    tree is the violation.
    """
    test_root = PROJECT_ROOT / "test"
    offenders: list[str] = []
    for sh in test_root.glob("**/*.sh"):
        if _DOC_SCRIPTS_DIR in sh.parents:
            continue
        try:
            meta = parse_doc_header(sh)
        except Exception:  # noqa: BLE001 — non-doc scripts are not our concern
            continue
        if meta.cast:
            offenders.append(str(sh))
    assert not offenders, (
        "EQ2 violated: cast:true script(s) outside test/doc_scripts/: "
        f"{offenders}"
    )


# ---------------------------------------------------------------------------
# EQ3 — single discovery path, no recordings/scripts glob literal
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "rel_path",
    [
        "test/recordings/conftest.py",
        "test/recordings/test_recordings.py",
        "website/recordings.taskfile.yml",
    ],
)
def test_eq3_no_recordings_scripts_glob_literal(rel_path: str) -> None:
    """No source file reintroduces a ``recordings/scripts`` discovery glob.

    A literal ``recordings/scripts`` substring is permitted *only* inside a
    comment/docstring that explicitly states the legacy path is removed (the
    conftest module docstring documents the convergence).  A
    glob/path-construction use of ``recordings/scripts`` is a second
    discovery path and an EQ3 violation, so we reject any occurrence that is
    not on a line also mentioning the removal ("legacy" / "removed").
    """
    path = PROJECT_ROOT / rel_path
    assert path.exists(), f"expected {rel_path} to exist"
    for lineno, line in enumerate(
        path.read_text().splitlines(), start=1
    ):
        if "recordings/scripts" not in line:
            continue
        low = line.lower()
        if "legacy" in low or "removed" in low:
            continue
        pytest.fail(
            f"EQ3 violated: {rel_path}:{lineno} references "
            f"'recordings/scripts' outside a removal note: {line.strip()!r}"
        )


def test_eq3_recordings_discovery_uses_single_doc_scripts_tree() -> None:
    """The recordings conftest discovers from the one ``test/doc_scripts/`` tree.

    ``collect_scripts()`` is the recordings discovery seam; assert it is
    rooted at ``test/doc_scripts/`` (the same tree the publish seam reads)
    and not at any ``recordings/scripts`` path.
    """
    from recordings import conftest as rec_conftest

    discovery_root = rec_conftest._DOC_SCRIPTS_DIR
    assert discovery_root == _DOC_SCRIPTS_DIR, (
        "EQ3 violated: recordings discovery root is "
        f"{discovery_root}, expected {_DOC_SCRIPTS_DIR}"
    )
    assert "recordings/scripts" not in str(discovery_root).replace(
        "\\", "/"
    ), f"EQ3 violated: discovery root points into recordings/scripts: {discovery_root}"


# ---------------------------------------------------------------------------
# EQ3b — cast-orphan sweep: manifest-scoped, foreign-safe, fail-closed
# ---------------------------------------------------------------------------


def _meta_for(slug: str, tmp: Path) -> object:
    """Minimal real ``DocScriptMeta`` whose ``# doc:`` slug is *slug*."""
    src = tmp / "src.sh"
    src.write_text(
        "#!/usr/bin/env bash\n"
        "# state: setup:basic\n"
        "# cast: true\n"
        f"# doc: {slug}\n"
        "set -euo pipefail\n"
        "# region cast\n"
        'ocx package install "$PKG_UV"\n'
        "# endregion cast\n"
    )
    return parse_doc_header(src)


def test_eq3b_sweep_removes_orphans_keeps_backed_and_foreign(
    tmp_path: Path,
) -> None:
    """Sweep deletes an orphan ``.cast``, keeps backed cast + foreign file."""
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    meta = _meta_for("getting-started/backed", tmp_path)
    # Backed cast at the slug-derived nested path the writer would use.
    backed_cast = casts_dir / "getting-started" / "backed.cast"
    backed_cast.parent.mkdir(parents=True)
    backed_cast.write_text("backed")

    orphan_cast = casts_dir / "foo.cast"
    orphan_cast.write_text("orphan")

    foreign = casts_dir / "keep.txt"
    foreign.write_text("not a cast")

    scripts = [{"meta": meta}]
    removed, ok = sweep_orphan_casts(casts_dir, scripts, [])

    assert ok is True
    assert orphan_cast not in {p for p in removed} or not orphan_cast.exists()
    assert not orphan_cast.exists(), "orphan .cast must be removed"
    assert backed_cast.exists(), "backed .cast must survive"
    assert foreign.exists(), "foreign non-.cast file must never be touched"
    assert [Path(p).name for p in removed] == ["foo.cast"]


def test_eq3b_sweep_skipped_when_discovery_errors_present(
    tmp_path: Path,
) -> None:
    """Non-empty discovery errors ⇒ sweep skipped, caller told to fail."""
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()
    orphan_cast = casts_dir / "would-be-orphan.cast"
    orphan_cast.write_text("orphan")

    discovery_errors = [(tmp_path / "broken.sh", "DocScriptParseError: boom")]
    removed, ok = sweep_orphan_casts(casts_dir, [], discovery_errors)

    assert ok is False, (
        "fail-closed: incomplete discovery must report not-ok so the "
        "caller marks the session failed"
    )
    assert removed == [], "no cast may be deleted on an incomplete backing set"
    assert orphan_cast.exists(), (
        "the (apparent) orphan must survive — its source script may merely "
        "have failed to parse"
    )


# ---------------------------------------------------------------------------
# EQ-T residue — single targeted regression for the one lossy fold slug
# ---------------------------------------------------------------------------
#
# EQ-T (the transitional fold-equivalence gate) ran once as a Hat-1 safety
# net.  Outcome: 20/21 converged slugs were lossless under the canonical
# transform (CLI taxonomy §7.2 + renderable-var render + repo-identity
# operand normalisation).  Exactly one slug diverged:
#
#   getting-started/env — converged region adds `ocx package install` before
#   `ocx package env`; the historical recordings `multi-version` SETUPS
#   fixture pre-installed corretto, the converged `setup:multi-version`
#   state does not, so the documented sequence must install first.
#
# That divergence is an *additive prerequisite*, not a dropped/reordered
# command, but it is still a real shape difference EQ-T flagged.  Per the
# user contract ("if ANY slug lossy → keep a targeted regression for that
# slug"), this single guard is retained to pin the intended converged shape
# for that slug so a future edit cannot silently drop the install step.


def test_eqt_residual_getting_started_env_region_shape() -> None:
    """`getting-started/env` region keeps its install-then-env shape.

    Pins the one slug EQ-T flagged as a non-lossless fold so the
    deliberate `ocx package install` prerequisite cannot be silently
    dropped (which would make the cast and drift gate diverge).
    """
    target = _DOC_SCRIPTS_DIR / "getting-started__env.sh"
    assert target.exists(), f"expected {target} to exist"
    meta = parse_doc_header(target)
    assert meta.doc == "getting-started/env"
    region = _extract_region_lines(meta)
    assert region == [
        'ocx package install "$PKG_CORRETTO"',
        'ocx package env "$PKG_CORRETTO"',
    ], (
        "getting-started/env region drifted from the converged "
        f"install-then-env shape EQ-T pinned: {region}"
    )
