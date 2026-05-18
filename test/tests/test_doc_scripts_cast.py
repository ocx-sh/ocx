"""Cast-layer specification tests (Phase 4, contract-first TDD).

Tests are written from design_spec_doc_command_scripts.md §4 (CA1–CA5) and
§1.3 (cast region) — NOT from the not-yet-written implementation.  They MUST
fail against the absent ``recordings.cast_layer`` module (ImportError /
AttributeError) and pass once Phase-4 implementation lands.

The proposed cast-layer seam:

    Module:   recordings.cast_layer
    Function: maybe_record_cast(
                  meta: DocScriptMeta,
                  provider: StateProvider,
                  recorder: CastRecorder,
                  casts_dir: Path,
              ) -> Path | None

Semantics:
  - Returns None and writes no file when ``meta.cast is False`` (CA1).
  - Returns the written Path when ``meta.cast is True``:
      - Filename is ``<flat-slug>.cast`` (``/`` → ``__``) when ``meta.doc``
        is set (CA2); falls back to ``<stem>.cast`` when ``meta.doc`` is None.
  - Never writes a cast on the verify path — enforced by ``run_doc_script``
    being the verify path entry point; ``maybe_record_cast`` is the
    website-build-only entry point (CA3 / EX8).
  - Uses the passed *provider* for display-name rewriting (CA4); does NOT
    import or consult legacy ``SETUPS`` directly.
  - Calls ``recorder.run_command()`` exactly once per line inside
    ``# region cast`` … ``# endregion cast``; lines outside the region
    (``set -euo pipefail``, ``$(…)`` captures, ``[[ … ]]`` assertions) are
    NEVER sent to the recorder (CA5).

Module-level ``pytestmark`` skips all cases on Windows, parity with
``test_scenarios_smoke.py`` (EX7 parity).

Design contract reference: design_spec_doc_command_scripts.md
§1.3, §4 (CA1–CA5), §10.
"""
from __future__ import annotations

import sys
import textwrap
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, patch

import pytest

# Windows skip — parity with test_scenarios_smoke.py and other doc-script tests.
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Cast-layer targets Linux/macOS; Windows behaviour covered by the pytest suite.",
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


class FakeRecorder:
    """Lightweight fake recorder that captures ``run_command`` call arguments.

    Avoids real PTY for CA5 (filter tests).  Tracks each ``(display_cmd,
    actual_cmd)`` pair passed to ``run_command`` so tests can assert on which
    lines were replayed and which were suppressed.
    """

    def __init__(self) -> None:
        self.run_command_calls: list[tuple[str, str]] = []
        self._open = False

    def open(self) -> None:
        self._open = True

    def close(self) -> None:
        self._open = False

    def run_command(self, display_cmd: str, actual_cmd: str, **kwargs: Any) -> str:
        self.run_command_calls.append((display_cmd, actual_cmd))
        return ""

    def silent_setup(self, command: str, **kwargs: Any) -> None:
        pass

    def pause(self, seconds: float) -> None:
        pass

    def build(self, title: str = "") -> "FakeRecording":
        return FakeRecording()


