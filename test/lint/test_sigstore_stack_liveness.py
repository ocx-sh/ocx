# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A running Sigstore stack is reused only while it belongs to this checkout.

Every worktree shares the compose project name, so a stack started from a
worktree that has since been deleted still reports `running` while its bind
sources are gone. Reusing it fails every sign with a dead mount.

No docker: the predicate is pure over `docker inspect` JSON, and the wiring
test swaps `subprocess.run` for a fake compose.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src import helpers

_DIR = "/work/a/test"
_LABEL = "com.docker.compose.project.working_dir"


def _inspect(working_dir: str, *bind_sources: str) -> str:
    return json.dumps(
        [
            {
                "Config": {"Labels": {_LABEL: working_dir}},
                "Mounts": [
                    *({"Type": "bind", "Source": source} for source in bind_sources),
                    {"Type": "volume", "Source": "/var/lib/docker/volumes/x/_data"},
                ],
            }
        ]
    )


def test_a_stack_from_this_checkout_with_live_sources_is_current() -> None:
    assert helpers._stack_is_current(_inspect(_DIR, f"{_DIR}/sigstore/keys"), lambda _p: True)


def test_a_stack_whose_bind_source_is_gone_is_stale() -> None:
    inspect = _inspect(_DIR, f"{_DIR}/sigstore/keys")
    assert not helpers._stack_is_current(inspect, lambda _p: False)


def test_a_stack_from_another_live_directory_is_current() -> None:
    inspect = _inspect("/work/b/test", "/work/b/test/sigstore/keys")
    assert helpers._stack_is_current(inspect, lambda _p: True)


def test_a_stack_from_another_directory_that_is_gone_is_stale() -> None:
    inspect = _inspect("/work/b/test", "/work/b/test/sigstore/keys")
    assert not helpers._stack_is_current(inspect, lambda p: not p.startswith("/work/b"))


def test_no_containers_is_not_current() -> None:
    assert not helpers._stack_is_current("[]", lambda _p: True)


def test_only_bind_mounts_are_checked_for_existence() -> None:
    seen: list[str] = []

    def exists(path: str) -> bool:
        seen.append(path)
        return True

    helpers._stack_is_current(_inspect(_DIR, f"{_DIR}/k"), exists)
    assert seen == [_DIR, f"{_DIR}/k"]  # the volume mount is never probed


class _FakeCompose:
    """Reports the whole profile running, with `inspect` answering `stale`."""

    def __init__(self, stale_inspect: str, inspect_returncode: int = 0) -> None:
        self.stale_inspect = stale_inspect
        self.inspect_returncode = inspect_returncode
        self.calls: list[str] = []

    def run(self, argv: list[str], **_kwargs: object) -> helpers.subprocess.CompletedProcess[str]:
        stdout = ""
        if "config" in argv:
            stdout = "mirror-registry\ndex\nfulcio" + ("\nrekor" if "--profile" in argv else "")
        elif "ps" in argv and "-q" in argv:
            stdout = "c1\nc2"
        elif "ps" in argv:
            stdout = "dex\nfulcio\nrekor"
        elif argv[:2] == ["docker", "inspect"]:
            stdout = self.stale_inspect
            return helpers.subprocess.CompletedProcess(argv, self.inspect_returncode, stdout, "")
        elif "down" in argv:
            self.calls.append("down")
        elif "up" in argv:
            self.calls.append("up")
        return helpers.subprocess.CompletedProcess(argv, 0, stdout, "")


def _bring_up(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, inspect: str, inspect_returncode: int = 0
) -> list[str]:
    fake = _FakeCompose(inspect, inspect_returncode)
    monkeypatch.setattr(helpers, "_COMPOSE_LOCK", tmp_path / "compose.lock")
    monkeypatch.setattr(helpers.subprocess, "run", fake.run)
    helpers._bring_up_serialized()
    return fake.calls


def test_bring_up_recreates_a_running_stack_whose_directory_is_gone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    gone = _inspect("/work/deleted/test", "/work/deleted/test/sigstore/keys")
    assert _bring_up(tmp_path, monkeypatch, gone) == ["down", "up"]


def test_bring_up_leaves_a_current_running_stack_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    keys = tmp_path / "keys"
    keys.mkdir()
    current = _inspect(str(helpers.COMPOSE_FILE.parent), str(keys))
    assert _bring_up(tmp_path, monkeypatch, current) == []


def test_bring_up_leaves_a_stack_alone_when_inspect_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    assert _bring_up(tmp_path, monkeypatch, "", inspect_returncode=1) == []


def test_bring_up_leaves_another_live_worktrees_stack_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    other = tmp_path / "other"
    keys = other / "keys"
    keys.mkdir(parents=True)
    assert _bring_up(tmp_path, monkeypatch, _inspect(str(other), str(keys))) == []
