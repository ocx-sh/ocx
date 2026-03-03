"""Shared fixtures and hooks for all test suites (tests/ and recordings/)."""
from __future__ import annotations

import os
import sys
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
