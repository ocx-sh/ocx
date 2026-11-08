# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx shell state` reports the inherited execution state (ADR exec_env_model § Dump surface).

Every state is built with the Python codec (`src/exec_state.py`) and handed to the binary in
`__OCX_ENV_STATE`. Whatever the state, the command exits 0: an unreadable, truncated or
other-major value is a finding in the report, never an error.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src import exec_state
from src.runner import OcxRunner

pytestmark = pytest.mark.command("shell_state")

CARRIER = "__OCX_ENV_STATE"

# ESC, the single-byte CSI (U+009B), a right-to-left override and a left-to-right isolate:
# one of each class the stand-down notice and the dump strip.
HOSTILE = "\x1b[31m\u009b\u202e\u2066"
HOSTILE_CHARS = ("\x1b", "\u009b", "\u202e", "\u2066")


def _state(
    ocx: OcxRunner,
    state: dict | None,
    *,
    raw: str | None = None,
    verbose: bool = False,
    **encode: object,
):
    """Run `ocx shell state --format json` against one carrier value and return (process, report)."""
    value = (
        raw
        if raw is not None
        else exec_state.encode(state or exec_state.default_state(), **encode)
    )
    args = ["shell", "state"] + (["--verbose"] if verbose else [])
    env = {CARRIER: value, "OCX_NO_PROJECT": "1"}
    result = ocx.run(*args, check=False, env_overrides=env)
    assert result.returncode == 0, (
        f"shell state must exit 0 in every reportable state:\n{result.stderr}"
    )
    return result, json.loads(result.stdout)


def _shown(path: str) -> str:
    """A path as the plain report prints it: a backslash is not bare text, so the path is quoted and escaped."""
    return '"' + path.replace("\\", "\\\\") + '"' if "\\" in path else path


def _note_types(report: dict) -> list[str]:
    return [note["type"] for note in report["notes"]]


def test_an_unset_carrier_reports_only_its_status(ocx: OcxRunner) -> None:
    result = ocx.run(
        "shell", "state", check=False, env_overrides={"OCX_NO_PROJECT": "1"}
    )
    assert result.returncode == 0
    report = json.loads(result.stdout)

    assert "ledger" not in report, (
        "the legacy `ledger` field is replaced by `env_state`"
    )
    env_state = report["env_state"]
    assert env_state["status"] == "absent"
    assert env_state["major"] == exec_state.MAJOR
    assert env_state["session_claimed"] is False
    for absent in (
        "minor",
        "found_major",
        "policy",
        "replay",
        "selection",
        "truncated",
        "applied",
    ):
        assert absent not in env_state, f"an unset carrier carries no `{absent}`"
    assert not {"env_state_unreadable", "env_state_stood_down"} & set(
        _note_types(report)
    )


