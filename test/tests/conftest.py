from __future__ import annotations

import dataclasses
import re
from collections.abc import Iterator
from pathlib import Path
from uuid import uuid4

import pytest

from src import registry_guard as registry_guard_mod
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
def published_package(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> PackageInfo:
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


@pytest.fixture()
def registry_guard() -> Iterator[registry_guard_mod.RegistryGuard]:
    """A counting refusal proxy (`src/registry_guard.py`), applied to no home until `deny` is called.

    `registry_guard.deny(runner)` blocks one home's registry traffic from then
    on; `registry_guard.count` is the number of requests it attempted;
    `registry_guard.allow(runner)` lifts the block.
    """
    with registry_guard_mod.running() as guard:
        yield guard


@pytest.fixture()
def second_home(ocx_binary: Path, tmp_path: Path, registry: str) -> OcxRunner:
    """A fresh `OcxRunner` with its own empty `OCX_HOME`, on the same registry as `ocx`."""
    home = tmp_path / "second-home"
    home.mkdir()
    return OcxRunner(ocx_binary, home, registry)


@dataclasses.dataclass
class RegistryPair:
    """One repository path on two registries, holding different content on each."""

    source: OcxRunner  # default registry = `registry`
    target: OcxRunner  # default registry = `target_registry`
    repo: str
    source_pkg: PackageInfo
    target_pkg: PackageInfo


@pytest.fixture()
def registry_pair(
    ocx: OcxRunner,
    ocx_binary: Path,
    tmp_path: Path,
    registry: str,
    target_registry: str,
    unique_repo: str,
) -> RegistryPair:
    """`registry` and `target_registry` both serve `unique_repo:1.0.0` with different content."""
    home = tmp_path / "target-home"
    home.mkdir()
    target = OcxRunner(ocx_binary, home, target_registry)
    target.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{target_registry}"
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{registry},{target_registry}"
    (tmp_path / "src").mkdir()
    (tmp_path / "dst").mkdir()
    source_pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "src", bins=["hello"]
    )
    target_pkg = make_package(
        target, unique_repo, "1.0.0", tmp_path / "dst", bins=["hello", "other"]
    )
    return RegistryPair(ocx, target, unique_repo, source_pkg, target_pkg)
