"""Publish-task contract tests (Phase 3, specification mode — all tests FAIL until Phase 3 implements).

Covers PT1–PT8 from design_spec_doc_command_scripts.md §5, plus RN1–RN7
(render-display layer, Phase 2) from §6e.

The publish mechanism (Phase 3 target):
- TEST-owned ``task test:doc-scripts:list`` emits JSON ``[{path,slug,cast,expect}]``.
- WEBSITE-owned publish task consumes that JSON and copies ``# doc:`` scripts to
  ``website/src/_scripts/<slug>.sh`` (nested, slug ``/`` = dir separator),
  maintaining ``website/src/_scripts/.published.json``.

Render layer (Phase 2 / RN1–RN7):
- ``render_display(script_text, *, cast_region, display_env, slug) -> str``
  is the pure deterministic function that produces the display artifact.
- ``_slug_to_relpath(slug) -> str`` converts ``a/b-c`` to ``a/b-c.sh``
  (nested path, no flattening).

Test hermeticity: every test that invokes the publish task sets ``OCX_SCRIPTS_OUT_DIR``
to a per-test ``tmp_path`` subdirectory so that no test ever writes into the real
``website/src/_scripts/`` tree.  PT6/PT7/PT8 are static-analysis tests that only read
existing files; they are safe without the output-dir override.

All tests that invoke ``task`` use ``subprocess.run`` with ``cwd=PROJECT_ROOT``.

Skip-on-Windows: any test that shells out ``task`` or ``bash`` is guarded by the
module-level ``pytestmark`` mark — parity with ``test_scenarios_smoke.py``.

Contract reference: design_spec_doc_command_scripts.md §5 (PT1–PT8) and §6e (RN1–RN7).
"""
from __future__ import annotations

import hashlib
import json
import os
import re
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

# The import below triggers the stub bodies (NotImplementedError); the module
# itself must be importable so tests can call the functions.
from publish_doc_scripts import (  # noqa: E402
    RenderError,
    _slug_to_relpath,
    _substitute_renderable,
    render_display,
)

from src.doc_scripts import substitute_renderable  # noqa: E402

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

_REAL_SCRIPTS_OUT: Path = PROJECT_ROOT / "website" / "src" / "_scripts"
"""Real production target — used ONLY by static-check tests (PT6/PT7/PT8)."""

_WEBSITE_TASKFILE: Path = PROJECT_ROOT / "website" / "taskfile.yml"
"""Website taskfile parsed for PT7 ordering assertions."""

_TEST_TASKFILE: Path = PROJECT_ROOT / "test" / "taskfile.yml"
"""Test taskfile checked for ``test:doc-scripts:list`` task (PT6)."""

_DOC_SCRIPTS_ROOT: Path = PROJECT_ROOT / "test" / "doc_scripts"
"""Root directory for fixture doc scripts."""


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
        cwd=str(PROJECT_ROOT),
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
        cwd=str(PROJECT_ROOT),
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


