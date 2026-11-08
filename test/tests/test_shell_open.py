# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for bare ``ocx shell``: the user's shell inside a pinned project environment.

The opened shell receives a pinned execution state: its selection names the project (or the global
tier) it was opened with, the composed values are session records the prompt hook refreshes, and
``--with`` / ``--env`` are invocation records the hook only keeps or removes. State is decoded with
the Python codec in ``src/exec_state.py``.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src import exec_state
from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("shell")

CARRIER = "__OCX_ENV_STATE"
SESSION_COMPOSED = {"session": {"scope": "project", "source": "composed"}}
SESSION_INVOCATION = {"session": {"scope": "project", "source": "invocation"}}

# The shell is the test's own Python reading its script from stdin, so the cases run on Windows too;
# `shutil.which` honours `PATHEXT` there.
STATE_SCRIPT = (
    "import os, shutil\n"
    "print('HELLO=' + (shutil.which('hello') or ''))\n"
    f"print('STATE=' + os.environ.get('{CARRIER}', ''))\n"
    "print('OUTER=' + os.environ.get('OCX_TEST_OUTER_MARKER', 'unset'))\n"
)


def _locked_project(ocx: OcxRunner, tmp_path: Path, suffix: str) -> Path:
    repo = f"t_{uuid4().hex[:8]}_{suffix}"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=dict(ocx.env), check=False
    )
    assert lock.returncode == 0, lock.stderr
    return project


def _open_shell(
    ocx: OcxRunner, cwd: Path, *args: str, root_args: tuple[str, ...] = (), env: dict[str, str] | None = None
) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
    """Run ``ocx shell`` with a Python shell that reports what it received; returns the process and its values."""
    shell_env = dict(ocx.env)
    shell_env["SHELL"] = sys.executable
    shell_env.update(env or {})
    result = subprocess.run(
        [str(ocx.binary), *root_args, "shell", *args], cwd=cwd, input=STATE_SCRIPT, capture_output=True,
        text=True, env=shell_env, check=False,
    )
    values = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
    return result, values


def _received_state(values: dict[str, str]) -> dict:
    state = exec_state.decode(values.get("STATE"))
    assert isinstance(state, dict), f"the shell must receive a readable execution state: {values.get('STATE')!r}"
    return state


def _canonical(path: Path) -> str:
    return str(path.resolve())


