#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Regenerate the CPython reference vectors in this directory.

    python3 crates/ocx_lib/tests/fixtures/index_wire/cpython/generate.py

CPython's `json` module *is* the byte authority for `ocx-sh/index`
`bot/CONTRACTS.md` §14, so these vectors are generated from it rather than
vendored from `ocx-sh/index` — they certify the reference, not ocx's own
behaviour. Everything is written with `json.dumps(..., indent=2,
sort_keys=False, ensure_ascii=True) + "\\n"`, the exact call §14 specifies.

Outputs
-------
`codepoint_escapes.txt`
    Escape truth table: `<UPPERCASE HEX>\\t<json.dumps(chr(code))>` per line.
    Not the full 1.1M-scalar sweep (3.6 MB) — a dense low range plus every
    boundary neighbourhood plus strides, sized so no single-code-point
    boundary error can hide while the committed file stays small.

`layout.json`, `top_level_array.json`
    Structural vectors: empty containers, nesting, insertion order, number
    spellings, escaped keys, array root. Each re-parses to the data that
    produced it, so a test asserts `serialize(parse(bytes)) == bytes`.
"""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))

# `indent=2, sort_keys=False, ensure_ascii=True` — CONTRACTS §14 verbatim.
def canonical(data: object) -> bytes:
    return (json.dumps(data, indent=2, sort_keys=False, ensure_ascii=True) + "\n").encode()


def is_scalar(code: int) -> bool:
    """Surrogates are not Unicode scalar values; `chr()` yields no Rust `char`."""
    return not 0xD800 <= code <= 0xDFFF


def sweep_codepoints() -> list[int]:
    codes: set[int] = set()

    # Dense: every scalar through U+02FF. Covers the C0 controls, the whole
    # printable-ASCII run, the DEL boundary, and the Latin-1 supplement.
    codes.update(range(0x0300))

    # Every boundary where a wrong comparison or a wrong format width would show,
    # with four neighbours on each side so an off-by-one cannot slip between.
    for boundary in (
        0x1F, 0x20,  # control -> raw
        0x7E, 0x7F, 0x80,  # printable -> DEL -> non-ASCII
        0xFF, 0x100,  # 2-hex-digit -> 3
        0x7FF, 0x800,  # UTF-8 2-byte -> 3-byte
        0xFFF, 0x1000,  # 3-hex-digit -> 4
        0xD7FF, 0xE000,  # edges of the surrogate hole
        0xFFFD, 0xFFFF, 0x10000,  # BMP -> surrogate pair
        0x103FF, 0x10400,  # a low-surrogate wrap boundary
        0x1FFFF, 0x20000,  # UTF-8 4-byte plane edges
        0xFFFFF, 0x100000,  # 5-hex-digit -> 6
        0x10FFFF,  # last scalar
    ):
        codes.update(range(boundary - 4, boundary + 5))

    # Strides across the bulk, so a systematic error anywhere is caught.
    codes.update(range(0x0300, 0x10000, 37))
    codes.update(range(0x10000, 0x110000, 1009))

    return sorted(code for code in codes if 0 <= code <= 0x10FFFF and is_scalar(code))


def write_codepoint_table() -> None:
    lines = [
        "# CPython `json.dumps(chr(code), indent=2, sort_keys=False, ensure_ascii=True)`",
        "# per scalar, as `<UPPERCASE HEX>\\t<literal>`. Regenerate with generate.py",
        "# in this directory; never hand-edit — a mismatch is a serializer bug.",
    ]
    for code in sweep_codepoints():
        lines.append("%04X\t%s" % (code, json.dumps(chr(code), ensure_ascii=True)))
    path = os.path.join(HERE, "codepoint_escapes.txt")
    with open(path, "wb") as handle:
        handle.write(("\n".join(lines) + "\n").encode())
    print("wrote %s (%d scalars)" % (path, len(lines) - 3))


# Every non-printable / non-ASCII scalar is written as an explicit escape so the
# vectors never depend on this source file's own byte encoding.
LAYOUT = {
    "empty_object": {},
    "empty_array": [],
    "nested_empties": {"a": {}, "b": [], "c": [{}, []]},
    "deep": {"a": [{"b": [{"c": ["d\u00e9", []]}]}]},
    "insertion_order": {"zebra": 1, "apple": 2, "mango": 3, "aardvark": 4},
    "numbers": {
        "int": 42,
        "negative": -17,
        "zero": 0,
        "beyond_f64": 9007199254740993,
        "u64_max": 18446744073709551615,
        "i64_min": -9223372036854775808,
        "yes": True,
        "no": False,
        "nothing": None,
    },
    "escaped_keys": {
        "caf\u00e9": 1,
        "a\nb": 2,
        "\U0001F600": 3,
        "a/b": 4,
        "\u007f": 5,
        "\u0000": 6,
        "": 7,
    },
    "del_boundary": {
        "del": "\u007f",
        "mid": "a\u007fb",
        "tilde": "~",
        "pair": "~\u007f",
    },
    "slash": {"path": "a/b/c", "url": "https://ocx.sh/p/x", "escaped": "a\\/b"},
    "c0_controls": {
        "all": "".join(chr(code) for code in range(0x20)),
        "shorts": "\b\f\n\r\t\"\\",
    },
    "non_ascii": {"latin": "caf\u00e9", "cjk": "\u4e2d\u6587", "greek": "\u03b1\u03b2"},
    "astral": {
        "first": "\U00010000",
        "last": "\U0010FFFF",
        "emoji": "a\U0001F600b\u00e9c",
        "last_bmp": "\uFFFF",
        "below_surrogates": "\uD7FF",
        "above_surrogates": "\uE000",
    },
    "whitespace": {"spaces": "  a  ", "tabs": "\ta\t", "nbsp": "\u00a0"},
}

TOP_LEVEL_ARRAY = [1, "caf\u00e9", {}, [], None, ["\u007f", {"k": []}]]


def write_structural_vectors() -> None:
    for name, data in (("layout", LAYOUT), ("top_level_array", TOP_LEVEL_ARRAY)):
        path = os.path.join(HERE, name + ".json")
        with open(path, "wb") as handle:
            handle.write(canonical(data))
        print("wrote %s" % path)


if __name__ == "__main__":
    write_codepoint_table()
    write_structural_vectors()
