"""Shared fixtures and hooks for all test suites (tests/ and recordings/)."""
from __future__ import annotations

import dataclasses
import os
import stat
import sys
import textwrap
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT, start_registry
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Session hooks
# ---------------------------------------------------------------------------


def pytest_sessionstart(session: pytest.Session) -> None:
    """Start the registry once before xdist workers spawn."""
    if os.environ.get("PYTEST_XDIST_WORKER") is not None:
        return
    registry = os.environ.get("REGISTRY", "localhost:5000")
    start_registry(registry)


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def registry() -> str:
    addr = os.environ.get("REGISTRY", "localhost:5000")
    start_registry(addr)
    return addr


@pytest.fixture(scope="session")
def ocx_binary() -> Path:
    if env_path := os.environ.get("OCX_COMMAND"):
        p = Path(env_path)
    else:
        p = PROJECT_ROOT / "test" / "bin" / "ocx"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    assert p.exists(), f"ocx binary not found at {p}"
    return p


# ---------------------------------------------------------------------------
# Function-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def ocx_home(tmp_path: Path) -> Path:
    home = tmp_path / "ocx-home"
    home.mkdir()
    return home


@pytest.fixture()
def ocx(ocx_binary: Path, ocx_home: Path, registry: str) -> OcxRunner:
    return OcxRunner(ocx_binary, ocx_home, registry)


# ---------------------------------------------------------------------------
# Mock docker credential helper
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class MockHelper:
    """A `docker-credential-test` shell-script helper installed under a tempdir.

    Tests prepend ``dir`` to the subprocess PATH and configure ``credsStore`` or
    ``credHelpers`` to point at the suffix ``test``. The helper's stdin is
    captured in the sidecar file.
    """

    path: Path
    dir: Path
    sidecar: Path
    docker_config_dir: Path


@pytest.fixture()
def mock_credential_helper(tmp_path: Path) -> MockHelper:
    """A mock ``docker-credential-test`` helper with parameterizable behavior.

    Default behavior persists stdin to a sidecar JSON file and responds to
    ``get`` by reading the same file. Tests parameterize behavior by editing
    the script body — see ``MockHelper.path``.
    """
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    bin_path = helper_dir / "docker-credential-test"

    # Default: persistent map keyed by server URL in the sidecar.
    script = textwrap.dedent(
        f"""\
        #!/bin/sh
        # Mock docker credential helper for OCX acceptance tests.
        # Default behavior: persist stdin to {sidecar}; emit on get.
        action="$1"
        sidecar="{sidecar}"
        input=$(cat)
        case "$action" in
            store)
                # JSON input on stdin.
                printf '%s' "$input" > "$sidecar"
                ;;
            get)
                if [ -f "$sidecar" ]; then
                    cat "$sidecar"
                else
                    echo 'credentials not found in native keychain'
                    exit 1
                fi
                ;;
            erase)
                rm -f "$sidecar"
                ;;
            list)
                echo '{{}}'
                ;;
            *)
                echo "unknown action: $action" >&2
                exit 2
                ;;
        esac
        """
    )
    bin_path.write_text(script)
    bin_path.chmod(bin_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()

    return MockHelper(
        path=bin_path,
        dir=helper_dir,
        sidecar=sidecar,
        docker_config_dir=docker_config_dir,
    )
