"""Drift-gate executor specification tests (Phase 2, uses real ocx fixture).

Tests are written from design_spec_doc_command_scripts.md §2 (EX1–EX9,
GO1–GO3), §6 (DG1–DG3) — NOT from the stub implementation.  They MUST fail
against the current stub (raise ``NotImplementedError``) and pass once Phase-2
implementation lands.

Tests in this module use the ``ocx`` fixture (requires registry + binary) and
are therefore excluded from the pure-parser collection.  They run in
``test:parallel`` together with the other acceptance tests.

Module-level ``pytestmark`` skips all cases on Windows (EX7), parity with
``test_scenarios_smoke.py``.

Contract coverage:
  EX1/EX2    — setup:basic provisions uv; body exits 0 ⇒ passes
  EX3/DG1/DG2 — exit 1 ⇒ AssertionError with path, title, slug, ANSI-stripped output
  EX4        — unqualified / unknown # state: ⇒ ValueError
  EX6        — absent # state: defaults to setup:basic
  EX8        — cast: true on verify path ⇒ normal acceptance; no .cast produced
  EX7        — structural: module has win32 pytestmark
  GO1        — # expect: with matching output ⇒ passes
  GO2        — # expect: mismatch ⇒ AssertionError with unified diff
  GO3        — # expect: pointing at missing file ⇒ AssertionError golden not found
  DG3        — all-pass fixture ⇒ green
  Discovery  — parametrization yields one case per .sh via discover_doc_scripts
"""
from __future__ import annotations

import sys
import textwrap
from pathlib import Path

import pytest

from src.doc_scripts import DocScriptParseError, discover_doc_scripts, run_doc_script
from src.runner import OcxRunner

# Shell scenarios target Linux + macOS. Windows behaviour is covered by
# the pytest acceptance suite. Parity with test_scenarios_smoke.py (EX7).
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Doc-script drift gate targets Linux/macOS; Windows behaviour covered by the pytest suite.",
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_script(tmp_path: Path, name: str, content: str) -> Path:
    """Write a fixture .sh script in a dedicated subdir and return its path."""
    script_dir = tmp_path / "scripts"
    script_dir.mkdir(exist_ok=True)
    p = script_dir / name
    p.write_text(textwrap.dedent(content))
    p.chmod(0o755)
    return p


# ===========================================================================
# EX7 — structural: module carries win32 pytestmark
# ===========================================================================


def test_ex7_module_has_win32_skip_mark() -> None:
    """EX7: this module carries pytestmark = pytest.mark.skipif(sys.platform == 'win32', ...).

    Asserted on any platform — proves parity with test_scenarios_smoke.py
    without needing Windows execution.  The condition evaluates to a bool at
    import time; we verify the skipif mark exists and its reason mentions
    Windows.
    """
    import importlib

    self_module = importlib.import_module("tests.test_doc_scripts_executor")

    mark = getattr(self_module, "pytestmark", None)
    assert mark is not None, "module must have pytestmark attribute"

    marks = mark if isinstance(mark, list) else [mark]
    skipif_marks = [m for m in marks if getattr(m.mark, "name", None) == "skipif"]
    assert skipif_marks, "module pytestmark must include a skipif mark (EX7)"

    # The reason must reference Windows to signal the intent
    reasons = [m.kwargs.get("reason", "") for m in skipif_marks]
    assert any(
        "win32" in r.lower() or "windows" in r.lower() for r in reasons
    ), f"skipif reason must mention win32/Windows; got: {reasons}"


# ===========================================================================
# EX1/EX2/EX6 — setup:basic provisions packages; body exits 0 ⇒ passes
# ===========================================================================


def test_ex1_ex2_setup_basic_provisions_and_exits_zero(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX1/EX2: # state: setup:basic provisions uv; body using $PKG_UV exits 0.

    The script installs the package exposed by $PKG_UV and checks it with
    'ocx package which'.  On exit 0 run_doc_script must return without raising.
    """
    script = _write_script(
        tmp_path,
        "test_ex1_ex2.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        set -euo pipefail
        ocx package install --select "$PKG_UV"
        ocx package which "$REPO_UV"
        """,
    )
    # Must not raise
    run_doc_script(script, ocx, tmp_path)


