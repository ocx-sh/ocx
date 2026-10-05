# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `[update]` configuration (``.claude/artifacts/plan_update_family.md``).

Every probe-dependent case runs on a real terminal (``run_on_a_terminal``): the background gate
skips when stderr is not a terminal, so over pipes a "no state file" assertion is green in every
state. Each such case shows its red control (the probe fires) and its green outcome on the same
fixture, reached through ``ocx clean --dry-run`` (local-only; ``version`` is in the skip list).
The self check goes to a loopback repository through the ``__OCX_SELF_IMAGE`` seam (``--features
ocx/__testing``), so no case reaches ocx.sh or publishes a package: the probe touches its state
file whether or not the repository exists. The managed tick's own switch is pinned by
``test_managed_config.py::test_no_config_refresh_kill_switch_stops_the_background_apply_tick``."""

from __future__ import annotations

import json
import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.registry import push_raw_config_package
from src.runner import OcxRunner
from src.terminal import requires_pty, run_on_a_terminal

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="State-file path assertions and the child-env probes use POSIX paths and `sh`.",
)

EXIT_DATA = 65
EXIT_CONFIG = 78

_HOUR = 3600
_DAY = 24 * _HOUR


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _config_path(ocx: OcxRunner) -> Path:
    return ocx.ocx_home / "config.toml"


def _write_config(ocx: OcxRunner, text: str) -> None:
    _config_path(ocx).write_text(text)


def _self_env(ocx: OcxRunner, unique_repo: str) -> dict[str, str]:
    """Redirects the self check to a loopback repository nothing needs to publish."""
    return {"__OCX_SELF_IMAGE": f"{ocx.registry}/{unique_repo}_self"}


def _on_terminal(
    ocx: OcxRunner,
    tmp_path: Path,
    extra_env: dict[str, str] | None = None,
    command: tuple[str, ...] = ("clean", "--dry-run"),
) -> tuple[int, str]:
    """Runs a check-reaching command with stderr on a pty (stdout to /dev/null).

    The env is explicit: pexpect replaces the environment, so a `CI` set on the
    host running the suite cannot reach the child and trip the CI gate.
    """
    script = tmp_path / f"run-{uuid4().hex[:8]}.sh"
    args = " ".join(shlex.quote(arg) for arg in command)
    script.write_text(f"{shlex.quote(str(ocx.binary))} {args} >/dev/null\n")
    return run_on_a_terminal(script, cwd=tmp_path, env={**ocx.env, **(extra_env or {})})


def _update_check_dir(ocx: OcxRunner) -> Path:
    return ocx.ocx_home / "state" / "update-check"


def _self_state_files(ocx: OcxRunner) -> list[Path]:
    """The self check's state files; the toolchain markers live in a subdirectory."""
    directory = _update_check_dir(ocx)
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.iterdir() if path.is_file())


def _seed(ocx: OcxRunner, tmp_path: Path, env: dict[str, str]) -> Path:
    """First probe on a fresh home: the state file does not exist before, and does after."""
    assert not _self_state_files(ocx), "fixture must start without a state file"
    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    files = _self_state_files(ocx)
    assert len(files) == 1, f"the probe must leave exactly one self state file; got {files}\n{terminal}"
    return files[0]


def _fires_after(
    ocx: OcxRunner, tmp_path: Path, env: dict[str, str], state_file: Path, age_seconds: float
) -> bool:
    """Backdates the state file, runs once, and reports whether the probe touched it again.

    A throttled run leaves the mtime exactly as set; a probe moves it to now.
    """
    stamp = time.time() - age_seconds
    os.utime(state_file, (stamp, stamp))
    backdated = state_file.stat().st_mtime_ns
    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, f"a background check must never fail the command:\n{terminal}"
    return state_file.stat().st_mtime_ns != backdated


def _update_warnings(terminal: str, key: str) -> list[str]:
    """Terminal lines that are an `[update]` warning about `key`."""
    pattern = re.compile(rf"\[update\][ .]+{re.escape(key)}\b")
    return [line for line in terminal.splitlines() if pattern.search(line)]


def _run_in(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        stdin=subprocess.DEVNULL,
        check=False,
    )


def _snapshot_digest(ocx: OcxRunner) -> str:
    snapshot = ocx.ocx_home / "state" / "managed-config" / "snapshot.json"
    return json.loads(snapshot.read_text())["digest"]


