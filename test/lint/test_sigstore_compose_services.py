# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The sigstore bring-up starts only the services the profile adds.

`--profile sigstore up -d` with no service names also starts every service
without a profile — the compose `registry`, `target-registry` and
`prod-registry`. Under native zot those ports are already taken, so the bare
`up` fails with "address already in use" and every signing test errors.

No docker: `subprocess.run` is a fake compose that knows which services carry
the profile and reports nothing running.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src import helpers

_DEFAULT = ["registry", "mirror-registry", "target-registry", "prod-registry", "ctdata"]
_PROFILE = ["dex", "fulcio", "rekor"]


class _FakeCompose:
    def __init__(self) -> None:
        self.ups: list[list[str]] = []

    def run(self, argv: list[str], **_kwargs: object) -> helpers.subprocess.CompletedProcess[str]:
        stdout = ""
        if "config" in argv:
            stdout = "\n".join(_DEFAULT + (_PROFILE if "--profile" in argv else []))
        elif "up" in argv:
            self.ups.append(argv)
        return helpers.subprocess.CompletedProcess(argv, 0, stdout, "")


def test_the_sigstore_up_names_only_the_profile_services(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = _FakeCompose()
    monkeypatch.setattr(helpers, "_COMPOSE_LOCK", tmp_path / "compose.lock")
    monkeypatch.setattr(helpers.subprocess, "run", fake.run)

    helpers._bring_up_serialized()

    assert len(fake.ups) == 1, f"expected one `up`, got {fake.ups}"
    started = fake.ups[0][fake.ups[0].index("-d") + 1 :]
    assert sorted(started) == sorted(_PROFILE), (
        f"`up -d` started {started or 'every service'}; a profile-less service "
        f"there takes a port the native zot processes already hold"
    )
