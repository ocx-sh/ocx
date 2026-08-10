"""Local static-file HTTP fixture encoding the `index.ocx.sh` wire shapes.

Ground truth for the wire shapes: `IndexRoot`, `RootTag`, `CatalogIndex` in
`crates/ocx_lib/src/oci/index/wire.rs`, and `IndexFormatConfig` in
`crates/ocx_lib/src/oci/index/ocx_index.rs`. Only the ``●`` frozen shapes in
`.claude/artifacts/adr_oci_index_only_dispatch.md` (Decision D1) are served
here:

    config.json                {"format_version": 1}
    c/index.json                {"format_version": 1, "packages": {"<repository>": "sha256:<root-digest>", ...}}
    p/<repository>.json         root: repository, tags{tag: {content, ...}}, status...
    p/<repository>/o/sha256/<hex>.json   a real OCI image index, verbatim
                                          ({"schemaVersion": 2, "mediaType":
                                          "application/vnd.oci.image.index.v1+json",
                                          "manifests": [...]})

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


def write_config(
    fixture_root: Path, *, format_version: int = 1, name_segments: int | None = None
) -> None:
    """Writes `config.json` (● `{"format_version": N}`).

    `name_segments` is the index's own declaration of its name grammar — the
    slash-separated segment count a package name must have *within* the
    namespace. `index.ocx.sh` serves ``2``. Omitted (the default) means the
    index declares no constraint and can express every name.
    """
    config: dict[str, object] = {"format_version": format_version}
    if name_segments is not None:
        config["name_segments"] = name_segments
    (fixture_root / "config.json").write_text(json.dumps(config))


def index_bytes(platform_digest: str, *, os: str = "linux", architecture: str = "amd64") -> bytes:
    """Minified, sorted-key bytes for a real OCI image index — the same shape
    a registry serves for a tag, e.g. `{"schemaVersion":2,"mediaType":
    "application/vnd.oci.image.index.v1+json","manifests":[...]}`.
    `manifests[].digest` is the platform-MANIFEST digest, never a second
    image-index digest.
    """
    index = {
        "manifests": [
            {
                "digest": platform_digest,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "platform": {"architecture": architecture, "os": os},
                "size": 314,  # placeholder: fixture only has the digest, not bytes to size
            }
        ],
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "schemaVersion": 2,
    }
    return json.dumps(index, sort_keys=True, separators=(",", ":")).encode()


@dataclasses.dataclass(slots=True)
class PackageEntry:
    """A published `p/<repository>` fixture entry."""

    repository: str
    tag: str
    root_digest: str
    index_digest: str
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
    """Writes a root document + its OCI image index under `repository`.

    Returns the entry's digests and its `ocx.sh/<repository>:<tag>` logical
    identifier for use in `ocx` invocations.
    """
    index_body = index_bytes(platform_digest, os=os, architecture=architecture)
    index_hex = hashlib.sha256(index_body).hexdigest()

    tag_entry: dict = {"content": f"sha256:{index_hex}", "observed": "2026-01-01T00:00:00Z"}
    if yanked:
        # Wire shape is an object (`RootTag::yanked` is `Option<YankMarker>` in
        # `wire.rs`), never a bare boolean — a publisher's reason + timestamp.
        tag_entry["yanked"] = {"reason": "critical security issue", "at": "2026-02-01T00:00:00Z"}
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

    index_path = fixture_root / "p" / repository / "o" / "sha256" / f"{index_hex}.json"
    index_path.parent.mkdir(parents=True, exist_ok=True)
    index_path.write_bytes(index_body)

    return PackageEntry(
        repository=repository,
        tag=tag,
        root_digest=f"sha256:{root_hex}",
        index_digest=f"sha256:{index_hex}",
        logical_id=f"ocx.sh/{repository}:{tag}",
    )


def write_catalog(fixture_root: Path, entries: dict[str, str], *, format_version: int = 1) -> None:
    """Writes `c/index.json` (● `{"format_version": N, "packages": {...}}`).

    The envelope is the served wire — `index.ocx.sh` emits the version pin
    alongside the listing, the same shape `config.json` carries.
    """
    path = fixture_root / "c" / "index.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    document = {"format_version": format_version, "packages": entries}
    path.write_text(json.dumps(document, sort_keys=True, separators=(",", ":")))


def read_catalog(path: Path) -> dict[str, str]:
    """Reads a persisted `c/index.json` and returns its `packages` map.

    Used by tests asserting on OCX's own local catalog: the local copy carries
    the same envelope the site serves, so tests must unwrap it rather than
    indexing the document directly.
    """
    return json.loads(path.read_text())["packages"]


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
    matching `If-None-Match` answers 304.

    A real CDN does conditional GET, so this fixture does too — deliberately,
    even though ocx never sends `If-None-Match` any more: it is what makes
    `RequestRecord.if_none_match` a falsifiable assertion surface rather than a
    field nothing could ever populate. The catalog sync (F2) is a digest diff
    against the local `c/index.json`, not a validator round trip.
    """

    server: StaticIndexServer  # narrows the inherited Any-typed attribute

    def do_GET(self) -> None:  # noqa: N802 (stdlib override name)
        with self.server.in_flight():
            self._serve()

    def _serve(self) -> None:
        if_none_match = self.headers.get("If-None-Match")
        # A path the test declared unservable — an origin whose catalog endpoint
        # demands credentials, or any non-404 refusal. Distinct from an absent
        # file on purpose: only a confirmed 404 may read as absence, and the
        # difference between the two is what several scenarios here turn on.
        if (status := self.server.refusals.get(self.path)) is not None:
            self.server.requests.append(RequestRecord(self.path, if_none_match, status))
            self.send_error(status)
            return

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


