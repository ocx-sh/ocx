# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors

"""Format-agnostic file cache with TTL expiration.

Stores raw bytes on disk, keyed by a domain tag and an arbitrary cache key.
Freshness is determined by the file's modification time — no metadata envelope.
The caller decides how to serialize and deserialize the data.

Typical usage::

    cache = FileCache("github", max_age=3600)

    # fetch-through with JSON: calls the loader only on cache miss
    releases = cache.fetch_json("corretto/corretto-21", load_releases)

    # manual store / get (raw bytes)
    cache.put("key", b"raw bytes")
    cached = cache.get("key")  # -> bytes | None

    # manual store / get (JSON)
    cache.put_json("key", {"count": 42})
    obj = cache.get_json("key")  # -> Any | None
"""

import json
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

_DEFAULT_ROOT = Path.home() / ".cache" / "ocx-gen"


class FileCache:
    """TTL-based blob cache with domain separation.

    Args:
        domain: Namespace subdirectory (e.g. ``"github"``).
        max_age: Maximum age of cache entries in seconds.  ``0`` disables
            caching (every ``get`` returns ``None``, ``fetch`` always calls
            the loader).
        root: Root cache directory.  Defaults to ``~/.cache/ocx-gen/``.
    """

    def __init__(
        self,
        domain: str,
        *,
        max_age: int = 3600,
        root: Path | None = None,
    ) -> None:
        self._dir = (root or _DEFAULT_ROOT) / domain
        self._max_age = max_age

    def _path(self, key: str) -> Path:
        """Turn a cache key into a file path (``/`` in key creates subdirs)."""
        return self._dir / key

    def get(self, key: str) -> bytes | None:
        """Return cached bytes for *key*, or ``None`` on miss / expiry."""
        if self._max_age <= 0:
            return None
        path = self._path(key)
        if not path.exists():
            return None
        age = time.time() - path.stat().st_mtime
        if age >= self._max_age:
            return None
        return path.read_bytes()

    def put(self, key: str, data: bytes) -> None:
        """Store *data* under *key*.  No-op when caching is disabled."""
        if self._max_age <= 0:
            return
        path = self._path(key)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def fetch(self, key: str, loader: Callable[[], bytes]) -> bytes:
        """Return cached bytes or call *loader*, cache the result, and return it."""
        cached = self.get(key)
        if cached is not None:
            return cached
        data = loader()
        self.put(key, data)
        return data

    # -- JSON convenience layer ------------------------------------------------

    def get_json(self, key: str) -> Any | None:
        """Return the deserialized JSON object for *key*, or ``None`` on miss."""
        raw = self.get(key)
        if raw is None:
            return None
        return json.loads(raw)

    def put_json(self, key: str, obj: Any) -> None:
        """Serialize *obj* as JSON and store it under *key*."""
        self.put(key, json.dumps(obj).encode())

    def fetch_json(self, key: str, loader: Callable[[], Any]) -> Any:
        """Return cached JSON or call *loader*, cache the result, and return it.

        Unlike :meth:`fetch`, the *loader* returns a JSON-serializable object
        (not raw bytes) and the return value is the deserialized object.
        """
        cached = self.get_json(key)
        if cached is not None:
            return cached
        obj = loader()
        self.put_json(key, obj)
        return obj
