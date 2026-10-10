# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx upgrade`` (#590).

It moves the declared tag in ``ocx.toml`` to the newest release and re-locks the retagged bindings,
never automatically (``ocx update`` only moves the lock). A tag keeps its precision and variant
(``3.28`` -> ``3.29``, ``debug-3.28`` -> ``debug-3.29``) and stays within its major: a newer major
lands in ``beyond_major`` and only ``--major`` crosses it. Digest-pinned, ``latest``, non-version
and prerelease/build tags are skipped as report rows. ``--check`` writes nothing and exits 65 on a
pending upgrade; guards mirror ``update`` (no lock 78, drift 65, unknown name or group 64,
``--offline`` / ``--frozen`` 81). Assertions read the JSON payload and ``ocx.toml``, never text.
"""
from __future__ import annotations

import json
import re
import subprocess
import tomllib
from pathlib import Path
from typing import Any
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner

pytestmark = pytest.mark.command("upgrade")

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # unknown binding name or group
EXIT_DATA = 65  # --check with an upgrade available, or a drifted ocx.toml
EXIT_CONFIG = 78  # no predecessor ocx.lock
EXIT_POLICY_BLOCKED = 81  # --offline / --frozen

# One ``[[tool]]`` per binding, each with a ``[tool.platforms]`` table of leaf digests.
_LEAF_RE = re.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')
_HASH_RE = re.compile(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"')


def _project_env(project: Path) -> dict[str, str]:
    """Select the project explicitly, so no test depends on the process CWD."""
    return {"OCX_PROJECT": str(project / "ocx.toml")}


def _write_project(tmp_path: Path, body: str, name: str = "proj") -> Path:
    project = tmp_path / name
    project.mkdir()
    (project / "ocx.toml").write_text(body)
    return project


def _run_lock(ocx: OcxRunner, project: Path) -> subprocess.CompletedProcess[str]:
    return ocx.plain("lock", check=False, env_overrides=_project_env(project))


def _upgrade(
    ocx: OcxRunner, project: Path, *extra: str, global_flags: tuple[str, ...] = ()
) -> subprocess.CompletedProcess[str]:
    """``ocx --format json upgrade`` — the machine contract."""
    return ocx.run(*global_flags, "upgrade", *extra, format="json", check=False, env_overrides=_project_env(project))


def _publish(ocx: OcxRunner, repo: str, tmp_path: Path, *tags: str, cascade: bool = True) -> None:
    for tag in tags:
        make_package(ocx, repo, tag, tmp_path, cascade=cascade)


def _toml(project: Path) -> dict[str, Any]:
    return tomllib.loads((project / "ocx.toml").read_text())


def _lock_text(project: Path) -> str:
    return (project / "ocx.lock").read_text()


def _leaves(project: Path) -> list[str]:
    return sorted(_LEAF_RE.findall(_lock_text(project)))


def _declaration_hash(project: Path) -> str:
    match = _HASH_RE.search(_lock_text(project))
    assert match is not None, "declaration_hash missing from ocx.lock"
    return match.group(1)


def _locked_project(ocx: OcxRunner, tmp_path: Path, body: str) -> Path:
    project = _write_project(tmp_path, body)
    locked = _run_lock(ocx, project)
    assert locked.returncode == EXIT_SUCCESS, locked.stderr
    return project


def _payload(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    return json.loads(result.stdout)


def _rows(payload: dict[str, Any], key: str) -> dict[str, dict[str, Any]]:
    """Rows of ``payload[key]`` keyed by binding name (names are unique in these projects)."""
    return {row["name"]: row for row in payload[key]}


def _minor_project(ocx: OcxRunner, tmp_path: Path, repo: str, body_extra: str = "") -> Path:
    """Lock ``cmake = <repo>:3.28`` with 3.29 published after the lock."""
    _publish(ocx, repo, tmp_path, "3.28.0")
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
# project toolchain
[tools]
cmake = "{ocx.registry}/{repo}:3.28"  # build system
{body_extra}""",
    )
    _publish(ocx, repo, tmp_path, "3.29.0")
    return project


