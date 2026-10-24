"""`make_package` passes `--no-bin-scan` on a host that cannot scan only for an env `create` would scan.

`ocx_package`'s `bin_scan.rs` refuses an absent `binaries` claim on such a host once the scan scope
is non-empty; a wider flag keeps the proceed path out of every Windows acceptance run.
"""

import pytest

from src.helpers import _scans_a_directory


def _path(value: str, visibility: str = "public", key: str = "PATH", kind: str = "path") -> dict:
    return {"key": key, "type": kind, "value": value, "visibility": visibility}


@pytest.mark.parametrize(
    "env",
    [
        [_path("${installPath}/bin")],
        [_path("${installPath}/bin", visibility="interface")],
        [_path("${self.installPath}/bin")],
        [_path("${self.installPath}/bin", visibility="interface")],
        [_path("${installPath}/")],
        [{"key": "A", "type": "constant", "value": "x"}, _path("${installPath}/tools/bin")],
    ],
)
def test_an_interface_path_rooted_at_install_path_is_scanned(env: list[dict]):
    assert _scans_a_directory(env)


@pytest.mark.parametrize(
    "env",
    [
        [],
        [{"key": "V", "type": "constant", "value": "x", "visibility": "interface"}],
        [_path("${installPath}/bin", visibility="private")],
        [{"key": "PATH", "type": "path", "value": "${installPath}/bin"}],
        [_path("${installPath}/bin", kind="constant")],
        [_path("${installPath}/bin", key="Path")],
        [_path("${installPath}")],
        [_path("${self.installPath}")],
        [_path("${installPath}/../etc")],
        [_path("${self.installPath}/../etc")],
        [_path("${installPath}/bin:${deps.dep.installPath}/bin")],
        [_path("${self.installPath}/bin:${deps.dep.installPath}/bin")],
        [_path("${self.installPath}/bin", visibility="private")],
        [_path("/usr/bin")],
    ],
)
def test_an_env_with_no_scannable_directory_is_not(env: list[dict]):
    assert not _scans_a_directory(env)