class _InFlight:
    """A concurrency counter and its high-water mark, shareable across servers."""

    def __init__(self) -> None:
        self.peak = 0
        self.current = 0
        self.lock = threading.Lock()


class StaticIndexServer(http.server.ThreadingHTTPServer):
    """A session-free `index.ocx.sh`-shaped fixture: one instance per test,
    bound to an ephemeral port, serving `root` verbatim.
    """

    def __init__(self, root: Path) -> None:
        self.root = root
        self.requests: list[RequestRecord] = []
        # Path -> HTTP status this fixture answers instead of serving the file.
        # Populated by tests that need an origin which refuses rather than 404s.
        self.refusals: dict[str, int] = {}
        # Seconds each request is held inside the handler. A static file is
        # served far faster than a client can fan out, so with no hold the peak
        # overlap is 1 however wide the fan-out is — the delay is what makes
        # concurrency observable at all rather than an artefact of scheduling.
        self.hold_seconds = 0.0
        self._counter = _InFlight()
        handler = functools.partial(_StaticIndexHandler, directory=str(root))
        super().__init__(("127.0.0.1", 0), handler)

    @property
    def peak_in_flight(self) -> int:
        """The high-water mark of concurrent requests, over every server sharing
        this counter."""
        return self._counter.peak

    @peak_in_flight.setter
    def peak_in_flight(self, value: int) -> None:
        self._counter.peak = value

    def share_in_flight_with(self, other: "StaticIndexServer") -> None:
        """Make `other` count into THIS server's high-water mark.

        A bound that spans two origins has to be measured as one number. Summing
        each server's own peak overstates it — the two maxima need not have
        occurred at the same instant, so a client holding a single 8-slot budget
        can show 8 on each and sum to 16 while never exceeding 8 in total.
        """
        other._counter = self._counter

    @contextlib.contextmanager
    def in_flight(self) -> Iterator[None]:
        """Counts one request as in flight for the duration of its handler, and
        records the high-water mark in `peak_in_flight`.
        """
        counter = self._counter
        with counter.lock:
            counter.current += 1
            counter.peak = max(counter.peak, counter.current)
        try:
            if self.hold_seconds:
                time.sleep(self.hold_seconds)
            yield
        finally:
            with counter.lock:
                counter.current -= 1

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
