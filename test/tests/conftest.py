from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, PackageInfo, current_platform

# ---------------------------------------------------------------------------
# CLI options
# ---------------------------------------------------------------------------

_PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--no-build",
        action="store_true",
        default=False,
        help="Skip the automatic ``cargo build --release`` step.",
    )


def pytest_sessionstart(session: pytest.Session) -> None:
    """Build ocx and start the registry once before xdist workers spawn.

    With ``pytest-xdist``, session-scoped fixtures run once *per worker*.
    Moving the expensive one-time setup here (guarded by the worker env-var)
    ensures it only happens once in the controller process.
    """
    # Inside an xdist worker — skip, controller already did this.
    if os.environ.get("PYTEST_XDIST_WORKER") is not None:
        return

    if not session.config.getoption("--no-build"):
        subprocess.run(
            ["cargo", "build", "--release", "-p", "ocx_cli"],
            cwd=_PROJECT_ROOT,
            check=True,
        )

    registry = os.environ.get("REGISTRY", "localhost:5000")
    _start_registry(registry)


# ---------------------------------------------------------------------------
# Docker-compose helpers
# ---------------------------------------------------------------------------

_COMPOSE_FILE = Path(__file__).resolve().parent.parent / "docker-compose.yml"


def _registry_is_reachable(registry: str) -> bool:
    """Return True if the registry responds to ``GET /v2/``."""
    try:
        urllib.request.urlopen(f"http://{registry}/v2/", timeout=2)
        return True
    except (urllib.error.URLError, OSError):
        return False


def _start_registry(registry: str) -> None:
    """Start the registry via docker-compose if it is not already running."""
    if _registry_is_reachable(registry):
        return

    subprocess.run(
        ["docker", "compose", "-f", str(_COMPOSE_FILE), "up", "-d"],
        check=True,
        capture_output=True,
    )

    # Wait for the registry to become reachable (up to 15 s).
    for _ in range(30):
        if _registry_is_reachable(registry):
            return
        time.sleep(0.5)

    raise RuntimeError(f"Registry at {registry} did not become reachable")


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def registry() -> str:
    """Return the registry address and ensure it is running.

    The heavy lifting (docker-compose up) is done in ``pytest_sessionstart``
    so that xdist workers don't duplicate it.  This fixture just resolves the
    address and double-checks reachability.
    """
    addr = os.environ.get("REGISTRY", "localhost:5000")
    _start_registry(addr)  # idempotent — no-op if already running
    return addr


@pytest.fixture(scope="session")
def ocx_binary() -> Path:
    """Resolve the ocx binary path.

    The actual ``cargo build`` is done in ``pytest_sessionstart`` so that
    xdist workers don't each trigger their own build.
    Resolution order: ``$OCX`` env var, then ``target/release/ocx``.
    """
    if env_path := os.environ.get("OCX"):
        p = Path(env_path)
    else:
        p = _PROJECT_ROOT / "target" / "release" / "ocx"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")

    assert p.exists(), f"ocx binary not found at {p}"
    return p


# ---------------------------------------------------------------------------
# Function-scoped fixtures (per-test isolation)
# ---------------------------------------------------------------------------


@pytest.fixture()
def ocx_home(tmp_path: Path) -> Path:
    """Create a fresh, isolated OCX_HOME for this test."""
    home = tmp_path / "ocx-home"
    home.mkdir()
    return home


@pytest.fixture()
def ocx(ocx_binary: Path, ocx_home: Path, registry: str) -> OcxRunner:
    """Return an :class:`OcxRunner` wired to an isolated home and the test registry."""
    return OcxRunner(ocx_binary, ocx_home, registry)


@pytest.fixture()
def unique_repo(request: pytest.FixtureRequest) -> str:
    """Generate a unique OCI repository name for this test.

    The name includes a short UUID and the (sanitised) test-function name so
    that parallel test runs never collide on the shared registry.
    """
    short_id = uuid4().hex[:8]
    name = re.sub(r"[^a-z0-9_]", "", request.node.name.lower())[:40]
    return f"t_{short_id}_{name}"


# ---------------------------------------------------------------------------
# Package publishing helpers & fixtures
# ---------------------------------------------------------------------------


def _make_package(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    *,
    new: bool = True,
) -> PackageInfo:
    """Create, bundle, push, and index a test package."""
    plat = current_platform()
    marker = f"marker-{uuid4().hex[:12]}"

    # Build content
    pkg_dir = tmp_path / f"pkg-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    hello = bin_dir / "hello"
    if sys.platform == "win32":
        hello = hello.with_suffix(".bat")
        hello.write_text(f"@echo {marker}\n")
    else:
        hello.write_text(f"#!/bin/sh\necho {marker}\n")
        hello.chmod(hello.stat().st_mode | stat.S_IEXEC)

    # Write metadata
    metadata = tmp_path / f"metadata-{tag}.json"
    metadata.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                    },
                    {
                        "key": "HELLO_HOME",
                        "type": "constant",
                        "value": "${installPath}",
                    },
                ],
            }
        )
    )

    # Create bundle
    bundle = tmp_path / f"bundle-{tag}.tar.xz"
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata),
        "-o",
        str(bundle),
        str(pkg_dir),
    )

    # Push
    fq = f"{ocx.registry}/{repo}:{tag}"
    push_args = ["package", "push", "-p", plat, "-m", str(metadata)]
    if new:
        push_args.append("-n")
    push_args += [fq, str(bundle)]
    ocx.plain(*push_args)

    # Update local index so install/find can discover the package
    short = f"{repo}:{tag}"
    ocx.plain("index", "update", short)

    return PackageInfo(
        repo=repo,
        tag=tag,
        short=short,
        fq=fq,
        content_dir=pkg_dir,
        marker=marker,
        platform=plat,
    )


@pytest.fixture()
def published_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> PackageInfo:
    """Push a single test package (v1.0.0) and return its metadata."""
    return _make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)



@pytest.fixture()
def published_two_versions(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """Push two versions of a test package and return ``(v1, v2)``."""
    v1 = _make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    v2 = _make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False)
    return v1, v2
