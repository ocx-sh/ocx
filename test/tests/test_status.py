# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx status``.

``status`` reports what ``ocx.toml`` and ``ocx.lock`` say, with no resolution:
no registry, no platform selection, no package metadata. Its whole point is the
states that make every other project-tier command refuse to run — an absent
lock (78 elsewhere), a drifted one (65 elsewhere), an unparseable one — so each
of those is asserted to exit 0 here and carry the condition in the payload.

Exit codes per quality-rust-exit_codes.md:
    0  = Success (including "no lock", "stale lock", "unreadable lock")
    64 = UsageError (no ocx.toml in scope; a selector was passed)
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner

EXIT_SUCCESS = 0
EXIT_USAGE = 64


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=ocx.env
    )


def _status(ocx: OcxRunner, cwd: Path) -> dict:
    result = _run(ocx, cwd, "--format", "json", "status")
    assert result.returncode == EXIT_SUCCESS, (
        f"status must exit 0, got {result.returncode}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)


def _project(ocx: OcxRunner, tmp_path: Path) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    result = _run(ocx, project, "init")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    return project


def test_status_without_lock_reports_declared_only(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No ``ocx.lock`` is a state, not a failure: exit 0, ``present: false``,
    every binding carrying ``declared`` and no ``platforms``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS
    (project / "ocx.lock").unlink()

    data = _status(ocx, project)

    assert data["lock"]["present"] is False
    assert "current" not in data["lock"], "an absent lock cannot be current or stale"
    assert data["lock"]["declaration_hash_expected"].startswith("sha256:")

    tool = data["groups"]["default"]["tools"][unique_repo]
    assert tool["declared"].endswith(f"{unique_repo}:1.0.0")
    assert "platforms" not in tool, "unlocked bindings carry no platforms key"


def test_status_with_lock_reports_every_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A locked binding carries the FULL platform map, not the host leaf.

    Picking the host's leaf is resolution — ``ocx inspect``'s job. Status hands
    back every platform the lock records.
    """
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "amd64",
        platform="linux/amd64", new=True, cascade=False,
    )
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "arm64",
        platform="linux/arm64", new=False, cascade=False,
    )
    project = _project(ocx, tmp_path)
    assert _run(
        ocx, project, "add", "--no-pull", f"{unique_repo}:1.0.0"
    ).returncode == EXIT_SUCCESS

    data = _status(ocx, project)

    assert data["lock"]["present"] is True
    assert data["lock"]["current"] is True
    assert data["lock"]["declaration_hash"] == data["lock"]["declaration_hash_expected"]
    assert data["lock"]["generated_by"].startswith("ocx ")

    platforms = data["groups"]["default"]["tools"][unique_repo]["platforms"]
    assert {"linux/amd64", "linux/arm64"} <= set(platforms), platforms
    for digest in platforms.values():
        assert digest.startswith("sha256:"), platforms


def test_status_reports_drift_instead_of_refusing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A drifted lock exits 0 with ``current: false`` — and names WHICH binding
    drifted, which the hash alone cannot.

    Every other project-tier command exits 65 here. Status is the one that has
    to answer.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS

    # Declare a second binding without re-locking.
    config = project / "ocx.toml"
    config.write_text(
        config.read_text() + f'undeclared-in-lock = "{ocx.registry}/{unique_repo}:1.0.0"\n'
    )

    # A command that enforces the staleness gate refuses outright...
    stale = _run(ocx, project, "pull")
    assert stale.returncode == 65, (
        f"expected the staleness gate to fire for a sibling command, got {stale.returncode}"
    )

    # ...while status answers.
    data = _status(ocx, project)
    assert data["lock"]["current"] is False
    assert data["lock"]["declaration_hash"] != data["lock"]["declaration_hash_expected"]

    tools = data["groups"]["default"]["tools"]
    assert "platforms" not in tools["undeclared-in-lock"], (
        "the binding added since the last lock is the one without platforms"
    )
    assert "platforms" in tools[unique_repo], "the already-locked sibling keeps its pins"


def test_status_reports_unreadable_lock(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A corrupt ``ocx.lock`` still exits 0, flags ``readable: false``, and
    leaves the declaration half of the report fully intact.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS
    (project / "ocx.lock").write_text("this is not valid TOML for a lock [[[\n")

    data = _status(ocx, project)

    assert data["lock"]["present"] is True
    assert data["lock"]["error"], (
        "the error key is the unreadable state; there is no separate boolean"
    )
    assert "readable" not in data["lock"], (
        "a boolean that is only ever false repeats what `error` already says"
    )
    assert "current" not in data["lock"], "nothing was parsed, so nothing can be current"

    tool = data["groups"]["default"]["tools"][unique_repo]
    assert tool["declared"].endswith(f"{unique_repo}:1.0.0"), (
        "the declaration is still readable and must still be reported"
    )


def test_status_reports_env_verbatim_per_scope(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``[env]`` lands in ``groups.default.env``; a group's env stays in its own
    scope; a relative ``path`` value is NOT resolved against the project root.

    Root ``[tools]``/``[env]`` ARE the default group — that is why ``default``
    is a reserved group name — so the report has no separate top-level env.
    """
    project = _project(ocx, tmp_path)
    config = project / "ocx.toml"
    config.write_text(
        config.read_text()
        + "\n[env]\nCI = \"1\"\nNODE_BIN = { type = \"path\", value = \"node_modules/.bin\" }\n"
        + "\n[group.lint.env]\nSTRICT = \"yes\"\n"
    )

    data = _status(ocx, project)

    default_env = data["groups"]["default"]["env"]
    assert default_env["CI"] == {"type": "constant", "value": "1"}, default_env
    assert default_env["NODE_BIN"] == {
        "type": "path",
        "value": "node_modules/.bin",
    }, "a relative path value must stay verbatim — anchoring it is composition"

    assert data["groups"]["lint"]["env"] == {"STRICT": {"type": "constant", "value": "yes"}}
    assert "CI" not in data["groups"]["lint"]["env"], "env is per-scope, never merged"
    assert "env" not in data, "there is no top-level env — root [env] IS group default's"


def test_status_reports_package_settings(ocx: OcxRunner, tmp_path: Path) -> None:
    """``[package."<id>"]`` is reported even though it is excluded from
    ``declaration_hash`` — which is exactly why nothing lock-derived can show it.
    """
    project = _project(ocx, tmp_path)
    config = project / "ocx.toml"
    before = _status(ocx, project)["lock"]["declaration_hash_expected"]
    config.write_text(
        config.read_text() + '\n[package."ocx.sh/example:1"]\nno-patches = true\n'
    )

    data = _status(ocx, project)

    assert data["package_settings"] == {"ocx.sh/example:1": {"no-patches": True}}
    assert data["lock"]["declaration_hash_expected"] == before, (
        "a [package.*] edit must not move the declaration hash"
    )


def test_status_outside_a_project_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """No ``ocx.toml`` in scope is a usage error, same as every project-tier
    command.
    """
    empty = tmp_path / "empty"
    empty.mkdir()

    result = _run(ocx, empty, "status")

    assert result.returncode == EXIT_USAGE, (
        f"expected 64, got {result.returncode}\nstderr: {result.stderr}"
    )


def test_status_rejects_selectors(ocx: OcxRunner, tmp_path: Path) -> None:
    """``status`` takes no ``-g`` and no NAME: the report is a keyed object the
    caller narrows itself, so a filter would only hide rows.
    """
    project = _project(ocx, tmp_path)

    assert _run(ocx, project, "status", "-g", "ci").returncode != EXIT_SUCCESS
    assert _run(ocx, project, "status", "some-binding").returncode != EXIT_SUCCESS


def test_status_makes_no_network_or_store_writes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--offline`` changes nothing, and no symlink appears for a declared but
    never-installed binding.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS

    online = _run(ocx, project, "--format", "json", "status")
    offline = _run(ocx, project, "--offline", "--format", "json", "status")

    assert offline.returncode == EXIT_SUCCESS, offline.stderr
    assert offline.stdout == online.stdout, "status must not behave differently offline"

    home = Path(ocx.env["OCX_HOME"])
    assert not (home / "symlinks").exists() or not any(
        (home / "symlinks").rglob(f"*{unique_repo}*")
    ), "status must not create install symlinks"