# ---------------------------------------------------------------------------
# The interval, from config and from the environment
# ---------------------------------------------------------------------------


@requires_pty
def test_config_interval_sets_the_self_check_window(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`[update] interval = "6h"`: a 5 h old probe is throttled, a 7 h old one fires.

    Control on the same fixture: with the setting removed the 1 d default keeps
    the same 7 h old state throttled, so the config value is what moved the window.
    """
    env = _self_env(ocx, unique_repo)
    _write_config(ocx, '[update]\ninterval = "6h"\n')
    state = _seed(ocx, tmp_path, env)

    assert not _fires_after(ocx, tmp_path, env, state, 5 * _HOUR), "inside the 6 h window: throttled"
    assert _fires_after(ocx, tmp_path, env, state, 7 * _HOUR), "past the 6 h window: probes"

    _config_path(ocx).unlink()
    assert not _fires_after(ocx, tmp_path, env, state, 7 * _HOUR), (
        "control: without `[update] interval` the 1 d default must still throttle a 7 h old probe"
    )


@requires_pty
@pytest.mark.parametrize(
    ("value", "inside", "beyond"),
    [
        pytest.param("1d", 23 * _HOUR, 25 * _HOUR, id="suffix-day"),
        pytest.param("3600", 50 * 60, 70 * 60, id="bare-seconds"),
        pytest.param("bogus", 23 * _HOUR, 25 * _HOUR, id="invalid-falls-back-to-1d"),
    ],
)
def test_env_interval_sets_the_self_check_window(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, value: str, inside: int, beyond: int
) -> None:
    """`OCX_UPDATE_CHECK_INTERVAL` sets the window; an invalid value is ignored and
    the 1 d default applies. The command exits 0 in every case."""
    env = {**_self_env(ocx, unique_repo), "OCX_UPDATE_CHECK_INTERVAL": value}
    state = _seed(ocx, tmp_path, env)

    assert not _fires_after(ocx, tmp_path, env, state, inside), f"{value!r}: inside the window, throttled"
    assert _fires_after(ocx, tmp_path, env, state, beyond), f"{value!r}: past the window, probes"


@requires_pty
def test_zero_env_interval_probes_on_every_command(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`OCX_UPDATE_CHECK_INTERVAL=0` probes even when the state file is a minute old.

    Control: `3600` leaves the same minute-old state alone.
    """
    base = _self_env(ocx, unique_repo)
    state = _seed(ocx, tmp_path, {**base, "OCX_UPDATE_CHECK_INTERVAL": "0"})

    assert _fires_after(ocx, tmp_path, {**base, "OCX_UPDATE_CHECK_INTERVAL": "0"}, state, 60)
    assert not _fires_after(ocx, tmp_path, {**base, "OCX_UPDATE_CHECK_INTERVAL": "3600"}, state, 60), (
        "control: a 3600 s window must throttle a state file that is a minute old"
    )


# ---------------------------------------------------------------------------
# `self = "manual"`, and env beating config
# ---------------------------------------------------------------------------


@requires_pty
def test_self_manual_in_config_never_probes_and_touches_no_state(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`[update] self = "manual"`: no probe, no state file. Control on the same
    fixture: removing the setting makes the same command probe."""
    env = _self_env(ocx, unique_repo)
    _write_config(ocx, '[update]\nself = "manual"\n')

    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    assert _self_state_files(ocx) == [], "manual must not touch the throttle state"

    _config_path(ocx).unlink()
    _seed(ocx, tmp_path, env)  # asserts the probe now runs and leaves its file


@requires_pty
def test_env_self_policy_beats_config(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Config `self = "manual"` plus `OCX_SELF_UPDATE=notify`: the probe runs.

    Red control on the same fixture: without the env value the config's manual holds.
    """
    env = _self_env(ocx, unique_repo)
    _write_config(ocx, '[update]\nself = "manual"\n')

    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    assert _self_state_files(ocx) == [], "control: config alone keeps the probe off"

    _seed(ocx, tmp_path, {**env, "OCX_SELF_UPDATE": "notify"})


# ---------------------------------------------------------------------------
# `[update]` in ocx.toml
# ---------------------------------------------------------------------------


def test_update_section_in_ocx_toml_is_refused_by_name(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A checked-in `[update]` section is a config error (78) that names `config.toml`.

    Control on the same project: without the section `ocx lock` succeeds.
    """
    repo = f"{unique_repo}_tool"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "proj"
    project.mkdir()
    manifest = project / "ocx.toml"
    body = f'[tools]\ntool = "{ocx.registry}/{repo}:1.0.0"\n'

    manifest.write_text(body)
    control = _run_in(ocx, project, "lock")
    assert control.returncode == 0, f"control: the plain project must lock\n{control.stderr}"

    manifest.write_text(body + '\n[update]\nself = "apply"\n')
    refused = _run_in(ocx, project, "lock")
    assert refused.returncode == EXIT_CONFIG, (
        f"`[update]` in ocx.toml must exit {EXIT_CONFIG}; got {refused.returncode}\n{refused.stderr}"
    )
    assert "[update]" in refused.stderr and "config.toml" in refused.stderr, (
        f"the message must say `[update]` belongs in config.toml; got: {refused.stderr!r}"
    )
    # `deny_unknown_fields` also exits 78 and names `[update]`; only the named arm drops this phrase.
    assert "unknown field" not in refused.stderr, (
        f"the refusal must come from the named `[update]` arm, not deny_unknown_fields; got: {refused.stderr!r}"
    )


# ---------------------------------------------------------------------------
# A config value that cannot be honoured warns and never fails
# ---------------------------------------------------------------------------


@requires_pty
def test_toolchain_apply_warns_once_in_config_and_stays_quiet_from_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`[update] toolchain = "apply"` in config.toml: exit 0, one warning naming the key.

    `OCX_TOOLCHAIN_UPDATE=apply` is the quiet half (debug log only), run on the
    same fixture first so the warning's absence there is not a run that never
    reached the resolver.
    """
    env = _self_env(ocx, unique_repo)

    status, terminal = _on_terminal(ocx, tmp_path, {**env, "OCX_TOOLCHAIN_UPDATE": "apply"})
    assert status == 0, terminal
    assert _self_state_files(ocx), "the run must have reached the check, or its silence proves nothing"
    assert _update_warnings(terminal, "toolchain") == [], f"an env value logs at debug only:\n{terminal}"

    _write_config(ocx, '[update]\ntoolchain = "apply"\n')
    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    assert len(_update_warnings(terminal, "toolchain")) == 1, f"exactly one warning expected:\n{terminal}"


@requires_pty
def test_invalid_update_values_warn_and_fall_back_to_the_defaults(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`interval = "bogus"` and `self = "someday"`: exit 0, one warning per key, the
    defaults apply — the probe still runs (self defaults to notify) on the 1 d window."""
    env = _self_env(ocx, unique_repo)
    _write_config(ocx, '[update]\ninterval = "bogus"\nself = "someday"\n')

    assert not _self_state_files(ocx)
    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    assert len(_update_warnings(terminal, "interval")) == 1, f"one warning naming `interval`:\n{terminal}"
    assert len(_update_warnings(terminal, "self")) == 1, f"one warning naming `self`:\n{terminal}"

    (state,) = _self_state_files(ocx)  # the default posture (notify) probed
    assert not _fires_after(ocx, tmp_path, env, state, 23 * _HOUR), "default 1 d window: throttled"
    assert _fires_after(ocx, tmp_path, env, state, 25 * _HOUR), "default 1 d window: probes"


# ---------------------------------------------------------------------------
# The managed tier
# ---------------------------------------------------------------------------


@requires_pty
def test_managed_payload_cannot_carry_update(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """A managed payload's `[update] self = "manual"` is ignored: the probe still runs.

    Red control on the same fixture: the identical value in the local config.toml
    switches the probe off, so the run really observes the posture.
    """
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[update]\nself = "manual"\n')
    _write_config(ocx, '[managed]\ninterval = "0"\n')
    ocx.run("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    env = {**_self_env(ocx, unique_repo), "OCX_MANAGED_CONFIG": ref, "OCX_NO_CONFIG_REFRESH": "1"}

    _seed(ocx, tmp_path, env)  # payload manual ignored: the probe ran

    for stale in _self_state_files(ocx):
        stale.unlink()
    _write_config(ocx, '[managed]\ninterval = "0"\n\n[update]\nself = "manual"\n')
    status, terminal = _on_terminal(ocx, tmp_path, env)
    assert status == 0, terminal
    assert _self_state_files(ocx) == [], "control: the same value in a local tier must stop the probe"


@requires_pty
def test_unknown_managed_refresh_posture_warns_and_falls_back(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """`[managed] refresh = "someday"` warns once and the default (notify) applies:
    newer payload content is not persisted. Control on the same fixture: `apply`
    persists it, so the tick demonstrably runs here."""
    ref = f"{registry}/{unique_repo}:v1"
    _write_config(ocx, '[managed]\nrefresh = "apply"\ninterval = "0"\n')
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "refresh-before.example"\n')
    ocx.run("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    before = _snapshot_digest(ocx)
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "refresh-after.example"\n')

    env = {"OCX_MANAGED_CONFIG": ref, "OCX_NO_UPDATE_CHECK": "1"}
    command = ("index", "catalog")

    _write_config(ocx, '[managed]\nrefresh = "someday"\ninterval = "0"\n')
    status, terminal = _on_terminal(ocx, tmp_path, env, command)
    assert status == 0, terminal
    assert len([line for line in terminal.splitlines() if "someday" in line]) == 1, (
        f"exactly one warning naming the unknown value:\n{terminal}"
    )
    assert _snapshot_digest(ocx) == before, "an unknown posture falls back to notify, which never persists"

    _write_config(ocx, '[managed]\nrefresh = "apply"\ninterval = "0"\n')
    status, terminal = _on_terminal(ocx, tmp_path, env, command)
    assert status == 0, terminal
    assert _snapshot_digest(ocx) != before, "control: `apply` must persist the newer payload"


def test_managed_payload_carrying_an_unknown_refresh_posture_still_applies(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    """A payload whose `[managed]` section holds a posture this ocx does not know
    must not fail `config update`: a value added by a later ocx never breaks an
    older one."""
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(
        registry,
        unique_repo,
        "v1",
        b'[managed]\nrefresh = "someday"\n\n[registry]\ndefault = "forward-compat.example"\n',
    )
    result = ocx.run("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref}, check=False)
    assert result.returncode == 0, f"an unknown refresh posture must not fail the update:\n{result.stderr}"
    assert _snapshot_digest(ocx), "the payload must have been persisted"


# ---------------------------------------------------------------------------
# Which update keys reach a child, and who may declare them
# ---------------------------------------------------------------------------


def test_clean_child_keeps_the_kill_switch_and_drops_the_update_preferences(
    ocx: OcxRunner, published_package
) -> None:
    """`OCX_NO_UPDATE_CHECK` reaches a `--clean` child; the three personal
    preferences set on the parent do not.

    The kill switch showing up in the same child is the control that the probe
    sees a real environment: only the preferences are absent, not everything.
    """
    ocx.plain("package", "install", published_package.short)
    probe = (
        'printf "kill=%s self=%s toolchain=%s interval=%s" '
        '"${OCX_NO_UPDATE_CHECK-unset}" "${OCX_SELF_UPDATE-unset}" '
        '"${OCX_TOOLCHAIN_UPDATE-unset}" "${OCX_UPDATE_CHECK_INTERVAL-unset}"'
    )
    parent = {
        "OCX_NO_UPDATE_CHECK": "1",
        "OCX_SELF_UPDATE": "apply",
        "OCX_TOOLCHAIN_UPDATE": "notify",
        "OCX_UPDATE_CHECK_INTERVAL": "6h",
    }
    result = ocx.plain(
        "package", "exec", "--clean", published_package.short, "--", "/bin/sh", "-c", probe,
        env_overrides=parent,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "kill=1 self=unset toolchain=unset interval=unset", (
        f"only the kill switch may reach a --clean child; got {result.stdout!r}"
    )

    unset = ocx.plain(
        "package", "exec", "--clean", published_package.short, "--", "/bin/sh", "-c", probe, check=False
    )
    assert unset.stdout == "kill=unset self=unset toolchain=unset interval=unset", (
        f"forwarding must not invent a kill switch the parent did not set; got {unset.stdout!r}"
    )


@pytest.mark.parametrize(
    "key",
    ["OCX_SELF_UPDATE", "OCX_TOOLCHAIN_UPDATE", "OCX_UPDATE_CHECK_INTERVAL", "OCX_NO_UPDATE_CHECK"],
)
def test_package_env_cannot_declare_an_update_key(ocx: OcxRunner, unique_repo: str, tmp_path: Path, key: str) -> None:
    """A package cannot publish an `OCX_*` update key into the env it composes
    (the reserved-namespace gate; exit 65). Control on the same bundle: a
    non-reserved key publishes.

    The read side of an already-published artifact is not reachable through the
    product (`push` is the only writer and it refuses), so the refusal is the
    observable half of "a package env declaring the key is dropped".
    """
    content = tmp_path / "content"
    (content / "bin").mkdir(parents=True)
    (content / "bin" / "app").write_text("#!/bin/sh\necho app\n")
    bundle = tmp_path / "bundle.tar.xz"
    ocx.plain("package", "create", "-o", str(bundle), str(content))

    def push(env_key: str, tag: str) -> subprocess.CompletedProcess[str]:
        metadata = tmp_path / f"metadata-{tag}.json"
        metadata.write_text(
            json.dumps(
                {
                    "type": "bundle",
                    "version": 1,
                    "env": [{"key": env_key, "type": "constant", "value": "apply"}],
                }
            )
        )
        return ocx.run(
            "package", "push", "-m", str(metadata), "-p", "any", "-i", f"{ocx.registry}/{unique_repo}:{tag}",
            str(bundle),
            check=False,
        )

    control = push("UPDATE_PROBE_KEY", "1.0.0")
    assert control.returncode == 0, f"control: a non-reserved key must publish\n{control.stderr}"

    refused = push(key, "1.0.1")
    assert refused.returncode == EXIT_DATA, (
        f"`{key}` in package env must be refused (exit {EXIT_DATA}); got {refused.returncode}\n{refused.stderr}"
    )
    assert key in refused.stderr, f"the refusal must name the key; got: {refused.stderr!r}"


def test_ocx_toml_env_cannot_declare_an_update_key(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`[env] OCX_SELF_UPDATE` in ocx.toml is refused (78) and names the key.

    Control on the same project: a non-reserved `[env]` key locks fine.
    """
    repo = f"{unique_repo}_tool"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "proj"
    project.mkdir()
    manifest = project / "ocx.toml"
    tools = f'[tools]\ntool = "{ocx.registry}/{repo}:1.0.0"\n'

    manifest.write_text(tools + '\n[env]\nUPDATE_PROBE_KEY = "apply"\n')
    control = _run_in(ocx, project, "lock")
    assert control.returncode == 0, f"control: a non-reserved [env] key must lock\n{control.stderr}"

    manifest.write_text(tools + '\n[env]\nOCX_SELF_UPDATE = "apply"\n')
    refused = _run_in(ocx, project, "lock")
    assert refused.returncode == EXIT_CONFIG, (
        f"a reserved [env] key must exit {EXIT_CONFIG}; got {refused.returncode}\n{refused.stderr}"
    )
    assert "OCX_SELF_UPDATE" in refused.stderr, f"the refusal must name the key; got: {refused.stderr!r}"


# ---------------------------------------------------------------------------
# The kill switches stay separate
# ---------------------------------------------------------------------------


@requires_pty
def test_kill_switch_stops_every_background_probe(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`OCX_NO_UPDATE_CHECK=1`: no self probe and no toolchain probe, no state files at all.

    Red control on the same fixture: the identical run without the switch probes.
    (The toolchain half has no red control until the drift check lands; the self
    half carries the proof that the run reaches the gate.)
    """
    env = _self_env(ocx, unique_repo)

    status, terminal = _on_terminal(ocx, tmp_path, {**env, "OCX_NO_UPDATE_CHECK": "1"})
    assert status == 0, terminal
    assert not _update_check_dir(ocx).exists() or list(_update_check_dir(ocx).rglob("*")) == [], (
        "the kill switch must leave no state file, self or toolchain"
    )

    _seed(ocx, tmp_path, env)


@requires_pty
def test_config_refresh_kill_switch_does_not_stop_the_self_check(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`OCX_NO_CONFIG_REFRESH` is the managed tick's switch only; the self check still probes."""
    _seed(ocx, tmp_path, {**_self_env(ocx, unique_repo), "OCX_NO_CONFIG_REFRESH": "1"})
