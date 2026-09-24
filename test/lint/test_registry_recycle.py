# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`_recycle_registry` right after a sibling session recycled.

Two sessions see the same full store and both call `_recycle_registry`. The
first takes `_COMPOSE_LOCK`, removes the container, runs `up -d` and releases
the lock. `up -d` returns once the container is started, not once the registry
listens, so the second session, which gets the lock next, probes a registry
that still refuses connections. It reads that as a full store and removes the
fresh registry that the first session's targets are about to push into.

No docker and no registry: `subprocess.run` and the write probe are replaced
by a fake registry that answers only after a few probes following its `up`,
`_COMPOSE_LOCK` points under `tmp_path`, and the two sessions are two calls in
a row — the second is what a sibling queued on the lock runs the moment the
first releases it.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src import helpers


class _FakeRegistry:
    """A registry container that is full until recycled and slow to start."""

    #: Probes a fresh container refuses before it listens.
    STARTUP_PROBES = 3

    def __init__(self) -> None:
        self.removals = 0
        self.refusals_left = 0
        self.up = True
        self.full = True

    def accepts_writes(self, _registry: str) -> bool:
        if not self.up or self.full:
            return False
        if self.refusals_left:
            self.refusals_left -= 1
            return False
        return True

    def run(self, argv: list[str], **_kwargs: object) -> list[str]:
        if "rm" in argv:
            self.removals += 1
            self.up = False
        elif "up" in argv:
            self.up, self.full, self.refusals_left = True, False, self.STARTUP_PROBES
        return argv  # truthy, like the `CompletedProcess` the real one returns


def test_a_recycle_right_after_a_siblings_leaves_the_fresh_registry_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = _FakeRegistry()
    monkeypatch.setattr(helpers, "_COMPOSE_LOCK", tmp_path / "compose.lock")
    monkeypatch.setattr(helpers, "registry_accepts_writes", fake.accepts_writes)
    monkeypatch.setattr(helpers.subprocess, "run", fake.run)
    monkeypatch.setattr(helpers.time, "sleep", lambda _seconds: None)

    helpers._recycle_registry("fake:5000")  # the first session
    helpers._recycle_registry("fake:5000")  # its sibling, next on the lock

    assert fake.removals == 1, (
        f"the full registry was removed {fake.removals} times: the session next on "
        f"the lock probed before the fresh registry listened, took 'not yet listening' "
        f"for 'still full', and removed the registry its sibling had just brought up"
    )
    assert fake.accepts_writes("fake:5000"), "no recycle left a registry that accepts writes"
