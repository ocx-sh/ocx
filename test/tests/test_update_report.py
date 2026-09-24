# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the ``ocx update`` diff report (#489).

``ocx update`` answers *what moved*, not *what is pinned*. It is the only
command holding both the predecessor ``ocx.lock`` and the candidate at the
same moment, so it is the only one that can. ``ocx lock`` and ``ocx add``
keep reporting the resulting lock.

Plain output is one five-column table — ``Binding | Group | Platform | From |
To`` — carrying the pins that moved. The pins that held still are counted by a
trailing hint naming ``--verbose``; ``--verbose`` lists them instead, with
``From`` equal to ``To``. ``--verbose`` is a rendering tier only: the
``--format json`` payload (``changes`` / ``unchanged`` / ``metadata_changed``)
is identical with and without it.

``ocx update --check`` prints the same report on stdout and *then* exits 65
when anything would move, so a refusal names the bindings instead of sending
the user back to re-run without ``--check``.

The project is selected through ``OCX_PROJECT`` rather than a working
directory, so every invocation here names the project it acts on.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("update")

EXIT_SUCCESS = 0
EXIT_DATA = 65  # a pin would change under --check


def _project_env(project: Path) -> dict[str, str]:
    """Select PROJECT explicitly, so no test depends on the process CWD."""
    return {"OCX_PROJECT": str(project / "ocx.toml")}


def _run_lock(ocx: OcxRunner, project: Path) -> subprocess.CompletedProcess[str]:
    return ocx.plain("lock", check=False, env_overrides=_project_env(project))


