from __future__ import annotations

import re
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.otlp_fixture import serve_collector
from src.runner import OcxRunner, PackageInfo

# Re-export the real Sigstore stack fixtures (`sigstore` compose profile) so the
# signing modules can consume them by name without importing directly.
from tests.fixtures.sigstore_stack import (  # noqa: F401
    SigstoreStack,
    identity_token,
    sigstore_stack,
)


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
def otlp_collector(request: pytest.FixtureRequest):
    """A capture OTLP/HTTP logs collector on a loopback port (`src/otlp_fixture.py`).

    Starts in mode `ok`; set `otlp_collector.mode` at any time, or choose it up
    front with `@pytest.mark.parametrize("otlp_collector", ["status=503"],
    indirect=True)`. Teardown wakes every blocked handler and joins the threads,
    then fails the test if a `/v1/logs` body did not decode as protobuf (set
    `collector.tolerate_decode_failures = True` to send garbage on purpose).
    """
    with serve_collector(getattr(request, "param", "ok")) as collector:
        yield collector
    collector.assert_no_decode_failures()


@pytest.fixture()
def published_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> PackageInfo:
    """Push a single test package (v1.0.0) and return its metadata."""
    return make_package(ocx, unique_repo, "1.0.0", tmp_path)


@pytest.fixture()
def published_two_versions(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """Push two versions of a test package and return ``(v1, v2)``."""
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path)
    return v1, v2