def _major_project(ocx: OcxRunner, tmp_path: Path, repo: str) -> Path:
    """Lock ``cmake = <repo>:3`` with 3.28.0 published, then 4.0.0 published after the lock."""
    _publish(ocx, repo, tmp_path, "3.28.0")
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
[tools]
cmake = "{ocx.registry}/{repo}:3"
""",
    )
    _publish(ocx, repo, tmp_path, "4.0.0")
    return project


# ---------------------------------------------------------------------------
# Report-and-apply: the tag moves, precision kept, comments kept
# ---------------------------------------------------------------------------


@pytest.mark.smoke
def test_upgrade_moves_minor_tag_and_keeps_precision_and_comments(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``cmake:3.28`` with 3.29 published becomes ``cmake:3.29`` (not ``3.29.0``),
    the comments around it survive, and the report lists exactly that row."""
    repo = f"t_{uuid4().hex[:8]}_up_minor"
    project = _minor_project(ocx, tmp_path, repo)
    before_toml = (project / "ocx.toml").read_text()
    before_leaves = _leaves(project)

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert set(payload) == {"schema_version", "upgrades", "skipped", "beyond_major", "lock"}, payload
    assert payload["upgrades"] == [{"name": "cmake", "group": "default", "from_tag": "3.28", "to_tag": "3.29"}], payload
    assert payload["beyond_major"] == [], payload

    after_toml = (project / "ocx.toml").read_text()
    assert _toml(project)["tools"]["cmake"] == f"{ocx.registry}/{repo}:3.29"
    assert after_toml == before_toml.replace(f"{repo}:3.28", f"{repo}:3.29"), (
        "only the tag may change; the comments and layout around it must survive"
    )

    assert _leaves(project) != before_leaves, "the re-lock must advance the pinned leaf"
    changed = _rows(payload["lock"], "changes")
    assert changed["cmake"]["from_version"] == "3.28.0", payload["lock"]
    assert changed["cmake"]["to_version"] == "3.29.0", payload["lock"]


def test_upgrade_relocks_to_what_a_fresh_lock_resolves(ocx: OcxRunner, tmp_path: Path) -> None:
    """After ``ocx upgrade`` the ``ocx.lock`` equals what ``ocx lock`` produces from the
    rewritten ``ocx.toml`` on a clean slate: same leaves, same declaration hash."""
    repo = f"t_{uuid4().hex[:8]}_up_relock"
    project = _minor_project(ocx, tmp_path, repo)

    assert _upgrade(ocx, project).returncode == EXIT_SUCCESS

    fresh = _write_project(tmp_path, (project / "ocx.toml").read_text(), name="fresh")
    assert _run_lock(ocx, fresh).returncode == EXIT_SUCCESS
    assert _leaves(project) == _leaves(fresh)
    assert _declaration_hash(project) == _declaration_hash(fresh), (
        "ocx.lock must be fresh against the rewritten ocx.toml"
    )


def test_upgrade_plain_output_exits_zero(ocx: OcxRunner, tmp_path: Path) -> None:
    """The default (plain) rendering is the same run: it applies the upgrade and exits 0."""
    repo = f"t_{uuid4().hex[:8]}_up_plain"
    project = _minor_project(ocx, tmp_path, repo)

    result = ocx.plain("upgrade", check=False, env_overrides=_project_env(project))

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _toml(project)["tools"]["cmake"] == f"{ocx.registry}/{repo}:3.29"


# ---------------------------------------------------------------------------
# Majors: listed, never crossed without --major
# ---------------------------------------------------------------------------


def test_upgrade_stays_within_major_and_lists_the_newer_major(ocx: OcxRunner, tmp_path: Path) -> None:
    """``cmake:3`` with 4.0 published: nothing moves, ``beyond_major`` names ``4``,
    ``ocx.toml`` is byte-identical and the pinned leaves are unchanged."""
    repo = f"t_{uuid4().hex[:8]}_up_major"
    project = _major_project(ocx, tmp_path, repo)
    before_toml = (project / "ocx.toml").read_bytes()
    before_leaves = _leaves(project)

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [], payload
    assert payload["beyond_major"] == [{"name": "cmake", "group": "default", "tag": "3", "newest_tag": "4"}], payload
    assert [row["reason"] for row in payload["skipped"] if row["name"] == "cmake"] in ([], ["up_to_date"]), payload
    assert (project / "ocx.toml").read_bytes() == before_toml
    assert _leaves(project) == before_leaves


