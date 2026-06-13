from __future__ import annotations

import dataclasses
import re
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo


@pytest.fixture()
def unique_repo(request: pytest.FixtureRequest) -> str:
    """Generate a unique OCI repository name for this test."""
    short_id = uuid4().hex[:8]
    # Truncating to 40 chars can leave a trailing `_`, which makes the resulting
    # repo name (`t_{8}_..._`) violate the OCI distribution spec component
    # regex (`[a-z0-9]+(...)*`). registry:2 then rejects pushes with a 404.
    # Strip trailing underscores so any test name maps to a valid repo.
    name = re.sub(r"[^a-z0-9_]", "", request.node.name.lower())[:40].rstrip("_")
    return f"t_{short_id}_{name}"


@pytest.fixture()
def published_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> PackageInfo:
    """Push a single test package (v1.0.0) and return its metadata."""
    return make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)


@pytest.fixture()
def published_two_versions(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """Push two versions of a test package and return ``(v1, v2)``."""
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path, new=False)
    return v1, v2


# ---------------------------------------------------------------------------
# Shared-store fixtures (P1.3)
# ---------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True, slots=True)
class SharedStore:
    """Two OcxRunner instances sharing one OCX_CACHE_DIR with distinct OCX_STATE_DIRs.

    Simulates the UC1 fleet scenario: a single shared content store (blobs,
    layers, packages) on one volume and per-instance state (symlinks,
    pins, project ledger) on separate paths.

    Fields
    ------
    shared_cache:
        The common directory used as OCX_CACHE_DIR for both runners.
    runner_a:
        First OcxRunner — has its own OCX_STATE_DIR and OCX_HOME.
    runner_b:
        Second OcxRunner — has its own OCX_STATE_DIR and OCX_HOME.
    """

    shared_cache: Path
    runner_a: OcxRunner
    runner_b: OcxRunner


@pytest.fixture()
def shared_store(ocx_binary: "Path", registry: str, tmp_path: Path) -> SharedStore:
    """Two runners sharing a single OCX_CACHE_DIR with distinct OCX_STATE_DIRs.

    System design §5 M2 UC1: N containers share the content store on one
    volume; each keeps its own install state.  This fixture creates the
    minimum two-instance configuration needed to exercise that contract.

    Both runners receive ``OCX_INSECURE_REGISTRIES`` and
    ``OCX_DEFAULT_REGISTRY`` from ``registry`` so they push/pull from the
    same test registry:2 instance.
    """
    shared_cache = tmp_path / "shared-cache"
    shared_cache.mkdir()

    home_a = tmp_path / "home-a"
    home_a.mkdir()
    state_a = tmp_path / "state-a"
    state_a.mkdir()

    home_b = tmp_path / "home-b"
    home_b.mkdir()
    state_b = tmp_path / "state-b"
    state_b.mkdir()

    runner_a = OcxRunner(
        ocx_binary,
        home_a,
        registry,
        extra_env={
            "OCX_CACHE_DIR": str(shared_cache),
            "OCX_STATE_DIR": str(state_a),
        },
    )
    runner_b = OcxRunner(
        ocx_binary,
        home_b,
        registry,
        extra_env={
            "OCX_CACHE_DIR": str(shared_cache),
            "OCX_STATE_DIR": str(state_b),
        },
    )

    return SharedStore(
        shared_cache=shared_cache,
        runner_a=runner_a,
        runner_b=runner_b,
    )
