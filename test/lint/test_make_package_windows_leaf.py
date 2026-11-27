"""`make_package` writes a `.bat`/`.exe` leaf only for a Windows target, whatever the host."""

import pytest

from src.helpers import _writes_windows_binary


@pytest.mark.parametrize("plat", ["wasip1/wasm", "linux/amd64", "darwin/arm64"])
def test_a_windows_host_keeps_the_leaf_name_of_a_foreign_target(plat: str):
    assert not _writes_windows_binary(plat, host="win32")


def test_a_windows_host_writes_a_windows_leaf_for_a_windows_target():
    assert _writes_windows_binary("windows/amd64", host="win32")


def test_a_unix_host_never_does():
    assert not _writes_windows_binary("windows/amd64", host="linux")
