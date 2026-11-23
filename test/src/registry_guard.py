"""Deny a home all registry traffic and count every request it attempts.

The guarded runner's `HTTP_PROXY`/`HTTPS_PROXY` point at a loopback listener
(empty `NO_PROXY`) that records each request line and answers 503, so the
command fails and the count says how often it tried. Each guard counts only
the runner it was applied to.
"""

from __future__ import annotations

import contextlib
import socket
import socketserver
import threading
import time
from collections.abc import Iterator

from src.runner import OcxRunner

_READ_TIMEOUT_SECONDS = 2.0
_MAX_HEAD_BYTES = 65536


class _Handler(socketserver.BaseRequestHandler):
    server: RegistryGuard

    def handle(self) -> None:
        self.request.settimeout(_READ_TIMEOUT_SECONDS)
        head = b""
        try:
            while b"\r\n\r\n" not in head and len(head) < _MAX_HEAD_BYTES:
                chunk = self.request.recv(4096)
                if not chunk:
                    break
                head += chunk
        except OSError:
            pass  # a client that never finished its head is still an attempt
        request_line = head.split(b"\r\n", 1)[0].decode("latin-1")
        with self.server.lock:
            self.server.requests.append(request_line)
        with contextlib.suppress(OSError):
            self.request.sendall(
                b"HTTP/1.1 503 Service Unavailable\r\n"
                b"Content-Length: 0\r\nConnection: close\r\n\r\n"
            )


class RegistryGuard(socketserver.ThreadingTCPServer):
    """A loopback listener that refuses every request and counts the attempts."""

    allow_reuse_address = True
    daemon_threads = True

    def __init__(self) -> None:
        self.requests: list[str] = []
        self.lock = threading.Lock()
        super().__init__(("127.0.0.1", 0), _Handler)

    @property
    def url(self) -> str:
        _, port = self.server_address[:2]
        return f"http://localhost:{port}"

    @property
    def count(self) -> int:
        """Requests attempted so far (connections that sent a head count once each)."""
        with self.lock:
            return len(self.requests)

    @property
    def env(self) -> dict[str, str]:
        return {
            "HTTP_PROXY": self.url,
            "HTTPS_PROXY": self.url,
            "http_proxy": self.url,
            "https_proxy": self.url,
            "NO_PROXY": "",
            "no_proxy": "",
        }

    def deny(self, runner: OcxRunner) -> OcxRunner:
        """Route every later invocation of `runner` into this guard."""
        runner.env.update(self.env)
        return runner

    def allow(self, runner: OcxRunner) -> OcxRunner:
        """Lift the guard from `runner`; the listener keeps its count."""
        for key in self.env:
            runner.env.pop(key, None)
        return runner

    def reset(self) -> None:
        with self.lock:
            self.requests.clear()


@contextlib.contextmanager
def running() -> Iterator[RegistryGuard]:
    """Start a `RegistryGuard`, wait until it accepts, and stop it on exit."""
    server = RegistryGuard()
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
            raise RuntimeError("registry guard did not become reachable")
        # The readiness probe above sent no head but still counts; start clean.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline and server.count == 0:
            time.sleep(0.01)
        server.reset()
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
