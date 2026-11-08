# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Hostile and garbled input to the execution-state model.

Every state is crafted with ``src.exec_state`` and handed to the real binary in ``__OCX_ENV_STATE``;
nothing here runs a parent ocx. The contract under test (ADR exec_env_model § Codec, § Policy, § Hook
behaviour): a value that cannot be trusted is refused whole, never half-read; a refusal is a report
and a stand-down, never a crash or a nonzero hook exit; a forged record can only undo what the reader
would let it undo; a relaxing received value never beats an explicit local one.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import zlib
from pathlib import Path

import pytest

import shell_matrix as matrix
from src import exec_state
from src.exec_state import _b64e
from src.runner import OcxRunner

pytestmark = pytest.mark.command("exec", "shell_state", "self_group/activate")

CARRIER = "__OCX_ENV_STATE"
EXIT_POLICY_BLOCKED = 81
UNINDEXED = "nonexistent_hardening_input_pkg:0"
_SENTINEL = "fixture-secret-value"
_PID = os.getpid()
_POSIX_ONLY = pytest.mark.skipif(
    sys.platform == "win32",
    reason="the children and the hook are probed through POSIX tools",
)

# ---------------------------------------------------------------------------
# Helpers (DAMP -- self-contained)
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    *args: str,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **(env or {})},
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=120,
    )


def _report(ocx: OcxRunner, raw: str, **env: str) -> dict:
    """``ocx shell state --format json`` over one carrier value; exit 0 is part of the contract."""
    result = _run(
        ocx,
        "--format",
        "json",
        "shell",
        "state",
        env={CARRIER: raw, "OCX_NO_PROJECT": "1", **env},
    )
    assert result.returncode == 0, (
        f"shell state must exit 0 whatever the carrier holds:\n{result.stderr}"
    )
    return json.loads(result.stdout)


def _raw_payload(body: bytes, *, header: str | None = None) -> str:
    """A current-major value around arbitrary inflated bytes."""
    return (
        (header if header is not None else exec_state.header())
        + "."
        + _b64e(zlib.compress(body))
    )


def _unreadable_reason(report: dict) -> dict:
    notes = [note for note in report["notes"] if note["type"] == "env_state_unreadable"]
    assert report["env_state"]["status"] == "unreadable", (
        f"expected unreadable; got {report['env_state']}"
    )
    assert len(notes) == 1, (
        f"exactly one unreadable note expected; got {report['notes']}"
    )
    return notes[0]


def _state(**fields: object) -> dict:
    state = exec_state.default_state()
    state.update(fields)
    return state


def _record(
    key: str,
    value: object,
    kind: str = "constant",
    *,
    owner: object = "inherited",
    **extra: object,
) -> dict:
    return {
        "key": key,
        "value": value,
        "kind": kind,
        "separator": None,
        "pre_existing": False,
        "owner": owner,
        "prior": "unset" if kind == "constant" else None,
        **extra,
    }


def _project(ocx: OcxRunner, root: Path, body: str) -> Path:
    """Write ``root/ocx.toml`` and lock it, which also records the consent."""
    root.mkdir(parents=True, exist_ok=True)
    (root / "ocx.toml").write_text(body, encoding="utf-8")
    done = _run(ocx, "lock", cwd=root)
    assert done.returncode == 0, f"ocx lock must succeed for the fixture: {done.stderr}"
    return root


def _probe(var: str) -> list[str]:
    """A child printing ``var`` as it received it, or ``unset``; Python so a Windows runner has it too."""
    code = f"import os, sys; sys.stdout.write(os.environ.get({var!r}, 'unset'))"
    return [sys.executable, "-c", code]


# ---------------------------------------------------------------------------
# Garbled values: refused whole, reported, exit 0
# ---------------------------------------------------------------------------

_VALID = exec_state.encode(exec_state.default_state())