@pytest.mark.parametrize("with_received_path", [False, True])
def test_an_explicit_missing_config_is_still_a_config_error(
    ocx: OcxRunner, with_received_path: bool
) -> None:
    """Only a path received from a parent is tolerated; `--config` to a missing file exits 79."""
    state = exec_state.default_state()
    if with_received_path:
        state["policy"] = {
            "config_path": exec_state.absolute("/etc/ocx-fixture/received.toml")
        }
    env = {CARRIER: exec_state.encode(state), "OCX_NO_PROJECT": "1"}
    result = ocx.run(
        "--config",
        exec_state.absolute("/etc/ocx-fixture/explicit-missing.toml"),
        "shell",
        "state",
        check=False,
        env_overrides=env,
    )
    assert result.returncode == 79, (
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_a_valid_explicit_config_leaves_a_vanished_received_one_unreported(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`--config` won, so the received path was never consulted and the report owes no note about it."""
    explicit = tmp_path / "explicit.toml"
    explicit.write_text("")
    state = exec_state.default_state()
    state["policy"] = {"config_path": str(tmp_path / "vanished-received.toml")}
    env = {CARRIER: exec_state.encode(state), "OCX_NO_PROJECT": "1"}
    result = ocx.run(
        "--config", str(explicit), "shell", "state", check=False, env_overrides=env
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert "received_config_missing" not in _note_types(json.loads(result.stdout))


def test_a_readable_state_reports_every_field(ocx: OcxRunner) -> None:
    # The received config path names a file that does not exist: the report skips it and says so (exit 0),
    # where an explicit `--config` to the same path would be a config error (79).
    config_path = Path(exec_state.absolute("/etc/ocx-fixture/config.toml"))
    state = exec_state.default_state()
    state["policy"] = {
        "remote": False,
        "no_verify": True,
        "config_path": str(config_path),
    }
    state["applied"] = [
        exec_state.applied_constant("FIXTURE_CONSTANT", "alpha"),
        exec_state.applied_constant("INHERITED_CONSTANT", "beta", session=False),
        {
            "key": "FIXTURE_TOKEN",
            "value": {"literal": "tok-sentinel"},
            "kind": "constant",
            "separator": None,
            "pre_existing": False,
            "owner": "inherited",
            "prior": "withheld",
        },
    ]
    state["inherited_scopes"] = [
        "global",
        {"project": {"root": exec_state.absolute("/work/outer")}},
    ]
    state["replay"] = {
        "env": [
            {"key": "FIXTURE_REPLAY", "kind": "constant", "value": {"literal": "x"}}
        ],
        "launch_identities": [],
    }
    state["selection"] = {
        "pinned": {
            "tier": {"project": {"dir": exec_state.absolute("/work/pinned")}},
            "groups": ["ci"],
            "with_names": ["ocx.sh/cmake:3"],
            "env_keys": ["FIXTURE_KEY"],
            "invocation_digest": "0" * 64,
        }
    }
    state["session"]["token"] = {"pid": 4242}
    _, report = _state(
        ocx,
        state,
        minor=3,
        narrowing=("offline", "frozen"),
        home=exec_state.absolute("/opt/ocx-fixture-home"),
    )

    env_state = report["env_state"]
    assert env_state["status"] == "readable"
    assert (env_state["major"], env_state["minor"]) == (exec_state.MAJOR, 3)
    assert env_state["narrowing"] == ["offline", "frozen"]
    assert env_state["home"] == exec_state.absolute("/opt/ocx-fixture-home")
    assert "truncated" not in env_state
    assert env_state["session_claimed"] is True
    assert "token" not in json.dumps(env_state), "the session token is not published"
    assert env_state["policy"]["remote"] is False
    assert env_state["policy"]["no_verify"] is True
    assert env_state["policy"]["config_path"] == str(config_path)
    assert {"type": "received_config_missing", "path": str(config_path)} in report[
        "notes"
    ]
    assert env_state["inherited_scopes"] == [
        {"type": "global"},
        {"type": "project", "root": exec_state.absolute("/work/outer")},
    ]
    assert env_state["replay"] == {"env_count": 1, "launch_identity_count": 0}
    assert env_state["selection"] == {
        "type": "pinned_project",
        "project_dir": exec_state.absolute("/work/pinned"),
        "groups": ["ci"],
        "with_names": ["ocx.sh/cmake:3"],
        "env_keys": ["FIXTURE_KEY"],
    }

    by_key = {record["key"]: record for record in env_state["applied"]}
    assert list(by_key) == [
        "FIXTURE_CONSTANT",
        "INHERITED_CONSTANT",
        "FIXTURE_TOKEN",
    ], "oldest first"
    assert by_key["FIXTURE_CONSTANT"]["owner"] == "session"
    assert (
        by_key["FIXTURE_CONSTANT"]["scope"],
        by_key["FIXTURE_CONSTANT"]["source"],
    ) == ("global", "composed")
    assert by_key["FIXTURE_CONSTANT"]["value"] == "alpha"
    assert by_key["INHERITED_CONSTANT"]["owner"] == "inherited"
    assert by_key["FIXTURE_TOKEN"]["value"] == "tok-sentinel", (
        "a record publishes the value as recorded"
    )
    assert "withheld" not in by_key["FIXTURE_TOKEN"], "the report has no withheld field"
    assert by_key["FIXTURE_TOKEN"]["prior"] == {"type": "withheld"}, (
        "a non-UTF-8 prior stays withheld"
    )


def test_the_relaxing_classification_names_relaxing_fields_and_home(
    ocx: OcxRunner,
) -> None:
    state = exec_state.default_state()
    # `remote` and `no_project` are neutral; the other two relax a protection.
    state["policy"] = {
        "remote": True,
        "no_project": True,
        "no_verify": True,
        "allow_yanked": True,
    }
    _, report = _state(ocx, state, home=exec_state.absolute("/opt/ocx-fixture-home"))

    assert report["env_state"]["relaxing_policy"] == [
        "allow_yanked",
        "home",
        "no_verify",
    ]
    relaxing = [note for note in report["notes"] if note["type"] == "relaxing_policy"]
    assert [note["names"] for note in relaxing] == [
        ["allow_yanked", "home", "no_verify"]
    ]


def test_a_neutral_state_has_no_relaxing_field_and_no_note(ocx: OcxRunner) -> None:
    state = exec_state.default_state()
    state["policy"] = {"remote": True, "no_project": True}
    _, report = _state(ocx, state)

    assert "relaxing_policy" not in report["env_state"]
    assert "relaxing_policy" not in _note_types(report)


@pytest.mark.parametrize(
    ("raw", "reason", "found_major"),
    [
        (
            exec_state.craft_other_major(
                3,
                narrowing=("offline",),
                home=exec_state.absolute("/opt/ocx-fixture-home"),
            ),
            "other_major",
            3,
        ),
        (
            exec_state.craft_garbled_payload("base64", narrowing=("offline",)),
            "base64",
            exec_state.MAJOR,
        ),
        (exec_state.craft_garbled_payload("inflate"), "inflate", exec_state.MAJOR),
        (exec_state.craft_garbled_payload("json"), "json", exec_state.MAJOR),
        (exec_state.craft_over_cap(), "too_long", exec_state.MAJOR),
    ],
    ids=["other-major", "base64", "inflate", "json", "too-long"],
)
def test_an_unreadable_state_keeps_the_header_and_notes_the_refusal(
    ocx: OcxRunner, raw: str, reason: str, found_major: int
) -> None:
    _, report = _state(ocx, None, raw=raw)

    env_state = report["env_state"]
    assert env_state["status"] == "unreadable"
    assert env_state["major"] == exec_state.MAJOR
    assert env_state["found_major"] == found_major
    for body in ("policy", "applied", "inherited_scopes", "replay", "selection"):
        assert body not in env_state, f"an unreadable payload publishes no `{body}`"

    unreadable = [
        note for note in report["notes"] if note["type"] == "env_state_unreadable"
    ]
    assert len(unreadable) == 1
    assert unreadable[0]["reason"] == reason
    assert unreadable[0]["major"] == exec_state.MAJOR
    assert unreadable[0]["found_major"] == found_major
    assert "env_state_stood_down" in _note_types(report), (
        "an unreadable state stands the shell hook down"
    )
    assert report["inert_reason"]["type"] == "state_unreadable"
    assert report["inert_reason"]["first_prompt"] is False


def test_an_other_major_header_still_reports_narrowing_and_home(ocx: OcxRunner) -> None:
    raw = exec_state.craft_other_major(
        3,
        narrowing=("offline", "no_consent"),
        home=exec_state.absolute("/opt/ocx-fixture-home"),
    )
    _, report = _state(ocx, None, raw=raw)

    assert report["env_state"]["narrowing"] == ["offline", "no_consent"]
    assert report["env_state"]["home"] == exec_state.absolute("/opt/ocx-fixture-home")


def test_the_stood_down_note_is_withheld_when_the_hook_is_disabled(
    ocx: OcxRunner,
) -> None:
    raw = exec_state.craft_other_major(3)
    result = ocx.run(
        "shell",
        "state",
        check=False,
        env_overrides={CARRIER: raw, "OCX_NO_PROJECT": "1", "OCX_NO_HOOK": "1"},
    )
    assert result.returncode == 0
    report = json.loads(result.stdout)

    notes = _note_types(report)
    assert "env_state_unreadable" in notes
    assert "env_state_stood_down" not in notes, (
        "a disabled hook has nothing to stand down"
    )


@pytest.mark.parametrize(
    ("truncated", "raw"),
    [
        ("inherited_dropped", None),
        ("all_applied_dropped", None),
        ("payload_dropped", exec_state.craft_header_only(narrowing=("frozen",))),
    ],
)
def test_a_truncated_state_is_a_note_never_an_error(
    ocx: OcxRunner, truncated: str, raw: str | None
) -> None:
    state = exec_state.default_state()
    state["truncated"] = truncated
    _, report = _state(ocx, state, raw=raw)

    assert report["env_state"]["status"] == "readable"
    assert report["env_state"]["truncated"] == truncated
    notes = [note for note in report["notes"] if note["type"] == "env_state_truncated"]
    assert [note["truncated"] for note in notes] == [truncated]
    assert "inert_reason" not in report, (
        "a truncated state still reads; the note reports it, not an inert reason"
    )


def test_a_readable_state_answers_the_fingerprint_and_the_recorded_tiers(
    ocx: OcxRunner,
) -> None:
    state = exec_state.default_state()
    state["session"]["fp"] = "0" * 64
    state["session"]["tiers"] = [
        exec_state.absolute("/work/overlay-fixture/config.toml")
    ]
    _, report = _state(ocx, state)

    assert report["fingerprint_current"] is False, (
        "a recorded fingerprint no watch set folds to is stale"
    )
    assert exec_state.absolute("/work/overlay-fixture/config.toml") in [
        member["path"] for member in report["watch_set"]
    ]
    assert "inert_reason" not in report, (
        "a readable state with nothing to block is not inert"
    )

    state["session"]["fp"] = ""
    _, report = _state(ocx, state)
    assert "fingerprint_current" not in report, (
        "no recorded fingerprint, nothing to compare"
    )


def test_an_unset_carrier_is_the_first_prompt_reason(ocx: OcxRunner) -> None:
    result = ocx.run(
        "shell", "state", check=False, env_overrides={"OCX_NO_PROJECT": "1"}
    )
    report = json.loads(result.stdout)
    assert report["inert_reason"] == {"type": "state_unreadable", "first_prompt": True}


def _install_as_current(ocx: OcxRunner, destination: Path) -> Path:
    """Copy the binary to `<home>/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx` and return it.

    Every directory is closed to group and world writes: the pin follows the installed link only
    through a chain no other principal can swap, whatever the host umask.
    """
    package = ocx.ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli"
    content = destination / "content" / "bin"
    content.mkdir(parents=True)
    package.mkdir(parents=True)
    shutil.copy2(ocx.binary, content / "ocx")
    (content / "ocx").chmod(0o755)
    for directory in (
        destination,
        destination / "content",
        content,
        *package.parents,
        package,
    ):
        if directory == ocx.ocx_home or ocx.ocx_home in directory.parents:
            directory.chmod(0o755)
    (package / "current").symlink_to(destination)
    return package / "current" / "content" / "bin" / "ocx"


def test_the_binary_pin_names_the_running_binary_and_matches_it(ocx: OcxRunner) -> None:
    _, report = _state(ocx, None)

    pin = report["binary_pin"]
    assert pin["exists"] is True
    assert pin["matches_running"] is True
    assert Path(pin["path"]).resolve() == ocx.binary.resolve()
    assert "binary_pin_missing" not in _note_types(report)


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="the installed-link pin needs a POSIX permission chain",
)
def test_the_binary_pin_follows_the_installed_link_and_still_matches_the_running_binary(
    ocx: OcxRunner,
) -> None:
    installed = _install_as_current(ocx, ocx.ocx_home / "pkg-fixture")
    env = {
        CARRIER: exec_state.encode(exec_state.default_state()),
        "OCX_NO_PROJECT": "1",
    }
    result = subprocess.run(
        [str(installed), "--format", "json", "shell", "state"],
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **env},
        stdin=subprocess.DEVNULL,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    pin = json.loads(result.stdout)["binary_pin"]

    assert pin["path"] == str(installed), (
        "the pin is the installed path through `current`, not its target"
    )
    assert pin["exists"] is True
    assert pin["matches_running"] is True


def test_no_report_keeps_a_ledger_field(ocx: OcxRunner) -> None:
    for verbose in (False, True):
        _, report = _state(ocx, exec_state.default_state(), verbose=verbose)
        assert "ledger" not in report


def _hostile_state() -> dict:
    state = exec_state.default_state()
    state["inherited_scopes"] = [
        {"project": {"root": exec_state.absolute(f"/work/outer{HOSTILE}root")}}
    ]
    state["selection"] = {
        "pinned": {
            "tier": {
                "project": {"dir": exec_state.absolute(f"/work/pinned{HOSTILE}dir")}
            },
            "groups": [],
            "with_names": [],
            "env_keys": [],
            "invocation_digest": "0" * 64,
        }
    }
    state["applied"] = [
        exec_state.applied_constant(
            "FIXTURE_HOSTILE", f"value{HOSTILE}end", session=False
        )
    ]
    return state


@pytest.mark.parametrize("verbose", [False, True], ids=["plain", "verbose"])
def test_state_derived_strings_never_reach_plain_stdout_with_a_control_character(
    ocx: OcxRunner, verbose: bool
) -> None:
    value = exec_state.encode(
        _hostile_state(), home=exec_state.absolute("/opt/ocx-fixture-home")
    )
    args = ["shell", "state"] + (["--verbose"] if verbose else [])
    result = ocx.run(
        *args,
        format=None,
        check=False,
        env_overrides={CARRIER: value, "OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == 0, result.stderr
    for char in HOSTILE_CHARS:
        assert char not in result.stdout, (
            f"U+{ord(char):04X} reached the {'verbose' if verbose else 'plain'} stdout"
        )
    if verbose:
        # The strip removes the characters, not the text around them.
        assert _shown(exec_state.absolute("/work/outer[31mroot")) in result.stdout
        assert _shown(exec_state.absolute("/work/pinned[31mdir")) in result.stdout
        assert "value[31mend" in result.stdout


def test_the_json_report_carries_the_state_verbatim_where_json_escapes_it(
    ocx: OcxRunner,
) -> None:
    result, report = _state(ocx, _hostile_state())

    env_state = report["env_state"]
    assert env_state["inherited_scopes"][0]["root"] == exec_state.absolute(
        f"/work/outer{HOSTILE}root"
    )
    assert env_state["selection"]["project_dir"] == exec_state.absolute(
        f"/work/pinned{HOSTILE}dir"
    )
    assert env_state["applied"][0]["value"] == f"value{HOSTILE}end"
    # JSON escaping carries the characters safely: the document's own bytes hold none of them raw.
    assert "\x1b" not in result.stdout, (
        "JSON escapes a control character; it never prints it raw"
    )
