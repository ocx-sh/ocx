# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx inspect`` (toolchain tier).

``inspect`` is the project-tier counterpart to ``ocx package inspect``: the same
report, keyed by ``ocx.toml`` binding instead of raw identifier, plus the
project's composed ``[env]``. Read-only — nothing is installed, no symlink is
created, neither project file is written.

Where ``ocx status`` reports a missing or drifted lock as payload, ``inspect``
refuses: without a pin there is no stable answer. Both halves of that split are
asserted here.

``--resolve`` is what selects a platform, exactly as on the OCI-tier command.
Default mode lists each binding's locked platform candidates and touches no
registry.

Exit codes per quality-rust-exit_codes.md:
    0  = Success
    64 = UsageError (unknown group or binding name)
    65 = DataError (stale lock; unrealizable closure surface)
    78 = ConfigError (ocx.lock absent)
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src.helpers import inspect_entry, inspect_names, make_package
from src.runner import OcxRunner, current_platform

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_DATA = 65
EXIT_CONFIG = 78


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=ocx.env
    )


def _inspect(ocx: OcxRunner, cwd: Path, *args: str) -> dict:
    result = _run(ocx, cwd, "--format", "json", "inspect", *args)
    assert result.returncode == EXIT_SUCCESS, (
        f"inspect must exit 0, got {result.returncode}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)


def _project(ocx: OcxRunner, tmp_path: Path) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    assert _run(ocx, project, "init").returncode == EXIT_SUCCESS
    return project


def test_inspect_is_keyed_by_binding_not_identifier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Each entry names itself by its ``ocx.toml`` binding, and carries the same
    per-package shape ``ocx package inspect`` emits.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(
        ocx, project, "add", "--no-pull", pkg.short
    ).returncode == EXIT_SUCCESS

    data = _inspect(ocx, project, "--resolve")

    assert inspect_names(data) == [unique_repo], (
        "entries are keyed by the ocx.toml binding, not by the requested identifier"
    )
    entry = inspect_entry(data, unique_repo)
    assert entry["identifier"].endswith(f"{unique_repo}:1.0.0"), (
        f"identifier is the ocx.toml declaration, tag and all: {entry['identifier']}"
    )
    assert entry["pinned_digest"].startswith("sha256:")
    assert entry["pinned_identifier"] == f"{entry['identifier']}@{entry['pinned_digest']}", (
        "pinned_identifier is the declared identifier plus the resolved digest, "
        "exactly as ocx package inspect emits it for the same package"
    )
    assert data["platform"], "--resolve selects a platform, so the report names it"


def test_inspect_default_lists_locked_candidates_without_resolving(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default mode is a pure projection of ``ocx.lock``.

    ``--resolve`` is what selects a platform, here exactly as in
    ``ocx package inspect``. Without it every binding lists the platform
    candidates the lock already pins, so nothing is fetched, no platform is
    named, and ``-p`` stays inert.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(
        ocx, project, "add", "--no-pull", pkg.short
    ).returncode == EXIT_SUCCESS
    locked = {
        platform: digest
        for platform, digest in json.loads(
            _run(ocx, project, "--format", "json", "status").stdout
        )["groups"]["default"]["tools"][unique_repo]["platforms"].items()
    }

    entry = inspect_entry(_inspect(ocx, project), unique_repo)

    assert "platform" not in _inspect(ocx, project), (
        "nothing was selected, so the report must not name a platform"
    )
    assert "pinned_digest" not in entry and "pinned_identifier" not in entry, (
        f"a lock projection pins no single artifact: {entry}"
    )
    assert "metadata" not in entry and "resolution" not in entry
    candidates = {c["platform"]: c["digest"] for c in entry["candidates"]}
    assert candidates == locked, (
        f"candidates are the lock's platform leaves verbatim: {candidates} != {locked}"
    )
    for candidate in entry["candidates"]:
        assert candidate["pinned"] == f"{entry['identifier']}@{candidate['digest']}"
        assert "media_type" not in candidate and "size" not in candidate, (
            "the lock records leaf digests, not descriptors"
        )

    offline = _run(ocx, project, "--format", "json", "--offline", "inspect")
    assert offline.returncode == EXIT_SUCCESS, (
        f"the default report needs no registry at all\n{offline.stderr}"
    )
    assert json.loads(offline.stdout) == _inspect(ocx, project)

    host = current_platform()
    other = "darwin/arm64" if host != "darwin/arm64" else "linux/amd64"
    assert _inspect(ocx, project, "-p", other) == _inspect(ocx, project), (
        "-p selects nothing without --resolve, so it must not change the report"
    )


def test_inspect_env_follows_application_order(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``env`` is an ordered array: ``[env]``, then the selected group's env,
    then ``--env`` last.

    An array, not an object: one key can legitimately appear more than once
    across the layers, which an object cannot express.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(
        ocx, project, "add", "--no-pull", "-g", "ci", pkg.short
    ).returncode == EXIT_SUCCESS
    config = project / "ocx.toml"
    config.write_text(
        config.read_text()
        + '\n[env]\nSTAGE = "project"\n'
        + '\n[group.ci.env]\nSTAGE = "group"\n'
    )

    data = _inspect(ocx, project, "-g", "ci", "--env", "STAGE=cli")

    stages = [entry["value"] for entry in data["env"] if entry["key"] == "STAGE"]
    assert stages == ["project", "group", "cli"], (
        f"env must keep every contributing layer in application order, got {stages}"
    )
    for entry in data["env"]:
        assert entry["type"] in ("constant", "path"), entry


def test_inspect_resolves_relative_path_env_unlike_status(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A relative ``type = "path"`` value is anchored to the project root here.

    This is the concrete difference between the two commands: ``status`` shows
    the file, ``inspect`` shows the composition.
    """
    project = _project(ocx, tmp_path)
    config = project / "ocx.toml"
    config.write_text(
        config.read_text()
        + '\n[env]\nNODE_BIN = { type = "path", value = "node_modules/.bin" }\n'
    )
    assert _run(ocx, project, "lock").returncode == EXIT_SUCCESS

    inspected = _inspect(ocx, project)
    node_bin = [e for e in inspected["env"] if e["key"] == "NODE_BIN"]
    assert node_bin, inspected["env"]
    assert node_bin[0]["value"].endswith("node_modules/.bin")
    assert Path(node_bin[0]["value"]).is_absolute(), (
        "inspect composes, so a relative path value is anchored to the project root"
    )

    status = json.loads(
        _run(ocx, project, "--format", "json", "status").stdout
    )
    assert status["groups"]["default"]["env"]["NODE_BIN"]["value"] == "node_modules/.bin", (
        "status shows the file verbatim — the two commands answer different questions"
    )


def test_inspect_name_filter_narrows_and_rejects_unknown(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """NAMEs narrow the report; an unknown name is a usage error."""
    first = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path)
    second = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    one, two = f"{unique_repo}_a", f"{unique_repo}_b"
    assert _run(ocx, project, "add", "--no-pull", first.short).returncode == EXIT_SUCCESS
    assert _run(ocx, project, "add", "--no-pull", second.short).returncode == EXIT_SUCCESS

    assert set(inspect_names(_inspect(ocx, project))) == {one, two}
    assert inspect_names(_inspect(ocx, project, two)) == [two]

    unknown = _run(ocx, project, "inspect", "nope")
    assert unknown.returncode == EXIT_USAGE, (
        f"expected 64 for an unknown binding, got {unknown.returncode}\n{unknown.stderr}"
    )


def test_inspect_subset_survives_an_unnamed_group_collision(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Two selected groups pinning one binding differently must not fail a run
    that named a different binding.

    The colliding binding still errors when it IS named, so the check is
    narrowed rather than dropped.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False, cascade=False)
    other = make_package(ocx, f"{unique_repo}_other", "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)

    assert _run(
        ocx, project, "add", "--no-pull", "-g", "alpha", f"{unique_repo}:1.0.0"
    ).returncode == EXIT_SUCCESS
    assert _run(
        ocx, project, "add", "--no-pull", "-g", "beta", f"{unique_repo}:2.0.0"
    ).returncode == EXIT_SUCCESS
    assert _run(
        ocx, project, "add", "--no-pull", "-g", "alpha", other.short
    ).returncode == EXIT_SUCCESS
    other_binding = f"{unique_repo}_other"

    narrowed = _inspect(ocx, project, "-g", "alpha,beta", other_binding)
    assert inspect_names(narrowed) == [other_binding]

    named = _run(
        ocx, project, "--format", "json", "inspect", "-g", "alpha,beta", unique_repo
    )
    assert named.returncode == EXIT_USAGE, (
        f"naming the colliding binding must still error, got {named.returncode}\n{named.stderr}"
    )

    whole = _run(ocx, project, "--format", "json", "inspect", "-g", "alpha,beta")
    assert whole.returncode == EXIT_USAGE, (
        f"the unfiltered selection must still error, got {whole.returncode}\n{whole.stderr}"
    )


def test_inspect_requires_a_current_lock(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Absent lock -> 78, drifted lock -> 65 — the two states ``status`` reports
    as payload and ``inspect`` refuses, because a moving tag would make the
    answer depend on the moment.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS

    lock = project / "ocx.lock"
    saved = lock.read_text()
    lock.unlink()
    missing = _run(ocx, project, "inspect")
    assert missing.returncode == EXIT_CONFIG, (
        f"expected 78 for an absent lock, got {missing.returncode}\n{missing.stderr}"
    )
    assert json.loads(
        _run(ocx, project, "--format", "json", "status").stdout
    )["lock"]["present"] is False, "status still answers where inspect refuses"

    lock.write_text(saved)
    config = project / "ocx.toml"
    config.write_text(config.read_text() + f'drifted = "{ocx.registry}/{unique_repo}:1.0.0"\n')
    stale = _run(ocx, project, "inspect")
    assert stale.returncode == EXIT_DATA, (
        f"expected 65 for a drifted lock, got {stale.returncode}\n{stale.stderr}"
    )


def test_inspect_closure_reports_surface_without_installing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--closure`` adds the dependency closure and surface projections, and
    creates no install symlink doing it.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, binaries=["tool-bin"])
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS

    data = _inspect(ocx, project, "--closure")
    closure = inspect_entry(data, unique_repo)["closure"]

    assert closure["conflicts"] == {"entrypoints": [], "repositories": []}
    interface = closure["surface"]["interface"]
    assert any(b["name"] == "tool-bin" for b in interface["binaries"]), interface

    home = Path(ocx.env["OCX_HOME"])
    symlinks = home / "symlinks"
    assert not symlinks.exists() or not any(symlinks.rglob(f"*{unique_repo}*")), (
        "inspect must not create install symlinks"
    )


def test_inspect_default_group_only_unless_scoped(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An omitted ``-g`` means the default group, not everything — matching
    ``ocx run`` / ``ocx env``. ``-g all`` is the explicit spelling.
    """
    default_pkg = make_package(ocx, f"{unique_repo}_d", "1.0.0", tmp_path)
    ci_pkg = make_package(ocx, f"{unique_repo}_c", "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(
        ocx, project, "add", "--no-pull", default_pkg.short
    ).returncode == EXIT_SUCCESS
    assert _run(
        ocx, project, "add", "--no-pull", "-g", "ci", ci_pkg.short
    ).returncode == EXIT_SUCCESS
    dflt, cit = f"{unique_repo}_d", f"{unique_repo}_c"

    assert inspect_names(_inspect(ocx, project)) == [dflt]
    assert inspect_names(_inspect(ocx, project, "-g", "ci")) == [cit]
    assert set(inspect_names(_inspect(ocx, project, "-g", "all"))) == {dflt, cit}

    unknown_group = _run(ocx, project, "inspect", "-g", "nope")
    assert unknown_group.returncode == EXIT_USAGE, unknown_group.stderr
