# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Every file ocx rewrites on the user's behalf is published in place.

A user-owned file may be a symlink (dotfile managers) and carries a mode the
user chose. Each writer must follow the link, keep the existing mode, and give a
newly created file the umask default instead of a private 0600. One matrix over
all such writers: shell profile, ``config.toml``, ``ocx.toml``, ``ocx.lock`` and
the docker ``config.json``.
"""

from __future__ import annotations

import functools
import json
import os
import shutil
import stat
import subprocess
import sys
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Literal
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="symlink and POSIX mode semantics; Windows has no umask or 0o640.",
    ),
    pytest.mark.command("self_group/setup", "init", "add", "lock", "login"),
]

State = Literal["symlink", "mode", "fresh"]

_UMASK = 0o022
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
_USER_TEXT = "# user content\n"


@dataclass
class _Case:
    """One prepared command: the file under test and how to rewrite it."""

    path: Path
    argv: list[str]
    env: dict[str, str]
    holds: Callable[[str], bool]  # True once the file carries the row's new content
    stdin: str | None = None


@dataclass(frozen=True)
class _Row:
    prepare: Callable[[OcxRunner, Path, State], _Case]
    fresh_mode: int


def _run(
    ocx: OcxRunner,
    argv: list[str],
    env: dict[str, str],
    *,
    cwd: Path | None = None,
    stdin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *argv],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        cwd=cwd,
        input=stdin if stdin is not None else "",
        umask=_UMASK,
        check=False,
    )


def _sandbox_env(ocx: OcxRunner, tmp_path: Path) -> dict[str, str]:
    """The runner env with HOME, XDG and docker state moved under ``tmp_path``."""
    home = tmp_path / "home"
    docker = tmp_path / "docker"
    home.mkdir(exist_ok=True)
    docker.mkdir(exist_ok=True)
    env = dict(ocx.env)
    env.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(home / ".config"),
        XDG_DATA_HOME=str(home / ".local" / "share"),
        XDG_STATE_HOME=str(home / ".local" / "state"),
        XDG_CACHE_HOME=str(home / ".cache"),
        SHELL="/bin/sh",
        DOCKER_CONFIG=str(docker),
    )
    return env


def _ok(result: subprocess.CompletedProcess[str], what: str) -> None:
    assert result.returncode == 0, f"{what} failed: rc={result.returncode}\nstderr:\n{result.stderr}"


def _place(path: Path, state: State, tmp_path: Path, content: str) -> None:
    """Materialise ``path`` as a symlink to a real file, a 0640 file, or nothing."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if state == "fresh":
        assert not os.path.lexists(path), f"{path} must not exist before a fresh write"
    elif state == "mode":
        path.write_text(content, encoding="utf-8")
        path.chmod(0o640)
    else:
        target = tmp_path / "real" / f"{uuid4().hex[:8]}-{path.name}"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        path.symlink_to(target)


def _seed_candidate(ocx: OcxRunner) -> None:
    """Pre-place the install candidate so ``--offline self setup`` needs no registry."""
    candidate = Path(ocx.env["OCX_HOME"]) / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ocx.binary, candidate)
    candidate.chmod(0o755)


def _publish(ocx: OcxRunner, tmp_path: Path, label: str) -> tuple[str, str]:
    """Push a package and return ``(repo, fully qualified reference)``."""
    repo = f"t_{uuid4().hex[:8]}_{label}"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path / f"pkg_{label}", cascade=False)
    return repo, pkg.fq


# --- rows -------------------------------------------------------------------


def _profile(ocx: OcxRunner, tmp_path: Path, state: State) -> _Case:
    env = _sandbox_env(ocx, tmp_path)
    _seed_candidate(ocx)
    profile = tmp_path / "home" / ".profile_under_test"
    _place(profile, state, tmp_path, _USER_TEXT)
    return _Case(
        path=profile,
        argv=["--offline", "self", "setup", "--profile", str(profile)],
        env=env,
        holds=lambda text: "# >>> ocx v1" in text,
    )


def _config_toml(ocx: OcxRunner, tmp_path: Path, state: State) -> _Case:
    env = _sandbox_env(ocx, tmp_path)
    _seed_candidate(ocx)
    config = Path(ocx.env["OCX_HOME"]) / "config.toml"
    _place(config, state, tmp_path, _USER_TEXT)
    return _Case(
        path=config,
        # `--no-modify-path` persists `[shell] modify_path = false`; no profile is touched.
        argv=["--offline", "self", "setup", "--no-modify-path"],
        env=env,
        holds=lambda text: "shell" in tomllib.loads(text),
    )


