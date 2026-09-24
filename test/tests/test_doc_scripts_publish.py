"""Publish-task contract tests (Phase 3, specification mode — all tests FAIL until Phase 3 implements).

Covers PT1–PT8 from design_spec_doc_command_scripts.md §5, plus RN1–RN7
(render-display layer, Phase 2) from §6e.

The publish mechanism (Phase 3 target):
- TEST-owned ``task test:doc-scripts:list`` emits JSON ``[{path,slug,cast,expect}]``.
- WEBSITE-owned publish task consumes that JSON and copies ``# doc:`` scripts to
  ``website/src/_scripts/<slug>.sh`` (nested, slug ``/`` = dir separator),
  maintaining ``website/src/_scripts/.published.json``.

Render layer (Phase 2 / RN1–RN7), the pure slug-to-path cases and the static
PT6b/PT7/PT8 reads live in ``test/lint/test_doc_scripts_publish_structure.py``:
they read the tree, spawn nothing, and run in the uncached lint tier.

Test hermeticity: every test that invokes the publish task sets ``OCX_SCRIPTS_OUT_DIR``
to a per-test ``tmp_path`` subdirectory so that no test ever writes into the real
``website/src/_scripts/`` tree.  PT6/PT7/PT8 are static-analysis tests that only read
existing files; they are safe without the output-dir override.

All tests that invoke ``task`` run it from ``taskfiles/``, where go-task walks up to the root Taskfile.

Skip-on-Windows: any test that shells out ``task`` or ``bash`` is guarded by the
module-level ``pytestmark`` mark — parity with ``test_scenarios_smoke.py``.

Contract reference: design_spec_doc_command_scripts.md §5 (PT1–PT8) and §6e (RN1–RN7).
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT

# ---------------------------------------------------------------------------
# Import the render-display layer under test (website-owned stdlib module).
# The ``website/`` directory is NOT on the pytest pythonpath (which is
# ``test/``), so we add it programmatically — same pattern used by any test
# that imports a non-test module from a sibling directory.
# ---------------------------------------------------------------------------

_WEBSITE_SCRIPTS_DIR: Path = PROJECT_ROOT / "website" / "scripts"
if str(_WEBSITE_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_WEBSITE_SCRIPTS_DIR))


# ---------------------------------------------------------------------------
# Module-level skip: task / bash invocations require Linux/macOS
# ---------------------------------------------------------------------------

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Publish-task tests invoke `task`/`bash`; Windows behaviour covered by the pytest suite.",
)

# ---------------------------------------------------------------------------
# Paths (constants for static-check tests only — dynamic tests use tmp_path)
# ---------------------------------------------------------------------------

# _REAL_SCRIPTS_OUT moved with PT6-PT8 to test/lint/test_doc_scripts_publish_structure.py.
"""Real production target — used ONLY by static-check tests (PT6/PT7/PT8)."""

_WEBSITE_TASKFILE: Path = PROJECT_ROOT / "website" / "taskfile.yml"
"""Website taskfile parsed for PT7 ordering assertions."""

_TEST_TASKFILE: Path = PROJECT_ROOT / "test" / "taskfile.yml"
"""Test taskfile checked for ``test:doc-scripts:list`` task (PT6)."""

_DOC_SCRIPTS_ROOT: Path = PROJECT_ROOT / "test" / "doc_scripts"
"""Root directory for fixture doc scripts."""

pytestmark = [
    pytestmark,
    pytest.mark.command(
        "install",
        "exec",
        "which",
        "env",
        "select",
        "deselect",
        "uninstall",
        "deps",
        "package_create",
        "package_push",
        "package_pull",
        "package_test",
        "toolchain_env",
        "toolchain_exec",
        "add",
        "init",
        "pull",
        "lock",
        "shell_state",
        "patch_test",
        "index_list",
        "index_update",
        "self_group/activate",
    ),
]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_doc_script(
    directory: Path,
    name: str,
    content: str,
    *,
    executable: bool = True,
) -> Path:
    """Write a fixture ``.sh`` file and return its path."""
    directory.mkdir(parents=True, exist_ok=True)
    p = directory / name
    p.write_text(textwrap.dedent(content))
    if executable:
        p.chmod(0o755)
    return p


def _run_publish(
    *,
    scripts_out: Path,
    doc_scripts_root: Path | None = None,
    extra_env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Invoke the website publish task with a hermetic per-test output dir.

    ``scripts_out`` is a per-test ``tmp_path`` subdirectory.  The task writes
    ALL output there via ``OCX_SCRIPTS_OUT_DIR``, never touching the real
    ``website/src/_scripts/`` tree.

    ``doc_scripts_root`` overrides the fixture discovery root via
    ``OCX_DOC_SCRIPTS_ROOT``.
    """
    env = os.environ.copy()
    env["OCX_SCRIPTS_OUT_DIR"] = str(scripts_out)
    if doc_scripts_root is not None:
        env["OCX_DOC_SCRIPTS_ROOT"] = str(doc_scripts_root)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        ["task", "website:scripts:publish"],
        cwd=str(PROJECT_ROOT / "taskfiles"),
        env=env,
        capture_output=True,
        text=True,
        check=check,
    )


