"""Unit tests for CastRecording.realign_tables (underlined-header style).

The CLI table has no vertical `│` and no rule line: a bold+underlined
header (underline continuous across the two-space GAP), space-aligned
columns, per-cell colour, and a dim zebra wrap on odd data rows. After
digest truncation the binary's original padding is too wide, so
realign_tables must recompute column widths and re-pad while preserving
each cell's raw ANSI (multi-part identifier ink, zebra dim) verbatim.
"""
from __future__ import annotations

import re

from recordings.cast_recorder import CastEvent, CastRecording

_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_HDR = "\x1b[1m\x1b[4m"  # bold + underline
_R = "\x1b[0m"
_SKY = "\x1b[38;5;117m"
_TAG = "\x1b[38;5;80m"
_GRN = "\x1b[38;5;114m"
_DIM = "\x1b[2m"


def _strip(s: str) -> str:
    return _ANSI.sub("", s)


def _table() -> str:
    """3 columns; Digest over-padded to 71 (the untruncated sha length).
    Package cells are multi-part (plain name + coloured :tag)."""
    w = [40, 10, 71]

    def hcell(text: str, width: int) -> str:
        return f"{_HDR}{text:<{width}}{_R}"

    hgap = f"{_HDR}  {_R}"
    header = hgap.join([hcell("Package", w[0]), hcell("Visibility", w[1]), hcell("Digest", w[2])])

    def drow(name_tag: str, vis: str, dig: str, zebra: bool) -> str:
        cells = [
            f"{name_tag}{' ' * (w[0] - len(_strip(name_tag)))}",
            f"{_GRN}{vis}{_R}{' ' * (w[1] - len(vis))}",
            f"{_SKY}{dig}{_R}{' ' * (w[2] - len(dig))}",
        ]
        if zebra:
            cells = [f"{_DIM}{c}{_R}" for c in cells]
            return f"{_DIM}  {_R}".join(cells)
        return "  ".join(cells)

    r0 = drow(f"ocx.sh/cmake{_TAG}:3.28{_R}", "public", "sha256:aa..", False)
    r1 = drow(f"ocx.sh/ninja{_TAG}:1.12{_R}", "private", "sha256:bb..", True)
    return f"{header}\r\n{r0}\r\n{r1}"


def test_recomputes_widths() -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", _table())])
    rec.realign_tables()
    header, r0, r1 = rec.events[0].data.split("\r\n")

    # Package: "ocx.sh/cmake:3.28" (17) > "Package" -> 17
    # Visibility: "Visibility" (10) > public/private -> 10
    # Digest: "sha256:aa.." (11) > "Digest" -> 11
    expect = f"{'Package':<17}  {'Visibility':<10}  {'Digest':<11}"
    assert _strip(header) == expect
    assert _strip(r0) == f"{'ocx.sh/cmake:3.28':<17}  {'public':<10}  {'sha256:aa..':<11}"
    assert len({len(_strip(x)) for x in (header, r0, r1)}) == 1  # all aligned


def test_header_underline_is_continuous_across_gap() -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", _table())])
    rec.realign_tables()
    header = rec.events[0].data.split("\r\n")[0]
    # The GAP between header columns is wrapped in the header SGR (bold+
    # underline), so the underline is one continuous line, not per-column
    # segments with bare gaps.
    assert f"{_HDR}  {_R}" in header
    # No reset is immediately followed by an unstyled double-space gap.
    assert f"{_R}  " not in header
    assert _strip(header).startswith("Package")


def test_preserves_multispan_and_zebra() -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", _table())])
    rec.realign_tables()
    _, r0, r1 = rec.events[0].data.split("\r\n")

    # Even row: both identifier spans (plain name + cyan :tag) survive,
    # digest stays sky-blue, no dim.
    assert f"ocx.sh/cmake{_TAG}:3.28{_R}" in r0
    assert f"{_SKY}sha256:aa.." in r0
    assert _DIM not in r0

    # Odd row: zebra dim retained on cells AND across the GAP; colour kept.
    assert _DIM in r1
    assert f"{_DIM}  {_R}" in r1  # gap dim-wrapped
    assert f"{_TAG}:1.12{_R}" in r1
    assert f"{_SKY}sha256:bb.." in r1


