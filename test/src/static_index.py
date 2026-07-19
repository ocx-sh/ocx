"""Local static-file HTTP fixture encoding the `index.ocx.sh` wire shapes.

Ground truth for the wire shapes: `IndexRoot`, `RootTag`, `Observation`,
`ObservationPlatform`, `CatalogIndex` in `crates/ocx_lib/src/oci/index/wire.rs`,
and `IndexFormatConfig` in `crates/ocx_lib/src/oci/index/ocx_index.rs`. Only the
``●`` frozen shapes in `.claude/artifacts/adr_index_indirection.md` (Decision F,
Data Model) are served here:

    config.json                {"format_version": 1}
    c/index.json                {"<repository>": "sha256:<root-digest>", ...}
    p/<repository>.json         root: repository, tags{tag: {content, ...}}, status...
    p/<repository>/o/sha256/<hex>.json   observation: {"platforms": [{"platform": {...}, "digest": ...}]}

`repository` is the identifier's full `<ns>/<pkg>` path (may nest further).
"""

from __future__ import annotations

import contextlib
import dataclasses
import functools
import hashlib
import http.server
import json
import socket
import threading
import time
from collections.abc import Iterator
from pathlib import Path


# ---------------------------------------------------------------------------
# Wire-shape builders
# ---------------------------------------------------------------------------


def write_config(fixture_root: Path, *, format_version: int = 1) -> None:
    """Writes `config.json` (● `{"format_version": N}`)."""
    (fixture_root / "config.json").write_text(json.dumps({"format_version": format_version}))


def observation_bytes(platform_digest: str, *, os: str = "linux", architecture: str = "amd64") -> bytes:
    """Minified, sorted-key observation-object bytes (`platforms[].digest` is
    the platform-MANIFEST digest, never an image-index digest).
    """
    obs = {"platforms": [{"platform": {"architecture": architecture, "os": os}, "digest": platform_digest}]}
    return json.dumps(obs, sort_keys=True, separators=(",", ":")).encode()


@dataclasses.dataclass(slots=True)
class PackageEntry:
    """A published `p/<repository>` fixture entry."""

    repository: str
    tag: str
    root_digest: str
    obs_digest: str
    logical_id: str


def write_package(
    fixture_root: Path,
    *,
    repository: str,
    tag: str,
    physical_repository: str,
    platform_digest: str,
    os: str = "linux",
    architecture: str = "amd64",
    status: str | None = None,
    deprecated_message: str | None = None,
    superseded_by: str | None = None,
    yanked: bool = False,
) -> PackageEntry:
    """Writes a root document + its observation object under `repository`.

    Returns the entry's digests and its `ocx.sh/<repository>:<tag>` logical
    identifier for use in `ocx` invocations.
    """
    obs_bytes = observation_bytes(platform_digest, os=os, architecture=architecture)
    obs_hex = hashlib.sha256(obs_bytes).hexdigest()

    tag_entry: dict = {"content": f"sha256:{obs_hex}", "observed": "2026-01-01T00:00:00Z"}
    if yanked:
        tag_entry["yanked"] = True
    root: dict = {"repository": physical_repository, "tags": {tag: tag_entry}}
    if status is not None:
        root["status"] = status
    if deprecated_message is not None:
        root["deprecated_message"] = deprecated_message
    if superseded_by is not None:
        root["superseded_by"] = superseded_by
    root_bytes = json.dumps(root, sort_keys=True, separators=(",", ":")).encode()
    root_hex = hashlib.sha256(root_bytes).hexdigest()

    root_path = fixture_root / "p" / f"{repository}.json"
    root_path.parent.mkdir(parents=True, exist_ok=True)
    root_path.write_bytes(root_bytes)

    obs_path = fixture_root / "p" / repository / "o" / "sha256" / f"{obs_hex}.json"
    obs_path.parent.mkdir(parents=True, exist_ok=True)
    obs_path.write_bytes(obs_bytes)

    return PackageEntry(
        repository=repository,
        tag=tag,
        root_digest=f"sha256:{root_hex}",
        obs_digest=f"sha256:{obs_hex}",
        logical_id=f"ocx.sh/{repository}:{tag}",
    )


def write_catalog(fixture_root: Path, entries: dict[str, str]) -> None:
    """Writes `c/index.json` (● `{"<repository>": "sha256:<root-digest>"}`)."""
    path = fixture_root / "c" / "index.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(entries, sort_keys=True, separators=(",", ":")))


# ---------------------------------------------------------------------------
# HTTP server: conditional GET (F2) + a per-request log for assertions
# ---------------------------------------------------------------------------


@dataclasses.dataclass(slots=True)
class RequestRecord:
    """One served request: the raw path, the `If-None-Match` it carried, and
    the status this fixture answered with.
    """

    path: str
    if_none_match: str | None
    status: int


class _StaticIndexHandler(http.server.SimpleHTTPRequestHandler):
    """Serves `directory` verbatim; every file's ETag is `sha256(bytes)`; a
    matching `If-None-Match` answers 304 (mirrors a CDN's conditional GET,
    the mechanism `c/index.json` catalog sync (F2) relies on).
    """

    server: StaticIndexServer  # narrows the inherited Any-typed attribute

    def do_GET(self) -> None:  # noqa: N802 (stdlib override name)
        if_none_match = self.headers.get("If-None-Match")
        local_path = Path(self.translate_path(self.path))
        if not local_path.is_file():
            self.server.requests.append(RequestRecord(self.path, if_none_match, 404))
            self.send_error(404)
            return

        body = local_path.read_bytes()
        etag = hashlib.sha256(body).hexdigest()
        if if_none_match == etag:
            self.server.requests.append(RequestRecord(self.path, if_none_match, 304))
            self.send_response(304)
            self.send_header("ETag", etag)
            self.end_headers()
            return

        self.server.requests.append(RequestRecord(self.path, if_none_match, 200))
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("ETag", etag)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 (stdlib signature)
        pass  # quiet test output — assertions read `server.requests` instead


class StaticIndexServer(http.server.ThreadingHTTPServer):
    """A session-free `index.ocx.sh`-shaped fixture: one instance per test,
    bound to an ephemeral port, serving `root` verbatim.
    """

    def __init__(self, root: Path) -> None:
        self.root = root
        self.requests: list[RequestRecord] = []
        handler = functools.partial(_StaticIndexHandler, directory=str(root))
        super().__init__(("127.0.0.1", 0), handler)

    @property
    def base_url(self) -> str:
        host, port = self.server_address[:2]
        return f"http://{host}:{port}"

    @property
    def host(self) -> str:
        """`host:port` form for `OCX_INSECURE_REGISTRIES`."""
        host, port = self.server_address[:2]
        return f"{host}:{port}"


@contextlib.contextmanager
def running(root: Path) -> Iterator[StaticIndexServer]:
    """Starts a `StaticIndexServer` on a background thread, waits for it to
    accept connections (mirroring the registry:2 readiness wait in
    `test/conftest.py::start_registry`), and tears it down on exit.
    """
    server = StaticIndexServer(root)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                with socket.create_connection(server.server_address, timeout=0.2):
                    break
            except OSError:
                time.sleep(0.05)
        else:
            raise RuntimeError("static index fixture server did not become reachable")
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
