# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Replay an asciicast v2 stream onto a character grid and report what a viewer sees.

Just enough of a terminal for what ocx prints: ``\\r``, ``\\n``, line wrap at
the width, scroll at the height, ``ESC[nA/B`` cursor moves, ``ESC[nK`` erase,
and SGR (only *underline* is kept: it marks a table header). Anything else is
dropped.
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path

_OPEN_ESCAPE = re.compile(r"\x1b(?:\[[0-9;]*)?\Z")
_CSI = re.compile(r"\x1b\[([0-9;]*)([@-~])")
_PREFIX = re.compile(r"(?<![A-Za-z0-9])t_[0-9a-f]{8}_")
_TEMP_ID = re.compile(r"/temp/[0-9a-f]{32}")
_TEST_HOST = re.compile(r"\blocalhost:\d+")
_FUSED_PROGRESS = re.compile(r"[A-Za-z0-9.\]'](?:Uploading|Downloading) ")
_SPINNER = re.compile(r"[\u2800-\u28FF]")
_TREE = re.compile(r"[│├└]")
_TABLE_GAP = re.compile(r"\x20{2,}")


@dataclass
class Row:
    chars: list[str] = field(default_factory=list)
    under: list[bool] = field(default_factory=list)
    wrapped: bool = False  # continuation of the row above, not a line of its own

    def text(self) -> str:
        return "".join(self.chars).rstrip()


@dataclass
class Screen:
    width: int
    height: int
    rows: list[Row] = field(default_factory=lambda: [Row()])
    scrolled: int = 0  # rows pushed out of the top: a viewer never sees them
    max_row: int = 0  # lowest row the cursor reached, counting wrapped rows
    _r: int = 0
    _c: int = 0
    _u: bool = False
    _pending: str = ""  # an escape the last chunk ended inside

    def _newline(self) -> None:
        self._r += 1
        if self._r == len(self.rows):
            self.rows.append(Row())
        if self._r >= self.height:
            self.rows.pop(0)
            self.scrolled += 1
            self._r -= 1
        self.max_row = max(self.max_row, self._r + self.scrolled)

    def _put(self, ch: str) -> None:
        if self._c >= self.width:
            self._newline()
            self.rows[self._r].wrapped = True
            self._c = 0
        row = self.rows[self._r]
        while len(row.chars) <= self._c:
            row.chars.append(" ")
            row.under.append(False)
        row.chars[self._c] = ch
        row.under[self._c] = self._u
        self._c += 1

    def feed(self, data: str) -> None:
        data = self._pending + data
        self._pending = ""
        i = 0
        while i < len(data):
            m = _CSI.match(data, i)
            if m:
                i = m.end()
                args, final = m.group(1), m.group(2)
                n = int(args.split(";")[0] or 1) if final in "AB" else 0
                if final == "m":
                    codes = args.split(";") if args else ["0"]
                    for code in codes:
                        if code in ("0", ""):
                            self._u = False
                        elif code == "4":
                            self._u = True
                elif final == "A":
                    self._r = max(0, self._r - n)
                elif final == "B":
                    for _ in range(n):
                        self._newline()
                elif final == "K":
                    del self.rows[self._r].chars[self._c:]
                    del self.rows[self._r].under[self._c:]
                continue
            if _OPEN_ESCAPE.match(data, i):
                self._pending = data[i:]
                return
            ch = data[i]
            i += 1
            if ch == "\r":
                self._c = 0
            elif ch == "\n":
                self._newline()
            elif ch >= " ":
                self._put(ch)


def logical_lines(screen: Screen) -> list[str]:
    """Screen rows with each wrapped continuation joined to the row it continues."""
    out: list[str] = []
    for row in screen.rows:
        if row.wrapped and out:
            out[-1] += row.text()
        else:
            out.append(row.text())
    return out


def replay(cast: Path, *, height: int | None = None) -> Screen:
    """Final screen of *cast*, drawn at its declared width and (unless given) height."""
    lines = cast.read_text().splitlines()
    header = json.loads(lines[0])
    screen = Screen(header["width"], height or header["height"])
    for line in lines[1:]:
        _, kind, data = json.loads(line)
        if kind == "o":
            screen.feed(data)
    return screen


_ROW_DEFECTS = [
    (_PREFIX, "test-run prefix leaked"),
    (_TEMP_ID, "random 32-hex temp id shown"),
    (_TEST_HOST, "test-only localhost:<port> host shown"),
    (_FUSED_PROGRESS, "progress text fused into a kept line"),
    (_SPINNER, "spinner glyph left in the text"),
]


def problems(cast: Path) -> list[str]:
    """Every defect a viewer would notice in the final frame of *cast*."""
    screen = replay(cast)
    found: list[str] = []
    if screen.scrolled or screen.rows[0].wrapped:
        found.append(
            f"first visible line is not the start of a command: {screen.rows[0].text()!r} "
            f"({screen.scrolled} line(s) scrolled off the top)"
        )
    for row in screen.rows:
        for pattern, what in _ROW_DEFECTS:
            if pattern.search(row.text()):
                found.append(f"{what}: {row.text()!r}")
    found.extend(_misaligned(screen.rows))
    return found


def _starts(chars: list[str], upto: int | None = None) -> list[int]:
    text = "".join(chars[:upto])
    out, prev = [], 0
    for m in _TABLE_GAP.finditer(text + "  "):
        seg = text[prev:m.start()]
        if seg.strip():
            out.append(prev + len(seg) - len(seg.lstrip()))
        prev = m.end()
    return out


def _misaligned(rows: list[Row]) -> list[str]:
    """Data rows under an underlined header whose cells start off the header's columns."""
    found: list[str] = []
    i = 0
    while i < len(rows):
        head = rows[i]
        if not any(head.under) or head.wrapped:
            i += 1
            continue
        starts = _starts(head.chars)
        j = i + 1
        if j < len(rows) and rows[j].wrapped:
            i = j  # the table is wider than the frame; its columns are not on screen
            continue
        while j < len(rows) and rows[j].text() and not any(rows[j].under) and not rows[j].wrapped:
            if _TREE.search(rows[j].text()):  # a tree after the table, not a row of it
                break
            cells = _starts(rows[j].chars)
            # Text with far fewer cells than the header (`Title:  mytool` under a
            # table) is not a row of it; a row missing one cell is.
            if len(cells) >= len(starts) - 1 and any(c not in starts for c in cells):
                found.append(f"table row off its header columns: {rows[j].text()!r} (header {head.text()!r})")
            j += 1
        i = max(j, i + 1)
    return found