def test_spinner_residue_after_the_erase_does_not_inflate_column_zero() -> None:
    """A cursor-movement escape between the last `\\x1b[2K` and the header
    must not be measured as content.

    Real shape, from an `ocx add` recording: the spinner tears down with
    `…\\x1b[1A\\r\\x1b[2K\\x1b[1B\\r\\x1b[2K\\x1b[1A` and the header follows
    immediately. `\\x1b[1A` is CSI-but-not-SGR, so `analyze` counted its four
    bytes as visible text: column 0 measured 4 wider than it rendered and every
    data row was padded to that phantom width. Both parts are asserted — the
    residue survives in the event, and the rows line up.
    """
    residue = "\x1b[1A\r\x1b[2K\x1b[1B\r\x1b[2K\x1b[1A"
    rec = CastRecording(events=[CastEvent(0.0, "o", residue + _table())])
    rec.realign_tables()
    data = rec.events[0].data

    assert data.startswith(residue), "control residue must be re-emitted verbatim"
    header, r0, r1 = data[len(residue) :].split("\r\n")
    assert _strip(header) == f"{'Package':<17}  {'Visibility':<10}  {'Digest':<11}"
    assert len({len(_strip(x)) for x in (header, r0, r1)}) == 1


def test_non_table_lines_untouched() -> None:
    # Tree chrome (no underline SGR) and a lone underlined hint (no data
    # rows -> block < 2) are both left exactly as-is.
    tree = "root\r\n\x1b[2m├── \x1b[0m\x1b[1mchild\x1b[0m\r\n\x1b[2m│   └── \x1b[0mleaf"
    hint = "\x1b[2m\x1b[3m\x1b[4mrun `ocx lock` first\x1b[0m"
    for blob in (tree, hint):
        rec = CastRecording(events=[CastEvent(0.0, "o", blob)])
        rec.realign_tables()
        assert rec.events[0].data == blob


# ---------------------------------------------------------------------------
# Regressions from the published `package push` cast: an empty cell, a prefix
# the display map does not know, and a command wider than the frame.
# ---------------------------------------------------------------------------

from pathlib import Path

from recordings.cast_layer import write_cast
from src.cast_screen import problems, replay

_DIGEST = "sha256:" + "a7a58964" + "0" * 56


def _push_table() -> str:
    """Identifier / Digest / Tags / Keep Tags / Layers, Keep Tags empty in the row."""
    w = [30, 71, 4, 9, 31]
    header = f"{_HDR}  {_R}".join(f"{_HDR}{t:<{n}}{_R}" for t, n in zip(
        ["Identifier", "Digest", "Tags", "Keep Tags", "Layers"], w))
    cells = ["localhost:5000/t_46fede8b_acme/mytool:1.0.0", _DIGEST, "1", "", "mounted=0,uploaded=1"]
    cells[0] = cells[0][:30]
    row = "  ".join(f"{c:<{n}}" for c, n in zip(cells, w))
    return f"{header}\r\n{row}\r\n"


def _write(tmp_path: Path, rec: CastRecording) -> Path:
    path = tmp_path / "x.cast"
    write_cast(rec, {"localhost:5000/t_46fede8b_acme/mytool": "acme/mytool", "localhost:5000/": ""}, path)
    return path


def test_table_with_an_empty_cell_is_realigned_after_shortening(tmp_path: Path) -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", _push_table())])
    path = _write(tmp_path, rec)
    assert problems(path) == []
    head, row = (r.text() for r in replay(path).rows[:2])
    assert row.index("sha256:a7a58964..") == head.index("Digest")
    assert row.index("1") == head.index("Tags")
    assert row.index("mounted") == head.index("Layers")


def test_unknown_test_run_prefix_is_removed_even_in_quotes() -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", "for identifier 't_46fede8b_acme/mytool'.\r\n")])
    rec.sanitize({})
    assert rec.events[0].data == "for identifier 'acme/mytool'.\r\n"


def test_command_wider_than_the_frame_keeps_its_start_visible(tmp_path: Path) -> None:
    command = "$ ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64"
    lines = "".join(f"{command}\r\nline {n}\r\n" for n in range(5))
    rec = CastRecording(width=40, height=5, events=[CastEvent(0.0, "o", lines)])
    path = _write(tmp_path, rec)
    assert problems(path) == []
    assert replay(path).rows[0].text() == command  # one row: the command is not wrapped


def test_width_follows_the_longest_output_line(tmp_path: Path) -> None:
    line = "x" * 102
    path = _write(tmp_path, CastRecording(width=100, events=[CastEvent(0.0, "o", f"{line}\r\n")]))
    screen = replay(path)
    assert screen.width == 102
    assert not any(r.wrapped for r in screen.rows)


