"""Shared fixtures and hooks for all test suites (tests/ and recordings/)."""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT, start_registry
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# CLI options & session hooks
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--no-build",
        action="store_true",
        default=False,
        help="Skip the automatic ``cargo build --release`` step.",
    )


def pytest_sessionstart(session: pytest.Session) -> None:
    """Build ocx and start the registry once before xdist workers spawn."""
    if os.environ.get("PYTEST_XDIST_WORKER") is not None:
        return
    if not session.config.getoption("--no-build"):
        subprocess.run(
            ["cargo", "build", "--release", "-p", "ocx_cli"],
            cwd=PROJECT_ROOT,
            check=True,
        )
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
    if env_path := os.environ.get("OCX"):
        p = Path(env_path)
    else:
        p = PROJECT_ROOT / "target" / "release" / "ocx"
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