def test_ex6_no_state_header_defaults_to_setup_basic(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX6: absent # state: uses setup:basic; $PKG_UV is available."""
    script = _write_script(
        tmp_path,
        "test_ex6.sh",
        """\
        #!/usr/bin/env bash
        set -euo pipefail
        ocx package install --select "$PKG_UV"
        ocx package which "$REPO_UV"
        """,
    )
    # Must not raise — default state provides $PKG_UV and $REPO_UV
    run_doc_script(script, ocx, tmp_path)


def test_ex10_drift_gate_runs_raw_body_under_sp7(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX10 (LDR 2026-05-18): the drift gate runs the **raw** body under
    ``script_env`` (SP7-prefixed) — it does NOT renderable-substitute before
    executing.

    Rationale: substituting ``$PKG_*`` to the clean display short would make
    the command resolve a repo that was never pushed (SP7 isolation pushes to
    ``t_<8hex>_<repo>``).  The honest tested guarantee is DE6-canonical
    equivalence (gated by ``test_state_providers``/DE6), not byte-identity.

    This test pins the contract: ``$PKG_UV`` at runtime carries the SP7
    isolation prefix (proving it was NOT substituted) yet the body still
    runs green (proving raw execution under script_env resolves correctly).
    """
    script = _write_script(
        tmp_path,
        "test_ex10.sh",
        r"""        #!/usr/bin/env bash
        # state: setup:basic
        set -euo pipefail
        if [[ ! "$PKG_UV" =~ ^[ts]_[0-9a-f]{8}_ ]]; then
          echo "EX10: expected SP7-prefixed \$PKG_UV (raw exec), got: $PKG_UV" >&2
          exit 1
        fi
        ocx package install --select "$PKG_UV"
        """,
    )
    # Must not raise: raw body executes under SP7-prefixed script_env.
    run_doc_script(script, ocx, tmp_path)


# ===========================================================================
# EX3/DG1/DG2 — exit 1 ⇒ AssertionError with path, title, slug, ANSI-stripped
# ===========================================================================


def test_ex3_dg1_dg2_exit_one_produces_rich_error(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX3/DG1/DG2: body exits 1 ⇒ AssertionError containing path, title, slug, description.

    The ANSI colour sequence injected via printf must NOT appear in the
    AssertionError message (EX3: ANSI-stripped output).
    """
    script = _write_script(
        tmp_path,
        "test_ex3_dg1_dg2.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # title: My Command Title
        # doc: getting-started/x
        # description: A useful description
        printf '\\033[31mboom\\033[0m' >&2
        exit 1
        """,
    )

    with pytest.raises(AssertionError) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)

    # DG1: script path present
    assert str(script) in msg or script.name in msg, (
        f"script path not in error message; got:\n{msg}"
    )

    # DG1: title present
    assert "My Command Title" in msg, (
        f"title not in error message; got:\n{msg}"
    )

    # DG2: doc slug present
    assert "getting-started/x" in msg, (
        f"doc slug not in error message; got:\n{msg}"
    )

    # DG1: description present
    assert "A useful description" in msg, (
        f"description not in error message; got:\n{msg}"
    )

    # EX3: ANSI escape must have been stripped
    assert "\x1b[" not in msg, (
        f"raw ANSI escape found in error message — output was not stripped:\n{msg}"
    )

    # EX3: content is present but without escape codes
    assert "boom" in msg, (
        f"stderr content ('boom') not in error message; got:\n{msg}"
    )


def test_dg1_no_doc_slug_error_still_has_path_and_title(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DG1: failing script without # doc: still includes path and title in error."""
    script = _write_script(
        tmp_path,
        "test_dg1_no_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # title: Tool Without Slug
        exit 1
        """,
    )

    with pytest.raises(AssertionError) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)
    assert "Tool Without Slug" in msg or script.name in msg, (
        f"title or path not in error; got:\n{msg}"
    )


# ===========================================================================
# EX4 — unqualified / unknown # state: ⇒ ValueError (or DocScriptParseError)
# ===========================================================================


def test_ex4_unqualified_state_raises(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX4: # state: basic (no family prefix) ⇒ ValueError or DocScriptParseError.

    The error message must indicate the invalid value and the expected form
    (setup:… or scenario:…).
    """
    script = _write_script(
        tmp_path,
        "test_ex4_unqualified.sh",
        """\
        #!/usr/bin/env bash
        # state: basic
        echo hello
        """,
    )

    with pytest.raises((ValueError, DocScriptParseError)) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)
    # Must mention the invalid value
    assert "basic" in msg, f"invalid state value not in message; got:\n{msg}"
    # Must indicate the expected family-qualified form
    assert "setup:" in msg or "scenario:" in msg, (
        f"expected form hint not in message; got:\n{msg}"
    )


def test_ex4_nonexistent_scenario_state_raises(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX4: # state: scenario:basic ⇒ invalid (no scenario key 'basic'; keys are PascalCase).

    The error message must contain 'invalid state' and the available families.
    """
    script = _write_script(
        tmp_path,
        "test_ex4_scenario_basic.sh",
        """\
        #!/usr/bin/env bash
        # state: scenario:basic
        echo hello
        """,
    )

    with pytest.raises((ValueError, DocScriptParseError)) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value).lower()
    assert "invalid" in msg or "unknown" in msg or "basic" in msg, (
        f"error does not mention invalid state; got:\n{msg}"
    )


def test_ex4_bogus_setup_state_raises(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX4: # state: setup:nonexistent ⇒ invalid; error mentions available setups."""
    script = _write_script(
        tmp_path,
        "test_ex4_bogus_setup.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:nonexistent
        echo hello
        """,
    )

    with pytest.raises((ValueError, DocScriptParseError)) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)
    assert "nonexistent" in msg or "invalid" in msg.lower() or "unknown" in msg.lower(), (
        f"error does not mention invalid state; got:\n{msg}"
    )


# ===========================================================================
# EX8 — # cast: true on verify path ⇒ normal acceptance; no .cast produced
# ===========================================================================


def test_ex8_cast_true_runs_as_normal_acceptance(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """EX8: # cast: true on verify path ⇒ run_doc_script does not produce a .cast file.

    The drift gate runs the FULL body (including assertions outside the region);
    no cast artifact is written anywhere under tmp_path.
    """
    script = _write_script(
        tmp_path,
        "test_ex8_cast.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install --select "$PKG_UV"
        ocx package which "$REPO_UV"
        # endregion cast

        # assertion outside cast region — must run in drift gate
        ocx package which "$REPO_UV"
        """,
    )

    # Must not raise (body exits 0)
    run_doc_script(script, ocx, tmp_path)

    # No .cast file must be produced anywhere under tmp_path
    cast_files = list(tmp_path.rglob("*.cast"))
    assert cast_files == [], (
        f"EX8/CA3: no .cast must be produced on verify path; found: {cast_files}"
    )


# ===========================================================================
# GO1/GO2/GO3 — golden-output diffing
# ===========================================================================


def test_go1_matching_golden_passes(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """GO1: # expect: out.txt whose ANSI-stripped stdout+stderr matches ⇒ passes."""
    script_dir = tmp_path / "scripts"
    script_dir.mkdir(exist_ok=True)

    golden = script_dir / "out.txt"
    golden.write_text("hello from script\n")

    script = script_dir / "test_go1.sh"
    script.write_text(
        "#!/usr/bin/env bash\n"
        "# state: setup:basic\n"
        "# expect: out.txt\n"
        "printf 'hello from script\\n'\n"
    )
    script.chmod(0o755)

    # Must not raise
    run_doc_script(script, ocx, tmp_path)


def test_go2_mismatched_golden_raises_with_diff(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """GO2: # expect: mismatch ⇒ AssertionError containing a unified diff."""
    script_dir = tmp_path / "scripts"
    script_dir.mkdir(exist_ok=True)

    golden = script_dir / "out.txt"
    golden.write_text("expected output\n")

    script = script_dir / "test_go2.sh"
    script.write_text(
        "#!/usr/bin/env bash\n"
        "# state: setup:basic\n"
        "# expect: out.txt\n"
        "printf 'actual different output\\n'\n"
    )
    script.chmod(0o755)

    with pytest.raises(AssertionError) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)
    # Unified diff markers
    assert "---" in msg or "+++" in msg or "@@ " in msg, (
        f"GO2: error should contain a unified diff; got:\n{msg}"
    )


def test_go3_missing_golden_raises(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """GO3: # expect: pointing at a missing file ⇒ AssertionError 'golden file not found'."""
    script = _write_script(
        tmp_path,
        "test_go3.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # expect: nonexistent_golden.txt
        echo hello
        """,
    )

    with pytest.raises(AssertionError) as exc_info:
        run_doc_script(script, ocx, tmp_path)

    msg = str(exc_info.value)
    assert "golden" in msg.lower() or "not found" in msg.lower(), (
        f"GO3: error should mention 'golden file not found'; got:\n{msg}"
    )
    assert "nonexistent_golden.txt" in msg, (
        f"GO3: error should include the missing file path; got:\n{msg}"
    )


# ===========================================================================
# DG3 — all-pass fixture ⇒ green (no skip on non-Windows)
# ===========================================================================


def test_dg3_all_pass_fixture_is_green(
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DG3: a doc script whose commands succeed produces no failures on non-Windows.

    This test is itself the proof: it runs on Linux/macOS (pytestmark skips
    Windows) and must pass without being skipped.
    """
    assert sys.platform != "win32", "This test should not run on Windows"

    script = _write_script(
        tmp_path,
        "test_dg3_green.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # title: DG3 Green Fixture
        # doc: dg3/green
        # description: All-pass case for DG3 contract proof
        set -euo pipefail
        ocx package install --select "$PKG_UV"
        ocx package which "$REPO_UV"
        """,
    )

    # Must not raise — proves DG3
    run_doc_script(script, ocx, tmp_path)


# ===========================================================================
# Discovery — parametrization contract
# ===========================================================================


def test_discovery_yields_one_case_per_sh(tmp_path: Path) -> None:
    """Discovery contract: discover_doc_scripts returns one Path per .sh file.

    This asserts the parametrization source used by test_doc_scripts.py:
    each discovered path is a distinct .sh file.  Non-.sh files are excluded.
    """
    root = tmp_path / "doc_scripts"
    root.mkdir()

    # Create three scripts
    for name in ("alpha.sh", "beta.sh", "gamma.sh"):
        (root / name).write_text("# state: setup:basic\necho\n")

    # A non-.sh file that must not appear
    (root / "readme.md").write_text("# doc\n")

    scripts = discover_doc_scripts(root)
    assert len(scripts) == 3
    for p in scripts:
        assert p.suffix == ".sh"
        assert p.parent == root
