"""Acceptance tests for ocx-mirror."""
from __future__ import annotations

import http.server
import os
import shutil
import stat
import sys
import tarfile
import threading
from pathlib import Path
from uuid import uuid4

import pytest

from src.mirror_runner import MirrorRunner
from src.runner import OcxRunner, current_platform

FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures" / "mirror" / "test-tool"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_tarball(tmp_path: Path, name: str, marker: str) -> Path:
    """Create a .tar.gz containing a bin/<name> script echoing marker."""
    pkg_dir = tmp_path / f"pkg-{name}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)

    script = bin_dir / name
    if sys.platform == "win32":
        script = script.with_suffix(".bat")
        script.write_text(f"@echo {marker}\n")
    else:
        script.write_text(f"#!/bin/sh\necho {marker}\n")
        script.chmod(script.stat().st_mode | stat.S_IEXEC)

    tarball = tmp_path / f"{name}.tar.gz"
    with tarfile.open(tarball, "w:gz") as tar:
        tar.add(bin_dir, arcname="bin")
    return tarball


def _write_spec_yaml(
    path: Path,
    *,
    name: str,
    registry: str,
    repo: str,
    versions: list[dict],
    metadata_path: str,
    cascade: bool = True,
    skip_prereleases: bool = False,
    versions_config: dict | None = None,
) -> None:
    """Write a mirror spec YAML file.

    versions: list of dicts with keys "version", "assets" (dict), optional "prerelease".
    The YAML source.versions is a map keyed by version string.
    """
    plat = current_platform()
    lines = [
        f"name: {name}",
        "source:",
        "  type: url_index",
        "  versions:",
    ]
    for v in versions:
        ver = v["version"]
        lines.append(f"    \"{ver}\":")
        lines.append("      assets:")
        for asset_name, url in v["assets"].items():
            lines.append(f"        \"{asset_name}\": \"{url}\"")
        if v.get("prerelease"):
            lines.append("      prerelease: true")

    lines += [
        "assets:",
        f"  \"{plat}\":",
        f"    - \"^{name}\\\\.tar\\\\.gz$\"",
        "target:",
        f"  registry: \"{registry}\"",
        f"  repository: \"{repo}\"",
        "metadata:",
        f"  default: \"{metadata_path}\"",
        f"cascade: {str(cascade).lower()}",
        "build_timestamp: none",
    ]
    if skip_prereleases:
        lines.append("skip_prereleases: true")
    if versions_config:
        lines.append("versions:")
        if "min" in versions_config:
            lines.append(f"  min: \"{versions_config['min']}\"")
        if "max" in versions_config:
            lines.append(f"  max: \"{versions_config['max']}\"")
        if "new_per_run" in versions_config:
            lines.append(f"  new_per_run: {versions_config['new_per_run']}")

    path.write_text("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def mirror_binary() -> Path:
    """Path to the compiled ocx-mirror binary."""
    if env_path := os.environ.get("OCX_MIRROR_COMMAND"):
        p = Path(env_path)
    else:
        from src.helpers import PROJECT_ROOT
        p = PROJECT_ROOT / "test" / "bin" / "ocx-mirror"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    assert p.exists(), f"ocx-mirror binary not found at {p}"
    return p


@pytest.fixture()
def mirror(mirror_binary: Path, registry: str, tmp_path: Path) -> MirrorRunner:
    temp_dir = tmp_path / "mirror-work"
    temp_dir.mkdir()
    return MirrorRunner(mirror_binary, registry, temp_dir)


@pytest.fixture()
def unique_mirror_repo(request: pytest.FixtureRequest) -> str:
    """Generate a unique OCI repository name for mirror tests."""
    import re
    short_id = uuid4().hex[:8]
    name = re.sub(r"[^a-z0-9_]", "", request.node.name.lower())[:40]
    return f"m_{short_id}_{name}"


@pytest.fixture()
def asset_server(tmp_path: Path):
    """Start a local HTTP server serving files from tmp_path/assets/."""
    assets_dir = tmp_path / "assets"
    assets_dir.mkdir()

    handler = http.server.SimpleHTTPRequestHandler
    httpd = http.server.HTTPServer(
        ("127.0.0.1", 0),
        lambda *args: handler(*args, directory=str(assets_dir)),
    )
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()

    class Server:
        def __init__(self):
            self.dir = assets_dir
            self.port = port
            self.base_url = f"http://127.0.0.1:{port}"

        def url(self, path: str) -> str:
            return f"{self.base_url}/{path}"

    yield Server()
    httpd.shutdown()


# ---------------------------------------------------------------------------
# Tests: validate command
# ---------------------------------------------------------------------------


def test_validate_valid_spec(mirror: MirrorRunner, tmp_path: Path, registry: str, unique_mirror_repo: str):
    """validate command exits 0 for a valid spec."""
    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[{"version": "1.0.0", "assets": {"test-tool.tar.gz": "https://example.com/test-tool.tar.gz"}}],
        metadata_path=metadata_path,
    )
    result = mirror.run("validate", str(spec_path))
    assert result.returncode == 0


