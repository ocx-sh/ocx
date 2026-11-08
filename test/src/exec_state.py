"""Standard-library codec for the `__OCX_ENV_STATE` wire value (ADR exec_env_model § Codec).

Wire: `header "." payload`; header `major *(";" segment)` with `n=` narrowing names and `h=` home;
payload empty (the rung-3 marker) or base64url-nopad(zlib(compact JSON)). Payload field names
and enum spellings are the Rust serde shapes of `ocx_config::exec_state::types` (snake_case enum
variants, externally tagged); `testdata/state.json` there is round-tripped by this codec.
"""

from __future__ import annotations

import base64
import json
import re
import zlib
from dataclasses import dataclass, field
from pathlib import Path

MAJOR = 2
MINOR = 0
MAX_ENCODED = 8_169
MAX_HEADER = 2_048
MAX_INFLATED = 256 * 1024

NARROWING_NAMES = ("offline", "frozen", "no_consent", "credential_helper")
ALPHABET = re.compile(r"^[0-9A-Za-z;=,._-]*$")
_B64URL = re.compile(r"^[A-Za-z0-9_-]*$")
_MAJOR = re.compile(r"^[0-9]{1,10}$")
_U32_MAX = 2**32 - 1


@dataclass(frozen=True)
class HeaderView:
    """What the plaintext header yields whatever the payload does."""

    major: int | None = None
    narrowing: tuple[str, ...] = ()
    home: str | None = None


@dataclass(frozen=True)
class Unreadable:
    """A value whose payload cannot be used; the header is still honoured."""

    reason: str
    header: HeaderView = field(default_factory=HeaderView)