def _run_list_export(
    *,
    doc_scripts_root: Path | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Invoke ``task test:doc-scripts:list`` and return the result."""
    cmd = ["task", "test:doc-scripts:list"]
    env = os.environ.copy()
    if doc_scripts_root is not None:
        env["OCX_DOC_SCRIPTS_ROOT"] = str(doc_scripts_root)
    return subprocess.run(
        cmd,
        cwd=str(PROJECT_ROOT / "taskfiles"),
        env=env,
        capture_output=True,
        text=True,
        check=check,
    )


# ===========================================================================
# PT1 — script with no ``# doc:`` is NOT copied
# ===========================================================================


def test_pt1_no_doc_header_script_not_published(tmp_path: Path) -> None:
    """PT1: a script lacking ``# doc:`` is not copied to the output directory.

    The publish task must skip tested-only scripts.  After publish, no file
    under the per-test out dir corresponds to the fixture script.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "tested_only.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # title: Tested-only, no doc slug
        echo hello
        """,
    )

    result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    # Whether the task succeeds or not (it may not exist yet), the fixture
    # must not appear in the out dir.
    if scripts_out.exists():
        published = list(scripts_out.glob("*.sh"))
        assert not any("tested_only" in p.name for p in published), (
            "PT1: tested_only.sh (no # doc:) must not be published; "
            f"found in out dir: {published}"
        )
    else:
        # out dir does not exist → task not implemented → test fails
        # for the right reason (missing task/directory).
        pytest.fail(
            "PT1: task website:scripts:publish does not exist or "
            f"output directory was not created (rc={result.returncode}, "
            f"stdout={result.stdout!r}, stderr={result.stderr!r})"
        )


# PT2 flat-`__` test removed by LDR 2026-05-17 (nested-path scheme).
# Replaced by test_pt2_nested_* (nested slug-dir behaviour).


# ===========================================================================
# PT3 — idempotent: second run makes no writes
# ===========================================================================


def _snapshot_dir(directory: Path) -> dict[str, str]:
    """Return a deterministic snapshot of ``directory`` as ``{filename: sha256}``.

    Only regular files (no symlinks, no subdirs) are included.  Used to detect
    any write made by a second publish run without relying on mtime or sleep.
    """
    snapshot: dict[str, str] = {}
    if not directory.exists():
        return snapshot
    for p in sorted(directory.rglob("*")):
        if p.is_file() and not p.is_symlink():
            digest = hashlib.sha256(p.read_bytes()).hexdigest()
            snapshot[str(p.relative_to(directory))] = digest
    return snapshot


def test_pt3_second_run_is_idempotent(tmp_path: Path) -> None:
    """PT3: rerunning the publish task with unchanged inputs makes no writes.

    Idempotency check: compare the full set of (file, content-hash, manifest
    bytes) before and after a second run.  A deterministic equality check —
    no sleep, no mtime dependency.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "idempotent.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: idempotent/test
        echo hello
        """,
    )

    expected = scripts_out / "idempotent" / "test.sh"  # nested (LDR 2026-05-17)

    # First run — must create the published file.
    first_result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    if not expected.exists():
        pytest.fail(
            "PT3: first publish run did not create idempotent/test.sh. "
            f"task rc={first_result.returncode}, stderr={first_result.stderr!r}"
        )

    # Capture full snapshot after first run: file→sha256 + manifest bytes.
    snapshot_after_first = _snapshot_dir(scripts_out)
    manifest_path = scripts_out / ".published.json"
    manifest_after_first = manifest_path.read_bytes() if manifest_path.exists() else b""

    # Second run — must be a no-op (identical inputs → no writes).
    _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    snapshot_after_second = _snapshot_dir(scripts_out)
    manifest_after_second = manifest_path.read_bytes() if manifest_path.exists() else b""

    assert snapshot_after_second == snapshot_after_first, (
        "PT3: second run changed file content or set — task is not idempotent.\n"
        f"Diff (first vs second): "
        f"added={set(snapshot_after_second) - set(snapshot_after_first)}, "
        f"removed={set(snapshot_after_first) - set(snapshot_after_second)}, "
        f"changed={{k for k in snapshot_after_first if snapshot_after_second.get(k) != snapshot_after_first[k]}}"
    )
    assert manifest_after_second == manifest_after_first, (
        "PT3: second run rewrote the manifest — task is not idempotent."
    )


# ===========================================================================
# PT4 — duplicate ``# doc:`` slug ⇒ task fails, no writes
# ===========================================================================


def test_pt4_duplicate_slug_fails_loudly_and_writes_nothing(tmp_path: Path) -> None:
    """PT4: two scripts sharing the same ``# doc:`` slug ⇒ task fails.

    The failure message must contain ``duplicate doc slug`` and both filenames.
    No file must be written to the output dir.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "alpha.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: shared/slug
        echo alpha
        """,
    )
    _write_doc_script(
        doc_scripts_dir,
        "beta.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: shared/slug
        echo beta
        """,
    )

    # Snapshot out dir before publish (may not exist yet)
    scripts_before: set[str] = (
        {p.name for p in scripts_out.glob("*")} if scripts_out.exists() else set()
    )

    result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    assert result.returncode != 0, (
        "PT4: publish task must fail on duplicate slug; exited 0 instead. "
        f"stdout={result.stdout!r}"
    )

    combined = result.stdout + result.stderr
    # The production message shape is:
    #   ERROR: duplicate doc slug '<slug>' (<first_file>, <second_file>)
    # Require the exact phrase (not a bare "duplicate" fallback) so typos in
    # the message string are caught here rather than silently accepted.
    assert "duplicate doc slug" in combined, (
        f"PT4: error output must contain exact phrase 'duplicate doc slug'; got:\n{combined}"
    )

    # Both exact filenames must appear — drop the bare-stem fallbacks so a
    # message that mentions only 'alpha' (without '.sh') is correctly flagged.
    assert "alpha.sh" in combined, (
        f"PT4: 'alpha.sh' not mentioned in error; got:\n{combined}"
    )
    assert "beta.sh" in combined, (
        f"PT4: 'beta.sh' not mentioned in error; got:\n{combined}"
    )

    # No writes: out dir content unchanged
    scripts_after: set[str] = (
        {p.name for p in scripts_out.glob("*")} if scripts_out.exists() else set()
    )
    new_files = scripts_after - scripts_before
    assert not new_files, (
        f"PT4: publish wrote files despite duplicate slug: {new_files}"
    )