# ===========================================================================
# PT2 — ``# doc: a/b-c`` ⇒ ``a__b-c.sh``
# ===========================================================================


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
    """PT3: re-running the publish task with unchanged inputs makes no writes.

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


def test_pt6_no_test_path_literal_in_website_files() -> None:
    """PT6b: no ``website/`` file hardcodes a ``test/`` discovery-path literal.

    The publish task must discover scripts exclusively via the JSON export seam
    (``task test:doc-scripts:list``).  Hardcoding ``test/`` paths in website
    taskfiles or source files would couple the website subsystem to the test
    tree (tenet separation PT6 contract).

    Grep every file under ``website/`` for ``test/`` path literals that look
    like discovery references (e.g. ``test/doc_scripts/``, ``../test/``).
    The ``test/bin/`` reference in ``recordings.taskfile.yml`` is allowed
    (binary path, not discovery).
    """
    website_dir = PROJECT_ROOT / "website"

    # Pattern: a "test/" reference that looks like a discovery path (doc_scripts,
    # or "../test/", not the binary bin/ path)
    # We allow "test/bin/" (the OCX binary path used by recordings taskfile).
    _discovery_path_re = re.compile(
        r"""(
            \.\./test/          # relative parent-reference
          | test/doc_scripts    # explicit doc_scripts path
          | test/src            # test/src module path
          | test/tests          # test/tests path
          | test/recordings     # test/recordings path
        )""",
        re.VERBOSE,
    )

    violations: list[str] = []
    for p in sorted(website_dir.rglob("*")):
        if not p.is_file():
            continue
        # Skip node_modules (third-party JS packages — not website source)
        if "node_modules" in p.parts:
            continue
        # Skip binary / lock / generated files
        if p.suffix in (".lock", ".cast", ".json", ".png", ".svg", ".ico", ".webmanifest"):
            continue
        try:
            text = p.read_text(errors="replace")
        except OSError:
            continue

        for lineno, line in enumerate(text.splitlines(), start=1):
            # Skip comments that are clearly not task commands
            stripped = line.strip()
            if stripped.startswith("#") and not stripped.startswith("# !"):
                continue
            if _discovery_path_re.search(line):
                violations.append(f"{p.relative_to(PROJECT_ROOT)}:{lineno}: {line.rstrip()}")

    assert violations == [], (
        "PT6b: website/ files must not hardcode test/ discovery paths. "
        "Violations:\n" + "\n".join(violations)
    )


def test_pt6_publish_task_consumes_list_export() -> None:
    """PT6c: the publish task shells out to ``task test:doc-scripts:list``.

    Asserts that some file under ``website/`` references the
    ``test:doc-scripts:list`` task (i.e. the seam is wired, not bypassed).
    This is a static text-search test — it does NOT run the task.
    """
    website_dir = PROJECT_ROOT / "website"
    pattern = re.compile(r"test:doc-scripts:list|doc-scripts:list|doc_scripts_export")

    references: list[str] = []
    for p in sorted(website_dir.rglob("*")):
        if not p.is_file():
            continue
        if p.suffix in (".lock", ".cast", ".png", ".svg", ".ico", ".webmanifest"):
            continue
        try:
            text = p.read_text(errors="replace")
        except OSError:
            continue
        if pattern.search(text):
            references.append(str(p.relative_to(PROJECT_ROOT)))

    assert references, (
        "PT6c: no file under website/ references 'test:doc-scripts:list' or "
        "'doc_scripts_export'. The publish task must consume the JSON export seam. "
        "Add a website taskfile that shells out to `task test:doc-scripts:list`."
    )


# ===========================================================================
# PT7 — ordering: publish before vitepress build, independent of recordings
# ===========================================================================


def test_pt7_publish_before_vitepress_independent_of_recordings() -> None:
    """PT7: parse ``website/taskfile.yml`` and assert ordering invariants.

    - The publish task appears in ``build.cmds`` BEFORE ``bunx vitepress build``.
    - The publish task is NOT listed as a ``deps:`` dependency of
      ``recordings:parallel`` (they are independent output sets).

    Parsed with stdlib only (no PyYAML dependency).  The assertion is a
    line-order check on the raw YAML text — sufficient because go-task
    executes ``cmds:`` in order.
    """
    taskfile_text = _WEBSITE_TASKFILE.read_text()
    lines = taskfile_text.splitlines()

    # Locate the build task's cmds section
    # Strategy: find "  build:" heading, then scan forward for cmds lines
    # until the next top-level task (un-indented non-blank line that isn't
    # a sub-key).

    build_section_start: int | None = None
    for i, line in enumerate(lines):
        # Top-level task declaration: "  build:" (2-space indent in this file)
        if re.match(r"^\s{2}build:\s*$", line):
            build_section_start = i
            break

    assert build_section_start is not None, (
        "PT7: could not find 'build:' task in website/taskfile.yml. "
        "Has the task been renamed?"
    )

    # Collect cmds lines from the build section
    build_cmds: list[tuple[int, str]] = []
    in_cmds = False
    for i in range(build_section_start + 1, len(lines)):
        line = lines[i]
        # Detect "  cmds:" at 4-space indent (under build task)
        if re.match(r"^\s{4}cmds:\s*$", line):
            in_cmds = True
            continue
        # Exit cmds section on a new same-or-higher level key
        if in_cmds:
            if re.match(r"^\s{4}[a-z]", line):
                # new key at same level (4-space) → cmds section over
                break
            if re.match(r"^\s{2}[a-z]", line):
                # new top-level task → done
                break
            # Collect cmd entries (6-space or deeper)
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                build_cmds.append((i, stripped))

    assert build_cmds, (
        "PT7: no cmds found in 'build' task of website/taskfile.yml. "
        "The publish task must be listed in build.cmds before 'bunx vitepress build'."
    )

    cmd_texts = [text for _, text in build_cmds]
    cmd_indices = {text: idx for idx, (lineno, text) in enumerate(build_cmds)}

    # Find the vitepress build command
    vitepress_idx: int | None = None
    for idx, text in enumerate(cmd_texts):
        if "vitepress build" in text:
            vitepress_idx = idx
            break

    assert vitepress_idx is not None, (
        "PT7: 'bunx vitepress build' not found in build.cmds of website/taskfile.yml"
    )

    # Find the publish task invocation
    publish_idx: int | None = None
    for idx, text in enumerate(cmd_texts):
        if "scripts:publish" in text or "publish" in text.lower():
            publish_idx = idx
            break

    assert publish_idx is not None, (
        "PT7: publish task not found in build.cmds of website/taskfile.yml. "
        "It must appear before 'bunx vitepress build'. "
        f"Current cmds:\n" + "\n".join(f"  [{i}] {t}" for i, t in enumerate(cmd_texts))
    )

    assert publish_idx < vitepress_idx, (
        f"PT7: publish task (index {publish_idx}) must appear before "
        f"'bunx vitepress build' (index {vitepress_idx}) in build.cmds. "
        f"Current order:\n" + "\n".join(f"  [{i}] {t}" for i, t in enumerate(cmd_texts))
    )

    # Assert publish is NOT a dependency of recordings:parallel
    # Find recordings: section and check its deps/cmds
    recordings_section: list[str] = []
    in_recordings = False
    for line in lines:
        if re.match(r"^\s{2}recordings:", line) or "recordings:parallel" in line:
            in_recordings = True
        if in_recordings:
            if re.match(r"^\s{2}[a-z]", line) and "recordings" not in line:
                break
            recordings_section.append(line)

    recordings_text = "\n".join(recordings_section)
    assert "scripts:publish" not in recordings_text and "publish" not in recordings_text.lower() or (
        # Allow "publish" in unrelated comments but not as a task reference
        not re.search(r"task.*publish|publish.*task|scripts:publish", recordings_text)
    ), (
        "PT7: publish task must NOT be a dependency of recordings:parallel — "
        "they are independent output sets (_scripts/*.sh vs casts/*.cast). "
        f"recordings section:\n{recordings_text}"
    )


# ===========================================================================
# PT8 — reverse leak removed: no ``website/`` path in ``test/``
# ===========================================================================


def test_pt8_no_website_path_literal_in_test_files() -> None:
    """PT8: no ``test/`` file hardcodes a ``website/`` **output** path.

    Today ``test/recordings/conftest.py:19`` hardcodes
    ``PROJECT_ROOT / "website" / "src" / "public" / "casts"`` — this
    test MUST FAIL until that is fixed (the casts output dir must be
    parameterized via env/arg, defaulting via the website seam).

    Scope of the check: output paths (paths where the test system WRITES
    generated content into the website tree).  Input paths (paths where the
    test system READS website source to check for NC1–NC3, command-line ref,
    etc.) are legitimate cross-references declared in the design spec and are
    excluded by ``_OUTPUT_PATH_SEGMENTS``.

    Specifically excluded from this scan:
    - Paths under ``website/src/docs/`` (walkthrough pages read for NC1–NC3
      checks, command-line reference checks, project-toolchain tests).
    - Paths under ``website/src/public/schemas/`` (schema generation tests that
      read the generated file at a known location).
    - Paths to ``website/src/public/install.sh`` (install-script acceptance test).
    - Paths under ``website/src/_scripts/`` when used as a read-only manifest
      reference (not as an output write target) — covered by the doc_binding
      NC2 export seam separately.
    - This file itself (test_doc_scripts_publish.py) — test infrastructure
      references the website layout by necessity.

    The check DOES catch:
    - ``website/src/public/casts/`` — the pre-existing casts output dir leak
      (``test/recordings/conftest.py:19`` must be parameterized — PT8's
      primary target per plan §3 Decisions D1).
    - Any other path under ``website/`` that the test system uses as a
      **write target** (e.g. writing generated assets into the website tree).

    Design gap: the design spec says "grep test/ for website/ path literals,
    assert none" (broad).  After discussion this test is scoped to output-path
    coupling only — doc_binding.py's input references to walkthrough pages are
    deliberately allowed (NC2 requires knowing which pages to check).  If the
    broader spec intent is "absolutely none", the tester flags this ambiguity
    here; the spec's described *motivation* is the casts-path leak, which this
    test does catch.
    """
    test_dir = PROJECT_ROOT / "test"

    # Pattern: detect Path constructs that hardcode the casts output directory
    # (or any generated output path) into the website tree.  Two forms:
    #
    # Form A — inline string:   "website/src/public/casts"
    # Form B — Path division:   PROJECT_ROOT / "website" / "src" / "public" / "casts"
    #
    # The pre-existing violation is Form B in test/recordings/conftest.py:19.
    # We detect it by looking for the specific segment sequence that encodes a
    # WRITE target (casts/) rather than a read-only source reference.
    #
    # Allowed patterns that reference website/ as a READ-ONLY source:
    #   - website/src/docs/…        (NC1–NC3, command-line ref, project tests)
    #   - website/src/public/schemas/… (schema generation tests)
    #   - website/src/public/install… (install-script test)
    #   - website/src/_scripts/…   (manifest read for NC2)
    #
    # Not allowed:
    #   - website/src/public/casts  (output dir — must be parameterized)

    # Detect the casts output path in any form
    _casts_output_re = re.compile(
        r"""(
            ["']website/src/public/casts   # inline string literal
          | /\s*["']casts["']              # Path / "casts" division
        )""",
        re.VERBOSE,
    )

    # Also detect any website/ reference that is NOT in the allowed read set.
    # Build an allowlist regex; if the line matches allowlist it is skipped.
    _allowed_website_re = re.compile(
        r"""(
            website/src/docs/
          | website/src/public/schemas/
          | website/src/public/install
          | website/src/_scripts
          | website/taskfile\.yml          # taskfile reference by name
          | ["']website[/"']              # bare segment (no sub-path) — checked by casts_re instead
        )""",
        re.VERBOSE,
    )

    _any_website_re = re.compile(r'website')

    violations: list[str] = []
    own_filename = Path(__file__).name

    for p in sorted(test_dir.rglob("*.py")):
        if p.name == own_filename:
            # Skip this specification file — it must reference the layout
            # to describe the contract.
            continue
        try:
            text = p.read_text()
        except OSError:
            continue

        for lineno, line in enumerate(text.splitlines(), start=1):
            if not _any_website_re.search(line):
                continue
            # Skip lines that reference the casts path through an allowed pattern
            # (i.e. only the specific casts output path is flagged)
            if _casts_output_re.search(line):
                violations.append(
                    f"{p.relative_to(PROJECT_ROOT)}:{lineno}: {line.rstrip()}"
                )

    assert violations == [], (
        "PT8: test/ files must not hardcode website/ output paths. "
        "The primary pre-existing leak is test/recordings/conftest.py:19 "
        "(_CASTS_DIR = PROJECT_ROOT / 'website' / 'src' / 'public' / 'casts'). "
        "Fix: parameterize the casts output dir via env var or CLI arg. "
        "Current violations:\n"
        + "\n".join(violations)
    )


# ===========================================================================
# PT2 (nested) — ``# doc: a/b-c`` ⇒ nested ``a/b-c.sh``, mkdir -p
# ===========================================================================


def test_pt2_nested_slug_to_relpath_no_flattening() -> None:
    """PT2 (nested) / _slug_to_relpath: slug ``a/b-c`` maps to ``a/b-c.sh``.

    LDR 2026-05-17 (ADR Decision D): the ``/`` → ``__`` flattening is
    **removed**.  ``_slug_to_relpath`` returns ``slug + ".sh"`` with ``/``
    preserved as the directory separator.  Distinct slugs produce distinct
    relative paths (injective).

    Contract: _slug_to_relpath("a/b-c") == "a/b-c.sh"
    """
    result = _slug_to_relpath("a/b-c")
    assert result == "a/b-c.sh", (
        f"PT2/nested: _slug_to_relpath('a/b-c') must return 'a/b-c.sh' "
        f"(no flattening); got {result!r}"
    )


def test_pt2_nested_flat_slug_unchanged() -> None:
    """PT2 (nested) / _slug_to_relpath: a slug without ``/`` appends ``.sh``.

    A single-component slug such as ``install`` maps to ``install.sh``
    (unchanged component + extension).
    """
    result = _slug_to_relpath("install")
    assert result == "install.sh", (
        f"PT2/nested: _slug_to_relpath('install') must return 'install.sh'; "
        f"got {result!r}"
    )


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

    Expected after re-publish:
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
        "PT5/nested/foreign: mygroup/alpha.sh (owned orphan) must be removed"
    )
    assert foreign_file.exists(), (
        "PT5/nested/foreign: mygroup/foreign.txt (foreign, not in manifest) "
        "must survive the orphan sweep"
    )
    assert (scripts_out / "mygroup").exists(), (
        "PT5/nested/foreign: mygroup/ directory must NOT be pruned because it "
        "still contains foreign content"
    )


# ===========================================================================
# RN1 — render_display: region present → only region body, blank-trimmed
# ===========================================================================


def test_rn1_region_present_yields_only_region_body() -> None:
    """RN1: when ``cast_region`` is not None, render_display returns only the
    lines strictly between the markers, with leading/trailing blank lines
    trimmed; header, set -euo pipefail, and assertions are absent.

    cast_region uses 1-based inclusive line numbers (marker lines themselves
    are included in the span).  The parser sets start=line_of_region_marker
    and end=line_of_endregion_marker.  The render layer slices
    all_lines[start:end-1] to exclude both markers.

    Contract reference: §6e RN1 (LDR cast_region 1-based inclusive note).
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn1/test
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install "$PKG_UV"
        ocx package which uv
        # endregion cast

        out="$(ocx package exec uv -- uv --version)"
        [[ "$out" == *"uv 0.10"* ]] || exit 1
        """
    )
    # Line numbers (1-based): "# region cast" is line 7, "# endregion cast" is line 10.
    # parse_doc_header sets cast_region = (7, 10).
    cast_region = (7, 10)

    result = render_display(
        script_text,
        cast_region=cast_region,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn1/test",
    )

    # RN6 (LDR 2026-05-18): NO disclaimer header is prepended.
    assert "Rendered for display" not in result, (
        "RN6: disclaimer header must NOT be present (render is now upstream "
        "of the drift gate — tested-by-construction)"
    )
    assert not result.startswith("#!"), "RN1: shebang absent"

    # Region body must be present (after substitution via RN3).
    assert "ocx package install" in result, "RN1: region body line must be present"
    assert "ocx package which uv" in result, "RN1: region body line must be present"

    # Lines outside the region must be absent.
    assert "set -euo pipefail" not in result, (
        "RN1: 'set -euo pipefail' is outside the region and must be absent"
    )
    assert '$(ocx package exec' not in result, (
        "RN1: capture/assertion outside region must be absent"
    )
    assert "#!/usr/bin/env bash" not in result, (
        "RN1: shebang must be absent (outside region)"
    )
    # Markers themselves must be absent.
    assert "# region cast" not in result, (
        "RN1: '# region cast' marker must not appear in output"
    )
    assert "# endregion cast" not in result, (
        "RN1: '# endregion cast' marker must not appear in output"
    )


def test_rn1_region_body_leading_trailing_blank_lines_trimmed() -> None:
    """RN1 (blank-trim): leading and trailing fully-blank lines inside the
    region are removed from the output.

    The markers delimit the span; blank lines at the start or end of that
    span are stripped before any further processing.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn1/blanktrim
        # cast: true
        set -euo pipefail

        # region cast

        ocx package install "$PKG_UV"

        # endregion cast
        """
    )
    # "# region cast" = line 7; "# endregion cast" = line 11.
    cast_region = (7, 11)

    result = render_display(
        script_text,
        cast_region=cast_region,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn1/blanktrim",
    )

    # The region body after trimming must not start or end with a blank line.
    # (RN6 LDR 2026-05-18: there is no header line to skip.)
    body_lines = result.splitlines()
    # First and last body lines must not be blank after trim.
    assert body_lines, "RN1/blanktrim: rendered body must be non-empty"
    assert body_lines[0].strip() != "", (
        "RN1/blanktrim: first body line after trimming must not be blank"
    )
    assert body_lines[-1].strip() != "", (
        "RN1/blanktrim: last body line after trimming must not be blank"
    )


# ===========================================================================
# RN2 — render_display: no region → full body minus shebang + header + set -e
# ===========================================================================


def test_rn2_no_region_strips_shebang_header_and_set_e() -> None:
    """RN2: when ``cast_region`` is None, render_display returns the full body
    minus: (a) leading shebang, (b) metadata header block, (c) a single
    leading ``set -euo pipefail`` (or ``set -e``/``set -eu``) if it is the
    first non-blank line after the header.

    Plain non-metadata comments are kept.

    Contract reference: §6e RN2.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn2/test
        # title: RN2 test
        set -euo pipefail

        # This is a plain comment (kept)
        ocx package install "$PKG_UV"
        out="$(ocx package which uv)"
        [[ -n "$out" ]] || exit 1
        """
    )

    result = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn2/test",
    )

    # Shebang must be absent.
    assert "#!/usr/bin/env bash" not in result, (
        "RN2: shebang must be stripped"
    )
    # Metadata header keys must be absent.
    assert "# state:" not in result, "RN2: '# state:' metadata must be stripped"
    assert "# doc:" not in result, "RN2: '# doc:' metadata must be stripped"
    assert "# title:" not in result, "RN2: '# title:' metadata must be stripped"
    # set -euo pipefail must be absent (first non-blank after header).
    assert "set -euo pipefail" not in result, (
        "RN2: leading 'set -euo pipefail' must be stripped"
    )
    # Plain comment must be present.
    assert "# This is a plain comment" in result, (
        "RN2: plain non-metadata comment must be kept"
    )
    # Body command must be present.
    assert "ocx package install" in result, "RN2: body command must be present"
    # RN6 (LDR 2026-05-18): no disclaimer header; no spurious leading blank.
    assert "Rendered for display" not in result, (
        "RN6: disclaimer header must NOT be present"
    )
    assert result.splitlines()[0].strip() != "", (
        "RN2 blank-trim: first line must not be blank (header-terminating "
        "blank no longer leaks)"
    )


def test_rn2_set_e_variants_stripped() -> None:
    """RN2: the shorter ``set -e`` and ``set -eu`` variants are also stripped
    when they are the first non-blank line after the header.
    """
    for set_variant in ("set -e", "set -eu", "set -euo pipefail"):
        script_text = textwrap.dedent(
            f"""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rn2/set-variant
            {set_variant}

            ocx package install "$PKG_UV"
            """
        )
        result = render_display(
            script_text,
            cast_region=None,
            display_env={"PKG_UV": "uv:0.10"},
            slug="rn2/set-variant",
        )
        assert set_variant not in result, (
            f"RN2: '{set_variant}' must be stripped as the leading set-e variant"
        )
        assert "ocx package install" in result, (
            f"RN2: body must be present for variant '{set_variant}'"
        )


# ===========================================================================
# RN3 — variable substitution for display_env keys
# ===========================================================================


def test_rn3_display_env_substitution_forms() -> None:
    """RN3: $NAME, ${NAME}, "$NAME", "${NAME}" for display_env keys are all
    replaced by value; surrounding quotes are preserved verbatim.

    Contract reference: §6e RN3.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn3/test
        set -euo pipefail

        ocx package install $PKG_UV
        ocx package install ${PKG_UV}
        ocx package install "$PKG_UV"
        ocx package install "${PKG_UV}"
        """
    )

    result = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn3/test",
    )

    # All four forms must be substituted.
    assert "ocx package install uv:0.10" in result, (
        "RN3: '$PKG_UV' (bare) must be substituted"
    )
    assert "ocx package install uv:0.10\n" in result or "uv:0.10" in result, (
        "RN3: '${PKG_UV}' must be substituted"
    )
    # Quoted forms: quotes preserved, value substituted.
    assert '"uv:0.10"' in result, (
        "RN3: '\"$PKG_UV\"' must become '\"uv:0.10\"' (quotes preserved)"
    )

    # Original var references must not remain.
    assert "$PKG_UV" not in result, (
        "RN3: no $PKG_UV references must remain after substitution"
    )
    assert "${PKG_UV}" not in result, (
        "RN3: no ${PKG_UV} references must remain after substitution"
    )


def test_rn3_single_quoted_var_still_substituted() -> None:
    """RN3 edge case: $NAME inside a single-quoted string is still substituted.

    RN3 is text substitution, not shell-semantics emulation.  Single quotes
    do not protect the variable from replacement.

    Contract reference: §6e RN3 (single-quoted edge case).
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn3/singlequote
        set -euo pipefail

        echo 'install $PKG_UV'
        """
    )

    result = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn3/singlequote",
    )

    # RN3 is text substitution: $PKG_UV replaced even inside single quotes.
    assert "$PKG_UV" not in result, (
        "RN3/single-quote: $PKG_UV inside single quotes must still be substituted"
    )
    assert "uv:0.10" in result, (
        "RN3/single-quote: substituted value must appear in output"
    )


# ===========================================================================
# RN3 — word-boundary / no-prefix false-substitution (A8)
# ===========================================================================


def test_rn3_no_prefix_false_substitution() -> None:
    """A8/RN3: word-boundary / no-prefix-false-substitution contract for substitute_renderable.

    Covers four cases:

    1. ``${PKG_UV}x`` → ``uv:0.10x``  (brace form: exact match → correct).
    2. ``$PKG_UVX`` with only ``PKG_UV`` declared → left verbatim
       (word-boundary: the bare form must not match the ``PKG_UV`` prefix
       inside ``$PKG_UVX``).
    3. ``$PKG_UV_2`` with only ``PKG_UV`` declared → left verbatim
       (word-boundary: ``_`` is a name char, so ``$PKG_UV_2`` is a distinct
       undeclared name).
    4. When both ``PKG_UV`` and ``PKG_UV_2`` are declared, longest-first
       ordering correctly replaces both.

    The substitution is word-boundary correct: the bare form is matched as
    ``$NAME(?![A-Za-z0-9_])`` and the brace form as ``${NAME}``, so a declared
    name that is a prefix of an undeclared reference never partially
    substitutes (no ``$PKG_UV`` → ``uv:0.10`` bleed inside ``$PKG_UVX``).

    Contract reference: §6e RN3 (longest-name-first / no-false-substitution).
    """
    from src.doc_scripts import substitute_renderable

    display_env = {"PKG_UV": "uv:0.10"}

    # Case 1 — brace form: ${PKG_UV}x → uv:0.10x (correct behaviour, must pass)
    brace_result = substitute_renderable("cmd ${PKG_UV}x end", display_env)
    assert brace_result == "cmd uv:0.10x end", (
        f"A8/RN3: ${{PKG_UV}}x must substitute the braced ref and leave 'x' adjacent; "
        f"got {brace_result!r}"
    )

    # Case 2 — bare $PKG_UVX must NOT be replaced (only PKG_UV declared)
    bare_extended_result = substitute_renderable("cmd $PKG_UVX end", display_env)
    assert bare_extended_result == "cmd $PKG_UVX end", (
        f"A8/RN3: $PKG_UVX must be left verbatim — the declared PKG_UV must not "
        f"prefix-match inside the undeclared $PKG_UVX; got {bare_extended_result!r}"
    )

    # Case 3 — bare $PKG_UV_2 must NOT be replaced (only PKG_UV declared)
    bare_suffix_result = substitute_renderable("cmd $PKG_UV_2 end", display_env)
    assert bare_suffix_result == "cmd $PKG_UV_2 end", (
        f"A8/RN3: $PKG_UV_2 must be left verbatim — '_' is a name char so it is a "
        f"distinct undeclared name, not a PKG_UV suffix; got {bare_suffix_result!r}"
    )

    # Case 4 — when both are declared, longest-first replaces both correctly
    display_env_both = {"PKG_UV": "uv:0.10", "PKG_UV_2": "uv2:0.11"}
    multi_result = substitute_renderable("cmd $PKG_UV and $PKG_UV_2 end", display_env_both)
    assert multi_result == "cmd uv:0.10 and uv2:0.11 end", (
        f"A8/RN3: when both PKG_UV and PKG_UV_2 are declared, longest-first ordering "
        f"must replace both correctly; got {multi_result!r}"
    )


# ===========================================================================
# RN4 — ambient / shell-special vars left verbatim, no error
# ===========================================================================


def test_rn4_ambient_and_special_vars_left_verbatim() -> None:
    """RN4: $HOME, $PATH, $PWD, $(…), `…`, $$, $@, $1 are left verbatim.

    No expansion, no RenderError — they are legitimate walkthrough vars.

    Contract reference: §6e RN4.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn4/test
        set -euo pipefail

        echo "$HOME"
        echo "$PATH"
        echo "$PWD"
        out=$(ocx package which uv)
        pid=$$
        args=$@
        first=$1
        backtick=`date`
        """
    )

    # Must not raise RenderError; all ambient vars preserved.
    result = render_display(
        script_text,
        cast_region=None,
        display_env={},
        slug="rn4/test",
    )

    assert "$HOME" in result, "RN4: $HOME must be left verbatim"
    assert "$PATH" in result, "RN4: $PATH must be left verbatim"
    assert "$PWD" in result, "RN4: $PWD must be left verbatim"
    assert "$(ocx package which uv)" in result, "RN4: $( ) must be left verbatim"
    assert "$$" in result, "RN4: $$ must be left verbatim"
    assert "$@" in result, "RN4: $@ must be left verbatim"
    assert "$1" in result, "RN4: $1 must be left verbatim"
    assert "`date`" in result, "RN4: backtick command substitution must be left verbatim"


# ===========================================================================
# RN5 — fixture/harness var not in display_env → RenderError
# ===========================================================================


def test_rn5_undeclared_pkg_var_raises_render_error() -> None:
    """RN5: a $PKG_<KEY> not in display_env raises RenderError (hard publish
    error; message names the variable and slug).

    Contract reference: §6e RN5.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn5/test
        set -euo pipefail

        ocx package install "$PKG_UNKNOWN"
        """
    )

    with pytest.raises(RenderError) as exc_info:
        render_display(
            script_text,
            cast_region=None,
            display_env={},
            slug="rn5/test",
        )

    error_msg = str(exc_info.value)
    assert "PKG_UNKNOWN" in error_msg, (
        "RN5: error message must name the undeclared fixture variable"
    )
    assert "rn5/test" in error_msg, (
        "RN5: error message must reference the slug"
    )


def test_rn5_repo_var_raises_render_error() -> None:
    """RN5: $REPO_<KEY> (fixture namespace) raises RenderError.

    REPO_*, FQ_*, TAG_*, MARKER_*, HOME_KEY_* are banned from displayed
    regions — they are UUID-prefixed at runtime and have no static canonical
    form.

    Contract reference: §6e RN5 (fixture namespace definition).
    """
    for var_name in ("REPO_UV", "FQ_UV", "TAG_UV", "MARKER_X", "HOME_KEY_X"):
        script_text = textwrap.dedent(
            f"""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rn5/repo
            set -euo pipefail

            echo "${var_name}"
            """
        )

        with pytest.raises(RenderError) as exc_info:
            render_display(
                script_text,
                cast_region=None,
                display_env={},
                slug="rn5/repo",
            )

        error_msg = str(exc_info.value)
        assert var_name in error_msg, (
            f"RN5: error message must name the fixture variable {var_name!r}"
        )


def test_rn5_runner_vars_raise_render_error() -> None:
    """RN5: runner-harness vars $REGISTRY, $SCENARIO_TMP, $OCX, $OCX_HOME
    raise RenderError when they appear in the display text.

    Contract reference: §6e RN5 (runner-harness var list).
    """
    for var_name in ("REGISTRY", "SCENARIO_TMP", "OCX", "OCX_HOME"):
        script_text = textwrap.dedent(
            f"""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: rn5/runner
            set -euo pipefail

            echo "${var_name}"
            """
        )

        with pytest.raises(RenderError) as exc_info:
            render_display(
                script_text,
                cast_region=None,
                display_env={},
                slug="rn5/runner",
            )

        error_msg = str(exc_info.value)
        assert var_name in error_msg, (
            f"RN5: runner var {var_name!r} must be named in the RenderError message"
        )


def test_rn5_declared_pkg_key_does_not_raise() -> None:
    """RN5 (negative): a $PKG_<KEY> that IS in display_env is substituted
    (RN3) and does NOT raise RenderError.

    Only the undeclared fixture vars are errors.

    Contract reference: §6e RN3 + RN5.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn5/declared
        set -euo pipefail

        ocx package install "$PKG_UV"
        """
    )

    # Must not raise.
    result = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn5/declared",
    )

    assert "uv:0.10" in result, (
        "RN5/declared: declared PKG_UV must be substituted, not errored"
    )


# ===========================================================================
# RN6 — NO disclaimer header (LDR 2026-05-18; render moved upstream of drift)
# ===========================================================================


def test_rn6_no_disclaimer_header() -> None:
    """RN6 (LDR 2026-05-18): the rendered output carries **no** disclaimer
    header — the drift gate now executes the renderable-substituted body
    (EX10/RN8), so the display artifact is tested-by-construction.

    The first line is real body content (no shebang, no
    ``# Rendered for display ... not the tested source.``); no
    ``$PKG_*`` token leaks (RN3 substituted them).

    Contract reference: §6e RN6/RN8, §2 EX10.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn6/test
        set -euo pipefail

        ocx package install "$PKG_UV"
        """
    )

    result = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn6/test",
    )

    assert "Rendered for display" not in result, (
        "RN6: disclaimer header must NOT be present"
    )
    assert "not the tested source" not in result, (
        "RN6: 'not the tested source' must NOT appear"
    )
    first_line = result.splitlines()[0] if result.splitlines() else ""
    assert first_line == 'ocx package install "uv:0.10"', (
        f"RN6: first line must be the substituted body command; got "
        f"{first_line!r}"
    )
    assert "$PKG_UV" not in result, "RN3: renderable var must be substituted"


# ===========================================================================
# RN7 — pure / idempotent: same inputs → byte-identical output
# ===========================================================================


def test_rn7_render_display_is_pure_and_idempotent() -> None:
    """RN7: calling render_display twice with identical arguments produces
    byte-identical output.

    This property feeds PT3: the render runs before the content-hash compare
    so an unchanged source + unchanged display_env produces no disk write.

    Contract reference: §6e RN7.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn7/test
        set -euo pipefail

        # Plain comment preserved
        ocx package install "$PKG_UV"
        out=$(ocx package which uv)
        """
    )

    first = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn7/test",
    )
    second = render_display(
        script_text,
        cast_region=None,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn7/test",
    )

    assert first == second, (
        "RN7: render_display must be pure — same inputs must produce byte-identical "
        f"output. First call length={len(first)}, second={len(second)}"
    )


def test_rn7_idempotency_with_region() -> None:
    """RN7: idempotency holds for the cast_region path (RN1 + RN7 combined).

    Two calls with the same region span and display_env must produce
    identical bytes.
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: rn7/region
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install "$PKG_UV"
        # endregion cast

        [[ "$(ocx package which uv)" ]] || exit 1
        """
    )
    cast_region = (7, 9)

    first = render_display(
        script_text,
        cast_region=cast_region,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn7/region",
    )
    second = render_display(
        script_text,
        cast_region=cast_region,
        display_env={"PKG_UV": "uv:0.10"},
        slug="rn7/region",
    )

    assert first == second, (
        "RN7: render_display (RN1 path) must be pure — same inputs → identical output"
    )


# ===========================================================================
# RN8 — single substitution source: canonical ↔ website mirror parity
# ===========================================================================


@pytest.mark.parametrize(
    "text",
    [
        'ocx package install "$PKG_UV"\n',
        "ocx index list $REPO_CORRETTO\n",
        '"${PKG_FOO_BAR}" and $PKG_FOO and ${REPO_X}\n',
        "no vars here; $HOME and $(date) and $REGISTRY untouched\n",
        "'$PKG_UV' single-quoted still substituted\n",
        "",
    ],
)
def test_substitute_renderable_parity(text: str) -> None:
    """RN8: the website mirror ``_substitute_renderable`` is byte-equivalent
    to the canonical ``src.doc_scripts.substitute_renderable``.

    Guards the PT6 hand-mirror coupling (parity-gate pattern, cf. DE5) — the
    drift gate and the publish render MUST substitute identically or the
    "display ⊆ tested" invariant breaks silently.
    """
    env = {
        "PKG_UV": "uv:0.10",
        "PKG_FOO": "foo:1",
        "PKG_FOO_BAR": "foobar:2",
        "REPO_CORRETTO": "corretto",
        "REPO_X": "x",
    }
    assert substitute_renderable(text, env) == _substitute_renderable(text, env), (
        "RN8: canonical and website-mirror substitution must be byte-identical"
    )


def test_display_is_substring_of_tested_render() -> None:
    """RN6/RN8/EX10: the display artifact is a verified slice of the text the
    drift gate executes.

    The drift gate runs ``substitute_renderable(full_body, display_env)``
    (EX10); ``render_display`` region-cuts then applies the *same* RN8 pass.
    Therefore every non-blank rendered display line MUST appear verbatim as a
    line of the renderable-substituted full body — proving the displayed
    bytes are a subset of the tested bytes (no divergence, the reason the
    RN6 disclaimer is gone).
    """
    script_text = textwrap.dedent(
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: inv/substr
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install "$PKG_UV"
        ocx index list "$REPO_UV"
        # endregion cast

        out="$(ocx package exec uv -- uv --version)"
        [[ "$out" == *"uv 0.10"* ]] || exit 1
        """
    )
    # "# region cast" line 7, "# endregion cast" line 10.
    cast_region = (7, 10)
    display_env = {"PKG_UV": "uv:0.10", "REPO_UV": "uv"}

    display = render_display(
        script_text,
        cast_region=cast_region,
        display_env=display_env,
        slug="inv/substr",
    )
    tested = substitute_renderable(script_text, display_env)
    tested_lines = set(tested.splitlines())

    for line in display.splitlines():
        if not line.strip():
            continue
        assert line in tested_lines, (
            f"display line not a substring of tested render: {line!r}\n"
            f"tested:\n{tested}"
        )
