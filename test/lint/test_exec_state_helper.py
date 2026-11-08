"""Self-test of the Python `__OCX_ENV_STATE` codec (test/src/exec_state.py): grammar and crafters."""

from __future__ import annotations

import base64

import pytest

from src import exec_state as es


def _home(path: str) -> str:
    return base64.urlsafe_b64encode(path.encode()).decode().rstrip("=")


def test_round_trip_keeps_payload_and_header() -> None:
    state = es.default_state()
    state["applied"] = [es.applied_constant("FOO", "bar")]
    value = es.encode(state, narrowing=("offline", "frozen"), home="/opt/ocx home")
    out = es.decode(value)
    assert isinstance(out, dict)
    assert out["applied"] == state["applied"]
    assert out["narrowing"] == ("offline", "frozen")
    assert out["home"] == "/opt/ocx home"
    assert out["minor"] == es.MINOR


def test_higher_minor_decodes() -> None:
    out = es.decode(es.encode(es.default_state(), minor=7))
    assert isinstance(out, dict)
    assert out["minor"] == 7


def test_unset_and_empty_are_absent() -> None:
    assert es.decode(None) is None
    assert es.decode("") is None


def test_header_repeated_n_in_any_order() -> None:
    out = es.decode(f"{es.MAJOR};n=frozen;n=offline,no_consent.")
    assert isinstance(out, dict)
    assert out["narrowing"] == ("frozen", "offline", "no_consent")


def test_header_unknown_segment_and_name_ignored() -> None:
    out = es.decode(f"{es.MAJOR};z=whatever;n=offline,future_name;novalue.")
    assert isinstance(out, dict)
    assert out["narrowing"] == ("offline",)


def test_first_dot_ends_the_header() -> None:
    view, rest = es.parse_header("2;n=offline.abc.def")
    assert view.narrowing == ("offline",)
    assert rest == "abc.def"


def test_invalid_home_dropped_alone() -> None:
    relative = es.decode(f"{es.MAJOR};n=offline;h={_home('rel/path')}.")
    assert isinstance(relative, dict)
    assert relative["home"] is None
    assert relative["narrowing"] == ("offline",)


def test_header_without_dot_is_invalid_but_keeps_narrowing() -> None:
    out = es.decode("2;n=frozen")
    assert isinstance(out, es.Unreadable)
    assert out.reason == "HeaderInvalid"
    assert out.header.narrowing == ("frozen",)


@pytest.mark.parametrize("major", ["", "x", "12345678901", "4294967296"])
def test_bad_major_is_header_invalid(major: str) -> None:
    out = es.decode(f"{major};n=offline.")
    assert isinstance(out, es.Unreadable)
    assert out.reason == "HeaderInvalid"
    assert out.header.narrowing == ("offline",)


def test_header_only_is_readable_payload_dropped() -> None:
    value = es.craft_header_only(("no_consent",), "/h")
    assert value.endswith(".")
    out = es.decode(value)
    assert isinstance(out, dict)
    assert out["truncated"] == "payload_dropped"
    assert out["narrowing"] == ("no_consent",)
    assert out["home"] == "/h"


@pytest.mark.parametrize(
    ("kind", "reason"),
    [
        ("base64", "Base64"),
        ("inflate", "Inflate"),
        ("json", "Json"),
        ("trailing", "TrailingBytes"),
    ],
)
def test_garbled_payload_names_its_step_and_keeps_header(
    kind: str, reason: str
) -> None:
    out = es.decode(es.craft_garbled_payload(kind, ("credential_helper",), "/h"))
    assert isinstance(out, es.Unreadable)
    assert out.reason == reason
    assert out.header.narrowing == ("credential_helper",)
    assert out.header.home == "/h"


def test_other_major_is_unreadable_with_header_honoured() -> None:
    out = es.decode(es.craft_other_major(3, ("offline", "frozen"), "/h"))
    assert isinstance(out, es.Unreadable)
    assert out.reason == "OtherMajor"
    assert out.header.major == 3
    assert out.header.narrowing == ("offline", "frozen")
    assert out.header.home == "/h"


def test_other_major_one_with_no_segments() -> None:
    out = es.decode("1.")
    assert isinstance(out, es.Unreadable)
    assert out.reason == "OtherMajor"
    assert out.header.major == 1


def test_craft_other_major_refuses_current_major() -> None:
    with pytest.raises(ValueError):
        es.craft_other_major(es.MAJOR)


def test_over_cap_length_and_decode() -> None:
    value = es.craft_over_cap(("offline",))
    assert len(value) == es.MAX_ENCODED + 1
    out = es.decode(value)
    assert isinstance(out, es.Unreadable)
    assert out.reason == "TooLong"
    assert out.header.narrowing == ("offline",)


def test_value_at_cap_is_not_too_long() -> None:
    value = es.craft_over_cap(extra=0)
    assert len(value) == es.MAX_ENCODED
    out = es.decode(value)
    assert isinstance(out, es.Unreadable)
    assert out.reason != "TooLong"


def test_inflate_limit() -> None:
    import zlib

    bomb = es.header() + "." + es._b64e(zlib.compress(b" " * (es.MAX_INFLATED + 10)))
    out = es.decode(bomb)
    assert isinstance(out, es.Unreadable)
    assert out.reason == "InflateLimit"


def test_output_alphabet_of_every_producer() -> None:
    values = [
        es.encode(es.default_state(), narrowing=("offline",), home="/ünï/hôme"),
        es.craft_header_only(("frozen",), "/h"),
        es.craft_other_major(3, ("offline",), "/h"),
        es.craft_over_cap(),
        *(
            es.craft_garbled_payload(k)
            for k in ("base64", "inflate", "json", "trailing")
        ),
    ]
    for value in values:
        assert es.ALPHABET.match(value), value


def test_rust_fixture_round_trips_through_the_python_codec() -> None:
    """The fixture the Rust tests decode (`testdata/`) is read back and re-encoded by this codec."""
    import json
    import pathlib

    data = (
        pathlib.Path(__file__).resolve().parents[2]
        / "crates/ocx_config/src/exec_state/testdata"
    )
    payload = json.loads((data / "state.json").read_text())
    value = (data / "state.encoded").read_text().strip()
    assert es.ALPHABET.match(value) and len(value) <= es.MAX_ENCODED
    out = es.decode(value)
    assert isinstance(out, dict)
    assert out["narrowing"] == ("offline", "frozen")
    assert out["home"] == "/opt/ocx"
    assert {k: out[k] for k in payload} == payload
    again = es.decode(
        es.encode(payload, narrowing=("offline", "frozen"), home="/opt/ocx")
    )
    assert again == out
