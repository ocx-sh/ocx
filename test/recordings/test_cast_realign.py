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
    return "\r\n".join([header, r0, r1])


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


def test_non_table_lines_untouched() -> None:
    # Tree chrome (no underline SGR) and a lone underlined hint (no data
    # rows -> block < 2) are both left exactly as-is.
    tree = "root\r\n\x1b[2m├── \x1b[0m\x1b[1mchild\x1b[0m\r\n\x1b[2m│   └── \x1b[0mleaf"
    hint = "\x1b[2m\x1b[3m\x1b[4mrun `ocx lock` first\x1b[0m"
    for blob in (tree, hint):
        rec = CastRecording(events=[CastEvent(0.0, "o", blob)])
        rec.realign_tables()
        assert rec.events[0].data == blob