# ===========================================================================
# PT5 — manifest-scoped orphan sweep
# ===========================================================================


def test_pt5_orphan_sweep_manifest_scoped(tmp_path: Path) -> None:
    """PT5: manifest-scoped orphan sweep.

    Setup:
    - Publish a script with ``# doc: real/slug`` → creates ``real/slug.sh`` (nested).
    - Place a foreign ``keep.txt`` and a foreign ``other.sh`` (not in manifest)
      in the out dir.
    - Remove the doc script from the source set (simulate slug removal).
    - Re-run publish.

    Expected:
    - ``real/slug.sh`` (previously owned, now orphaned) is deleted; the now-empty
      owned ``real/`` dir is pruned.
    - ``keep.txt`` survives (not in manifest — PT5 contract: non-.sh files untouched).
    - ``other.sh`` survives (not in manifest — foreign .sh untouched by sweep).
    - Subdirectories in the out dir are untouched.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    # --- First run: publish real__slug.sh ---
    _write_doc_script(
        doc_scripts_dir,
        "real_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: real/slug
        echo hello
        """,
    )

    first_result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    if not scripts_out.exists():
        pytest.fail(
            "PT5: out dir not created after first publish. "
            f"rc={first_result.returncode}, stderr={first_result.stderr!r}"
        )

    orphan_candidate = scripts_out / "real" / "slug.sh"  # nested (LDR 2026-05-17)
    if not orphan_candidate.exists():
        pytest.fail(
            f"PT5: expected real/slug.sh to be published; not found. "
            f"Contents of out dir: {list(scripts_out.glob('*'))}"
        )

    # --- Place foreign files in the per-test out dir ---
    keep_txt = scripts_out / "keep.txt"
    other_sh = scripts_out / "other.sh"
    sub_dir = scripts_out / "subdir"
    keep_txt.write_text("foreign non-sh file — must survive sweep")
    other_sh.write_text("#!/usr/bin/env bash\n# not in manifest\necho foreign\n")
    sub_dir.mkdir(exist_ok=True)
    (sub_dir / "nested.sh").write_text("# nested\n")

    # --- Remove the source script (orphan the slug) ---
    real_slug_source = doc_scripts_dir / "real_slug.sh"
    real_slug_source.unlink()

    # --- Second run: empty doc_scripts_dir → orphan sweep ---
    second_result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    # real__slug.sh must be gone (was task-owned, now orphaned)
    assert not orphan_candidate.exists(), (
        f"PT5: real__slug.sh should have been removed as an orphan; still exists. "
        f"rc={second_result.returncode}, stderr={second_result.stderr!r}"
    )

    # Foreign files must survive
    assert keep_txt.exists(), (
        "PT5: keep.txt (not in manifest) must survive the orphan sweep"
    )
    assert other_sh.exists(), (
        "PT5: other.sh (not in manifest) must survive the orphan sweep — "
        "sweep is manifest-scoped, not glob-scoped"
    )

    # Subdirectory untouched
    assert sub_dir.exists(), (
        "PT5: subdirectory in out dir must not be deleted by orphan sweep"
    )