def test_upgrade_major_flag_crosses_the_major(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--major`` moves ``cmake:3`` to ``cmake:4`` (precision kept) and nothing is left beyond."""
    repo = f"t_{uuid4().hex[:8]}_up_cross"
    project = _major_project(ocx, tmp_path, repo)
    before_leaves = _leaves(project)

    result = _upgrade(ocx, project, "--major")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [{"name": "cmake", "group": "default", "from_tag": "3", "to_tag": "4"}], payload
    assert payload["beyond_major"] == [], payload
    assert _toml(project)["tools"]["cmake"] == f"{ocx.registry}/{repo}:4"
    assert _leaves(project) != before_leaves


def test_upgrade_check_ignores_a_major_it_would_not_apply(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--check`` exits 65 only for a within-policy upgrade: a newer major alone is
    information, so the run exits 0 and still writes nothing."""
    repo = f"t_{uuid4().hex[:8]}_up_checkmajor"
    project = _major_project(ocx, tmp_path, repo)
    before_toml = (project / "ocx.toml").read_bytes()
    before_lock = (project / "ocx.lock").read_bytes()

    result = _upgrade(ocx, project, "--check")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _payload(result)["beyond_major"], "the newer major must still be reported"
    assert (project / "ocx.toml").read_bytes() == before_toml
    assert (project / "ocx.lock").read_bytes() == before_lock


# ---------------------------------------------------------------------------
# --check
# ---------------------------------------------------------------------------


def test_upgrade_check_exits_65_and_writes_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """With an upgrade available ``--check`` prints the report, exits 65, and leaves
    ``ocx.toml`` and ``ocx.lock`` byte-identical."""
    repo = f"t_{uuid4().hex[:8]}_up_check"
    project = _minor_project(ocx, tmp_path, repo)
    before_toml = (project / "ocx.toml").read_bytes()
    before_lock = (project / "ocx.lock").read_bytes()

    result = _upgrade(ocx, project, "--check")

    assert result.returncode == EXIT_DATA, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [{"name": "cmake", "group": "default", "from_tag": "3.28", "to_tag": "3.29"}], payload
    assert "1 tag would move; run `ocx upgrade` to apply" in result.stderr, result.stderr
    assert "error:" not in result.stderr, f"drift under --check is an answer, not an error:\n{result.stderr}"
    assert (project / "ocx.toml").read_bytes() == before_toml
    assert (project / "ocx.lock").read_bytes() == before_lock


# ---------------------------------------------------------------------------
# Skipped rows are report rows, never errors
# ---------------------------------------------------------------------------


def test_upgrade_skips_digest_pinned_and_latest_bindings_with_a_reason(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A digest-pinned binding and a ``latest`` / bare binding are skipped with
    ``digest_pinned`` / ``latest`` while a sibling binding still upgrades; exit 0."""
    short = uuid4().hex[:8]
    mover, pinned, rolling = f"t_{short}_up_mv", f"t_{short}_up_pin", f"t_{short}_up_roll"
    _publish(ocx, mover, tmp_path, "3.28.0")
    _publish(ocx, pinned, tmp_path, "3.28.0")
    _publish(ocx, rolling, tmp_path, "3.28.0")
    index_digest = fetch_manifest_digest(ocx.registry, pinned, "3.28.0")
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
[tools]
mover = "{ocx.registry}/{mover}:3.28"
pinned = "{ocx.registry}/{pinned}@{index_digest}"
rolling = "{ocx.registry}/{rolling}:latest"
bare = "{ocx.registry}/{rolling}"
""",
    )
    _publish(ocx, mover, tmp_path, "3.29.0")
    _publish(ocx, pinned, tmp_path, "3.29.0")

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert [row["name"] for row in payload["upgrades"]] == ["mover"], payload
    assert {name: row["reason"] for name, row in _rows(payload, "skipped").items()} == {
        "pinned": "digest_pinned",
        "rolling": "latest",
        "bare": "latest",
    }, payload
    tools = _toml(project)["tools"]
    assert tools["pinned"] == f"{ocx.registry}/{pinned}@{index_digest}", "a digest pin must not be rewritten"
    assert tools["rolling"] == f"{ocx.registry}/{rolling}:latest"
    assert tools["bare"] == f"{ocx.registry}/{rolling}"
    assert tools["mover"] == f"{ocx.registry}/{mover}:3.29"


def test_upgrade_skips_non_version_and_prerelease_tags_with_a_reason(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``nightly`` is ``not_a_version`` and ``1.0.0-rc1`` is ``prerelease_or_build``;
    neither moves, even with a newer release published, and the run exits 0."""
    short = uuid4().hex[:8]
    nightly, prerelease = f"t_{short}_up_night", f"t_{short}_up_rc"
    _publish(ocx, nightly, tmp_path, "nightly", cascade=False)
    _publish(ocx, prerelease, tmp_path, "1.0.0-rc1", cascade=False)
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
[tools]
night = "{ocx.registry}/{nightly}:nightly"
rc = "{ocx.registry}/{prerelease}:1.0.0-rc1"
""",
    )
    _publish(ocx, prerelease, tmp_path, "1.0.0", cascade=False)
    before_toml = (project / "ocx.toml").read_bytes()

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [], payload
    assert {name: row["reason"] for name, row in _rows(payload, "skipped").items()} == {
        "night": "not_a_version",
        "rc": "prerelease_or_build",
    }, payload
    assert (project / "ocx.toml").read_bytes() == before_toml


def test_upgrade_reports_an_up_to_date_binding_as_skipped(ocx: OcxRunner, tmp_path: Path) -> None:
    """A binding with no newer release is a ``skipped`` row (``up_to_date``), the run
    exits 0, ``--check`` exits 0 too, and ``ocx.toml`` and the pinned leaves hold."""
    repo = f"t_{uuid4().hex[:8]}_up_current"
    _publish(ocx, repo, tmp_path, "3.28.0")
    project = _locked_project(ocx, tmp_path, f'[tools]\ncmake = "{ocx.registry}/{repo}:3.28"\n')
    before_toml = (project / "ocx.toml").read_bytes()
    before_lock = (project / "ocx.lock").read_bytes()
    before_leaves = _leaves(project)

    check = _upgrade(ocx, project, "--check")
    assert check.returncode == EXIT_SUCCESS, check.stderr
    assert (project / "ocx.lock").read_bytes() == before_lock

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [], payload
    assert {name: row["reason"] for name, row in _rows(payload, "skipped").items()} == {"cmake": "up_to_date"}, payload
    assert (project / "ocx.toml").read_bytes() == before_toml
    assert _leaves(project) == before_leaves


# ---------------------------------------------------------------------------
# Variant tracks
# ---------------------------------------------------------------------------


def test_upgrade_keeps_the_variant_track(ocx: OcxRunner, tmp_path: Path) -> None:
    """``debug-3.28`` moves to ``debug-3.29``; a newer release on the default track
    (``3.30``) is never a candidate."""
    repo = f"t_{uuid4().hex[:8]}_up_variant"
    _publish(ocx, repo, tmp_path, "debug-3.28.0")
    project = _locked_project(ocx, tmp_path, f'[tools]\ncmake = "{ocx.registry}/{repo}:debug-3.28"\n')
    _publish(ocx, repo, tmp_path, "debug-3.29.0", "3.30.0")

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    payload = _payload(result)
    assert payload["upgrades"] == [
        {"name": "cmake", "group": "default", "from_tag": "debug-3.28", "to_tag": "debug-3.29"}
    ], payload
    assert _toml(project)["tools"]["cmake"] == f"{ocx.registry}/{repo}:debug-3.29"


# ---------------------------------------------------------------------------
# Scoping (shared with ``ocx update``)
# ---------------------------------------------------------------------------


def test_upgrade_named_binding_leaves_the_others_declared(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx upgrade a`` retags ``a`` only; ``b`` keeps its declared tag and its pin."""
    short = uuid4().hex[:8]
    repo_a, repo_b = f"t_{short}_up_sa", f"t_{short}_up_sb"
    _publish(ocx, repo_a, tmp_path, "3.28.0")
    _publish(ocx, repo_b, tmp_path, "3.28.0")
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:3.28"
b = "{ocx.registry}/{repo_b}:3.28"
""",
    )
    _publish(ocx, repo_a, tmp_path, "3.29.0")
    _publish(ocx, repo_b, tmp_path, "3.29.0")

    result = _upgrade(ocx, project, "a")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert [row["name"] for row in _payload(result)["upgrades"]] == ["a"]
    tools = _toml(project)["tools"]
    assert tools["a"] == f"{ocx.registry}/{repo_a}:3.29"
    assert tools["b"] == f"{ocx.registry}/{repo_b}:3.28"


def test_upgrade_group_scope_leaves_the_default_table_declared(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx upgrade -g ci`` retags the ``ci`` group and leaves ``[tools]`` untouched;
    the report names each row's group."""
    short = uuid4().hex[:8]
    repo_default, repo_ci = f"t_{short}_up_gd", f"t_{short}_up_gc"
    _publish(ocx, repo_default, tmp_path, "3.28.0")
    _publish(ocx, repo_ci, tmp_path, "3.28.0")
    project = _locked_project(
        ocx,
        tmp_path,
        f"""\
[tools]
tool = "{ocx.registry}/{repo_default}:3.28"

[group.ci.tools]
citool = "{ocx.registry}/{repo_ci}:3.28"
""",
    )
    _publish(ocx, repo_default, tmp_path, "3.29.0")
    _publish(ocx, repo_ci, tmp_path, "3.29.0")

    result = _upgrade(ocx, project, "-g", "ci")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _payload(result)["upgrades"] == [
        {"name": "citool", "group": "ci", "from_tag": "3.28", "to_tag": "3.29"}
    ]
    config = _toml(project)
    assert config["tools"]["tool"] == f"{ocx.registry}/{repo_default}:3.28"
    assert config["group"]["ci"]["tools"]["citool"] == f"{ocx.registry}/{repo_ci}:3.29"


# ---------------------------------------------------------------------------
# Guards: exit codes, and nothing is written on a refusal
# ---------------------------------------------------------------------------


def _locked_single(ocx: OcxRunner, tmp_path: Path, suffix: str) -> tuple[Path, str]:
    repo = f"t_{uuid4().hex[:8]}_{suffix}"
    _publish(ocx, repo, tmp_path, "3.28.0")
    project = _locked_project(ocx, tmp_path, f'[tools]\ncmake = "{ocx.registry}/{repo}:3.28"\n')
    _publish(ocx, repo, tmp_path, "3.29.0")
    return project, repo


def _assert_untouched(project: Path, toml_bytes: bytes, lock_bytes: bytes | None) -> None:
    assert (project / "ocx.toml").read_bytes() == toml_bytes, "a refused upgrade must not rewrite ocx.toml"
    if lock_bytes is None:
        assert not (project / "ocx.lock").exists(), "a refused upgrade must not create ocx.lock"
    else:
        assert (project / "ocx.lock").read_bytes() == lock_bytes, "a refused upgrade must not rewrite ocx.lock"


@pytest.mark.parametrize("scope", [["does-not-exist"], ["-g", "no-such-group"]], ids=["name", "group"])
def test_upgrade_unknown_name_or_group_exits_64(ocx: OcxRunner, tmp_path: Path, scope: list[str]) -> None:
    project, _ = _locked_single(ocx, tmp_path, "up_unknown")
    toml_bytes, lock_bytes = (project / "ocx.toml").read_bytes(), (project / "ocx.lock").read_bytes()

    result = _upgrade(ocx, project, *scope)

    assert result.returncode == EXIT_USAGE, result.stderr
    _assert_untouched(project, toml_bytes, lock_bytes)


def test_upgrade_without_a_lock_exits_78_before_any_resolve(ocx: OcxRunner, tmp_path: Path) -> None:
    """No predecessor lock: exit 78. The registry host is unresolvable on purpose, so
    an exit 78 (rather than a network failure) shows the guard ran before any resolve."""
    project = _write_project(tmp_path, '[tools]\ncmake = "fake.registry.invalid/up_nolock:3.28"\n')
    toml_bytes = (project / "ocx.toml").read_bytes()

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_CONFIG, result.stderr
    _assert_untouched(project, toml_bytes, None)


def test_upgrade_on_a_drifted_toml_exits_65(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx.toml`` edited since ``ocx lock``: the freshness gate refuses with 65."""
    project, repo = _locked_single(ocx, tmp_path, "up_drift")
    lock_bytes = (project / "ocx.lock").read_bytes()
    drifted = f'[tools]\ncmake = "{ocx.registry}/{repo}:3.28"\nextra = "{ocx.registry}/{repo}:3.28"\n'
    (project / "ocx.toml").write_text(drifted)

    result = _upgrade(ocx, project)

    assert result.returncode == EXIT_DATA, result.stderr
    _assert_untouched(project, drifted.encode(), lock_bytes)


@pytest.mark.parametrize(("flag", "detail"), [("--offline", "offline_mode"), ("--frozen", "frozen_refused")])
def test_upgrade_under_offline_or_frozen_exits_81(
    ocx: OcxRunner, tmp_path: Path, flag: str, detail: str
) -> None:
    """Choosing a newer tag needs the registry, which both flags forbid."""
    project, _ = _locked_single(ocx, tmp_path, "up_policy")
    toml_bytes, lock_bytes = (project / "ocx.toml").read_bytes(), (project / "ocx.lock").read_bytes()

    result = _upgrade(ocx, project, global_flags=(flag,))

    assert result.returncode == EXIT_POLICY_BLOCKED, result.stderr
    error = _payload(result)["error"]
    assert error["kind"] == "permission_denied", result.stdout
    assert error["detail"] == detail, result.stdout
    _assert_untouched(project, toml_bytes, lock_bytes)