def _scratch_project(ocx: OcxRunner, tmp_path: Path, env: dict[str, str], refs: list[str]) -> tuple[str, str]:
    """Run ``ocx add`` per reference; return ``(ocx.toml, ocx.lock)`` text after the last add."""
    scratch = tmp_path / "scratch"
    scratch.mkdir(exist_ok=True)
    _ok(_run(ocx, ["--project", str(scratch), "init", "--no-consent"], env), "ocx init")
    lock_after_first = ""
    for index, ref in enumerate(refs):
        _ok(_run(ocx, ["--project", str(scratch / "ocx.toml"), "add", ref], env), f"ocx add {ref}")
        if index == 0:
            lock_after_first = (scratch / "ocx.lock").read_text(encoding="utf-8")
    return (scratch / "ocx.toml").read_text(encoding="utf-8"), lock_after_first


def _ocx_toml(ocx: OcxRunner, tmp_path: Path, state: State) -> _Case:
    env = _sandbox_env(ocx, tmp_path)
    project = tmp_path / "proj"
    toml = project / "ocx.toml"
    if state == "fresh":
        project.mkdir()
        return _Case(
            path=toml,
            argv=["--project", str(project), "init", "--no-consent"],
            env=env,
            holds=lambda text: "[tools]" in text,
        )
    repo, ref = _publish(ocx, tmp_path, "toml")
    skeleton, _ = _scratch_project(ocx, tmp_path, env, [])
    _place(toml, state, tmp_path, skeleton)
    return _Case(
        path=toml,
        argv=["--project", str(toml), "add", ref],
        env=env,
        holds=lambda text: repo in text,
    )


def _ocx_lock(ocx: OcxRunner, tmp_path: Path, state: State) -> _Case:
    env = _sandbox_env(ocx, tmp_path)
    _, ref_a = _publish(ocx, tmp_path, "locka")
    repo_b, ref_b = _publish(ocx, tmp_path, "lockb")
    project = tmp_path / "proj"
    project.mkdir()
    lock = project / "ocx.lock"
    # The lock predates tool B, so `ocx lock` has a real change to write.
    toml_text, stale_lock = _scratch_project(ocx, tmp_path, env, [ref_a, ref_b])
    toml = project / "ocx.toml"
    toml.write_text(toml_text, encoding="utf-8")
    _place(lock, state, tmp_path, stale_lock)

    def holds(text: str) -> bool:
        tomllib.loads(text)  # the lock must stay valid TOML
        return repo_b in text

    return _Case(path=lock, argv=["--project", str(toml), "lock"], env=env, holds=holds)


def _docker_config(ocx: OcxRunner, tmp_path: Path, state: State) -> _Case:
    env = _sandbox_env(ocx, tmp_path)
    config = Path(env["DOCKER_CONFIG"]) / "config.json"
    _place(config, state, tmp_path, "{}\n")
    registry = ocx.registry

    def holds(text: str) -> bool:
        return any(registry in key for key in json.loads(text).get("auths", {}))

    return _Case(
        path=config,
        argv=["login", "--no-verify", "--allow-insecure-store", "-u", "user", "--password-stdin", registry],
        env=env,
        holds=holds,
        stdin="token\n",
    )


_ROWS = [
    pytest.param(_Row(_profile, 0o644), id="profile"),
    pytest.param(_Row(_config_toml, 0o644), id="config_toml"),
    pytest.param(_Row(_ocx_toml, 0o644), id="ocx_toml"),
    pytest.param(_Row(_ocx_lock, 0o644), id="ocx_lock"),
    # docker creates its config owner-only; ocx login keeps that contract.
    pytest.param(_Row(_docker_config, 0o600), id="docker_config"),
]