# ===========================================================================
# PT6 — discovery seam: task test:doc-scripts:list exists + no test/ literal
# ===========================================================================


def test_pt6_list_task_exists_and_emits_valid_json(tmp_path: Path) -> None:
    """PT6a: ``task test:doc-scripts:list`` exists and emits valid JSON.

    The exported schema is ``[{path, slug, cast, expect}]`` (one entry per
    discovered ``.sh`` file).  An empty root ⇒ ``[]``.
    """
    doc_scripts_dir = tmp_path / "doc_scripts_list"

    # One script with # doc:, one without
    _write_doc_script(
        doc_scripts_dir,
        "has_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: pt6/test
        echo hello
        """,
    )
    _write_doc_script(
        doc_scripts_dir,
        "no_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        echo hello
        """,
    )

    result = _run_list_export(
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    assert result.returncode == 0, (
        f"PT6: task test:doc-scripts:list failed (rc={result.returncode}). "
        f"stderr={result.stderr!r}\nDoes the task exist in test/taskfile.yml?"
    )

    # stdout must be parseable JSON
    try:
        export = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"PT6: task test:doc-scripts:list output is not valid JSON: {exc!r}. "
            f"stdout={result.stdout!r}"
        )

    assert isinstance(export, list), (
        f"PT6: export must be a list, got {type(export).__name__!r}"
    )

    # Each entry must have path, slug, cast, expect
    required_keys = {"path", "slug", "cast", "expect"}
    for entry in export:
        missing = required_keys - set(entry.keys())
        assert not missing, (
            f"PT6: export entry missing keys {missing}; entry={entry!r}"
        )

    # Entry for has_slug.sh must have slug="pt6/test"
    slugged = [e for e in export if Path(e["path"]).name == "has_slug.sh"]
    assert len(slugged) == 1, (
        f"PT6: expected one entry for has_slug.sh; got {slugged!r}"
    )
    assert slugged[0]["slug"] == "pt6/test", (
        f"PT6: expected slug='pt6/test' for has_slug.sh; got {slugged[0]['slug']!r}"
    )

    # Entry for no_slug.sh must have slug=null
    no_slug = [e for e in export if Path(e["path"]).name == "no_slug.sh"]
    assert len(no_slug) == 1, (
        f"PT6: expected one entry for no_slug.sh; got {no_slug!r}"
    )
    assert no_slug[0]["slug"] is None, (
        f"PT6: expected slug=null for no_slug.sh; got {no_slug[0]['slug']!r}"
    )