@pytest.mark.smoke
def test_bare_shell_runs_the_shell_in_the_project_environment(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell`` execs ``$SHELL`` with the project tools on ``PATH`` and a pinned state naming the project."""
    project = _locked_project(ocx, tmp_path, "shell_open")

    result, values = _open_shell(ocx, project)

    assert result.returncode == 0, result.stderr
    assert values.get("HELLO") and Path(values["HELLO"]).is_absolute(), f"hello is not on PATH: {result.stdout!r}"
    state = _received_state(values)
    pinned = state["selection"]["pinned"]
    assert Path(pinned["tier"]["project"]["dir"]).samefile(project), pinned
    assert pinned["groups"] == ["default"]
    assert pinned["with_names"] == [] and pinned["env_keys"] == []
    assert re.fullmatch(r"[0-9a-f]{64}", pinned["invocation_digest"]), pinned


def test_bare_shell_records_the_composition_as_session_values_of_the_project(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Every value the project composed is a session record of the project scope the hook refreshes."""
    project = _locked_project(ocx, tmp_path, "shell_session")

    result, values = _open_shell(ocx, project)

    assert result.returncode == 0, result.stderr
    state = _received_state(values)
    path_records = [record for record in state["applied"] if record["key"].upper() == "PATH"]
    assert path_records, f"the project tools must be recorded on PATH: {state['applied']!r}"
    assert all(record["owner"] == SESSION_COMPOSED for record in path_records), path_records


def test_bare_shell_does_not_write_the_retired_exec_scope(ocx: OcxRunner, tmp_path: Path) -> None:
    """The execution state carries the scope now; the retired marker variable is never set."""
    project = _locked_project(ocx, tmp_path, "shell_scope")
    script = "import os\nprint('SCOPE=' + repr(os.environ.get('__OCX_EXEC_SCOPE')))\n"
    env = dict(ocx.env)
    env["SHELL"] = sys.executable

    result = subprocess.run(
        [str(ocx.binary), "shell"], cwd=project, input=script, capture_output=True, text=True, env=env, check=False
    )

    assert result.returncode == 0, result.stderr
    assert "SCOPE=None" in result.stdout, result.stdout


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_with_a_group_puts_the_group_tool_on_path(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell -g <group>`` composes the named group, whose tool the default set lacks."""
    repo = f"t_{uuid4().hex[:8]}_shell_group"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[group.extra.tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=dict(ocx.env), check=False
    )
    assert lock.returncode == 0, lock.stderr

    stub = tmp_path / "stub-shell"
    stub.write_text('#!/bin/sh\necho "HELLO=$(command -v hello)"\n')
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    plain = subprocess.run([str(ocx.binary), "shell"], cwd=project, capture_output=True, text=True, env=env, check=False)
    assert plain.returncode == 0, plain.stderr
    assert plain.stdout.strip() == "HELLO=", f"the default set must not carry the group tool: {plain.stdout!r}"
    grouped = subprocess.run(
        [str(ocx.binary), "shell", "-g", "extra"], cwd=project, capture_output=True, text=True, env=env, check=False
    )
    assert grouped.returncode == 0, grouped.stderr
    assert "HELLO=/" in grouped.stdout, f"the group tool is not on PATH: {grouped.stdout!r}"


def test_bare_shell_pins_the_groups_it_was_opened_with(ocx: OcxRunner, tmp_path: Path) -> None:
    """A later ``cd`` or ``ocx.toml`` edit cannot change which groups the hook refreshes: the selection names them."""
    repo = f"t_{uuid4().hex[:8]}_shell_pin"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[group.extra.tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=dict(ocx.env), check=False
    )
    assert lock.returncode == 0, lock.stderr

    result, values = _open_shell(ocx, project, "-g", "extra", "--pinned")

    assert result.returncode == 0, result.stderr
    pinned = _received_state(values)["selection"]["pinned"]
    assert pinned["groups"] == ["extra"]
    assert pinned["pinned_paths"] is True


def test_bare_shell_records_with_and_env_as_invocation_values(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--with`` and ``--env`` are session invocation records; the selection keeps names and keys, never values."""
    project = _locked_project(ocx, tmp_path, "shell_inv")
    extra = f"t_{uuid4().hex[:8]}_shell_with"
    make_package(ocx, extra, "1.0.0", tmp_path, cascade=False, bins=["extra-tool"])
    identifier = f"{ocx.registry}/{extra}:1.0.0"

    result, values = _open_shell(ocx, project, "--with", identifier, "--env", "SHELL_OPEN_SENTINEL=fixture-value")

    assert result.returncode == 0, result.stderr
    state = _received_state(values)
    pinned = state["selection"]["pinned"]
    assert pinned["with_names"] == [identifier]
    assert pinned["env_keys"] == ["SHELL_OPEN_SENTINEL"]
    assert "fixture-value" not in json.dumps(pinned), "the selection must never hold an --env value"
    invocation = [record for record in state["applied"] if record["owner"] == SESSION_INVOCATION]
    constants = {record["key"]: record["value"] for record in invocation if record["kind"] == "constant"}
    assert constants.get("SHELL_OPEN_SENTINEL") == {"literal": "fixture-value"}, invocation
    assert any(record["key"].upper() == "PATH" for record in invocation), f"--with must record its PATH entries: {invocation!r}"
    composed = [record for record in state["applied"] if record["owner"] == SESSION_COMPOSED]
    assert composed, "the project composition stays a composed session record beside the invocation"


def test_bare_shell_invocation_digest_follows_the_invocation(ocx: OcxRunner, tmp_path: Path) -> None:
    """The digest is stable for one invocation and differs when an ``--env`` value differs."""
    project = _locked_project(ocx, tmp_path, "shell_digest")

    digests = []
    for value in ("one", "one", "two"):
        result, values = _open_shell(ocx, project, "--env", f"SHELL_OPEN_DIGEST={value}")
        assert result.returncode == 0, result.stderr
        digests.append(_received_state(values)["selection"]["pinned"]["invocation_digest"])

    assert digests[0] == digests[1]
    assert digests[0] != digests[2]


def test_global_shell_pins_the_global_tier(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global shell`` composes the global toolchain as a global session scope with a global pin."""
    repo = f"t_{uuid4().hex[:8]}_shell_global"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    (ocx.ocx_home / "ocx.toml").write_text(f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = ocx.run("--global", "lock", check=False)
    assert lock.returncode == 0, lock.stderr
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()

    result, values = _open_shell(ocx, elsewhere, root_args=("--global",))

    assert result.returncode == 0, result.stderr
    state = _received_state(values)
    assert state["selection"]["pinned"]["tier"] == "global", state["selection"]
    path_records = [record for record in state["applied"] if record["key"].upper() == "PATH"]
    assert path_records and all(
        record["owner"] == {"session": {"scope": "global", "source": "composed"}} for record in path_records
    ), path_records


def test_bare_shell_clean_starts_from_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell --clean`` drops the caller's variables; the default layers on top of them."""
    project = _locked_project(ocx, tmp_path, "shell_clean")
    marker = {"OCX_TEST_OUTER_MARKER": "kept"}

    layered, layered_values = _open_shell(ocx, project, env=marker)
    clean, clean_values = _open_shell(ocx, project, "--clean", env=marker)

    assert layered.returncode == 0, layered.stderr
    assert layered_values["OUTER"] == "kept"
    assert clean.returncode == 0, clean.stderr
    assert clean_values["OUTER"] == "unset"
    assert Path(clean_values["HELLO"]).is_absolute(), "the composed tools stay reachable under --clean"


def test_bare_shell_refuses_the_retired_stack_flag(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--stack`` no longer exists: layering is the default, so the flag is a usage error."""
    project = _locked_project(ocx, tmp_path, "shell_stack")

    result, _ = _open_shell(ocx, project, "--stack")

    assert result.returncode == 64, (result.returncode, result.stderr)


def test_bare_shell_isolate_and_clean_conflict(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--isolate`` and ``--clean`` together are a usage error."""
    project = _locked_project(ocx, tmp_path, "shell_conflict")

    result, _ = _open_shell(ocx, project, "--isolate", "--clean")

    assert result.returncode == 64, (result.returncode, result.stderr)


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_exits_with_the_shells_exit_code(ocx: OcxRunner, tmp_path: Path) -> None:
    """``exit 7`` inside the opened shell is ``ocx shell``'s own exit status."""
    project = _locked_project(ocx, tmp_path, "shell_exit")
    stub = tmp_path / "stub-shell"
    stub.write_text("#!/bin/sh\nexit 7\n")
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    result = subprocess.run(
        [str(ocx.binary), "shell"], cwd=project, capture_output=True, text=True, env=env, check=False
    )
    assert result.returncode == 7, (result.returncode, result.stderr)


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_inside_an_exec_of_the_same_project_layers_without_a_warning(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A shell opened under ``ocx exec`` of the same project is an ordinary layer: no nesting warning."""
    project = _locked_project(ocx, tmp_path, "shell_nested")
    stub = tmp_path / "stub-shell"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    result = subprocess.run(
        [str(ocx.binary), "exec", "--", str(ocx.binary), "shell"],
        cwd=project, capture_output=True, text=True, env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "already inside" not in result.stderr, result.stderr
    assert "--stack" not in result.stderr, result.stderr