def _run_update(ocx: OcxRunner, project: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    """``ocx update`` in plain mode (the default a user sees)."""
    return ocx.plain("update", *extra, check=False, env_overrides=_project_env(project))


def _run_update_json(ocx: OcxRunner, project: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    """``ocx --format json update`` — the machine contract."""
    return ocx.run("update", *extra, format="json", check=False, env_overrides=_project_env(project))


def _write_project(tmp_path: Path, body: str) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    path = project / "ocx.toml"
    path.write_text(body)
    return project


def _two_tools_one_moved(ocx: OcxRunner, tmp_path: Path) -> Path:
    """Publish ``mover`` at a moving ``:latest`` and ``steady`` at an exact
    version, lock both, then advance ``mover`` upstream.

    Returns the project directory, whose ``ocx.lock`` is one tag bump behind
    for exactly one of its two bindings.
    """
    short = uuid4().hex[:8]
    mover_repo = f"t_{short}_rep_mover"
    steady_repo = f"t_{short}_rep_steady"

    make_package(ocx, mover_repo, "1.0.0", tmp_path, cascade=True)
    make_package(ocx, steady_repo, "1.0.0", tmp_path, cascade=False)

    project = _write_project(
        tmp_path,
        f"""\
[tools]
mover = "{ocx.registry}/{mover_repo}:latest"
steady = "{ocx.registry}/{steady_repo}:1.0.0"
""",
    )
    locked = _run_lock(ocx, project)
    assert locked.returncode == EXIT_SUCCESS, locked.stderr

    # ``:latest`` now points at the 2.0.0 digest; ``steady`` cannot move.
    make_package(ocx, mover_repo, "2.0.0", tmp_path, cascade=True)
    return project


def test_update_lists_the_moved_binding_and_hides_the_rest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The default table carries only the pins that moved, and a trailing hint
    names how many held still and the flag that lists them.

    Without the hint, "one row" and "one binding in this project" are
    indistinguishable in the output.
    """
    project = _two_tools_one_moved(ocx, tmp_path)

    result = _run_update(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "mover:latest" in result.stdout, (
        f"the moved binding must be listed, tag included:\n{result.stdout}"
    )
    assert "steady" not in result.stdout, (
        f"an unchanged binding must not be listed by default:\n{result.stdout}"
    )
    assert "unchanged" in result.stdout and "--verbose" in result.stdout, (
        f"a hidden unchanged pin must be named by a hint:\n{result.stdout}"
    )
    assert "sha256:" in result.stdout, (
        "the From/To columns carry the CLI-wide short-digest abbreviation, "
        f"algorithm prefix included, not a bare hex tail:\n{result.stdout}"
    )


def test_update_verbose_lists_the_unchanged_pins_too(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--verbose`` appends the pins that held still and drops the hint that
    stood in for them."""
    project = _two_tools_one_moved(ocx, tmp_path)

    result = _run_update(ocx, project, "--verbose")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "mover:latest" in result.stdout, result.stdout
    assert "steady:1.0.0" in result.stdout, (
        f"--verbose must list the unchanged binding:\n{result.stdout}"
    )
    assert "not shown" not in result.stdout, (
        f"nothing is hidden under --verbose, so no hint belongs there:\n{result.stdout}"
    )


def test_update_check_lists_what_would_move_then_exits_65(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--check`` prints the report on stdout BEFORE returning exit 65, and
    still writes nothing."""
    project = _two_tools_one_moved(ocx, tmp_path)
    before = (project / "ocx.lock").read_bytes()

    result = _run_update(ocx, project, "--check")

    assert result.returncode == EXIT_DATA, (
        f"a moved pin must still exit 65; stderr:\n{result.stderr}"
    )
    assert "mover:latest" in result.stdout, (
        f"--check must name what would move, not just refuse:\n{result.stdout}"
    )
    assert (project / "ocx.lock").read_bytes() == before, (
        "ocx update --check must not rewrite ocx.lock"
    )


def test_update_json_payload_is_identical_at_both_verbosities(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--verbose`` is a plain-rendering tier: the JSON document carries
    ``changes``, ``unchanged`` and ``metadata_changed`` either way.

    Both runs use ``--check`` so neither writes, which is what makes the two
    payloads comparable at all.
    """
    project = _two_tools_one_moved(ocx, tmp_path)

    plain_run = _run_update_json(ocx, project, "--check")
    verbose_run = _run_update_json(ocx, project, "--check", "--verbose")

    assert plain_run.returncode == EXIT_DATA, plain_run.stderr
    assert verbose_run.returncode == EXIT_DATA, verbose_run.stderr

    plain_payload = json.loads(plain_run.stdout)
    verbose_payload = json.loads(verbose_run.stdout)

    assert set(plain_payload) == {"changes", "unchanged", "metadata_changed"}, plain_payload
    assert plain_payload == verbose_payload, (
        "--verbose must not change the JSON payload"
    )
    moved = [entry["name"] for entry in plain_payload["changes"]]
    held = [entry["name"] for entry in plain_payload["unchanged"]]
    assert moved == ["mover"], plain_payload
    assert held == ["steady"], plain_payload
    assert plain_payload["metadata_changed"] is False, (
        "ocx.toml was untouched, so the declaration hash cannot have moved"
    )
    change = plain_payload["changes"][0]
    assert change["tag"] == "latest", change
    assert change["from"] != change["to"], change
    assert change["from"] and change["to"], (
        "both sides exist — this binding was re-pinned, not added or dropped"
    )


def test_update_with_nothing_moved_reports_an_empty_change_set(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A lock already current: exit 0, no changes, and the one pin reported as
    unchanged."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_rep_still"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = _write_project(
        tmp_path,
        f"""\
[tools]
still = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    locked = _run_lock(ocx, project)
    assert locked.returncode == EXIT_SUCCESS, locked.stderr

    result = _run_update_json(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = json.loads(result.stdout)
    assert payload["changes"] == [], payload
    assert payload["metadata_changed"] is False, payload
    assert [entry["name"] for entry in payload["unchanged"]] == ["still"], payload


def test_update_check_with_nothing_moved_exits_zero_and_writes_nothing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--check`` refuses only when a pin *would* move.

    Every other ``--check`` row starts from a lock one bump behind and asserts
    the 65, which leaves the other half of the gate untested: a ``--check``
    that exited 65 unconditionally passes all of them, and fails every CI job
    wired to it as a drift gate. This row is that half — a current lock, exit
    0, an empty change set, and the lock still byte-identical.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_rep_check"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = _write_project(
        tmp_path,
        f"""\
[tools]
current = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    locked = _run_lock(ocx, project)
    assert locked.returncode == EXIT_SUCCESS, locked.stderr
    before = (project / "ocx.lock").read_bytes()

    result = _run_update_json(ocx, project, "--check")

    assert result.returncode == EXIT_SUCCESS, (
        f"nothing would move, so --check must not refuse; stderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload["changes"] == [], payload
    assert [entry["name"] for entry in payload["unchanged"]] == ["current"], payload
    assert (project / "ocx.lock").read_bytes() == before, (
        "ocx update --check must not rewrite ocx.lock, moved pins or not"
    )