# ===========================================================================
# PT2 (nested) — ``# doc: a/b-c`` ⇒ nested ``a/b-c.sh``, mkdir -p
# ===========================================================================


def test_pt2_nested_publish_writes_nested_dir(tmp_path: Path) -> None:
    """PT2 (nested): publishing ``# doc: getting-started/install`` writes
    ``<out>/_scripts/getting-started/install.sh`` (nested directory created
    ``mkdir -p``), NOT ``getting-started__install.sh``.

    Contract (LDR 2026-05-17): the publish task must create parent dirs
    and write the file at the nested path.  The old flat ``__``-separated
    filename must NOT appear.

    This test invokes the website publish task end-to-end via
    ``task website:scripts:publish`` with ``OCX_SCRIPTS_OUT_DIR`` override so
    nothing writes to the real tree.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "nested_install.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: getting-started/install
        # title: Nested publish test
        echo hello
        """,
    )

    result = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    # The nested file must exist.
    expected_nested = scripts_out / "getting-started" / "install.sh"
    # The old flat file must NOT exist.
    unexpected_flat = scripts_out / "getting-started__install.sh"

    if not scripts_out.exists():
        pytest.fail(
            "PT2/nested: output directory not created after publish. "
            f"rc={result.returncode}, stderr={result.stderr!r}"
        )

    assert expected_nested.exists(), (
        f"PT2/nested: expected nested file {expected_nested.relative_to(scripts_out)} "
        f"was not written. "
        f"Files in out dir: {[str(p.relative_to(scripts_out)) for p in scripts_out.rglob('*') if p.is_file()]}"
    )
    assert not unexpected_flat.exists(), (
        "PT2/nested: old flat-underscore form getting-started__install.sh "
        "must NOT be written (flattening is removed, LDR 2026-05-17)"
    )


# ===========================================================================
# PT5 (nested orphan + empty-dir prune)
# ===========================================================================