class FakeRecording:
    """Minimal stand-in for ``CastRecording`` returned by ``FakeRecorder.build``."""

    def strip_progress(self) -> "FakeRecording":
        return self

    def sanitize(self, replacements: dict) -> "FakeRecording":
        return self

    def truncate_digests(self) -> "FakeRecording":
        return self

    def realign_tables(self) -> "FakeRecording":
        return self

    def auto_height(self, **kwargs: Any) -> "FakeRecording":
        return self

    def write(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('{"version":2}\n')


# ---------------------------------------------------------------------------
# CA1 — cast: false (or absent) ⇒ maybe_record_cast returns None, no .cast
# ---------------------------------------------------------------------------


def test_ca1_cast_false_returns_none_and_writes_no_file(tmp_path: Path) -> None:
    """CA1: script with ``# cast: false`` ⇒ maybe_record_cast returns None.

    The function must not write any ``.cast`` file to the casts_dir.

    Design ref: §4 CA1 — 'script with # cast: false (or absent): no .cast written'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "no_cast.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # cast: false
        ocx package install --select "$PKG_UV"
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()

    result = maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    assert result is None, "CA1: cast: false must return None"
    cast_files = list(casts_dir.glob("*.cast"))
    assert not cast_files, f"CA1: no .cast must be written; found: {cast_files}"


def test_ca1_cast_absent_returns_none_and_writes_no_file(tmp_path: Path) -> None:
    """CA1: script without any ``# cast:`` header ⇒ maybe_record_cast returns None.

    Absence of the header defaults to cast=False (design spec §1.1 default).

    Design ref: §4 CA1 — '(or absent)'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "cast_absent.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        ocx package install --select "$PKG_UV"
        """,
    )
    meta = parse_doc_header(script)
    assert meta.cast is False, "precondition: absent # cast: defaults to False"

    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()

    result = maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    assert result is None, "CA1: absent cast must return None"
    assert not list(casts_dir.glob("*.cast")), "CA1: no .cast must be written"


# ---------------------------------------------------------------------------
# CA2 — cast: true + # doc: ⇒ <flat-slug>.cast; no # doc: ⇒ <stem>.cast
# ---------------------------------------------------------------------------


def test_ca2_cast_true_with_doc_slug_writes_nested_slug_cast(tmp_path: Path) -> None:
    """CA2: # cast: true + # doc: a/b ⇒ writes <casts_dir>/a/b.cast (nested).

    LDR 2026-05-17: slug ``/`` is the directory separator (same rule as the
    nested PT2 scheme / ADR Decision D) — no ``__`` flattening.

    Design ref: §4 CA2 / §6i — '.cast written at the nested slug path'.
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "with_slug.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: getting-started/install-select
        # cast: true
        # title: Install and select
        set -euo pipefail
        # region cast
        ocx package install --select "$PKG_UV"
        ocx package which "$REPO_UV"
        # endregion cast
        out=$(ocx package exec "$PKG_UV" -- uv --version)
        [[ "$out" == *"uv"* ]] || exit 1
        """,
    )
    meta = parse_doc_header(script)
    assert meta.cast is True, "precondition: # cast: true"
    assert meta.doc == "getting-started/install-select", "precondition: doc slug set"

    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    # display_map() must return the expected shape: (sanitize_map, repo_map)
    fake_provider.display_map.return_value = ({}, {})

    result = maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    expected_cast = casts_dir / "getting-started" / "install-select.cast"
    assert result == expected_cast, (
        f"CA2: expected nested cast path {expected_cast}, got {result}"
    )
    assert expected_cast.exists(), (
        f"CA2: expected .cast file to exist at {expected_cast}"
    )


def test_ca2_cast_true_without_doc_writes_stem_named_cast(tmp_path: Path) -> None:
    """CA2: # cast: true with no # doc: ⇒ writes <casts_dir>/<stem>.cast.

    Demo-only casts (no prose binding) keep stem-named .cast.

    Design ref: §4 CA2 — 'A # cast: true script with no # doc: (demo-only cast)
    keeps stem-named .cast.'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "demo_only.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # cast: true
        # title: Demo only cast
        # region cast
        ocx package install --select "$PKG_UV"
        # endregion cast
        """,
    )
    meta = parse_doc_header(script)
    assert meta.cast is True, "precondition: # cast: true"
    assert meta.doc is None, "precondition: no # doc: header"

    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    result = maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    expected_cast = casts_dir / "demo_only.cast"
    assert result == expected_cast, (
        f"CA2: stem fallback: expected {expected_cast}, got {result}"
    )
    assert expected_cast.exists(), (
        f"CA2: stem-named .cast must exist at {expected_cast}"
    )


def test_ca2_slug_multi_segment_nested_dirs(tmp_path: Path) -> None:
    """CA2: a slug with multiple / segments maps to nested directories.

    E.g. ``user-guide/env/compose`` ⇒ ``user-guide/env/compose.cast``
    (LDR 2026-05-17 — slug ``/`` = dir separator; no ``__`` flatten).

    Design ref: §4 CA2 / §6i nested-path scheme.
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "multi_segment.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: user-guide/env/compose
        # cast: true
        # region cast
        ocx package install --select "$PKG_UV"
        # endregion cast
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    result = maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    expected_cast = casts_dir / "user-guide" / "env" / "compose.cast"
    assert result == expected_cast, (
        f"CA2: multi-segment slug → nested dirs; expected {expected_cast}, got {result}"
    )


# ---------------------------------------------------------------------------
# CA3 — verify path (run_doc_script) NEVER writes a .cast (EX8 re-asserted)
# ---------------------------------------------------------------------------


def test_ca3_run_doc_script_never_writes_cast() -> None:
    """CA3: run_doc_script source must NOT reference cast_layer or maybe_record_cast.

    EX8 (runtime coverage with a real OcxRunner + registry) lives in
    ``test_doc_scripts_executor.py::test_ex8_cast_true_runs_as_normal_acceptance``.
    That test calls run_doc_script with live fixtures and asserts no .cast is
    produced.

    This test covers the complementary *structural* invariant: the
    verify-path executor (``run_doc_script``) must not contain any import or
    call to the cast layer (``recordings.cast_layer`` / ``maybe_record_cast``).
    A structural gate catches any accidental import that would be masked by a
    mocked module in a runtime test.

    Design ref: §4 CA3 — 'the same script run on the verify path: no .cast written (EX8)'
    """
    import inspect
    from src.doc_scripts import run_doc_script

    source = inspect.getsource(run_doc_script)

    assert "cast_layer" not in source, (
        "CA3: run_doc_script source contains 'cast_layer' — the verify-path "
        "executor must never import or call the cast layer (EX8 / CA3)"
    )
    assert "maybe_record_cast" not in source, (
        "CA3: run_doc_script source contains 'maybe_record_cast' — the verify-path "
        "executor must never call the cast recording function (EX8 / CA3)"
    )


# ---------------------------------------------------------------------------
# CA4 — same provider/state as drift gate; display_map drives rewrites
# ---------------------------------------------------------------------------


def test_ca4_cast_layer_calls_provider_display_map(tmp_path: Path) -> None:
    """CA4: maybe_record_cast uses the passed provider's display_map, not SETUPS.

    The cast layer must call ``provider.display_map()`` to obtain the
    sanitise_map and repo_map for output rewriting.  It must NOT import or
    consult ``recordings.setups.SETUPS`` directly.

    Verified by passing a MagicMock provider and asserting ``display_map()``
    was called; also asserting SETUPS was never accessed during the call.

    Design ref: §4 CA4 — 'uses the same adapter state as the drift gate
    (no second state definition)'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "ca4_display_map.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: user-guide/env-compose
        # cast: true
        # region cast
        ocx package install --select "$PKG_UV"
        # endregion cast
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()

    # Provider mock — display_map returns a concrete pair so rewrite logic can run
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = (
        {"t_abc123_uv": "uv"},  # sanitize_map: actual_repo → display_name
        {"uv": "t_abc123_uv"},  # repo_map: display_name → actual_repo
    )

    # Patch SETUPS to detect any access
    with patch("recordings.setups.SETUPS", new_callable=dict) as mock_setups:
        maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

        # SETUPS must NOT have been accessed
        assert not mock_setups, (
            "CA4: cast layer must not access SETUPS directly; "
            f"found access: {list(mock_setups.keys())}"
        )

    # display_map() must have been called (drives output sanitisation)
    # raises AssertionError if never called
    fake_provider.display_map.assert_called()


def test_ca4_actual_repo_names_come_from_provider_display_map(
    tmp_path: Path,
    ocx: "OcxRunner",  # noqa: F821 — forward ref; resolved at runtime
) -> None:
    """CA4 (integration): actual repo names in recorder commands derive from provider.

    Provisions a real setup:basic state, then calls maybe_record_cast with
    that provider and a FakeRecorder.  The actual_cmd passed to run_command
    must contain the UUID-prefixed repo name from ``provider.display_map()``,
    not a hardcoded or display-name version.

    Design ref: §4 CA4 — same adapter state; §3 SP4 display_map drives command rewriting.
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header
    from src.state_providers import resolve_state

    script = _write_script(
        tmp_path,
        "ca4_integration.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: getting-started/install
        # cast: true
        # region cast
        ocx package install --select uv
        # endregion cast
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    # Provision via real provider (requires registry)
    provider = resolve_state("setup:basic")
    provider.provision(ocx, tmp_path)

    # display_map must now contain actual UUID-prefixed repo names
    sanitize_map, repo_map = provider.display_map()
    assert repo_map, (
        "CA4: provider.display_map() must return non-empty repo_map after provision"
    )

    fake_recorder = FakeRecorder()
    maybe_record_cast(meta, provider, fake_recorder, casts_dir)

    # Verify the recorder received commands with actual (UUID-prefixed) repo names
    # The display-name "uv" in the script must be rewritten to the actual repo.
    for display_cmd, actual_cmd in fake_recorder.run_command_calls:
        for display_name, actual_repo in repo_map.items():
            if display_name in display_cmd:
                assert actual_repo in actual_cmd or actual_cmd == display_cmd, (
                    f"CA4: display name {display_name!r} in display_cmd {display_cmd!r} "
                    f"must be rewritten to actual repo {actual_repo!r} in actual_cmd "
                    f"{actual_cmd!r} — cast layer must use provider.display_map()"
                )


# ---------------------------------------------------------------------------
# CA5 — cast recorder sees ONLY the # region cast lines; scaffolding excluded
# ---------------------------------------------------------------------------


def test_ca5_only_region_lines_sent_to_recorder(tmp_path: Path) -> None:
    """CA5: recorder.run_command receives ONLY the two lines inside # region cast.

    The script has:
    - ``set -euo pipefail``              (outside region — must NOT be sent)
    - ``# region cast``
    - ``ocx package install --select uv``  (inside region — must be sent)
    - ``ocx package which uv``             (inside region — must be sent)
    - ``# endregion cast``
    - ``out=$(ocx package exec ...)``    (outside — $() capture, must NOT be sent)
    - ``[[ "$out" == *"uv"* ]] || exit 1`` (outside — assertion, must NOT be sent)

    Exactly two run_command calls must be made, one per in-region line.

    Design ref: §1.3, §4 CA5 — 'the PTY replay sees only the lines inside the
    region; lines outside (set -euo pipefail, $(…) captures, [[ ]] assertions)
    are never sent to the recorder — no hang, no test-scaffold leakage'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "ca5_region_filter.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: getting-started/install-select
        # cast: true
        # title: Install and select a tool
        set -euo pipefail

        # region cast
        ocx package install --select uv
        ocx package which uv
        # endregion cast

        out=$(ocx package exec uv:0.10 -- uv --version)
        [[ "$out" == *"uv 0.10"* ]] || { echo "unexpected: $out" >&2; exit 1; }
        """,
    )
    meta = parse_doc_header(script)
    assert meta.cast is True, "precondition: # cast: true"
    assert meta.cast_region is not None, "precondition: cast_region parsed"

    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    calls = fake_recorder.run_command_calls
    assert len(calls) == 2, (
        f"CA5: expected exactly 2 run_command calls (one per region line); "
        f"got {len(calls)}: {calls}"
    )

    # The two in-region commands must be present (display form may be rewritten
    # but the command text must contain the original line content).
    display_cmds = [display for display, _ in calls]
    assert any("ocx package install" in cmd for cmd in display_cmds), (
        f"CA5: first region line 'ocx package install ...' must be in recorder calls; "
        f"got display_cmds: {display_cmds}"
    )
    assert any("ocx package which" in cmd for cmd in display_cmds), (
        f"CA5: second region line 'ocx package which ...' must be in recorder calls; "
        f"got display_cmds: {display_cmds}"
    )


def test_ca5_set_euo_pipefail_not_sent_to_recorder(tmp_path: Path) -> None:
    """CA5: ``set -euo pipefail`` (outside cast region) is never sent to recorder.

    If the cast layer naively replayed the whole script body through the PTY,
    ``set -euo pipefail`` would be sent, potentially causing the PTY shell to
    exit immediately on any non-zero command.  The cast layer must filter it out.

    Design ref: §1.3 — 'Everything outside — set -euo pipefail, $(…) capture,
    [[ … ]] || exit 1 assertions — runs in the drift gate but is never in the cast'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "ca5_no_set_euo.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: in-depth/env
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install --select uv
        # endregion cast

        [[ -n "$PKG_UV" ]] || exit 1
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    all_cmds = [display for display, _ in fake_recorder.run_command_calls]
    set_euo_sent = any("set -euo" in cmd for cmd in all_cmds)
    assert not set_euo_sent, (
        f"CA5: 'set -euo pipefail' must never be sent to the recorder; "
        f"found in calls: {all_cmds}"
    )


def test_ca5_subshell_capture_not_sent_to_recorder(tmp_path: Path) -> None:
    """CA5: ``out=$(…)`` subshell captures outside the region are never sent.

    Sending ``out=$(ocx ...)`` to a PTY would cause the shell to block waiting
    for the inner command to complete in a subshell — a known hang path that the
    cast-region design specifically prevents.

    Design ref: §1.3 — 'multiline constructs, heredocs, or assertion scaffolding
    without hanging or leaking test-only commands into the demo'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "ca5_no_subshell.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: getting-started/exec
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install --select uv
        # endregion cast

        out=$(ocx package exec uv -- uv --version)
        [[ "$out" == *"uv"* ]] || exit 1
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    all_cmds = [display for display, _ in fake_recorder.run_command_calls]
    subshell_sent = any("$(" in cmd for cmd in all_cmds)
    assert not subshell_sent, (
        f"CA5: '$(' subshell captures must never be sent to the recorder; "
        f"found in calls: {all_cmds}"
    )


def test_ca5_assertion_scaffolding_not_sent_to_recorder(tmp_path: Path) -> None:
    """CA5: ``[[ … ]] || exit 1`` assertion lines outside region are never sent.

    These lines are drift-gate-only test scaffolding and must never leak into
    the cast (would produce confusing demo output).

    Design ref: §4 CA5 — 'no test-scaffold leakage into the cast'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    script = _write_script(
        tmp_path,
        "ca5_no_assertion.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: in-depth/install
        # cast: true
        set -euo pipefail

        # region cast
        ocx package install --select uv
        # endregion cast

        [[ -n "$PKG_UV" ]] || { echo "PKG_UV not set" >&2; exit 1; }
        """,
    )
    meta = parse_doc_header(script)
    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    all_cmds = [display for display, _ in fake_recorder.run_command_calls]
    assertion_sent = any("[[" in cmd for cmd in all_cmds)
    assert not assertion_sent, (
        f"CA5: '[[ … ]] || exit 1' assertions must never be sent to the recorder; "
        f"found in calls: {all_cmds}"
    )


