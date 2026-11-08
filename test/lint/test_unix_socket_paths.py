"""AF_UNIX socket paths in the suite must stay under sun_path (107 bytes) at any tmp depth."""

from __future__ import annotations

import os
import re
from pathlib import Path

from src.unix_socket import SUN_PATH_MAX, short_socket

_TESTS = Path(__file__).resolve().parent.parent / "tests"
# The Bazel runner's basetemp plus the test's own nesting, measured from a failing run.
_BAZEL_DEEP = Path("/home/user/.cache/ocx-pytest/checkout-3846400384-bazel/test_exec_spawned_shell/test_a_tmux_pane0/w")


def test_short_socket_fits_beneath_a_bazel_deep_tmp_path(monkeypatch) -> None:
    monkeypatch.setenv("TMPDIR", str(_BAZEL_DEEP))
    assert len(os.fsencode(_BAZEL_DEEP / "tmux-0123abcd.sock")) > SUN_PATH_MAX
    with short_socket("tmux-0123abcd.sock") as path:
        assert len(os.fsencode(path)) <= SUN_PATH_MAX
        assert path.parent.is_dir()
    assert not path.parent.exists()


def test_tmux_sockets_are_not_placed_under_the_test_tmp_dir() -> None:
    offenders = [
        f"{module.name}:{number}"
        for module in sorted(_TESTS.glob("test_*.py"))
        for number, line in enumerate(module.read_text(encoding="utf-8").splitlines(), 1)
        if re.search(r"\.root\s*/\s*f?[\"'][^\"']*\.sock|tmp_path\s*/\s*f?[\"'][^\"']*\.sock", line)
    ]
    assert not offenders, f"socket built under the tmp dir (use src.unix_socket.short_socket): {offenders}"
