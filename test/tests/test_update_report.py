# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the ``ocx update`` diff report (#489).

``ocx update`` answers *what moved*, not *what is pinned*. It is the only
command holding both the predecessor ``ocx.lock`` and the candidate at the
same moment, so it is the only one that can. ``ocx lock`` and ``ocx add``
keep reporting the resulting lock.

Plain output is one row per moved tool, releases only; held tools are counted
by a hint naming ``--verbose``, which instead lists every platform with
digests. The ``--format json`` payload is identical at both verbosities.

``ocx update --check`` prints the same report, then one plain stderr line
naming the count and the command to run, and exits 65 without an error log.

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
    assert "sha256:" not in result.stdout, (
        f"the default view names releases, never digests:\n{result.stdout}"
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
    assert "sha256:" in result.stdout, (
        "--verbose carries the CLI-wide short-digest abbreviation, "
        f"algorithm prefix included:\n{result.stdout}"
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
    assert "1 tool would move; run `ocx update` to apply" in result.stderr, result.stderr
    assert "ERROR" not in result.stderr, (
        f"drift is the answer --check asks for, not an error:\n{result.stderr}"
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


# ---------------------------------------------------------------------------
# Concrete versions behind an advisory tag (#479): ``changes[]`` gains
# ``from_version`` / ``to_version`` and ``unchanged[]`` gains ``version``, found by
# probing the repository's patch tags. A miss reports ``null``, never a new exit code.
# ---------------------------------------------------------------------------

AMD64 = "linux/amd64"
ARM64 = "linux/arm64"


def _advisory_project(ocx: OcxRunner, tmp_path: Path, repo: str, tag: str) -> Path:
    """Bind ``repo:tag`` and lock it without materialising a host leaf, so a
    fixture publishing only foreign platforms still locks."""
    project = _write_project(
        tmp_path,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag}"
""",
    )
    locked = ocx.plain(
        "lock", "--no-pull", check=False, env_overrides=_project_env(project)
    )
    assert locked.returncode == EXIT_SUCCESS, locked.stderr
    return project


def _check_report(
    ocx: OcxRunner, project: Path, *global_flags: str
) -> tuple[subprocess.CompletedProcess[str], dict]:
    """``ocx --format json [flags] update --check`` and its parsed report."""
    result = ocx.run(
        *global_flags,
        "update",
        "--check",
        format="json",
        check=False,
        env_overrides=_project_env(project),
    )
    return result, json.loads(result.stdout)


def _by_platform(rows: list[dict]) -> dict[str, dict]:
    by_platform = {row["platform"]: row for row in rows}
    assert len(by_platform) == len(rows), f"one row per platform expected: {rows}"
    return by_platform


def test_update_check_names_the_patch_versions_an_advisory_tag_moved_between(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``3`` moved from 3.28.3 to 3.28.4: the changed row says so, and the
    exit code is still the 65 a moved pin always carried."""
    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "3.28.3", tmp_path / "v3", cascade=True)
    project = _advisory_project(ocx, tmp_path, repo, "3")
    make_package(ocx, repo, "3.28.4", tmp_path / "v4", cascade=True)

    result, payload = _check_report(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    assert len(payload["changes"]) == 1, payload
    change = payload["changes"][0]
    assert change["tag"] == "3", change
    assert change["from_version"] == "3.28.3", change
    assert change["to_version"] == "3.28.4", change


def test_update_check_versions_are_null_offline_and_the_exit_code_holds(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Offline there is no registry to probe, so the version fields are
    ``null`` while the same move still exits 65.

    The online run on the same fixture is the control: without it a lookup
    that never resolves anything would pass the ``null`` assertions.
    """
    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "3.28.3", tmp_path / "v3", cascade=True)
    project = _advisory_project(ocx, tmp_path, repo, "3")
    # The default index=True also refreshes the local index, which is all an
    # offline update can read the moved tag from.
    make_package(ocx, repo, "3.28.4", tmp_path / "v4", cascade=True)

    online, online_payload = _check_report(ocx, project)
    offline, offline_payload = _check_report(ocx, project, "--offline")

    assert online.returncode == EXIT_DATA, online.stderr
    assert online_payload["changes"][0]["to_version"] == "3.28.4", online_payload
    assert offline.returncode == EXIT_DATA, (
        f"--offline must not change the exit code; stderr:\n{offline.stderr}"
    )
    assert len(offline_payload["changes"]) == 1, offline_payload
    change = offline_payload["changes"][0]
    assert "from_version" in change and change["from_version"] is None, change
    assert "to_version" in change and change["to_version"] is None, change


def test_update_check_digest_pinned_binding_reports_no_version(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A digest pin names no advisory tag, so its ``version`` is ``null`` even
    though its digest equals the leaf of a real release — the sibling binding
    that does move still gets its versions and sets the exit code."""
    from src.registry import fetch_manifest_digest

    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "3.28.3", tmp_path / "v3", cascade=True)
    pinned_digest = fetch_manifest_digest(ocx.registry, repo, "3.28.3")
    project = _write_project(
        tmp_path,
        f"""\
[tools]
mover = "{ocx.registry}/{repo}:3"
pinned = "{ocx.registry}/{repo}@{pinned_digest}"
""",
    )
    locked = ocx.plain(
        "lock", "--no-pull", check=False, env_overrides=_project_env(project)
    )
    assert locked.returncode == EXIT_SUCCESS, locked.stderr
    make_package(ocx, repo, "3.28.4", tmp_path / "v4", cascade=True)

    result, payload = _check_report(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    moved = {row["name"]: row for row in payload["changes"]}
    held = {row["name"]: row for row in payload["unchanged"]}
    assert set(moved) == {"mover"}, payload
    assert moved["mover"]["from_version"] == "3.28.3", moved
    assert moved["mover"]["to_version"] == "3.28.4", moved
    assert set(held) == {"pinned"}, payload
    assert held["pinned"]["tag"] is None, held
    assert "version" in held["pinned"] and held["pinned"]["version"] is None, held


def test_update_check_variant_track_reports_variant_versions(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A ``debug-3`` binding is matched against ``debug-3.x`` releases only.

    A newer default-track release is published too: the reported versions must
    stay on the variant's track. A variant version renders with its variant
    prefix, the way ``Version`` displays it.
    """
    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "debug-3.28.3", tmp_path / "d3", cascade=True)
    project = _advisory_project(ocx, tmp_path, repo, "debug-3")
    make_package(ocx, repo, "3.28.9", tmp_path / "default", cascade=True)
    make_package(ocx, repo, "debug-3.28.4", tmp_path / "d4", cascade=True)

    result, payload = _check_report(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    assert len(payload["changes"]) == 1, payload
    change = payload["changes"][0]
    assert change["tag"] == "debug-3", change
    assert change["from_version"] == "debug-3.28.3", change
    assert change["to_version"] == "debug-3.28.4", change


def test_update_check_each_platform_row_carries_its_own_version(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Cascade is per platform, so one advisory tag can point at different
    releases on different platforms: ``3`` ends on 3.28.5 for amd64 and on
    3.28.4 for arm64. Each row's version is looked up from its own leaf, never
    from the tag's index digest."""
    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "3.28.3", tmp_path / "amd64_3", platform=AMD64)
    make_package(ocx, repo, "3.28.3", tmp_path / "arm64_3", platform=ARM64)
    project = _advisory_project(ocx, tmp_path, repo, "3")
    make_package(ocx, repo, "3.28.5", tmp_path / "amd64_5", platform=AMD64)
    make_package(ocx, repo, "3.28.4", tmp_path / "arm64_4", platform=ARM64)

    result, payload = _check_report(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    rows = _by_platform(payload["changes"])
    assert set(rows) == {AMD64, ARM64}, payload
    assert (rows[AMD64]["from_version"], rows[AMD64]["to_version"]) == ("3.28.3", "3.28.5"), rows
    assert (rows[ARM64]["from_version"], rows[ARM64]["to_version"]) == ("3.28.3", "3.28.4"), rows


def test_update_check_unchanged_platform_row_carries_its_current_version(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """3.28.4 ships for amd64 only, so cascade leaves arm64 on 3.28.3: amd64
    moves 3.28.3 → 3.28.4 while the arm64 pin that held still reports the
    version it stays on."""
    repo = f"t_{uuid4().hex[:8]}_rep_cmake"
    make_package(ocx, repo, "3.28.3", tmp_path / "amd64_3", platform=AMD64)
    make_package(ocx, repo, "3.28.3", tmp_path / "arm64_3", platform=ARM64)
    project = _advisory_project(ocx, tmp_path, repo, "3")
    make_package(ocx, repo, "3.28.4", tmp_path / "amd64_4", platform=AMD64)

    result, payload = _check_report(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    moved = _by_platform(payload["changes"])
    held = _by_platform(payload["unchanged"])
    assert set(moved) == {AMD64}, payload
    assert set(held) == {ARM64}, payload
    assert (moved[AMD64]["from_version"], moved[AMD64]["to_version"]) == ("3.28.3", "3.28.4"), moved
    assert held[ARM64]["version"] == "3.28.3", held
