"""Helpers for tests that damage or inspect an export directory or blob cache.

Layout (system design § 8): `DIR/ocx-export.json`, `DIR/index/<slug>/...`,
`DIR/blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/{data,digest}`.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any

MARKER = "ocx-export.json"


def blob_data_files(directory: Path) -> list[Path]:
    """Every `data` file under `directory/blobs`, sorted; a bare cache dir passes `directory` as its own `blobs`."""
    root = directory / "blobs" if (directory / "blobs").is_dir() else directory
    return sorted(root.glob("*/*/??/*/data"))


def blob_path(directory: Path, registry_slug: str, digest: str) -> Path:
    """The `data` file of `digest` (`sha256:<hex>`) for `registry_slug`."""
    algorithm, hex_digest = digest.split(":", 1)
    root = directory / "blobs" if (directory / "blobs").is_dir() else directory
    return root / registry_slug / algorithm / hex_digest[:2] / hex_digest[2:32] / "data"


def flip_byte(path: Path, offset: int = 0) -> None:
    """Invert one byte of `path` in place, keeping its length."""
    data = bytearray(path.read_bytes())
    data[offset] ^= 0xFF
    path.write_bytes(bytes(data))


def truncate_blob(path: Path, keep: int | None = None) -> None:
    """Cut `path` to `keep` bytes (default: half, at least one byte short of whole)."""
    data = path.read_bytes()
    path.write_bytes(data[: len(data) // 2 if keep is None else keep])


def delete_blob(path: Path) -> None:
    """Remove a blob's whole `<30hex>` entry (`data`, `digest`, referrers) from the directory."""
    entry = path.parent
    for child in sorted(entry.rglob("*"), reverse=True):
        child.rmdir() if child.is_dir() else child.unlink()
    entry.rmdir()


def index_roots(directory: Path) -> list[Path]:
    """Every package root `DIR/index/<slug>/p/<ns>/<pkg>.json`, sorted (dispatch objects sit deeper, under `o/`)."""
    return sorted((directory / "index").glob("*/p/*/*.json"))


def corrupt_index_root(directory: Path, which: int = 0) -> Path:
    """Overwrite one package root with bytes that are not JSON and return its path."""
    roots = index_roots(directory)
    target = roots[which]
    target.write_text("{ not json", encoding="utf-8")
    return target


def read_marker(directory: Path) -> dict[str, Any]:
    """Parse `DIR/ocx-export.json`."""
    return json.loads((directory / MARKER).read_text(encoding="utf-8"))


def run_together[T](calls: Sequence[Callable[[], T]], timeout: float = 30.0) -> list[T]:
    """Run each zero-argument `call` on its own thread, released together by a barrier.

    Every call blocks until all have started, so the processes they spawn overlap
    instead of running in thread-start order. Results come back in `calls` order;
    the first exception raised by a call is re-raised after all have finished.
    """
    barrier = threading.Barrier(len(calls), timeout=timeout)
    results: list[Any] = [None] * len(calls)
    errors: list[BaseException | None] = [None] * len(calls)

    def worker(index: int) -> None:
        try:
            barrier.wait()
            results[index] = calls[index]()
        except BaseException as exc:  # noqa: BLE001 - re-raised on the caller's thread
            errors[index] = exc

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(len(calls))]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout)
    for exc in errors:
        if exc is not None:
            raise exc
    return results