def test_ca5_exact_region_line_count_regardless_of_body_size(tmp_path: Path) -> None:
    """CA5: regardless of total script body size, recorder call count == region line count.

    Script has 1 in-region line and 10 out-of-region lines.  Exactly 1
    run_command call expected.

    Design ref: §4 CA5 — 'one PTY command per line; [scaffolding] never sent'
    """
    from recordings.cast_layer import maybe_record_cast  # type: ignore[import]
    from src.doc_scripts import parse_doc_header

    # 1 in-region line; many out-of-region lines
    script = _write_script(
        tmp_path,
        "ca5_count.sh",
        """\
        #!/usr/bin/env bash
        # state: setup:basic
        # doc: in-depth/count
        # cast: true
        set -euo pipefail
        export SETUP_VAR=hello
        echo "running setup"

        # region cast
        ocx package install --select uv
        # endregion cast

        result=$(ocx package which uv)
        [[ -n "$result" ]] || exit 1
        echo "done"
        """,
    )
    meta = parse_doc_header(script)
    assert meta.cast_region is not None

    casts_dir = tmp_path / "casts"
    casts_dir.mkdir()

    fake_recorder = FakeRecorder()
    fake_provider = MagicMock()
    fake_provider.display_map.return_value = ({}, {})

    maybe_record_cast(meta, fake_provider, fake_recorder, casts_dir)

    calls = fake_recorder.run_command_calls
    assert len(calls) == 1, (
        f"CA5: script has 1 region line; expected 1 recorder call, got {len(calls)}: {calls}"
    )


# ---------------------------------------------------------------------------
# Module structure assertions (parity with other doc-script test modules)
# ---------------------------------------------------------------------------


def test_module_has_win32_skip_mark() -> None:
    """This module carries pytestmark = pytest.mark.skipif(win32, ...) (EX7 parity).

    Assertable on any platform — proves structural parity with
    test_scenarios_smoke.py and test_doc_scripts_executor.py without
    requiring Windows execution.
    """
    import importlib

    self_module = importlib.import_module("tests.test_doc_scripts_cast")
    mark = getattr(self_module, "pytestmark", None)
    assert mark is not None, "module must have pytestmark attribute"

    marks = mark if isinstance(mark, list) else [mark]
    skipif_marks = [m for m in marks if getattr(m.mark, "name", None) == "skipif"]
    assert skipif_marks, "module pytestmark must include a skipif mark (EX7 parity)"

    reasons = [m.kwargs.get("reason", "") for m in skipif_marks]
    assert any(
        "win32" in r.lower() or "windows" in r.lower() for r in reasons
    ), f"skipif reason must mention win32/Windows; got: {reasons}"
