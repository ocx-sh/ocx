from __future__ import annotations

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
    name = re.sub(r"[^a-z0-9_]", "", request.node.name.lower())[:40]
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
