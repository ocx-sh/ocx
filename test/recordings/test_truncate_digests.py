"""Unit tests for CastRecording.truncate_digests.

Regression coverage for the decorated-table refactor (commit bc26e1ea):
`StyledInk for Identifier` paints `@` (punct) and `sha256:…` (digest) as
separate SGR spans, wedging an escape run between `@` and `sha256:`. The
ref-digest regex must tolerate that run while preserving it verbatim, so
the recording stays coloured *and* shortened.
"""

from __future__ import annotations

import re

from recordings.cast_recorder import CastEvent, CastRecording

_ANSI = re.compile(r"\x1b\[[0-9;]*m")
_R = "\x1b[0m"
_DIM = "\x1b[2m"  # punct (`@`)
_SKY = "\x1b[38;5;117m"  # digest


def _trunc(data: str) -> str:
    rec = CastRecording(events=[CastEvent(0.0, "o", data)])
    rec.truncate_digests()
    return rec.events[0].data


def test_plain_ref_digest_still_truncated() -> None:
    h = "a" * 64
    out = _trunc(f"ocx.sh/cmake@sha256:{h}")
    assert out == "ocx.sh/cmake@sha256:aaaaaaaa.."


def test_coloured_ref_digest_truncated_and_styled() -> None:
    # The exact byte form the binary now emits: dim `@`, then the digest
    # span. The reset+colour run sits between `@` and `sha256:`.
    h = "a" * 64
    raw = f"ocx.sh/cmake{_DIM}@{_R}{_SKY}sha256:{h}{_R}"
    out = _trunc(raw)

    # Hex shortened to 8 chars.
    assert _ANSI.sub("", out) == "ocx.sh/cmake@sha256:aaaaaaaa.."
    # Styling preserved verbatim: dim `@`, sky-blue digest, trailing reset.
    assert out == f"ocx.sh/cmake{_DIM}@{_R}{_SKY}sha256:aaaaaaaa..{_R}"


def test_object_store_path_truncated() -> None:
    h8, h8b, h16 = "a" * 8, "b" * 8, "c" * 16
    out = _trunc(f"~/.ocx/blobs/sha256/{h8}/{h8b}/{h16}/data")
    assert out == "~/.ocx/blobs/sha256/aaaaaaaa../data"


def test_bare_column_digest_plain_truncated() -> None:
    # `ocx package deps --flat` digest column: bare, no `@`, no `/`.
    h = "a" * 64
    out = _trunc(f"nodejs:24.0.0  public  sha256:{h}")
    assert out == "nodejs:24.0.0  public  sha256:aaaaaaaa.."


def test_bare_column_digest_coloured_truncated() -> None:
    # Even (non-zebra) table rows keep per-cell colour around the digest.
    h = "a" * 64
    out = _trunc(f"{_SKY}sha256:{h}{_R}")
    assert _ANSI.sub("", out) == "sha256:aaaaaaaa.."
    assert out == f"{_SKY}sha256:aaaaaaaa..{_R}"


def test_already_short_digest_not_double_truncated() -> None:
    # Path/ref subs already shortened these; the bare sub must not re-touch.
    assert _trunc("sha256:aaaaaaaa..") == "sha256:aaaaaaaa.."
    assert _trunc("repo@sha256:aaaaaaaa..") == "repo@sha256:aaaaaaaa.."
