# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Every recorded terminal cast shows what a reader needs, at its final frame.

A cast is a recording of a real run, post-processed by `test/recordings/`. Three
defects got past that processing and reached the published docs: a test-run
prefix (`t_<8 hex>_`) in the text, a table whose header and rows no longer line
up after the values were shortened, and a first visible line that is the tail of
a wrapped command because the frame was too short for the wrapped rows. This
sweep replays each cast (`src/cast_screen.py`) and names the casts that show any.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from src.cast_screen import problems

# The recorder's own output seam (`website/recordings.taskfile.yml` sets it); PT8 keeps
# website paths out of `test/`. The casts are generated and git-ignored, so with the
# variable unset there are none to read; `write_cast` runs the same check on every
# recording as it is written, and the cases below prove the check can go red.
_DIR = os.environ.get("OCX_DOC_CASTS_DIR")
_CASTS = sorted(Path(_DIR).rglob("*.cast")) if _DIR else []

_UL = "\x1b[4m"
_END = "\x1b[0m"


def _cast(tmp_path: Path, text: str, *, width: int = 40, height: int = 6) -> Path:
    path = tmp_path / "x.cast"
    path.write_text(json.dumps({"version": 2, "width": width, "height": height}) + "\n" + json.dumps([0.0, "o", text]) + "\n")
    return path


def test_a_clean_cast_has_no_problems(tmp_path: Path) -> None:
    table = f"{_UL}Name  {_END}{_UL}  {_END}{_UL}Tag{_END}\r\nmytool  1.0\r\n"
    assert problems(_cast(tmp_path, "$ ocx list\r\n" + table)) == []


def test_a_leaked_run_prefix_is_reported(tmp_path: Path) -> None:
    (found,) = problems(_cast(tmp_path, "for identifier 't_46fede8b_acme/mytool'.\r\n"))
    assert "t_46fede8b_" in found


def test_a_frame_that_scrolls_a_command_to_its_tail_is_reported(tmp_path: Path) -> None:
    text = "$ ocx package create build --long-flag value --other\r\n" + "x\r\n" * 4
    (found,) = problems(_cast(tmp_path, text, height=4))
    assert "first visible line" in found


def test_a_table_row_off_its_header_columns_is_reported(tmp_path: Path) -> None:
    table = f"{_UL}Name       {_END}{_UL}  {_END}{_UL}Tag{_END}\r\nmytool  1.0\r\n"
    (found,) = problems(_cast(tmp_path, table))
    assert "off its header columns" in found


@pytest.mark.parametrize(
    ("text", "needle"),
    [
        ("to ~/.ocx/temp/24ab47c51379b50c5dc5035cea5fdce9\r\n", "32-hex temp id"),
        ("Plain HTTP  localhost:43557\r\n", "localhost:<port>"),
        ("metadata mytoUploading 1/1 x [==]\r\n", "fused"),
        ("\u2801 Verifying\r\n", "spinner glyph"),
    ],
)
def test_run_specific_text_is_reported(tmp_path: Path, text: str, needle: str) -> None:
    (found,) = problems(_cast(tmp_path, text, width=60))
    assert needle in found


def test_recorded_casts_are_clean() -> None:
    bad = {cast.as_posix(): found for cast in _CASTS if (found := problems(cast))}
    assert bad == {}
