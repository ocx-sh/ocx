"""AF_UNIX socket paths that fit ``sun_path`` however deep the test's tmp dir is.

Linux caps a socket path at 107 bytes. Pytest's basetemp under the Bazel runner is already ~90
bytes deep, so a socket beside the test's files fails with "File name too long". The socket
lives in its own short directory under /tmp instead.
"""

from __future__ import annotations

import contextlib
import os
import shutil
import tempfile
from collections.abc import Iterator
from pathlib import Path

SUN_PATH_MAX = 107


@contextlib.contextmanager
def short_socket(name: str = "s.sock") -> Iterator[Path]:
    """Yield a socket path in a fresh short directory; the directory is removed on exit."""
    base = "/tmp" if os.path.isdir("/tmp") else tempfile.gettempdir()
    directory = Path(tempfile.mkdtemp(prefix="ocx-", dir=base))
    try:
        path = directory / name
        assert len(os.fsencode(path)) <= SUN_PATH_MAX, f"socket path too long for AF_UNIX: {path}"
        yield path
    finally:
        shutil.rmtree(directory, ignore_errors=True)