def _mode(path: Path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


# --- tests ------------------------------------------------------------------


@pytest.mark.parametrize("row", _ROWS)
def test_symlinked_file_stays_a_symlink_and_target_is_updated(
    ocx: OcxRunner, tmp_path: Path, row: _Row
) -> None:
    case = row.prepare(ocx, tmp_path, "symlink")
    link_target = os.readlink(case.path)
    seed = Path(link_target).read_text(encoding="utf-8")
    assert not case.holds(seed), "the seeded content must not already satisfy the marker"

    _ok(_run(ocx, case.argv, case.env, stdin=case.stdin), "rewriting command")

    assert os.path.islink(case.path), f"{case.path} was replaced by a regular file"
    assert os.readlink(case.path) == link_target, "the symlink must keep pointing at the same target"
    assert case.holds(Path(link_target).read_text(encoding="utf-8")), (
        f"the link target must carry the new content; got:\n{Path(link_target).read_text(encoding='utf-8')}"
    )


@pytest.mark.parametrize("row", _ROWS)
def test_existing_file_mode_is_preserved(ocx: OcxRunner, tmp_path: Path, row: _Row) -> None:
    case = row.prepare(ocx, tmp_path, "mode")
    assert _mode(case.path) == 0o640, "fixture must start at 0640"
    assert not case.holds(case.path.read_text(encoding="utf-8")), (
        "the seeded content must not already satisfy the marker"
    )

    _ok(_run(ocx, case.argv, case.env, stdin=case.stdin), "rewriting command")

    assert case.holds(case.path.read_text(encoding="utf-8")), "the command must have rewritten the file"
    assert _mode(case.path) == 0o640, f"mode changed to {_mode(case.path):#o}; the user's 0640 must survive"


@pytest.mark.parametrize("row", _ROWS)
def test_new_file_gets_the_umask_default_mode(ocx: OcxRunner, tmp_path: Path, row: _Row) -> None:
    case = row.prepare(ocx, tmp_path, "fresh")
    assert not os.path.lexists(case.path), "fixture must start without the file"

    _ok(_run(ocx, case.argv, case.env, stdin=case.stdin), "creating command")

    assert os.path.isfile(case.path) and not os.path.islink(case.path), f"{case.path} must be a regular file"
    assert case.holds(case.path.read_text(encoding="utf-8")), "the new file must carry the row's content"
    assert _mode(case.path) == row.fresh_mode, (
        f"fresh file mode is {_mode(case.path):#o}, expected {row.fresh_mode:#o} under umask {_UMASK:#o}"
    )


# --- owner and group the user cannot carry ------------------------------------


@functools.cache
def _sudo_available() -> bool:
    """True for a non-root user with passwordless sudo, computed once per process."""
    # Windows has no `geteuid`; the marker below is evaluated at import on every platform.
    if sys.platform == "win32" or os.geteuid() == 0:
        return False
    probe = subprocess.run(["sudo", "-n", "true"], capture_output=True, check=False)
    return probe.returncode == 0


_needs_sudo = pytest.mark.skipif(
    not _sudo_available(),
    reason="needs passwordless sudo to give a file an owner or group the test user cannot set",
)


def _sudo(*argv: str) -> None:
    result = subprocess.run(["sudo", "-n", *argv], capture_output=True, text=True, encoding="utf-8", check=False)
    assert result.returncode == 0, f"sudo {' '.join(argv)} failed: rc={result.returncode}\n{result.stderr}"


def _arrange_profile(ocx: OcxRunner, tmp_path: Path, via_symlink: bool) -> tuple[_Case, Path]:
    """The profile row with user content; returns the case and the file that holds the bytes."""
    case = _profile(ocx, tmp_path, "fresh")
    target = case.path
    if via_symlink:
        target = tmp_path / "real" / "profile"
        target.parent.mkdir(parents=True, exist_ok=True)
        case.path.symlink_to(target)
    target.write_text(_USER_TEXT, encoding="utf-8")
    return case, target


def _describe(path: Path) -> str:
    st = os.stat(path)
    return f"ino={st.st_ino} uid={st.st_uid} gid={st.st_gid} mode={stat.S_IMODE(st.st_mode):#o}"


@_needs_sudo
@pytest.mark.parametrize("via_symlink", [False, True], ids=["file", "symlink"])
def test_uncarriable_group_is_kept_by_writing_in_place(ocx: OcxRunner, tmp_path: Path, via_symlink: bool) -> None:
    """A file whose group the user cannot set is rewritten in place: same inode, owner, group and mode."""
    if 0 in os.getgroups() or os.getgid() == 0:
        pytest.skip("the test user is in group 0, so that group can be carried")
    case, target = _arrange_profile(ocx, tmp_path, via_symlink)
    _sudo("chown", f"{os.getuid()}:0", str(target))
    target.chmod(0o660)
    before = os.stat(target)

    _ok(_run(ocx, case.argv, case.env), "ocx self setup")

    after = os.stat(target)
    seen = _describe(target)
    assert after.st_ino == before.st_ino, f"file was replaced, not written in place: {seen}"
    assert after.st_uid == os.getuid(), f"owner changed: {seen}"
    assert after.st_gid == 0, f"group changed: {seen}"
    assert stat.S_IMODE(after.st_mode) == 0o660, f"mode changed: {seen}"
    assert os.path.islink(case.path) == via_symlink, f"{case.path} link state changed"
    text = target.read_text(encoding="utf-8")
    assert case.holds(text), f"the ocx block is missing:\n{text}"
    assert _USER_TEXT in text, f"the user's text was lost:\n{text}"


@_needs_sudo
@pytest.mark.parametrize("via_symlink", [False, True], ids=["file", "symlink"])
def test_uncarriable_owner_is_kept_by_writing_in_place(ocx: OcxRunner, tmp_path: Path, via_symlink: bool) -> None:
    """A root-owned, group-writable file is rewritten in place: same inode, owner, group and mode."""
    case, target = _arrange_profile(ocx, tmp_path, via_symlink)
    _sudo("chown", f"0:{os.getgid()}", str(target))
    _sudo("chmod", "0664", str(target))
    before = os.stat(target)

    _ok(_run(ocx, case.argv, case.env), "ocx self setup")

    after = os.stat(target)
    seen = _describe(target)
    assert after.st_ino == before.st_ino, f"file was replaced, not written in place: {seen}"
    assert after.st_uid == 0, f"owner changed: {seen}"
    assert after.st_gid == os.getgid(), f"group changed: {seen}"
    assert stat.S_IMODE(after.st_mode) == 0o664, f"mode changed: {seen}"
    assert os.path.islink(case.path) == via_symlink, f"{case.path} link state changed"
    text = target.read_text(encoding="utf-8")
    assert case.holds(text), f"the ocx block is missing:\n{text}"
    assert _USER_TEXT in text, f"the user's text was lost:\n{text}"


@_needs_sudo
def test_unwritable_profile_is_left_untouched_and_reported_skipped(ocx: OcxRunner, tmp_path: Path) -> None:
    """A root-owned 0644 profile is reported ``skipped_unwritable``, left byte-identical, and setup exits 0."""
    case, target = _arrange_profile(ocx, tmp_path, via_symlink=False)
    _sudo("chown", "0:0", str(target))
    _sudo("chmod", "0644", str(target))
    before_bytes = target.read_bytes()
    before = os.stat(target)

    result = _run(ocx, ["--format", "json", *case.argv], case.env)

    _ok(result, "ocx self setup")
    payload = json.loads(result.stdout)
    outcomes = {entry["path"]: entry["outcome"] for entry in payload["profiles"]}
    assert outcomes == {str(case.path): "skipped_unwritable"}, f"got: {payload!r}"
    after = os.stat(target)
    seen = _describe(target)
    assert target.read_bytes() == before_bytes, "the unwritable file must stay byte-identical"
    assert after.st_ino == before.st_ino, f"file was replaced: {seen}"
    assert (after.st_uid, after.st_gid) == (0, 0), f"owner changed: {seen}"
    assert stat.S_IMODE(after.st_mode) == 0o644, f"mode changed: {seen}"


def test_carriable_owner_and_group_replace_by_rename(ocx: OcxRunner, tmp_path: Path) -> None:
    """A file whose owner and group the user can carry is replaced by rename and keeps owner, group and mode."""
    case, target = _arrange_profile(ocx, tmp_path, via_symlink=False)
    target.chmod(0o640)
    before = os.stat(target)

    _ok(_run(ocx, case.argv, case.env), "ocx self setup")

    after = os.stat(target)
    seen = _describe(target)
    assert after.st_ino != before.st_ino, f"expected a rename-replace, inode is unchanged: {seen}"
    assert (after.st_uid, after.st_gid) == (before.st_uid, before.st_gid), f"owner or group changed: {seen}"
    assert stat.S_IMODE(after.st_mode) == 0o640, f"mode changed: {seen}"
    assert case.holds(target.read_text(encoding="utf-8")), "the ocx block is missing"


def test_hard_linked_profile_is_written_in_place(ocx: OcxRunner, tmp_path: Path) -> None:
    """A profile with a second hard link is rewritten in place, so both names see the new content."""
    case, target = _arrange_profile(ocx, tmp_path, via_symlink=False)
    target.chmod(0o640)
    second = tmp_path / "second-name"
    os.link(target, second)
    before = os.stat(target)

    _ok(_run(ocx, case.argv, case.env), "ocx self setup")

    after = os.stat(target)
    seen = _describe(target)
    assert after.st_ino == before.st_ino, f"file was replaced, not written in place: {seen}"
    assert after.st_nlink == 2, f"the second hard link was severed: nlink={after.st_nlink} {seen}"
    assert stat.S_IMODE(after.st_mode) == 0o640, f"mode changed: {seen}"
    for name in (target, second):
        text = name.read_text(encoding="utf-8")
        assert case.holds(text), f"the ocx block is missing from {name}:\n{text}"
        assert _USER_TEXT in text, f"the user's text was lost from {name}:\n{text}"