def test_validate_invalid_spec(mirror: MirrorRunner, tmp_path: Path):
    """validate command exits non-zero for an invalid spec."""
    spec_path = tmp_path / "bad-spec.yaml"
    spec_path.write_text("name: test\n")  # Missing required fields
    result = mirror.run("validate", str(spec_path), check=False)
    assert result.returncode != 0


# ---------------------------------------------------------------------------
# Tests: check (dry-run)
# ---------------------------------------------------------------------------


def test_check_shows_would_mirror(
    mirror: MirrorRunner, tmp_path: Path, registry: str,
    unique_mirror_repo: str, asset_server,
):
    """check command lists versions that would be mirrored."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-check")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
            {"version": "2.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
    )

    result = mirror.run("check", str(spec_path), "--work-dir", str(mirror.temp_dir))
    assert "would mirror" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Tests: sync — full lifecycle
# ---------------------------------------------------------------------------


def test_sync_mirrors_versions(
    mirror: MirrorRunner, ocx: OcxRunner, tmp_path: Path,
    registry: str, unique_mirror_repo: str, asset_server,
):
    """sync mirrors upstream versions into the OCI registry."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-sync")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
            {"version": "2.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
    )

    result = mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))
    assert "pushed" in result.stdout.lower() or "mirror complete" in result.stderr.lower()

    # Verify tags exist via ocx
    ocx.plain("index", "update", f"{unique_mirror_repo}:1.0.0")
    data = ocx.json("index", "list", unique_mirror_repo)
    tags = data[unique_mirror_repo]
    assert "1.0.0" in tags
    assert "2.0.0" in tags


def test_sync_idempotent(
    mirror: MirrorRunner, tmp_path: Path,
    registry: str, unique_mirror_repo: str, asset_server,
):
    """Re-running sync with same versions does nothing new."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-idem")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
    )

    # First sync
    mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))

    # Second sync — should find nothing new
    result = mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))
    assert "nothing to mirror" in result.stderr.lower()


# ---------------------------------------------------------------------------
# Tests: sync — cascade
# ---------------------------------------------------------------------------


def test_sync_cascade_creates_rolling_tags(
    mirror: MirrorRunner, ocx: OcxRunner, tmp_path: Path,
    registry: str, unique_mirror_repo: str, asset_server,
):
    """sync with cascade creates rolling tags (1.2, 1, latest)."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-cascade")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.2.3", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
        cascade=True,
    )

    mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))

    # Verify rolling tags
    ocx.plain("index", "update", f"{unique_mirror_repo}:1.2.3")
    data = ocx.json("index", "list", unique_mirror_repo)
    tags = data[unique_mirror_repo]
    for expected in ["1.2.3", "1.2", "1", "latest"]:
        assert expected in tags, f"Expected tag '{expected}' in {tags}"


# ---------------------------------------------------------------------------
# Tests: sync — version filtering
# ---------------------------------------------------------------------------


