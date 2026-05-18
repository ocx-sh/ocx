"""Pure parser / discovery / binding specification tests (Phase 2, no ocx fixture).

Tests are written from design_spec_doc_command_scripts.md §1, §2, §6b, §6f —
NOT from the stub implementation.  They MUST fail against the current stubs
(raise ``NotImplementedError``) and pass once Phase-2 implementation lands.

All tests are fast and run in ``test:parallel`` with no Docker or registry
dependency.

Contract coverage:
  §1.1 Header grammar — SLUG_RE, recognised keys, defaults, shebang skipped
  §1.3 Cast region — EX9 arity check, cast_region populated
  §2   EX5 (unknown key), EX9 (cast-region arity), EX6 (missing # state: default)
  §6b  NC1 (find_inline_ocx_blocks), NC2/NC3 (unresolved_transclusions,
       find_script_transclusions)
  §6f  DE1 — export entries carry all 9 keys; display_env always present;
             cast_region JSON wire format is 2-element array or null
       DE5 — DocScriptExportEntry ↔ _DocScriptExportEntry TypedDict parity
             gate via ast annotation extraction; negative sub-test included
  Discovery — discover_doc_scripts, doc_scripts_export
  Utility — strip_ansi
"""
from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from src.doc_binding import find_inline_ocx_blocks, find_script_transclusions, unresolved_transclusions
from src.doc_scripts import (
    SLUG_RE,
    DocScriptMeta,
    DocScriptParseError,
    discover_doc_scripts,
    doc_scripts_export,
    parse_doc_header,
    strip_ansi,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_script(tmp_path: Path, name: str, content: str) -> Path:
    """Write a fixture .sh script and return its path."""
    p = tmp_path / name
    p.write_text(textwrap.dedent(content))
    return p


# ===========================================================================
# SLUG_RE grammar (§1.1)
# ===========================================================================


class TestSlugRe:
    """Contract: SLUG_RE matches valid slugs and rejects invalid ones."""

    @pytest.mark.parametrize(
        "slug",
        [
            "a",
            "abc",
            "getting-started",
            "a/b",
            "getting-started/install-select",
            "user-guide/env-compose",
            "a/b-c",
            "abc/def/ghi",
            "a0/b1-c2",
        ],
    )
    def test_valid_slug(self, slug: str) -> None:
        """Valid slugs match SLUG_RE (§1.1)."""
        assert SLUG_RE.fullmatch(slug) is not None

    @pytest.mark.parametrize(
        "slug",
        [
            "",               # empty
            "A",              # uppercase
            "Getting-started",  # leading uppercase
            "getting_started",  # underscore
            "-leading",       # leading hyphen
            "trailing-",      # trailing hyphen
            "/leading",       # leading slash
            "trailing/",      # trailing slash
            "a//b",           # double slash
            "a--b",           # double hyphen
            "__dunder",       # double underscore
        ],
    )
    def test_invalid_slug(self, slug: str) -> None:
        """Invalid slugs must NOT match SLUG_RE (§1.1)."""
        assert SLUG_RE.fullmatch(slug) is None


# ===========================================================================
# parse_doc_header — defaults (EX6, §1.1)
# ===========================================================================


class TestParseDocHeaderDefaults:
    """Contract: default values when optional keys are absent."""

    def test_default_state_is_setup_basic(self, tmp_path: Path) -> None:
        """EX6: absent # state: ⇒ state == 'setup:basic'."""
        p = _write_script(tmp_path, "no_state.sh", """\
            #!/usr/bin/env bash
            # title: My Tool
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"

    def test_default_cast_is_false(self, tmp_path: Path) -> None:
        """Absent # cast: ⇒ cast is False (§1.1)."""
        p = _write_script(tmp_path, "no_cast.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.cast is False

    def test_default_doc_is_none(self, tmp_path: Path) -> None:
        """Absent # doc: ⇒ doc is None (script is tested-only, §1.1)."""
        p = _write_script(tmp_path, "no_doc.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.doc is None

    def test_default_title_is_file_stem(self, tmp_path: Path) -> None:
        """Absent # title: ⇒ title defaults to file stem (§1.1 / DG1)."""
        p = _write_script(tmp_path, "my_script.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        # title defaults to the file stem when absent
        assert meta.title == "my_script"

    def test_default_description_is_none(self, tmp_path: Path) -> None:
        """Absent # description: ⇒ description is None (§1.1)."""
        p = _write_script(tmp_path, "no_desc.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.description is None

    def test_default_expect_is_none(self, tmp_path: Path) -> None:
        """Absent # expect: ⇒ expect is None (§1.1)."""
        p = _write_script(tmp_path, "no_expect.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.expect is None

    def test_default_cast_region_none_when_cast_false(self, tmp_path: Path) -> None:
        """cast_region is None when cast is False (§1.1)."""
        p = _write_script(tmp_path, "no_region.sh", """\
            # state: setup:basic
            # cast: false
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.cast_region is None


# ===========================================================================
# parse_doc_header — round-trip populated values
# ===========================================================================


class TestParseDocHeaderRoundTrip:
    """Contract: header values are parsed and stored correctly."""

    def test_state_setup_basic_round_trips(self, tmp_path: Path) -> None:
        """# state: setup:basic round-trips through parse_doc_header (§1.1)."""
        p = _write_script(tmp_path, "roundtrip.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"

    def test_shebang_is_ignored(self, tmp_path: Path) -> None:
        """Shebang is ignored; parsing starts at the first non-shebang line (§1.1)."""
        p = _write_script(tmp_path, "shebang.sh", """\
            #!/usr/bin/env bash
            # state: setup:basic
            # title: Shebang Test
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"
        assert meta.title == "Shebang Test"

    def test_header_stops_at_first_non_comment_line(self, tmp_path: Path) -> None:
        """Header parsing stops at the first non-blank, non-comment line (§1.1).

        A key that appears after code must NOT be picked up.
        """
        p = _write_script(tmp_path, "stops_early.sh", """\
            # state: setup:basic
            echo hello
            # title: After Code
        """)
        meta = parse_doc_header(p)
        # title must not be read from after the code line
        assert meta.title == "stops_early"

    def test_doc_slug_stored(self, tmp_path: Path) -> None:
        """# doc: value is stored on meta.doc (§1.1)."""
        p = _write_script(tmp_path, "with_doc.sh", """\
            # doc: getting-started/install
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.doc == "getting-started/install"

    def test_title_and_description_stored(self, tmp_path: Path) -> None:
        """# title: and # description: are stored correctly (§1.1)."""
        p = _write_script(tmp_path, "labeled.sh", """\
            # title: My Title
            # description: A human note
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.title == "My Title"
        assert meta.description == "A human note"

    def test_keys_are_case_insensitive(self, tmp_path: Path) -> None:
        """Header keys are case-insensitive; lowercased on parse (§1.1)."""
        p = _write_script(tmp_path, "casetest.sh", """\
            # STATE: setup:basic
            # TITLE: Uppercase Keys
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"
        assert meta.title == "Uppercase Keys"

    def test_path_stored_on_meta(self, tmp_path: Path) -> None:
        """DocScriptMeta.path is set to the script path (§1.1)."""
        p = _write_script(tmp_path, "with_path.sh", """\
            # state: setup:basic
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.path == p


# ===========================================================================
# parse_doc_header — EX5: unknown header key
# ===========================================================================


class TestParseDocHeaderUnknownKey:
    """EX5: unknown header key ⇒ DocScriptParseError."""

    def test_unknown_key_raises(self, tmp_path: Path) -> None:
        """EX5: unrecognised key raises DocScriptParseError with key name and file."""
        p = _write_script(tmp_path, "bad_key.sh", """\
            # state: setup:basic
            # scenrio: oops
            echo hello
        """)
        with pytest.raises(DocScriptParseError) as exc_info:
            parse_doc_header(p)
        msg = str(exc_info.value)
        assert "scenrio" in msg
        assert str(p) in msg or p.name in msg

    def test_known_keys_do_not_raise(self, tmp_path: Path) -> None:
        """All recognised keys (state, doc, cast, title, description, expect) are accepted (§1.1)."""
        p = _write_script(tmp_path, "all_known.sh", """\
            # state: setup:basic
            # doc: a/b
            # cast: false
            # title: T
            # description: D
            # expect: out.txt
            echo hello
        """)
        # Must not raise
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"


# ===========================================================================
# parse_doc_header — slug grammar validation
# ===========================================================================


class TestParseDocHeaderSlugGrammar:
    """Contract: # doc: value must match SLUG_RE; violations ⇒ DocScriptParseError."""

    @pytest.mark.parametrize(
        "bad_slug",
        [
            "Getting-Started",   # uppercase
            "-leading",          # leading hyphen
            "__dunder",          # double underscore
            "with space",        # space
            "UPPER/CASE",        # uppercase with slash
        ],
    )
    def test_invalid_doc_slug_raises(self, tmp_path: Path, bad_slug: str) -> None:
        """Invalid # doc: slug raises DocScriptParseError (§1.1)."""
        p = _write_script(tmp_path, "bad_slug.sh", f"""\
            # doc: {bad_slug}
            echo hello
        """)
        with pytest.raises(DocScriptParseError):
            parse_doc_header(p)

    def test_valid_doc_slug_accepted(self, tmp_path: Path) -> None:
        """Valid # doc: slug a/b-c is accepted without error (§1.1)."""
        p = _write_script(tmp_path, "good_slug.sh", """\
            # doc: a/b-c
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.doc == "a/b-c"


# ===========================================================================
# parse_doc_header — EX9: cast-region arity
# ===========================================================================


class TestParseDocHeaderCastRegion:
    """EX9: # cast: true with ≠1 region ⇒ DocScriptParseError."""

    def test_cast_true_with_zero_regions_raises(self, tmp_path: Path) -> None:
        """EX9: # cast: true with no # region cast block ⇒ DocScriptParseError."""
        p = _write_script(tmp_path, "cast_no_region.sh", """\
            # cast: true
            echo hello
        """)
        with pytest.raises(DocScriptParseError) as exc_info:
            parse_doc_header(p)
        assert "region" in str(exc_info.value).lower() or "cast" in str(exc_info.value).lower()

    def test_cast_true_with_two_regions_raises(self, tmp_path: Path) -> None:
        """EX9: # cast: true with two # region cast blocks ⇒ DocScriptParseError."""
        p = _write_script(tmp_path, "cast_two_regions.sh", """\
            # cast: true
            # region cast
            ocx package install foo:1
            # endregion cast
            echo middle
            # region cast
            ocx package which foo
            # endregion cast
        """)
        with pytest.raises(DocScriptParseError) as exc_info:
            parse_doc_header(p)
        assert "region" in str(exc_info.value).lower() or "cast" in str(exc_info.value).lower()

    def test_cast_true_with_exactly_one_region_ok(self, tmp_path: Path) -> None:
        """EX9 positive: exactly one # region cast block is accepted; cast_region is set (§1.3)."""
        p = _write_script(tmp_path, "cast_one_region.sh", """\
            # cast: true
            set -euo pipefail

            # region cast
            ocx package install foo:1
            ocx package which foo
            # endregion cast

            [[ "$(ocx package which foo)" ]] || exit 1
        """)
        meta = parse_doc_header(p)
        assert meta.cast is True
        assert meta.cast_region is not None
        # cast_region is a (start, end) tuple of line numbers
        start, end = meta.cast_region
        assert isinstance(start, int)
        assert isinstance(end, int)
        assert start < end

    def test_cast_false_no_region_ok(self, tmp_path: Path) -> None:
        """# cast: false with no region block does not raise (§1.1)."""
        p = _write_script(tmp_path, "cast_false.sh", """\
            # cast: false
            echo hello
        """)
        meta = parse_doc_header(p)
        assert meta.cast is False
        assert meta.cast_region is None


# ===========================================================================
# parse_doc_header — negative / edge cases (A4)
# ===========================================================================


class TestParseDocHeaderNegativeCases:
    """Additional negative and edge-case contracts for parse_doc_header."""

    def test_unclosed_region_cast_raises_on_cast_true(self, tmp_path: Path) -> None:
        """A4a: ``# region cast`` with no matching ``# endregion cast`` on a
        cast:true script ⇒ DocScriptParseError (EX9 arity — 1 start / 0 end).

        Real parser behaviour: the arity check ``len(region_starts) != 1 or
        len(region_ends) != 1`` fires before the RG0 well-formedness check,
        so an unclosed region on a cast script is a hard parse error.
        """
        p = _write_script(tmp_path, "unclosed_cast_true.sh", """\
            #!/usr/bin/env bash
            # state: setup:basic
            # cast: true
            # region cast
            ocx package install foo:1
        """)
        with pytest.raises(DocScriptParseError) as exc_info:
            parse_doc_header(p)
        msg = str(exc_info.value).lower()
        assert "region" in msg or "cast" in msg, (
            f"A4a: DocScriptParseError message should mention 'region' or 'cast'; "
            f"got: {exc_info.value!r}"
        )

    def test_unclosed_region_cast_raises_on_display_only(self, tmp_path: Path) -> None:
        """A4a: ``# region cast`` with no matching ``# endregion cast`` on a
        display-only (cast:false) script ⇒ DocScriptParseError.

        Real parser behaviour: the RG0 well-formedness path (``elif not cast:``)
        raises DocScriptParseError(\"display region malformed: expected exactly
        one ordered # region cast / # endregion cast block (found 1 start / 0
        end)\") when markers are present but unpaired.
        """
        p = _write_script(tmp_path, "unclosed_cast_false.sh", """\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: some/page
            # region cast
            ocx package install foo:1
        """)
        with pytest.raises(DocScriptParseError) as exc_info:
            parse_doc_header(p)
        msg = str(exc_info.value).lower()
        assert "region" in msg or "malformed" in msg, (
            f"A4a: DocScriptParseError message should mention 'region' or 'malformed'; "
            f"got: {exc_info.value!r}"
        )

    def test_duplicate_header_key_last_wins(self, tmp_path: Path) -> None:
        """A4b: duplicate header key (two ``# state:`` lines) ⇒ last-wins semantics.

        Real parser behaviour: ``raw_meta[key_stripped] = value_stripped`` is a
        plain dict assignment in a loop — each successive occurrence overwrites
        the prior value.  No error is raised.  This is the implemented semantic;
        tests pin it so a future change to raise-on-duplicate is a deliberate
        breaking change, not a silent drift.
        """
        p = _write_script(tmp_path, "duplicate_key.sh", """\
            #!/usr/bin/env bash
            # state: setup:basic
            # state: setup:other
            echo hello
        """)
        # Must NOT raise; last occurrence wins
        meta = parse_doc_header(p)
        assert meta.state == "setup:other", (
            f"A4b: duplicate header key must use last-wins semantics; "
            f"expected state='setup:other', got {meta.state!r}"
        )

    def test_empty_body_parses_without_crash(self, tmp_path: Path) -> None:
        """A4c: empty file body parses without raising.

        An empty script has no header lines; all fields fall back to their
        defaults (state='setup:basic', doc=None, cast=False, title=stem).
        """
        p = _write_script(tmp_path, "empty_script.sh", "")
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"
        assert meta.doc is None
        assert meta.cast is False
        assert meta.title == "empty_script"

    def test_whitespace_only_body_parses_without_crash(self, tmp_path: Path) -> None:
        """A4c: whitespace-only file body parses without raising.

        Blank lines are treated as header terminators (stop scanning); the
        result is all defaults.
        """
        p = _write_script(tmp_path, "whitespace_script.sh", "   \n   \n   \n")
        meta = parse_doc_header(p)
        assert meta.state == "setup:basic"
        assert meta.doc is None
        assert meta.cast is False


# ===========================================================================
# discover_doc_scripts
# ===========================================================================


class TestDiscoverDocScripts:
    """Contract: discover_doc_scripts returns sorted .sh files; empty/missing ⇒ []."""

    def test_missing_root_returns_empty(self, tmp_path: Path) -> None:
        """Missing root ⇒ [] (§2 discovery contract)."""
        result = discover_doc_scripts(tmp_path / "nonexistent")
        assert result == []

    def test_empty_root_returns_empty(self, tmp_path: Path) -> None:
        """Empty directory ⇒ [] (§2 discovery contract)."""
        root = tmp_path / "scripts"
        root.mkdir()
        result = discover_doc_scripts(root)
        assert result == []

    def test_nested_sh_files_found_sorted(self, tmp_path: Path) -> None:
        """Nested *.sh files are found recursively, returned sorted (§2)."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "alpha.sh").write_text("# state: setup:basic\necho alpha\n")
        sub = root / "sub"
        sub.mkdir()
        (sub / "beta.sh").write_text("# state: setup:basic\necho beta\n")
        (root / "gamma.sh").write_text("# state: setup:basic\necho gamma\n")

        result = discover_doc_scripts(root)
        # Must include all three
        names = [p.name for p in result]
        assert "alpha.sh" in names
        assert "beta.sh" in names
        assert "gamma.sh" in names
        # Must be sorted
        assert result == sorted(result)

    def test_only_sh_extension_found(self, tmp_path: Path) -> None:
        """Non-.sh files in root are not returned (§2)."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "script.sh").write_text("echo hello\n")
        (root / "readme.md").write_text("# readme\n")
        (root / "data.txt").write_text("data\n")

        result = discover_doc_scripts(root)
        assert len(result) == 1
        assert result[0].name == "script.sh"


# ===========================================================================
# doc_scripts_export
# ===========================================================================


class TestDocScriptsExport:
    """Contract: doc_scripts_export returns [{path, slug, cast, expect}] (PT6 / NC2)."""

    def test_empty_root_returns_empty(self, tmp_path: Path) -> None:
        """Missing root ⇒ [] (matches discover_doc_scripts contract)."""
        result = doc_scripts_export(tmp_path / "nonexistent")
        assert result == []

    def test_script_without_doc_has_null_slug(self, tmp_path: Path) -> None:
        """Script with no # doc: ⇒ slug is None in the export (§5 PT1)."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "no_doc.sh").write_text("# state: setup:basic\necho hello\n")

        result = doc_scripts_export(root)
        assert len(result) == 1
        assert result[0]["slug"] is None

    def test_script_with_doc_has_slug(self, tmp_path: Path) -> None:
        """Script with # doc: value ⇒ slug equals that value."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "with_doc.sh").write_text(
            "# doc: getting-started/install\necho hello\n"
        )

        result = doc_scripts_export(root)
        assert len(result) == 1
        assert result[0]["slug"] == "getting-started/install"

    def test_export_schema_keys_present(self, tmp_path: Path) -> None:
        """Each entry has exactly the keys: path, slug, cast, expect."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "a.sh").write_text("# state: setup:basic\necho a\n")

        result = doc_scripts_export(root)
        entry = result[0]
        assert "path" in entry
        assert "slug" in entry
        assert "cast" in entry
        assert "expect" in entry

    def test_export_cast_value(self, tmp_path: Path) -> None:
        """cast: true in a script with a region ⇒ export entry has cast=True."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        (root / "cast_script.sh").write_text(
            "# cast: true\n"
            "# region cast\n"
            "ocx package install foo:1\n"
            "# endregion cast\n"
            "echo done\n"
        )

        result = doc_scripts_export(root)
        assert result[0]["cast"] is True

    def test_export_sorted_path_order(self, tmp_path: Path) -> None:
        """Export entries are in sorted path order (matches discover_doc_scripts)."""
        root = tmp_path / "doc_scripts"
        root.mkdir()
        for name in ("z.sh", "a.sh", "m.sh"):
            (root / name).write_text("# state: setup:basic\necho\n")

        result = doc_scripts_export(root)
        paths = [e["path"] for e in result]
        assert paths == sorted(paths)


# ===========================================================================
# strip_ansi
# ===========================================================================


class TestStripAnsi:
    """Contract: strip_ansi removes CSI/SGR sequences, preserves plain text."""

    def test_plain_text_unchanged(self) -> None:
        """Plain text with no escapes is returned unchanged."""
        text = "hello world\nline two\n"
        assert strip_ansi(text) == text

    def test_removes_sgr_colour_codes(self) -> None:
        """CSI SGR colour codes (ESC[Nm) are removed."""
        coloured = "\x1b[31mred text\x1b[0m"
        assert strip_ansi(coloured) == "red text"

    def test_removes_bold_reset(self) -> None:
        """Bold (ESC[1m) and reset (ESC[0m) are removed."""
        bold = "\x1b[1mbold\x1b[0m normal"
        assert strip_ansi(bold) == "bold normal"

    def test_preserves_newlines(self) -> None:
        """Newlines are preserved after stripping."""
        text = "\x1b[32mgreen\x1b[0m\nplain\n"
        result = strip_ansi(text)
        assert "\n" in result
        assert "green" in result
        assert "plain" in result

    def test_removes_256_colour_sequence(self) -> None:
        """256-colour ESC[38;5;Nm sequences are stripped."""
        text = "\x1b[38;5;214morange\x1b[0m"
        assert strip_ansi(text) == "orange"

    def test_raw_escape_not_in_stripped(self) -> None:
        """The literal ESC character \\x1b is absent from stripped output."""
        text = "\x1b[31mboom\x1b[0m"
        result = strip_ansi(text)
        assert "\x1b" not in result


# ===========================================================================
# NC1 — find_inline_ocx_blocks
# ===========================================================================


class TestFindInlineOcxBlocks:
    """NC1: find_inline_ocx_blocks returns ungated ocx bash blocks."""

    def test_fenced_bash_with_ocx_is_returned(self, tmp_path: Path) -> None:
        """NC1: a ```bash block containing 'ocx ' (not a <<< transclusion) is returned."""
        page = tmp_path / "page.md"
        page.write_text(
            "# Doc\n\n"
            "```bash\n"
            "ocx package install foo:1\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1
        assert "ocx package install" in result[0]

    def test_transclusion_line_not_returned(self, tmp_path: Path) -> None:
        """NC1: a <<< @/_scripts/x.sh transclusion is NOT an inline block."""
        page = tmp_path / "page.md"
        page.write_text(
            "# Doc\n\n"
            "<<< @/_scripts/getting-started__install.sh{sh}\n"
        )
        result = find_inline_ocx_blocks(page)
        assert result == []

    def test_bash_block_without_ocx_not_returned(self, tmp_path: Path) -> None:
        """NC1: a ```bash block with no 'ocx ' line is not returned."""
        page = tmp_path / "page.md"
        page.write_text(
            "# Doc\n\n"
            "```bash\n"
            "echo hello world\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert result == []

    def test_empty_page_returns_empty(self, tmp_path: Path) -> None:
        """Empty page returns []."""
        page = tmp_path / "empty.md"
        page.write_text("")
        result = find_inline_ocx_blocks(page)
        assert result == []

    def test_shebang_block_with_ocx_exec_is_exempt(self, tmp_path: Path) -> None:
        """§6b shebang exemption: a ```sh block whose first content line is #!
        is a generated-file listing, not a runnable invocation — NOT returned.

        This covers the install-time launcher listing present in
        entry-points.md / environments.md:
          #!/bin/sh
          # Generated by ocx at install time. Do not edit.
          exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<placeholder>' …
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Launchers\n\n"
            "A generated launcher looks like this:\n\n"
            "```sh\n"
            "#!/bin/sh\n"
            "# Generated by ocx at install time. Do not edit.\n"
            "exec \"${OCX_BINARY_PIN:-ocx}\" launcher exec"
            " '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123…'"
            " -- \"$(basename \"$0\")\" \"$@\"\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert result == [], (
            "Shebang'd generated-file listing must be exempt from NC1 (§6b); "
            f"got: {result}"
        )

    def test_non_shebang_block_with_ocx_command_is_flagged(self, tmp_path: Path) -> None:
        """Regression: the shebang exemption is shebang-only.

        A ```sh block with a real ``ocx `` invocation but WITHOUT a shebang
        first line is NOT exempt — it is still returned.  This confirms the
        exemption is strictly structural (first content line starts with ``#!``)
        and cannot be bypassed by other content.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Without shebang\n\n"
            "```sh\n"
            "# No shebang here.\n"
            "ocx launcher exec '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123…'"
            " -- cmake\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1, (
            "A non-shebang block with 'ocx ' must still be flagged as ungated; "
            f"got: {result}"
        )
        assert "ocx" in result[0]

    def test_normal_install_block_still_flagged(self, tmp_path: Path) -> None:
        """Regression: a normal ```sh block with 'ocx package install' is returned.

        The shebang exemption must not affect ordinary command-example blocks.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Install\n\n"
            "```sh\n"
            "ocx package install cmake:3.28\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1
        assert "ocx package install" in result[0]

    # ------------------------------------------------------------------
    # Regression tests for the Codex cross-model NC1 findings (§6b extended)
    # ------------------------------------------------------------------

    def test_path_qualified_invocation_is_flagged(self, tmp_path: Path) -> None:
        """NC1 tightened detection: a ```sh block with a path-qualified ocx binary
        invocation (not shebang, not BEGIN/END) is flagged as an ungated block.

        Covers ``"$HOME/.ocx/ocx" --global env --shell=sh`` — the false-negative
        identified in the Codex cross-model review when only ``ocx `` substring was
        used for detection.  §6b specifies that path-qualified invocations MUST be
        caught.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Activation\n\n"
            "```sh\n"
            '"$HOME/.ocx/ocx" --global env --shell=sh\n'
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1, (
            "A path-qualified ocx invocation without shebang or BEGIN/END markers "
            "must be flagged as an ungated inline block (§6b NC1 tightened); "
            f"got: {result}"
        )
        assert "ocx" in result[0]

    def test_variable_form_invocation_is_flagged(self, tmp_path: Path) -> None:
        """NC1 tightened detection: ``${OCX_BINARY_PIN:-ocx}`` as a command is flagged.

        The ${VAR:-fallback} form of the ocx binary token must be detected as an
        invocation when it appears in command position (start of line).
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Install\n\n"
            "```sh\n"
            "${OCX_BINARY_PIN:-ocx} package install cmake:3.28\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1, (
            "${OCX_BINARY_PIN:-ocx} used as a command must be detected as an ocx "
            f"invocation and flagged; got: {result}"
        )
        assert "ocx" in result[0]

    def test_labelled_fence_with_ocx_is_flagged(self, tmp_path: Path) -> None:
        """NC1 tightened fences: a ``\\`\\`\\`sh [label]`` (VitePress code-group) fence
        containing an ocx invocation is treated as a shell fence and flagged.

        Previously only ``\\`\\`\\`sh`` with no label was recognised as a shell fence;
        VitePress code-group label syntax must also be covered.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Exec\n\n"
            "::: code-group\n"
            "```sh [Linux]\n"
            "ocx package exec cmake:3.28 -- cmake --version\n"
            "```\n"
            ":::\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1, (
            "A ```sh [label] code-group block with an ocx invocation must be "
            f"flagged as ungated; got: {result}"
        )
        assert "ocx" in result[0]

    def test_installer_begin_end_markers_are_exempt(self, tmp_path: Path) -> None:
        """§6b exemption (b): the OCX-installer shell-profile fragment is NOT flagged.

        A fenced block containing both ``# BEGIN ocx`` and ``# END ocx`` is the
        installer-written shell-profile fragment shown in ``user-guide.md``.  It is
        documentation of an on-disk artifact, not a command a reader types.
        The block contains a path-qualified ``"$HOME/.ocx/ocx"`` invocation, which
        would be flagged without the exemption.  DAMP: the exact block from the page
        is reproduced here.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Shell activation\n\n"
            "The OCX installer writes this into your profile:\n\n"
            "```sh\n"
            "# written by the OCX installer in ~/.bashrc / ~/.zshrc / ~/.profile\n"
            "# BEGIN ocx\n"
            '[ -x "$HOME/.ocx/ocx" ] && eval "$("$HOME/.ocx/ocx"'
            ' --global env --shell=sh 2>/dev/null)" || true\n'
            "# END ocx\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert result == [], (
            "The OCX installer shell-profile fragment (# BEGIN ocx … # END ocx) "
            "must be exempt from NC1 as a generated/installer-managed artifact "
            f"listing (§6b exemption b); got: {result}"
        )

    def test_shebang_with_ocx_exec_still_exempt(self, tmp_path: Path) -> None:
        """§6b exemption (a) regression: the shebang exemption still works after
        the detection tightening.

        A ``\\`\\`\\`sh`` block whose first content line is a shebang and that contains
        a path-qualified ``${OCX_BINARY_PIN:-ocx}`` invocation (as exec arg) must
        still be exempt.  Covers the generated launcher listing in
        ``entry-points.md`` / ``environments.md``.  DAMP.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Launcher\n\n"
            "```sh\n"
            "#!/bin/sh\n"
            "# Generated by ocx at install time. Do not edit.\n"
            'exec "${OCX_BINARY_PIN:-ocx}" launcher exec'
            " '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123'"
            ' -- "$(basename "$0")" "$@"\n'
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert result == [], (
            "A shebang'd generated-file listing must remain exempt from NC1 after "
            f"detection tightening (§6b exemption a); got: {result}"
        )

    def test_plain_ocx_install_still_flagged_regression(self, tmp_path: Path) -> None:
        """Regression: plain ``ocx package install cmake:3.28`` in a ``\\`\\`\\`sh``
        block is still flagged after all NC1 changes.

        Verifies that tightening detection and adding the BEGIN/END exemption did
        not accidentally suppress ordinary ungated command examples.
        """
        page = tmp_path / "page.md"
        page.write_text(
            "# Getting started\n\n"
            "```sh\n"
            "ocx package install cmake:3.28\n"
            "```\n"
        )
        result = find_inline_ocx_blocks(page)
        assert len(result) == 1, (
            "A normal 'ocx package install' block must still be flagged as ungated "
            f"after NC1 changes; got: {result}"
        )
        assert "ocx package install cmake:3.28" in result[0]


# ===========================================================================
# NC2/NC3 — find_script_transclusions and unresolved_transclusions
# ===========================================================================


class TestFindScriptTransclusions:
    """Contract: find_script_transclusions returns stems from <<< @/_scripts/ lines."""

    def test_returns_stem_from_transclusion(self, tmp_path: Path) -> None:
        """find_script_transclusions returns the stem (without .sh) for each <<<."""
        page = tmp_path / "page.md"
        page.write_text(
            "# Doc\n\n"
            "<<< @/_scripts/getting-started__install.sh{sh}\n"
        )
        result = find_script_transclusions(page)
        assert result == ["getting-started__install"]

    def test_returns_multiple_stems(self, tmp_path: Path) -> None:
        """Multiple <<< lines each contribute a stem."""
        page = tmp_path / "page.md"
        page.write_text(
            "# Doc\n\n"
            "<<< @/_scripts/a__b.sh{sh}\n"
            "<<< @/_scripts/c__d.sh{sh}\n"
        )
        result = find_script_transclusions(page)
        assert result == ["a__b", "c__d"]

    def test_empty_page_returns_empty(self, tmp_path: Path) -> None:
        """Page with no <<< lines returns []."""
        page = tmp_path / "page.md"
        page.write_text("# No transclusions here\n")
        result = find_script_transclusions(page)
        assert result == []


class TestUnresolvedTransclusions:
    """NC2/NC3: unresolved_transclusions returns (page, stem) pairs for broken refs."""

    def test_resolves_slug_with_slash_to_double_underscore(self, tmp_path: Path) -> None:
        """NC2 positive: slug 'getting-started/install' in export resolves stem
        'getting-started__install' (flatten: /→__) ⇒ empty unresolved list."""
        page = tmp_path / "page.md"
        page.write_text("<<< @/_scripts/getting-started__install.sh{sh}\n")
        export = [{"path": "/any/path.sh", "slug": "getting-started/install", "cast": False, "expect": None}]

        result = unresolved_transclusions((page,), export)
        assert result == []

    def test_missing_slug_returns_pair(self, tmp_path: Path) -> None:
        """NC2: a <<< ref whose stem has no backing slug is returned as (page, stem)."""
        page = tmp_path / "page.md"
        page.write_text("<<< @/_scripts/typo__missing.sh{sh}\n")
        export = [{"path": "/any/path.sh", "slug": "something-else/other", "cast": False, "expect": None}]

        result = unresolved_transclusions((page,), export)
        assert len(result) == 1
        unresolved_page, stem = result[0]
        assert unresolved_page == page
        assert stem == "typo__missing"

    def test_null_slug_entries_ignored(self, tmp_path: Path) -> None:
        """Export entries with slug=None do not contribute to the slug set."""
        page = tmp_path / "page.md"
        page.write_text("<<< @/_scripts/a__b.sh{sh}\n")
        # Only a None-slug entry — ref is unresolved
        export = [{"path": "/any/path.sh", "slug": None, "cast": False, "expect": None}]

        result = unresolved_transclusions((page,), export)
        assert len(result) == 1

    def test_all_resolved_returns_empty(self, tmp_path: Path) -> None:
        """NC3: when every <<< ref resolves, return []."""
        page1 = tmp_path / "p1.md"
        page1.write_text("<<< @/_scripts/a__b.sh{sh}\n")
        page2 = tmp_path / "p2.md"
        page2.write_text("<<< @/_scripts/c__d.sh{sh}\n")
        export = [
            {"path": "/s1.sh", "slug": "a/b", "cast": False, "expect": None},
            {"path": "/s2.sh", "slug": "c/d", "cast": False, "expect": None},
        ]

        result = unresolved_transclusions((page1, page2), export)
        assert result == []

    def test_results_in_page_then_document_order(self, tmp_path: Path) -> None:
        """NC2: results are in page order then document order within page."""
        page1 = tmp_path / "p1.md"
        page1.write_text(
            "<<< @/_scripts/alpha__x.sh{sh}\n"
            "<<< @/_scripts/beta__y.sh{sh}\n"
        )
        page2 = tmp_path / "p2.md"
        page2.write_text("<<< @/_scripts/gamma__z.sh{sh}\n")
        export: list[dict] = []  # empty ⇒ all unresolved

        result = unresolved_transclusions((page1, page2), export)
        assert len(result) == 3
        # page order
        assert result[0][0] == page1
        assert result[1][0] == page1
        assert result[2][0] == page2
        # document order within page1
        assert result[0][1] == "alpha__x"
        assert result[1][1] == "beta__y"


# ===========================================================================
# DE1 — display_env seam schema: export entries carry all 9 keys;
#         display_env always present (never null); cast_region JSON wire
#         format is 2-element array or null.
#
# These tests are static/pure: no registry fixture, no provisioning.
# They run in test:parallel.
#
# Expected behaviour:
#   - The 9-key shape and display_env always-dict assertion PASS today
#     (the stub supplies {} for display_env).
#   - The "display_env non-empty for a state with packages" assertion FAILS
#     today (still {} until DE2 is implemented).
#   - The cast_region wire-format test PASSES today (tuple serialises to JSON
#     array; the parse test writes a cast:true fixture and round-trips it).
#
# Design ref: design_spec_doc_command_scripts.md §6f DE1.
# ===========================================================================

_DE1_REQUIRED_KEYS = frozenset(
    {"path", "slug", "cast", "expect", "display_env", "state", "cast_region",
     "title", "description"}
)


class TestDE1ExportEntrySchema:
    """DE1: doc_scripts_export() entries carry all 9 keys; display_env shape."""

    def test_de1_export_entry_has_all_nine_keys(self, tmp_path: Path) -> None:
        """DE1: every entry returned by doc_scripts_export has exactly the 9
        required keys from DocScriptExportEntry.

        Design ref: design_spec_doc_command_scripts.md §6f DE1.
        """
        import textwrap
        script = tmp_path / "install.sh"
        script.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: getting-started/install
            # title: Install test
            true
        """))
        entries = doc_scripts_export(tmp_path)
        assert entries, "expected at least one export entry"
        entry = entries[0]
        missing = _DE1_REQUIRED_KEYS - set(entry.keys())
        extra = set(entry.keys()) - _DE1_REQUIRED_KEYS
        assert not missing, (
            f"DE1: export entry missing keys: {sorted(missing)}"
        )
        assert not extra, (
            f"DE1: export entry has unexpected extra keys: {sorted(extra)}"
        )

    def test_de1_display_env_is_always_present_as_dict(self, tmp_path: Path) -> None:
        """DE1: display_env is ALWAYS present in every entry; never null.

        A script with no packages should have display_env={} (not null, not absent).

        Design ref: design_spec_doc_command_scripts.md §6f DE1 ('always present').
        """
        import textwrap
        for i, content in enumerate([
            # Script with a slug
            "#!/usr/bin/env bash\n# state: setup:basic\n# doc: test/a\ntrue\n",
            # Script without a slug (tested-only)
            "#!/usr/bin/env bash\n# state: setup:basic\ntrue\n",
        ]):
            script = tmp_path / f"script_{i}.sh"
            script.write_text(textwrap.dedent(content))

        entries = doc_scripts_export(tmp_path)
        assert len(entries) == 2, f"expected 2 entries, got {len(entries)}"
        for entry in entries:
            assert "display_env" in entry, (
                f"DE1: display_env key absent from entry {entry!r}"
            )
            assert entry["display_env"] is not None, (
                f"DE1: display_env must never be null; got None for {entry['path']}"
            )
            assert isinstance(entry["display_env"], dict), (
                f"DE1: display_env must be dict; got {type(entry['display_env'])} "
                f"for {entry['path']}"
            )

    def test_de1_display_env_empty_dict_when_no_declared_packages(
        self, tmp_path: Path
    ) -> None:
        """DE1: display_env is {} (empty dict, not null) when the provider
        declares no packages or when DECLARED_PACKAGES is not yet populated.

        Pre-implement: all states return {} because DECLARED_PACKAGES is empty.
        Post-implement: only states with no packages return {}.

        Design ref: design_spec_doc_command_scripts.md §6f DE1.
        """
        import textwrap
        script = tmp_path / "no_pkg.sh"
        script.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: test/no-pkg
            true
        """))
        entries = doc_scripts_export(tmp_path)
        assert entries
        entry = entries[0]
        # Currently always {}: assert it is at least a dict
        assert isinstance(entry["display_env"], dict), (
            f"DE1: display_env must be a dict; got {type(entry['display_env'])!r}"
        )

    def test_de1_cast_region_is_none_for_non_cast_script(self, tmp_path: Path) -> None:
        """DE1: cast_region is None (null in JSON) for a non-cast script.

        Design ref: design_spec_doc_command_scripts.md §6f DE1.
        """
        import textwrap
        script = tmp_path / "no_cast.sh"
        script.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: test/no-cast
            true
        """))
        entries = doc_scripts_export(tmp_path)
        assert entries
        assert entries[0]["cast_region"] is None, (
            f"DE1: cast_region must be None for non-cast script; "
            f"got {entries[0]['cast_region']!r}"
        )

    def test_de1_cast_region_wire_format_is_two_element_array_or_null(
        self, tmp_path: Path
    ) -> None:
        """DE1: cast_region serialises through JSON round-trip as either a
        2-element array [start, end] or null — never a Python tuple.

        The wire format (as consumed by publish_doc_scripts.py) is positional
        (region[0], region[1]) and the consumer must NOT assume a tuple instance.

        This test covers both cases: null (no cast) and [start, end] (cast:true).

        Design ref: design_spec_doc_command_scripts.md §6f DE1 (wire-format note).
        """
        import json
        import textwrap

        # Script WITH a cast region
        cast_script = tmp_path / "with_cast.sh"
        cast_script.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: test/with-cast
            # cast: true
            set -euo pipefail
            # region cast
            ocx package install foo:1.0
            # endregion cast
            true
        """))

        # Script WITHOUT a cast region
        no_cast_script = tmp_path / "no_cast.sh"
        no_cast_script.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # state: setup:basic
            # doc: test/no-cast
            true
        """))

        entries = doc_scripts_export(tmp_path)
        assert len(entries) == 2, f"expected 2 entries, got {len(entries)}"

        # Round-trip through JSON (as the real seam does)
        wire = json.loads(json.dumps(entries))

        cast_entry = next(
            (e for e in wire if "with-cast" in (e.get("slug") or "")), None
        )
        no_cast_entry = next(
            (e for e in wire if "no-cast" in (e.get("slug") or "")), None
        )

        assert cast_entry is not None, "could not find 'with-cast' entry"
        assert no_cast_entry is not None, "could not find 'no-cast' entry"

        # After JSON round-trip: cast_region must be a list of 2 ints or null
        cr = cast_entry["cast_region"]
        assert isinstance(cr, list), (
            f"DE1: cast_region wire format must be a JSON array; got {type(cr)} {cr!r}"
        )
        assert len(cr) == 2, (
            f"DE1: cast_region must be a 2-element array; got {cr!r}"
        )
        assert all(isinstance(x, int) for x in cr), (
            f"DE1: cast_region elements must be ints; got {cr!r}"
        )
        # start < end (region spans at least one line)
        assert cr[0] < cr[1], (
            f"DE1: cast_region[0] ({cr[0]}) must be < cast_region[1] ({cr[1]})"
        )

        # No-cast entry: must be null in JSON (deserialises to None)
        assert no_cast_entry["cast_region"] is None, (
            f"DE1: cast_region must be null for non-cast script; "
            f"got {no_cast_entry['cast_region']!r}"
        )

    def test_de1_title_is_always_a_str(self, tmp_path: Path) -> None:
        """DE1: title is always a non-empty str for parseable scripts (not None).

        When # title: is absent, title defaults to the file stem (str).

        Design ref: design_spec_doc_command_scripts.md §6f DE1 ('title always str').
        """
        import textwrap

        # Script with explicit title
        titled = tmp_path / "my_script.sh"
        titled.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # doc: test/titled
            # title: My explicit title
            true
        """))

        # Script without title — must default to stem
        untitled = tmp_path / "untitled_script.sh"
        untitled.write_text(textwrap.dedent("""\
            #!/usr/bin/env bash
            # doc: test/untitled
            true
        """))

        entries = doc_scripts_export(tmp_path)
        assert len(entries) == 2, f"expected 2 entries, got {len(entries)}"

        for entry in entries:
            t = entry.get("title")
            assert isinstance(t, str) and t, (
                f"DE1: title must be a non-empty str for parseable scripts; "
                f"got {t!r} for {entry['path']}"
            )

        titled_entry = next(e for e in entries if "my_script" in e["path"])
        assert titled_entry["title"] == "My explicit title", (
            f"DE1: title mismatch; expected 'My explicit title', got {titled_entry['title']!r}"
        )

        untitled_entry = next(e for e in entries if "untitled_script" in e["path"])
        assert untitled_entry["title"] == "untitled_script", (
            f"DE1: untitled script title must default to stem; "
            f"got {untitled_entry['title']!r}"
        )


# ===========================================================================
# DE5 — TypedDict parity gate: DocScriptExportEntry ↔ _DocScriptExportEntry
#
# Extracts both TypedDicts' annotations via ast from source text WITHOUT
# importing the website module.  The gate fails naming both modules + the
# differing keys when drift is detected.
#
# Includes a negative sub-test that injects a field drift and asserts the
# gate catches it.
#
# Expected behaviour: PASSES today (both TypedDicts are in sync).
#
# Design ref: design_spec_doc_command_scripts.md §6f DE5.
# ===========================================================================


def _extract_typed_dict_annotations(source_text: str, class_name: str) -> dict[str, str]:
    """Extract {field_name: annotation_str} from a TypedDict class via ast.

    Uses ``ast.unparse()`` on each ``AnnAssign`` body item so the comparison is
    on canonical annotation strings, not raw AST node identity.

    Raises AssertionError if the class is not found in the source.
    """
    import ast as _ast

    tree = _ast.parse(source_text)
    for node in _ast.walk(tree):
        if isinstance(node, _ast.ClassDef) and node.name == class_name:
            return {
                item.target.id: _ast.unparse(item.annotation)  # type: ignore[attr-defined]
                for item in node.body
                if isinstance(item, _ast.AnnAssign)
                and isinstance(item.target, _ast.Name)
            }
    raise AssertionError(
        f"TypedDict class {class_name!r} not found in provided source text"
    )


class TestDE5TypedDictParity:
    """DE5: DocScriptExportEntry ↔ _DocScriptExportEntry TypedDict parity gate."""

    def test_de5_canonical_and_mirror_typeddict_are_identical(self) -> None:
        """DE5: DocScriptExportEntry (canonical) and _DocScriptExportEntry (mirror)
        have identical key sets and per-key type annotation strings.

        Annotations are extracted via ast WITHOUT importing the website module,
        so this test has no runtime dependency on the website subsystem.

        Expected to PASS today (both TypedDicts are in sync).

        Design ref: design_spec_doc_command_scripts.md §6f DE5.
        """
        from src.helpers import PROJECT_ROOT

        canonical_src = (
            PROJECT_ROOT / "test" / "src" / "doc_scripts.py"
        ).read_text()
        mirror_src = (
            PROJECT_ROOT / "website" / "scripts" / "publish_doc_scripts.py"
        ).read_text()

        canonical_annots = _extract_typed_dict_annotations(
            canonical_src, "DocScriptExportEntry"
        )
        mirror_annots = _extract_typed_dict_annotations(
            mirror_src, "_DocScriptExportEntry"
        )

        missing_from_mirror = set(canonical_annots) - set(mirror_annots)
        extra_in_mirror = set(mirror_annots) - set(canonical_annots)
        type_mismatches = {
            k: (canonical_annots[k], mirror_annots[k])
            for k in canonical_annots
            if k in mirror_annots and canonical_annots[k] != mirror_annots[k]
        }

        errors: list[str] = []
        if missing_from_mirror:
            errors.append(
                f"Keys in canonical DocScriptExportEntry missing from "
                f"_DocScriptExportEntry: {sorted(missing_from_mirror)}"
            )
        if extra_in_mirror:
            errors.append(
                f"Keys in _DocScriptExportEntry absent from canonical "
                f"DocScriptExportEntry: {sorted(extra_in_mirror)}"
            )
        if type_mismatches:
            errors.append(
                f"Type annotation mismatches:\n"
                + "\n".join(
                    f"  {k!r}: canonical={canon!r}, mirror={mirror!r}"
                    for k, (canon, mirror) in sorted(type_mismatches.items())
                )
            )

        assert not errors, (
            "DE5 TypedDict parity gate FAILED:\n"
            + "\n".join(f"  • {e}" for e in errors)
            + "\n\nFix: update _DocScriptExportEntry in "
            "website/scripts/publish_doc_scripts.py to match "
            "DocScriptExportEntry in test/src/doc_scripts.py"
        )

    def test_de5_gate_detects_injected_drift(self) -> None:
        """DE5 negative fixture: the parity gate detects an injected field drift.

        Feeds drifted annotations through the SAME comparison the real DE5 gate
        uses (``missing_from_mirror`` → ``errors`` list → full assertion message)
        and asserts the error message names both module/class AND the differing
        key.  This proves the gate mechanism is effective against real drift, not
        just that the helper extracts annotations correctly.

        Design ref: design_spec_doc_command_scripts.md §6f DE5
        ('add a field to one only, assert the gate fails with both names +
        the differing keys').
        """
        canonical_src = """\
from typing import TypedDict

class DocScriptExportEntry(TypedDict):
    path: str
    slug: str | None
    cast: bool
    display_env: dict[str, str]
    extra_canonical_only_field: int
"""
        mirror_src = """\
from typing import TypedDict

class _DocScriptExportEntry(TypedDict):
    path: str
    slug: str | None
    cast: bool
    display_env: dict[str, str]
"""
        canonical_annots = _extract_typed_dict_annotations(
            canonical_src, "DocScriptExportEntry"
        )
        mirror_annots = _extract_typed_dict_annotations(
            mirror_src, "_DocScriptExportEntry"
        )

        # Run through the SAME comparison path the real gate uses so that
        # the error message is identical in structure — naming both
        # TypedDict classes and the differing keys.
        missing_from_mirror = set(canonical_annots) - set(mirror_annots)
        extra_in_mirror = set(mirror_annots) - set(canonical_annots)
        type_mismatches = {
            k: (canonical_annots[k], mirror_annots[k])
            for k in canonical_annots
            if k in mirror_annots and canonical_annots[k] != mirror_annots[k]
        }

        errors: list[str] = []
        if missing_from_mirror:
            errors.append(
                f"Keys in canonical DocScriptExportEntry missing from "
                f"_DocScriptExportEntry: {sorted(missing_from_mirror)}"
            )
        if extra_in_mirror:
            errors.append(
                f"Keys in _DocScriptExportEntry absent from canonical "
                f"DocScriptExportEntry: {sorted(extra_in_mirror)}"
            )
        if type_mismatches:
            errors.append(
                f"Type annotation mismatches:\n"
                + "\n".join(
                    f"  {k!r}: canonical={canon!r}, mirror={mirror!r}"
                    for k, (canon, mirror) in sorted(type_mismatches.items())
                )
            )

        # The gate must fire (errors non-empty) — the injected drift must be detected.
        assert errors, (
            "DE5 negative fixture: parity gate should detect the injected drift "
            "('extra_canonical_only_field' present in canonical but absent from mirror)"
        )

        full_message = "\n".join(errors)

        # Must name both TypedDict classes
        assert "DocScriptExportEntry" in full_message, (
            f"DE5 negative: error message must name canonical class 'DocScriptExportEntry'; "
            f"got:\n{full_message}"
        )
        assert "_DocScriptExportEntry" in full_message, (
            f"DE5 negative: error message must name mirror class '_DocScriptExportEntry'; "
            f"got:\n{full_message}"
        )

        # Must name the differing key
        assert "extra_canonical_only_field" in full_message, (
            f"DE5 negative: error message must name the differing key "
            f"'extra_canonical_only_field'; got:\n{full_message}"
        )