def test_width_is_capped_and_a_longer_line_wraps(tmp_path: Path) -> None:
    path = _write(tmp_path, CastRecording(width=100, events=[CastEvent(0.0, "o", f"$ {'x' * 181}\r\n")]))
    screen = replay(path)
    assert screen.width == 120
    assert [r.wrapped for r in screen.rows[:2]] == [False, True]


def test_prefix_split_across_two_events_is_removed() -> None:
    """A PTY read can end inside the word; the published cast showed `'t_46fede8b_acme/mytool'`."""
    rec = CastRecording(events=[
        CastEvent(0.0, "o", "for identifier 't_46fe"),
        CastEvent(1.0, "o", "de8b_acme/mytool'.\r\n"),
    ])
    rec.sanitize({})
    assert [e.data for e in rec.events] == ["for identifier '", "acme/mytool'.\r\n"]


def test_footnote_under_a_table_is_not_a_row(tmp_path: Path) -> None:
    """`2 binaries available (…)` follows the env table; it must not widen the Key column."""
    note = "\x1b[2m\x1b[3m\x1b[4m2 binaries available (java, javac); use --format json\x1b[0m"
    hdr = f"{_HDR}Key{_R}{_HDR}  {_R}{_HDR}Value{_R}"
    rec = CastRecording(events=[CastEvent(0.0, "o", f"{hdr}\r\nPATH  /bin\r\n{note}\r\n")])
    path = _write(tmp_path, rec)
    assert [r.text() for r in replay(path).rows[:3]] == ["Key   Value", "PATH  /bin", "2 binaries available (java, javac); use --format json"]


def test_row_whose_only_cell_is_the_first_stays_in_the_table(tmp_path: Path) -> None:
    """`index list --variants`: a package with no variant prints one cell, the next row prints two."""
    hdr = f"{_HDR}Package    {_R}{_HDR}  {_R}{_HDR}Variant{_R}"
    rec = CastRecording(events=[CastEvent(0.0, "o", f"{hdr}\r\ncmake\r\ncmake       slim\r\n")])
    path = _write(tmp_path, rec)
    assert problems(path) == []
    assert [r.text() for r in replay(path).rows[:3]] == ["Package  Variant", "cmake", "cmake    slim"]


def test_progress_bar_padding_does_not_widen_or_wrap_the_frame(tmp_path: Path) -> None:
    """A bar pads itself to the PTY width; the cast must not carry that padding."""
    bar = "Uploading [====] 184 B/184 B" + " " * 90
    rec = CastRecording(width=120, events=[CastEvent(0.0, "o", f"{bar}\r\x1b[2K{bar}\r\x1b[2Kdone\r\n")])
    path = _write(tmp_path, rec)
    assert replay(path).width == 100


def test_strip_progress_keeps_the_line_before_a_spinner() -> None:
    """The published cascade cast read `…metadata mytoUploading 1/1`: the kept tail was eaten."""
    rec = CastRecording(events=[CastEvent(0.0, "o", "metadata mytool.json\r\n⠁ Verifying pins   \r\x1b[2Kstaging x\r\n")])
    rec.strip_progress()
    assert rec.events[0].data == "metadata mytool.json\r\nstaging x\r\n"


def test_screen_joins_an_escape_split_across_events(tmp_path: Path) -> None:
    """A PTY read can end between ESC and `[3m`; the player joins them, so must the decoder."""
    path = tmp_path / "x.cast"
    path.write_text('{"version":2,"width":40,"height":4}\n[0,"o","pulling \\u001b"]\n[0.1,"o","[3mcount\\u001b[0m=1\\r\\n"]\n')
    assert replay(path).rows[0].text() == "pulling count=1"


def test_temp_ids_are_shortened_like_digests() -> None:
    rec = CastRecording(events=[CastEvent(0.0, "o", "to ~/.ocx/temp/24ab47c51379b50c5dc5035cea5fdce9\r\n")])
    rec.truncate_digests()
    assert rec.events[0].data == "to ~/.ocx/temp/24ab47c5..\r\n"


def test_display_hosts_name_the_registries_a_page_talks_about() -> None:
    from recordings.cast_layer import display_hosts

    assert display_hosts({}, "localhost:5000") == {"localhost:5000": "registry.example.com"}
    promo = display_hosts({"STAGING": "localhost:6000", "PROD": "localhost:7000"}, "localhost:5000")
    assert promo == {
        "localhost:5000": "dev.example.com",
        "localhost:6000": "staging.example.com",
        "localhost:7000": "prod.example.com",
    }