@pytest.mark.parametrize(
    "raw",
    [
        pytest.param(_VALID[: len(_VALID) // 2], id="truncated-half"),
        pytest.param(_VALID[:-1], id="truncated-last-char"),
        pytest.param("garbage", id="no-dot"),
        pytest.param("2.not base64!", id="space-and-bang"),
        pytest.param("2.abc\ndef", id="newline-in-payload"),
        pytest.param("x.AAAA", id="non-numeric-major"),
        pytest.param("-1.AAAA", id="negative-major"),
        pytest.param("99999999999.AAAA", id="major-past-u32"),
        pytest.param("é.AAAA", id="non-ascii-header"),
        pytest.param(".AAAA", id="empty-major"),
        pytest.param("2;n=" + "offline," * 400 + ".AAAA", id="header-past-cap"),
    ],
)
def test_garbled_carrier_is_refused_reported_and_exits_zero(
    ocx: OcxRunner, raw: str
) -> None:
    """A carrier no codec step accepts is one unreadable note and a stood-down hook, never an error."""
    report = _report(ocx, raw)
    assert report["env_state"]["status"] == "unreadable"
    assert "env_state_stood_down" in [note["type"] for note in report["notes"]]
    for body in ("applied", "policy", "selection"):
        assert body not in report["env_state"], (
            f"an unreadable carrier publishes no `{body}`"
        )


def test_an_empty_carrier_reads_as_absent(ocx: OcxRunner) -> None:
    """Unset mid-session and exported empty are the same: nothing was received."""
    report = _report(ocx, "")
    assert report["env_state"]["status"] == "absent"


def test_a_duplicate_json_field_refuses_the_whole_payload(ocx: OcxRunner) -> None:
    reason = _unreadable_reason(_report(ocx, _raw_payload(b'{"minor":0,"minor":1}')))
    assert reason["reason"] == "json"


def test_json_nesting_past_the_recursion_limit_is_refused_not_a_crash(
    ocx: OcxRunner,
) -> None:
    reason = _unreadable_reason(
        _report(
            ocx, _raw_payload(b'{"applied":' + b"[" * 60_000 + b"]" * 60_000 + b"}")
        )
    )
    assert reason["reason"] == "json"


def test_an_inflate_bomb_inside_the_length_cap_is_refused(ocx: OcxRunner) -> None:
    """300 KiB of zeros deflates to under 1 KiB: the cap is on the inflated size too."""
    raw = _raw_payload(b"0" * (300 * 1024))
    assert len(raw) < exec_state.MAX_ENCODED
    assert _unreadable_reason(_report(ocx, raw))["reason"] == "inflate_limit"


@pytest.mark.parametrize(
    ("state", "reason"),
    [
        pytest.param(
            _state(
                replay={
                    "env": [{"key": "K", "kind": "path", "value": {"literal": "x"}}]
                    * 4097,
                    "launch_identities": [],
                }
            ),
            "record_limit",
            id="records",
        ),
        pytest.param(
            _state(inherited_scopes=["global"] * 17), "scope_limit", id="scopes"
        ),
        pytest.param(
            _state(
                session={
                    **exec_state.default_state()["session"],
                    "tiers": [f"/t{i}" for i in range(9)],
                }
            ),
            "tier_limit",
            id="tiers",
        ),
    ],
)
def test_counts_past_the_documented_limits_are_refused(
    ocx: OcxRunner, state: dict, reason: str
) -> None:
    raw = exec_state.encode(state)
    assert len(raw) <= exec_state.MAX_ENCODED, (
        "the fixture must fit the length cap to reach the count check"
    )
    assert _unreadable_reason(_report(ocx, raw))["reason"] == reason


def test_a_higher_minor_with_unknown_fields_is_read_and_the_unknowns_ignored(
    ocx: OcxRunner,
) -> None:
    state = _state(future_field={"nested": [1, 2, 3]})
    state["policy"] = {"remote": True, "from_a_newer_minor": "x"}
    raw = exec_state.encode(state, minor=99)
    report = _report(ocx, raw)
    assert report["env_state"]["status"] == "readable"
    assert report["env_state"]["minor"] == 99
    assert report["env_state"]["policy"]["remote"] is True


def test_an_other_major_keeps_offline_and_a_garbled_current_major_keeps_it_too(
    ocx: OcxRunner,
) -> None:
    """The header is honoured whatever the payload does; the received narrowing blocks the registry."""
    for raw in (
        exec_state.craft_other_major(7, narrowing=("offline",)),
        exec_state.craft_garbled_payload("inflate", narrowing=("offline",)),
        exec_state.craft_over_cap(narrowing=("offline",)),
    ):
        done = _run(
            ocx,
            "package",
            "install",
            UNINDEXED,
            env={CARRIER: raw, "OCX_OFFLINE": "0", "OCX_REMOTE": "1"},
        )
        assert done.returncode == EXIT_POLICY_BLOCKED, (
            f"{raw[:40]!r}: rc={done.returncode} stderr={done.stderr!r}"
        )


# ---------------------------------------------------------------------------
# Forged fields: fail closed on the whole payload
# ---------------------------------------------------------------------------

_DIGEST = "sha256:" + "a" * 64


def _identity(names: list[str]) -> dict:
    return {"digest": _DIGEST, "names": names, "owner": "inherited"}


def _pinned(tier: object, **extra: object) -> dict:
    return {
        "pinned": {
            "tier": tier,
            "groups": [],
            "with_names": [],
            "env_keys": [],
            "invocation_digest": "0" * 64,
            **extra,
        }
    }


_FORGED = [
    pytest.param(
        _state(selection=_pinned({"project": {"dir": "relative/dir"}})),
        "selection.pinned.dir",
        id="relative-pinned-dir",
    ),
    pytest.param(
        _state(selection=_pinned({"project": {"dir": "."}})),
        "selection.pinned.dir",
        id="dot-pinned-dir",
    ),
    pytest.param(
        _state(selection=_pinned("global", groups=["a/b"])),
        "selection.pinned.groups",
        id="group-separator",
    ),
    pytest.param(
        _state(selection=_pinned("global", groups=["../x"])),
        "selection.pinned.groups",
        id="group-dotdot",
    ),
    pytest.param(
        _state(selection=_pinned("global", invocation_digest="zz")),
        "selection.pinned.invocation_digest",
        id="bad-digest",
    ),
    pytest.param(
        _state(inherited_scopes=[{"project": {"root": "rel"}}]),
        "inherited_scopes.root",
        id="relative-scope-root",
    ),
    pytest.param(
        _state(applied=[_record("OCX_HOME", {"literal": "/x"})]),
        "applied.key",
        id="reserved-key-home",
    ),
    pytest.param(
        _state(applied=[_record("__OCX_ENV_STATE", {"literal": "x"})]),
        "applied.key",
        id="reserved-key-carrier",
    ),
    pytest.param(
        _state(applied=[_record("A=B", {"literal": "x"})]),
        "applied.key",
        id="key-with-equals",
    ),
    pytest.param(
        _state(applied=[_record("", {"literal": "x"})]), "applied.key", id="empty-key"
    ),
    pytest.param(
        _state(applied=[_record("PATH", {"literal": "/x"})]),
        "applied.key",
        id="path-as-constant",
    ),
    pytest.param(
        _state(applied=[_record("OCX_AUTH_h_TOKEN", {"literal": _SENTINEL})]),
        "applied.key",
        id="credential-slot-literal",
    ),
    pytest.param(
        _state(applied=[_record("FIXTURE_L", {"literal": "x"}, "list")]),
        "applied.separator",
        id="list-without-separator",
    ),
    pytest.param(
        _state(applied=[_record("FIXTURE_C", {"literal": "x"}, separator=":")]),
        "applied.separator",
        id="constant-with-separator",
    ),
    pytest.param(
        _state(
            applied=[
                _record("FIXTURE_P", {"literal": "/x"}, "path", prior={"value": "y"})
            ]
        ),
        "applied.prior",
        id="prior-on-path",
    ),
    pytest.param(
        _state(
            replay={
                "env": [
                    {"key": "OCX_HOME", "kind": "constant", "value": {"literal": "/x"}}
                ],
                "launch_identities": [],
            }
        ),
        "replay.env.key",
        id="replay-reserved-key",
    ),
    pytest.param(
        _state(
            replay={
                "env": [
                    {"key": "PATH", "kind": "constant", "value": {"literal": "/x"}}
                ],
                "launch_identities": [],
            }
        ),
        "replay.env.key",
        id="replay-path-constant",
    ),
    pytest.param(
        _state(replay={"env": [], "launch_identities": [_identity(["a@b/c:1"])]}),
        "replay.launch_identities.names",
        id="identity-userinfo",
    ),
    pytest.param(
        _state(
            replay={"env": [], "launch_identities": [_identity(["ocx.sh/a\x1bb:1"])]}
        ),
        "replay.launch_identities.names",
        id="identity-control",
    ),
    pytest.param(
        _state(replay={"env": [], "launch_identities": [_identity(["not a ref"])]}),
        "replay.launch_identities.names",
        id="identity-garbage",
    ),
    pytest.param(
        _state(
            replay={
                "env": [],
                "launch_identities": [{**_identity([]), "digest": "sha256:short"}],
            }
        ),
        "replay.launch_identities.digest",
        id="identity-digest",
    ),
    pytest.param(
        _state(policy={"config_path": "rel.toml"}),
        "policy.config_path",
        id="relative-config-path",
    ),
    pytest.param(
        _state(policy={"index_dir": "rel"}), "policy.index_dir", id="relative-index-dir"
    ),
    pytest.param(
        _state(policy={"records_dir": "rel"}),
        "policy.records_dir",
        id="relative-records-dir",
    ),
    pytest.param(
        _state(policy={"patch_snapshot_path": "rel"}),
        "policy.patch_snapshot_path",
        id="relative-snapshot",
    ),
    pytest.param(
        _state(policy={"records_name": "../x"}),
        "policy.records_name",
        id="records-name-separator",
    ),
    pytest.param(
        _state(policy={"records_name": ".."}),
        "policy.records_name",
        id="records-name-dotdot",
    ),
    pytest.param(
        _state(policy={"mirrors": "not json"}), "policy.mirrors", id="mirrors-not-json"
    ),
    pytest.param(
        _state(
            policy={
                "mirrors": json.dumps(
                    {
                        "reg.example": {
                            "registry": f"https://fixture-user:{_SENTINEL}@m.example/"
                        }
                    }
                )
            }
        ),
        "policy.mirrors",
        id="mirrors-userinfo",
    ),
    pytest.param(
        _state(policy={"patches": json.dumps({"registry": "a/b"})}),
        "policy.patches",
        id="patches-registry-path",
    ),
    pytest.param(
        _state(policy={"patches": "[]"}), "policy.patches", id="patches-not-object"
    ),
    pytest.param(
        _state(policy={"managed_config": "::"}),
        "policy.managed_config",
        id="managed-config-garbage",
    ),
    pytest.param(
        _state(policy={"insecure_registries": "ok.example,evil.example/x"}),
        "policy.insecure_registries",
        id="insecure-with-path",
    ),
    pytest.param(
        _state(policy={"insecure_registries": "u@h"}),
        "policy.insecure_registries",
        id="insecure-with-userinfo",
    ),
    pytest.param(
        _state(policy={"default_registry": "a b"}),
        "policy.default_registry",
        id="default-registry-space",
    ),
    pytest.param(
        _state(
            session={
                **exec_state.default_state()["session"],
                "tiers": ["rel/tier.toml"],
            }
        ),
        "session.tiers",
        id="relative-session-tier",
    ),
    pytest.param(
        _state(
            session={
                **exec_state.default_state()["session"],
                "project": {"dir": "rel", "key": "0" * 16},
            }
        ),
        "session.project.dir",
        id="relative-session-project",
    ),
]


@pytest.mark.parametrize(("state", "field"), _FORGED)
def test_a_forged_field_refuses_the_whole_payload_naming_the_field(
    ocx: OcxRunner, state: dict, field: str
) -> None:
    note = _unreadable_reason(
        _report(ocx, exec_state.encode(state, narrowing=("offline",)))
    )
    assert note["reason"] == "invalid", note
    assert note.get("field") == field, note


def test_a_refused_forgery_still_honours_the_header_narrowing(ocx: OcxRunner) -> None:
    raw = exec_state.encode(_state(policy={"index_dir": "rel"}), narrowing=("offline",))
    done = _run(
        ocx, "package", "install", UNINDEXED, env={CARRIER: raw, "OCX_OFFLINE": "0"}
    )
    assert done.returncode == EXIT_POLICY_BLOCKED, done.stderr


def test_a_from_inherited_replay_payload_is_an_unreadable_state(
    ocx: OcxRunner,
) -> None:
    """The retired reference form has no reader: the whole payload is unreadable and nothing leaks."""
    replay = {
        "env": [
            {"key": "FIXTURE_PLAIN", "kind": "constant", "value": "from_inherited"}
        ],
        "launch_identities": [],
    }
    report = _report(
        ocx, exec_state.encode(_state(replay=replay)), FIXTURE_PLAIN=_SENTINEL
    )
    assert report["env_state"]["status"] == "unreadable"
    assert _SENTINEL not in json.dumps(report)


def test_a_literal_under_a_secret_key_or_a_url_with_userinfo_is_published_as_recorded(
    ocx: OcxRunner,
) -> None:
    url = f"https://user:{_SENTINEL}@host.example/"
    state = _state(
        applied=[
            _record("FIXTURE_API_TOKEN", {"literal": _SENTINEL}),
            _record("FIXTURE_REMOTE", {"literal": url}),
            _record("FIXTURE_PLAIN", {"literal": "plain-visible"}),
        ]
    )
    raw = exec_state.encode(state)
    for verbose in (False, True):
        args = [
            "--format",
            "json",
            "shell",
            "state",
            *(["--verbose"] if verbose else []),
        ]
        done = _run(ocx, *args, env={CARRIER: raw, "OCX_NO_PROJECT": "1"})
        assert done.returncode == 0
        assert done.stderr == ""
    by_key = {
        record["key"]: record
        for record in json.loads(done.stdout)["env_state"]["applied"]
    }
    assert by_key["FIXTURE_API_TOKEN"]["value"] == _SENTINEL
    assert by_key["FIXTURE_REMOTE"]["value"] == url
    assert all("withheld" not in record for record in by_key.values())
    assert by_key["FIXTURE_PLAIN"]["value"] == "plain-visible"


# ---------------------------------------------------------------------------
# A forged record can only undo what the reader lets it undo
# ---------------------------------------------------------------------------


@pytest.fixture
def isolate_project(ocx: OcxRunner, tmp_path: Path) -> Path:
    return _project(ocx, tmp_path / "proj", "[tools]\n")


def _isolate(
    ocx: OcxRunner, project: Path, state: dict, probe: str, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return _run(
        ocx,
        "exec",
        "--isolate",
        "--",
        *_probe(probe),
        env={CARRIER: exec_state.encode(state), **env},
        cwd=project,
    )


def test_isolate_removes_a_constant_a_record_owns_byte_for_byte(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    """Control for the forgeries below: the revert is reachable, so a kept variable is meaningful."""
    state = _state(applied=[_record("FIXTURE_PLAIN", {"literal": "alpha"})])
    done = _isolate(
        ocx, isolate_project, state, "FIXTURE_PLAIN", {"FIXTURE_PLAIN": "alpha"}
    )
    assert (done.returncode, done.stdout) == (0, "unset"), done.stderr


@pytest.mark.parametrize(
    "record",
    [
        pytest.param(_record("FIXTURE_PLAIN", "present"), id="retired-presence-form"),
        pytest.param(
            _record("FIXTURE_PLAIN", {"literal": "other"}),
            id="literal-differs-from-live",
        ),
        pytest.param(
            _record("FIXTURE_PLAIN", {"literal": "alpha"}, prior="withheld"),
            id="prior-withheld",
        ),
    ],
)
def test_a_forged_record_cannot_delete_an_ordinary_variable(
    ocx: OcxRunner, isolate_project: Path, record: dict
) -> None:
    done = _isolate(
        ocx,
        isolate_project,
        _state(applied=[record]),
        "FIXTURE_PLAIN",
        {"FIXTURE_PLAIN": "alpha"},
    )
    assert (done.returncode, done.stdout) == (0, "alpha"), done.stderr


@pytest.mark.parametrize(
    "record",
    [
        pytest.param(_record("PATH", {"literal": ""}, "path"), id="empty-path-element"),
        pytest.param(
            _record("PATH", "present", "path"), id="retired-presence-form-path"
        ),
        pytest.param(
            _record("PATH", {"literal": "/"}, "path", pre_existing=True),
            id="pre-existing",
        ),
    ],
)
def test_a_forged_path_record_never_wipes_or_shrinks_the_search_path(
    ocx: OcxRunner, isolate_project: Path, record: dict
) -> None:
    done = _isolate(ocx, isolate_project, _state(applied=[record]), "PATH", {})
    assert done.returncode == 0, done.stderr
    assert (
        shutil.which("env") is not None
        and Path(done.stdout.split(os.pathsep)[0]).is_absolute()
    ), done.stdout
    assert len(done.stdout.split(os.pathsep)) >= len(
        ocx.env["PATH"].split(os.pathsep)
    ), "no segment may be lost"


def test_isolate_under_an_unreadable_state_warns_and_runs(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    done = _run(
        ocx,
        "exec",
        "--isolate",
        "--",
        *_probe("FIXTURE_PLAIN"),
        env={CARRIER: exec_state.craft_other_major(9), "FIXTURE_PLAIN": "alpha"},
        cwd=isolate_project,
    )
    assert (done.returncode, done.stdout) == (0, "alpha"), done.stderr
    assert "isolate" in done.stderr.lower(), (
        "the user is told the isolation could not be applied"
    )


# ---------------------------------------------------------------------------
# Hook: credential variables, forged pinned directories
# ---------------------------------------------------------------------------


def _hook(
    ocx: Path,
    tmp_path: Path,
    forge: dict,
    extra_env: dict[str, str],
    *,
    cwd: Path | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
    """Claim a session with a first prompt, graft ``forge`` fields into its state, run a second prompt.

    Returns the second prompt's process and the environment a bash that evaluated its stream holds.
    """
    bash = shutil.which("bash")
    assert bash, "bash is required to evaluate the emitted stream"
    home = tmp_path / "hook-home"
    (home / ".ocx").mkdir(parents=True, exist_ok=True)
    outside = tmp_path / "outside"
    outside.mkdir(exist_ok=True)
    work = cwd or outside
    argv = [
        str(ocx),
        "--offline",
        "self",
        "activate",
        "--reconcile",
        "--shell=bash",
        "--shell-pid",
        str(_PID),
    ]

    def prompt(
        env: dict[str, str],
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
        process = subprocess.run(
            argv,
            cwd=str(work),
            env=env,
            capture_output=True,
            text=True,
            check=False,
            timeout=120,
        )
        assert process.returncode == 0, (
            f"the hook path exits 0 in every state:\n{process.stderr}"
        )
        evaluated = subprocess.run(
            [bash, "--norc", "-c", 'eval "$1"; env -0', "bash", process.stdout],
            cwd=str(work),
            env=env,
            capture_output=True,
            check=False,
            timeout=60,
        )
        after = dict(
            entry.decode().partition("=")[::2]
            for entry in evaluated.stdout.split(b"\0")
            if b"=" in entry
        )
        return process, after

    env = matrix.clean_env(home, bash, ocx_home=home / ".ocx", **extra_env)
    _, claimed = prompt(env)
    state = exec_state.decode(claimed.get(CARRIER))
    assert isinstance(state, dict), (
        f"the first prompt must leave a readable state; got {state!r}"
    )
    payload = {
        key: value
        for key, value in state.items()
        if key not in ("narrowing", "home", "major")
    }
    for key, value in forge.items():
        if isinstance(value, list):
            payload[key] = payload.get(key, []) + value
        elif isinstance(value, dict) and isinstance(payload.get(key), dict):
            payload[key] = {**payload[key], **value}
        else:
            payload[key] = value
    forged_env = {
        **claimed,
        **extra_env,
        CARRIER: exec_state.encode(
            payload, narrowing=tuple(state["narrowing"]), home=state["home"]
        ),
    }
    return prompt(forged_env)


def _session_owner() -> dict:
    return {"session": {"scope": "global", "source": "composed"}}


@_POSIX_ONLY
def test_the_hook_unsets_a_secret_named_variable_a_session_record_owns(
    ocx_binary: Path, tmp_path: Path
) -> None:
    """Control: a literal session record on a secret-named key is honoured while the value matches, so the next tests can go red."""
    record = _record(
        "FIXTURE_API_TOKEN", {"literal": _SENTINEL}, owner=_session_owner()
    )
    process, after = _hook(
        ocx_binary,
        tmp_path,
        {"applied": [record], "session": {"fp": "0" * 64}},
        {"FIXTURE_API_TOKEN": _SENTINEL},
    )
    assert "FIXTURE_API_TOKEN" not in after, process.stdout[-600:]


@_POSIX_ONLY
def test_the_hook_keeps_an_ordinary_variable_a_retired_presence_record_names(
    ocx_binary: Path, tmp_path: Path
) -> None:
    record = _record("FIXTURE_PLAIN", "present", owner=_session_owner())
    _, after = _hook(
        ocx_binary,
        tmp_path,
        {"applied": [record], "session": {"fp": "0" * 64}},
        {"FIXTURE_PLAIN": "alpha"},
    )
    assert after.get("FIXTURE_PLAIN") == "alpha"


@_POSIX_ONLY
def test_a_forged_session_record_cannot_make_the_hook_drop_a_credential_variable(
    ocx_binary: Path, tmp_path: Path
) -> None:
    record = _record(
        "OCX_AUTH_fixture_example_TOKEN",
        {"literal": _SENTINEL},
        owner=_session_owner(),
    )
    _, after = _hook(
        ocx_binary,
        tmp_path,
        {"applied": [record], "session": {"fp": "0" * 64}},
        {"OCX_AUTH_fixture_example_TOKEN": _SENTINEL},
    )
    assert after.get("OCX_AUTH_fixture_example_TOKEN") == _SENTINEL


_USER_CREDENTIAL = "OCX_AUTH_fixture_example_TOKEN"


@_POSIX_ONLY
def test_an_isolate_child_keeps_a_user_supplied_registry_credential(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    """Control: the per-registry credential is forwarded, so losing it below is a regression."""
    done = _isolate(
        ocx, isolate_project, _state(), _USER_CREDENTIAL, {_USER_CREDENTIAL: _SENTINEL}
    )
    assert (done.returncode, done.stdout) == (0, _SENTINEL), done.stderr


@_POSIX_ONLY
def test_an_isolate_child_never_holds_a_signing_credential(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    done = _isolate(
        ocx,
        isolate_project,
        _state(),
        "OCX_IDENTITY_TOKEN",
        {"OCX_IDENTITY_TOKEN": _SENTINEL},
    )
    assert (done.returncode, done.stdout) == (0, "unset"), done.stderr


@_POSIX_ONLY
def test_a_forged_record_cannot_make_an_isolate_child_lose_a_forwarded_credential(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    state = _state(applied=[_record(_USER_CREDENTIAL, {"literal": _SENTINEL})])
    done = _isolate(
        ocx, isolate_project, state, _USER_CREDENTIAL, {_USER_CREDENTIAL: _SENTINEL}
    )
    assert (done.returncode, done.stdout) == (0, _SENTINEL), done.stderr


_LIVE_TOKEN = "live-token-sentinel"


@_POSIX_ONLY
def test_isolate_keeps_a_live_secret_named_variable_a_record_on_another_value_names(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    """A record on a secret-named key never deletes a value the user holds (exact-value revert)."""
    state = _state(applied=[_record("GITHUB_TOKEN", {"literal": "recorded-token"})])
    done = _isolate(
        ocx, isolate_project, state, "GITHUB_TOKEN", {"GITHUB_TOKEN": _LIVE_TOKEN}
    )
    assert (done.returncode, done.stdout) == (0, _LIVE_TOKEN), done.stderr


@_POSIX_ONLY
def test_the_hook_keeps_a_live_secret_named_variable_a_record_on_another_value_names(
    ocx_binary: Path, tmp_path: Path
) -> None:
    record = _record(
        "GITHUB_TOKEN", {"literal": "recorded-token"}, owner=_session_owner()
    )
    _, after = _hook(
        ocx_binary,
        tmp_path,
        {"applied": [record], "session": {"fp": "0" * 64}},
        {"GITHUB_TOKEN": _LIVE_TOKEN},
    )
    assert after.get("GITHUB_TOKEN") == _LIVE_TOKEN


@_POSIX_ONLY
def test_a_record_on_a_signing_credential_makes_the_payload_unreadable_and_keeps_the_credential(
    ocx: OcxRunner, isolate_project: Path
) -> None:
    state = _state(applied=[_record("OCX_IDENTITY_TOKEN", {"literal": _SENTINEL})])
    note = _unreadable_reason(_report(ocx, exec_state.encode(state)))
    assert note.get("field") == "applied.key", note
    done = _isolate(
        ocx,
        isolate_project,
        state,
        "OCX_IDENTITY_TOKEN",
        {"OCX_IDENTITY_TOKEN": _SENTINEL},
    )
    assert done.returncode == 0, done.stderr
    assert done.stderr.lower().count("unreadable") == 1, done.stderr
    assert _SENTINEL not in done.stdout + done.stderr


_MALFORMED = "this = is [not valid toml\n"


@_POSIX_ONLY
@pytest.mark.parametrize("shape", ["direct", "symlinked", "noncanonical"])
def test_a_pinned_dir_without_consent_is_never_parsed(
    ocx_binary: Path, tmp_path: Path, shape: str
) -> None:
    """A forged pin at an unconsented directory holding a malformed ``ocx.toml`` stays unread, whatever its spelling."""
    real = tmp_path / "forged"
    real.mkdir()
    (real / "ocx.toml").write_text(_MALFORMED, encoding="utf-8")
    spelled = {
        "direct": real,
        "symlinked": tmp_path / "link",
        "noncanonical": tmp_path / "forged" / ".." / "forged",
    }[shape]
    if shape == "symlinked":
        spelled.symlink_to(real)
    state = {
        "selection": _pinned({"project": {"dir": str(spelled)}}, groups=["default"])
    }
    process, after = _hook(ocx_binary, tmp_path, state, {"FIXTURE_PLAIN": "alpha"})
    assert process.returncode == 0, process.stderr
    assert "is not activated" in process.stdout, process.stdout[-600:]
    for text in (process.stdout, process.stderr):
        assert "invalid toml" not in text.lower(), "the file must not be parsed"
        assert "parse error" not in text.lower(), "the file must not be parsed"
    assert after.get("FIXTURE_PLAIN") == "alpha"


@_POSIX_ONLY
def test_the_malformed_fixture_is_reported_when_a_command_does_read_it(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Control: reading the same malformed file is observable, so the never-parsed test can go red."""
    project = tmp_path / "forged"
    project.mkdir()
    (project / "ocx.toml").write_text(_MALFORMED, encoding="utf-8")
    done = _run(ocx, "--project", str(project), "exec", "--", "true", cwd=tmp_path)
    assert done.returncode != 0
    assert "invalid toml" in done.stderr.lower(), done.stderr


@_POSIX_ONLY
def test_a_pinned_dir_that_does_not_exist_is_quiet_and_not_a_failure(
    ocx_binary: Path, tmp_path: Path
) -> None:
    state = {
        "selection": _pinned(
            {"project": {"dir": str(tmp_path / "vanished")}}, groups=["default"]
        )
    }
    process, after = _hook(ocx_binary, tmp_path, state, {"FIXTURE_PLAIN": "alpha"})
    assert process.returncode == 0, process.stderr
    assert process.stderr == ""
    assert ">&2" not in process.stdout, "the stream must not write a notice to stderr"
    assert "vanished" not in process.stdout
    assert after.get("FIXTURE_PLAIN") == "alpha"


_PROJECT_VAR = "FIXTURE_PROJECT_LAYER"


def _project_layer_forge(selection: object) -> dict:
    owner = {"session": {"scope": "project", "source": "composed"}}
    record = _record(_PROJECT_VAR, {"literal": "spawn-time"}, owner=owner)
    return {
        "applied": [record],
        "selection": selection,
        "session": {"fp": "0" * 64},
    }


@_POSIX_ONLY
def test_a_vanished_pinned_dir_keeps_the_recorded_project_layer(
    ocx_binary: Path, tmp_path: Path
) -> None:
    """The spawn-time project layer survives a pin whose directory is gone."""
    pin = _pinned({"project": {"dir": str(tmp_path / "vanished")}}, groups=["default"])
    _, after = _hook(
        ocx_binary,
        tmp_path,
        _project_layer_forge(pin),
        {_PROJECT_VAR: "spawn-time"},
    )
    assert after.get(_PROJECT_VAR) == "spawn-time"


@_POSIX_ONLY
def test_an_unpinned_prompt_retires_the_same_recorded_project_layer(
    ocx_binary: Path, tmp_path: Path
) -> None:
    """Control: with no pin the same record is retired, so the keep test can see a change."""
    _, after = _hook(
        ocx_binary,
        tmp_path,
        _project_layer_forge("follow_cwd"),
        {_PROJECT_VAR: "spawn-time"},
    )
    assert _PROJECT_VAR not in after


# ---------------------------------------------------------------------------
# Ambient variables against a received policy
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("field", "received", "variable", "ambient"),
    [
        pytest.param(
            "insecure_registries",
            "received.example:5000",
            "OCX_INSECURE_REGISTRIES",
            "ambient.example:5000",
            id="insecure",
        ),
        pytest.param("no_verify", True, "OCX_NO_VERIFY", "0", id="no-verify"),
        pytest.param(
            "default_registry",
            "received.example",
            "OCX_DEFAULT_REGISTRY",
            "ambient.example",
            id="default-registry",
        ),
        pytest.param(
            "mirrors",
            json.dumps({"r.example": {"registry": "https://m.example"}}),
            "OCX_MIRRORS",
            "{}",
            id="mirrors",
        ),
        pytest.param(
            "patches",
            json.dumps({"registry": "received.example"}),
            "OCX_PATCHES",
            json.dumps({"registry": "ambient.example"}),
            id="patches",
        ),
    ],
)
def test_an_explicit_ambient_value_beats_a_relaxing_received_one_with_one_warning(
    ocx: OcxRunner, field: str, received: object, variable: str, ambient: str
) -> None:
    raw = exec_state.encode(_state(policy={field: received}))
    done = _run(
        ocx,
        "--offline",
        "package",
        "install",
        UNINDEXED,
        env={CARRIER: raw, variable: ambient},
    )
    naming = [line for line in done.stderr.splitlines() if variable in line]
    assert len(naming) == 1, (
        f"exactly one warning naming {variable} expected; stderr={done.stderr!r}"
    )
    assert "OCX_NO_VERIFY=1" not in done.stderr


def test_a_received_narrowing_survives_every_contradicting_ambient_spelling(
    ocx: OcxRunner,
) -> None:
    raw = exec_state.encode(_state(), narrowing=("offline",))
    for ambient in (
        {"OCX_OFFLINE": "0"},
        {"OCX_OFFLINE": "false"},
        {"OCX_REMOTE": "1"},
        {"OCX_OFFLINE": ""},
    ):
        done = _run(ocx, "package", "install", UNINDEXED, env={CARRIER: raw, **ambient})
        assert done.returncode == EXIT_POLICY_BLOCKED, f"{ambient}: {done.stderr}"


def test_the_relaxing_classification_lists_every_hostile_field(ocx: OcxRunner) -> None:
    policy = {
        "insecure_registries": "evil.example",
        "mirrors": json.dumps({"r.example": {"registry": "https://m.example"}}),
        "patches": json.dumps({"registry": "evil.example"}),
        "managed_config": "evil.example/cfg:1",
        "no_verify": True,
        "config_path": exec_state.absolute("/etc/ocx-fixture/evil.toml"),
    }
    report = _report(ocx, exec_state.encode(_state(policy=policy)))
    assert report["env_state"]["status"] == "readable"
    assert set(report["env_state"]["relaxing_policy"]) == set(policy)


# ---------------------------------------------------------------------------
# Stale and contradicting process-level variables
# ---------------------------------------------------------------------------


@_POSIX_ONLY
@pytest.mark.parametrize("value", ["1", "true", "garbage", ""])
def test_a_stale_ocx_global_is_one_notice_and_changes_nothing(
    ocx: OcxRunner, isolate_project: Path, value: str
) -> None:
    done = _run(
        ocx,
        "exec",
        "--",
        *_probe("OCX_GLOBAL"),
        env={"OCX_GLOBAL": value},
        cwd=isolate_project,
    )
    assert done.returncode == 0, done.stderr
    if value:
        assert (
            len([line for line in done.stderr.splitlines() if "--global" in line]) == 1
        ), done.stderr


@pytest.fixture
def two_projects(ocx: OcxRunner, tmp_path: Path) -> tuple[Path, Path]:
    body = '[tools]\n\n[env]\nWHICH_PROJECT = "{name}"\n\n[group.g1.env]\nWHICH_GROUP = "{name}-g1"\n'
    return (
        _project(ocx, tmp_path / "a", body.format(name="a")),
        _project(ocx, tmp_path / "b", body.format(name="b")),
    )


@_POSIX_ONLY
def test_the_project_flag_beats_an_exported_project(
    ocx: OcxRunner, two_projects: tuple[Path, Path], tmp_path: Path
) -> None:
    a, b = two_projects
    done = _run(
        ocx,
        "--project",
        str(b),
        "exec",
        "--",
        *_probe("WHICH_PROJECT"),
        env={"OCX_PROJECT": str(a)},
        cwd=tmp_path,
    )
    assert (done.returncode, done.stdout) == (0, "b"), done.stderr


@_POSIX_ONLY
def test_an_exported_project_is_found_from_any_directory_and_accepts_a_group(
    ocx: OcxRunner, two_projects: tuple[Path, Path], tmp_path: Path
) -> None:
    a, _ = two_projects
    done = _run(
        ocx,
        "exec",
        "-g",
        "g1",
        "--",
        *_probe("WHICH_GROUP"),
        env={"OCX_PROJECT": str(a)},
        cwd=tmp_path,
    )
    assert (done.returncode, done.stdout) == (0, "a-g1"), done.stderr


@_POSIX_ONLY
def test_the_global_flag_ignores_an_exported_project(
    ocx: OcxRunner, two_projects: tuple[Path, Path], tmp_path: Path
) -> None:
    a, _ = two_projects
    done = _run(
        ocx,
        "exec",
        "--global",
        "--",
        *_probe("WHICH_PROJECT"),
        env={"OCX_PROJECT": str(a)},
        cwd=tmp_path,
    )
    assert "a" != done.stdout, (
        f"--global must never compose the exported project: rc={done.returncode} {done.stderr}"
    )


@_POSIX_ONLY
def test_an_exported_project_beats_the_working_directory_walk(
    ocx: OcxRunner, two_projects: tuple[Path, Path]
) -> None:
    a, b = two_projects
    done = _run(
        ocx, "exec", "--", *_probe("WHICH_PROJECT"), env={"OCX_PROJECT": str(a)}, cwd=b
    )
    assert (done.returncode, done.stdout) == (0, "a"), done.stderr


@_POSIX_ONLY
def test_no_project_env_prunes_an_exported_project_but_not_the_flag(
    ocx: OcxRunner, two_projects: tuple[Path, Path], tmp_path: Path
) -> None:
    a, b = two_projects
    env = {"OCX_PROJECT": str(a), "OCX_NO_PROJECT": "1"}
    pruned = _run(ocx, "exec", "--", *_probe("WHICH_PROJECT"), env=env, cwd=tmp_path)
    assert pruned.stdout != "a", (
        f"OCX_NO_PROJECT must prune OCX_PROJECT: {pruned.stdout!r} {pruned.stderr}"
    )
    flagged = _run(
        ocx,
        "--project",
        str(b),
        "exec",
        "--",
        *_probe("WHICH_PROJECT"),
        env=env,
        cwd=tmp_path,
    )
    assert (flagged.returncode, flagged.stdout) == (0, "b"), flagged.stderr