def test_sync_version_min_filter(
    mirror: MirrorRunner, ocx: OcxRunner, tmp_path: Path,
    registry: str, unique_mirror_repo: str, asset_server,
):
    """sync with min version filter skips versions below threshold."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-min")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
            {"version": "2.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
        versions_config={"min": "1.1.0"},
        cascade=False,
    )

    mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))

    ocx.plain("index", "update", f"{unique_mirror_repo}:2.0.0")
    data = ocx.json("index", "list", unique_mirror_repo)
    tags = data[unique_mirror_repo]
    assert "2.0.0" in tags
    assert "1.0.0" not in tags


def test_sync_new_per_run_cap(
    mirror: MirrorRunner, ocx: OcxRunner, tmp_path: Path,
    registry: str, unique_mirror_repo: str, asset_server,
):
    """sync with new_per_run limits versions mirrored per invocation."""
    tarball = _make_tarball(tmp_path, "test-tool", "marker-cap")
    shutil.copy(tarball, asset_server.dir / "test-tool.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    spec_path = tmp_path / "mirror-test.yaml"
    _write_spec_yaml(
        spec_path,
        name="test-tool",
        registry=registry,
        repo=unique_mirror_repo,
        versions=[
            {"version": "1.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
            {"version": "2.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
            {"version": "3.0.0", "assets": {"test-tool.tar.gz": asset_server.url("test-tool.tar.gz")}},
        ],
        metadata_path=metadata_path,
        versions_config={"new_per_run": 1},
        cascade=False,
    )

    # First run: should mirror only 1 version
    result = mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))
    assert "pushed" in result.stdout.lower()

    # Second run should still have work to do
    result2 = mirror.run("sync", str(spec_path), "--work-dir", str(mirror.temp_dir))
    assert "nothing to mirror" not in result2.stderr.lower()


# ---------------------------------------------------------------------------
# Tests: sync-all
# ---------------------------------------------------------------------------


def test_sync_all_processes_multiple_specs(
    mirror: MirrorRunner, ocx: OcxRunner, tmp_path: Path,
    registry: str, asset_server,
):
    """sync-all processes all */mirror.yml specs in a directory."""
    tarball_a = _make_tarball(tmp_path, "tool-a", "marker-a")
    tarball_b = _make_tarball(tmp_path, "tool-b", "marker-b")
    shutil.copy(tarball_a, asset_server.dir / "tool-a.tar.gz")
    shutil.copy(tarball_b, asset_server.dir / "tool-b.tar.gz")

    metadata_path = str(FIXTURES_DIR / "metadata.json")
    specs_dir = tmp_path / "specs"

    tool_a_dir = specs_dir / "tool-a"
    tool_a_dir.mkdir(parents=True)
    tool_b_dir = specs_dir / "tool-b"
    tool_b_dir.mkdir(parents=True)

    short_id = uuid4().hex[:8]
    repo_a = f"m_{short_id}_toola"
    repo_b = f"m_{short_id}_toolb"

    _write_spec_yaml(
        tool_a_dir / "mirror.yml",
        name="tool-a",
        registry=registry,
        repo=repo_a,
        versions=[{"version": "1.0.0", "assets": {"tool-a.tar.gz": asset_server.url("tool-a.tar.gz")}}],
        metadata_path=metadata_path,
        cascade=False,
    )
    _write_spec_yaml(
        tool_b_dir / "mirror.yml",
        name="tool-b",
        registry=registry,
        repo=repo_b,
        versions=[{"version": "1.0.0", "assets": {"tool-b.tar.gz": asset_server.url("tool-b.tar.gz")}}],
        metadata_path=metadata_path,
        cascade=False,
    )

    mirror.run("sync-all", str(specs_dir), "--work-dir", str(mirror.temp_dir))

    # Verify both repos have tags
    ocx.plain("index", "update", f"{repo_a}:1.0.0")
    data_a = ocx.json("index", "list", repo_a)
    assert "1.0.0" in data_a[repo_a]

    ocx.plain("index", "update", f"{repo_b}:1.0.0")
    data_b = ocx.json("index", "list", repo_b)
    assert "1.0.0" in data_b[repo_b]