def test_pt5_nested_orphan_sweep_prunes_empty_slug_dir(tmp_path: Path) -> None:
    """PT5 (nested): orphan sweep removes an empty slug directory it owns.

    Setup:
    - Publish ``# doc: nested/slug`` → writes ``nested/slug.sh``.
    - Remove the source script (orphan the slug).
    - Re-run with empty doc_scripts_dir.

    Expected:
    - ``nested/slug.sh`` is deleted (was task-owned, now orphaned).
    - The now-empty ``nested/`` directory is pruned (owned dir with no
      remaining files).

    Contract (PT5 LDR 2026-05-17): the orphan sweep must also prune slug
    directories that the task owns and that became fully empty.
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "nested_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: nested/slug
        echo hello
        """,
    )

    first = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    nested_file = scripts_out / "nested" / "slug.sh"
    nested_dir = scripts_out / "nested"

    if not nested_file.exists():
        pytest.fail(
            "PT5/nested: expected nested/slug.sh after first publish; not found. "
            f"rc={first.returncode}, stderr={first.stderr!r}, "
            f"files={[str(p.relative_to(scripts_out)) for p in scripts_out.rglob('*') if p.is_file()]}"
        )

    # Remove the source script to orphan the slug.
    (doc_scripts_dir / "nested_slug.sh").unlink()

    second = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    assert not nested_file.exists(), (
        "PT5/nested: nested/slug.sh should have been removed as an orphan; "
        f"still exists. rc={second.returncode}, stderr={second.stderr!r}"
    )
    assert not nested_dir.exists(), (
        "PT5/nested: the now-empty 'nested/' slug directory should have been "
        "pruned (owned dir, fully empty after orphan sweep). "
        f"rc={second.returncode}"
    )


def test_pt5_nested_foreign_file_in_owned_dir_survives(tmp_path: Path) -> None:
    """PT5 (nested): a foreign file inside an owned slug dir survives sweep.

    Contract: the orphan sweep must NOT delete a directory that still
    contains foreign (non-manifest) files, even if all manifest-owned files
    inside it were removed.

    Setup:
    - Publish ``# doc: mygroup/alpha`` → writes ``mygroup/alpha.sh``.
    - Place a foreign ``mygroup/foreign.txt`` (not in manifest).
    - Remove ``mygroup/alpha.sh`` source → orphan alpha.

    Expected after republishing:
    - ``mygroup/alpha.sh`` deleted (owned orphan).
    - ``mygroup/foreign.txt`` survives (foreign content).
    - ``mygroup/`` directory survives (still has foreign content).
    """
    doc_scripts_dir = tmp_path / "doc_scripts"
    scripts_out = tmp_path / "_scripts"

    _write_doc_script(
        doc_scripts_dir,
        "alpha_script.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: mygroup/alpha
        echo alpha
        """,
    )

    first = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    owned_file = scripts_out / "mygroup" / "alpha.sh"
    if not owned_file.exists():
        pytest.fail(
            "PT5/nested/foreign: expected mygroup/alpha.sh after first publish; "
            f"not found. rc={first.returncode}, stderr={first.stderr!r}"
        )

    # Inject a foreign file into the owned directory.
    foreign_file = scripts_out / "mygroup" / "foreign.txt"
    foreign_file.write_text("I am a foreign file — must survive sweep\n")

    # Orphan the alpha slug.
    (doc_scripts_dir / "alpha_script.sh").unlink()

    second = _run_publish(
        scripts_out=scripts_out,
        doc_scripts_root=doc_scripts_dir,
        check=False,
    )

    assert not owned_file.exists(), (
        "PT5/nested/foreign: mygroup/alpha.sh (owned orphan) must be removed; "
        f"rc={second.returncode}, stderr={second.stderr!r}"
    )
    assert foreign_file.exists(), (
        "PT5/nested/foreign: mygroup/foreign.txt (foreign, not in manifest) "
        "must survive the orphan sweep"
    )
    assert (scripts_out / "mygroup").exists(), (
        "PT5/nested/foreign: mygroup/ directory must NOT be pruned because it "
        "still contains foreign content"
    )