def _b64e(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _b64d(text: str) -> bytes | None:
    """Strict base64url without padding; None when the text is not that."""
    if not _B64URL.match(text) or len(text) % 4 == 1:
        return None
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def absolute(posix: str) -> str:
    """A POSIX-spelled absolute path as an absolute path of this platform (``/work/a`` -> ``C:\\work\\a``).

    The decoder refuses a payload path that is not absolute, and a rooted path with no drive is
    relative on Windows: a fixture naming ``/work/a`` there would be an unreadable state.
    """
    assert posix.startswith("/"), posix
    return str(Path(Path.cwd().anchor) / posix.lstrip("/"))


def default_state() -> dict:
    """A minimal payload with every state field at its empty value."""
    return {
        "minor": MINOR,
        "policy": {},
        "applied": [],
        "inherited_scopes": [],
        "replay": {"env": [], "launch_identities": []},
        "selection": "follow_cwd",
        "session": {
            "token": None,
            "fp": "",
            "verdict": None,
            "tiers": [],
            "ws": "",
            "messages_fp": "",
            "project": None,
        },
        "truncated": "none",
    }


def applied_constant(key: str, value: str, *, session: bool = True) -> dict:
    """One `AppliedValue` record for a Constant, owned by a composed global session or Inherited."""
    owner: dict | str = (
        {"session": {"scope": "global", "source": "composed"}}
        if session
        else "inherited"
    )
    return {
        "key": key,
        "value": {"literal": value},
        "kind": "constant",
        "separator": None,
        "pre_existing": False,
        "owner": owner,
        "prior": "unset",
    }


def header(
    major: int = MAJOR, narrowing: tuple[str, ...] = (), home: str | None = None
) -> str:
    """Render the plaintext header."""
    parts = [str(major)]
    if narrowing:
        parts.append("n=" + ",".join(narrowing))
    if home is not None:
        parts.append("h=" + _b64e(home.encode("utf-8")))
    return ";".join(parts)


def _payload(state: dict) -> str:
    compact = json.dumps(state, separators=(",", ":"), ensure_ascii=True).encode(
        "utf-8"
    )
    return _b64e(zlib.compress(compact))


def encode(
    state: dict,
    *,
    major: int = MAJOR,
    minor: int = MINOR,
    narrowing: tuple[str, ...] = (),
    home: str | None = None,
) -> str:
    """Encode a payload dict in full (rung 0); the caller keeps it under MAX_ENCODED."""
    return header(major, narrowing, home) + "." + _payload({**state, "minor": minor})


def parse_header(raw: str) -> tuple[HeaderView, str | None]:
    """Parse the header; returns the view and the text after the first dot (None when no valid header).

    The view carries every narrowing name found even when the header is otherwise invalid.
    """
    head, dot, rest = raw.partition(".")
    if not dot or len(head.encode("utf-8", "replace")) > MAX_HEADER:
        head = raw[:MAX_HEADER]
        rest_or_none = None
    else:
        rest_or_none = rest
    segments = head.split(";")
    names: list[str] = []
    home: str | None = None
    for seg in segments[1:]:
        name, eq, text = seg.partition("=")
        if not eq or not seg.isascii():
            continue
        if name == "n":
            names.extend(
                n for n in text.split(",") if n in NARROWING_NAMES and n not in names
            )
        elif name == "h":
            decoded = _b64d(text)
            try:
                path = decoded.decode("utf-8") if decoded is not None else None
            except UnicodeDecodeError:
                path = None
            if path and (path.startswith("/") or re.match(r"^[A-Za-z]:[\\/]", path)):
                home = path
    major_text = segments[0]
    major = (
        int(major_text)
        if _MAJOR.match(major_text) and int(major_text) <= _U32_MAX
        else None
    )
    view = HeaderView(major, tuple(names), home)
    return view, (rest_or_none if major is not None else None)


def decode(value: str | None) -> dict | Unreadable | None:
    """Decode per the codec table. None = Absent; Unreadable carries a reason name and the header view.

    A readable value returns the payload dict plus `narrowing`, `home` and `major` from the header.
    """
    if not value:
        return None
    view, payload = parse_header(value)
    if payload is None:
        return Unreadable("HeaderInvalid", view)
    if view.major != MAJOR:
        return Unreadable("OtherMajor", view)
    if len(value) > MAX_ENCODED:
        return Unreadable("TooLong", view)
    meta = {"narrowing": view.narrowing, "home": view.home, "major": view.major}
    if payload == "":
        return {**default_state(), "truncated": "payload_dropped", **meta}
    raw = _b64d(payload)
    if raw is None:
        return Unreadable("Base64", view)
    stream = zlib.decompressobj()
    try:
        inflated = stream.decompress(raw, MAX_INFLATED + 1)
    except zlib.error:
        return Unreadable("Inflate", view)
    if len(inflated) > MAX_INFLATED:
        return Unreadable("InflateLimit", view)
    if not stream.eof:
        return Unreadable("Inflate", view)
    if stream.unused_data:
        return Unreadable("TrailingBytes", view)
    try:
        state = json.loads(inflated)
    except ValueError, RecursionError:
        return Unreadable("Json", view)
    if not isinstance(state, dict):
        return Unreadable("Json", view)
    return {**state, **meta}


def craft_other_major(
    major: int, narrowing: tuple[str, ...] = (), home: str | None = None
) -> str:
    """A value of another major: decodes Unreadable(OtherMajor) with the header honoured."""
    if major == MAJOR:
        raise ValueError("pick a major other than the current one")
    return encode(default_state(), major=major, narrowing=narrowing, home=home)


def craft_header_only(narrowing: tuple[str, ...] = (), home: str | None = None) -> str:
    """The rung-3 marker: header plus empty payload."""
    return header(MAJOR, narrowing, home) + "."


def craft_garbled_payload(
    kind: str = "base64", narrowing: tuple[str, ...] = (), home: str | None = None
) -> str:
    """A current-major value whose payload fails at one decode step, all inside the value alphabet.

    kind: base64 (bad length), inflate (not zlib), json (zlib of non-JSON), trailing (bytes after zlib).
    """
    good = zlib.compress(b"{}")
    bodies = {
        "base64": "AAAAA",
        "inflate": _b64e(b"this is not a zlib stream"),
        "json": _b64e(zlib.compress(b"{not json")),
        "trailing": _b64e(good + b"x"),
    }
    if kind not in bodies:
        raise ValueError(f"unknown garble kind: {kind}")
    return header(MAJOR, narrowing, home) + "." + bodies[kind]


def craft_over_cap(
    narrowing: tuple[str, ...] = (), home: str | None = None, extra: int = 1
) -> str:
    """A current-major value `extra` chars longer than MAX_ENCODED (decodes TooLong)."""
    head = header(MAJOR, narrowing, home) + "."
    return head + "A" * (MAX_ENCODED + extra - len(head))
