# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`wait-for-stack.py`'s CT probe is ready only when the write path routes.

A log that serves `get-roots` but cannot take a submission must read as not
ready, or Fulcio fails the first sign with a 500. The probe is driven against a
stdlib HTTP server that answers `add-chain` with a chosen status.
"""

from __future__ import annotations

import importlib.util
import json
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest

_SCRIPT = Path(__file__).resolve().parents[1] / "sigstore" / "wait-for-stack.py"
_spec = importlib.util.spec_from_file_location("wait_for_stack", _SCRIPT)
assert _spec is not None and _spec.loader is not None
wait_for_stack = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wait_for_stack)


class _Log:
    def __init__(self, add_chain_status: int, roots: list[str]) -> None:
        self.add_chain_status = add_chain_status
        self.roots = roots
        self.paths: list[str] = []


@pytest.fixture
def serve() -> Iterator[object]:
    servers: list[HTTPServer] = []

    def start(
        add_chain_status: int, roots: list[str] | None = None
    ) -> tuple[str, _Log]:
        log = _Log(add_chain_status, ["root"] if roots is None else roots)

        class Handler(BaseHTTPRequestHandler):
            def _reply(self, status: int, body: bytes = b"{}") -> None:
                log.paths.append(f"{self.command} {self.path}")
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                self._reply(200, json.dumps({"certificates": log.roots}).encode())

            def do_POST(self) -> None:
                self.rfile.read(int(self.headers.get("Content-Length", 0)))
                self._reply(log.add_chain_status)

            def log_message(self, *_args: object) -> None:
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        servers.append(server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        return f"http://127.0.0.1:{server.server_port}/ocx-test/ct/v1/get-roots", log

    yield start
    for server in servers:
        server.shutdown()
        server.server_close()


def test_a_400_from_add_chain_means_ready(serve) -> None:
    url, log = serve(400)
    assert wait_for_stack.probe_ct(url)
    assert log.paths == [
        "GET /ocx-test/ct/v1/get-roots",
        "POST /ocx-test/ct/v1/add-chain",
    ]


@pytest.mark.parametrize("status", [404, 500, 200])
def test_any_other_add_chain_status_is_not_ready(serve, status: int) -> None:
    url, _log = serve(status)
    assert not wait_for_stack.probe_ct(url)


def test_a_log_with_no_roots_is_not_ready(serve) -> None:
    url, log = serve(400, roots=[])
    assert not wait_for_stack.probe_ct(url)
    assert log.paths == ["GET /ocx-test/ct/v1/get-roots"]


def test_an_unreachable_log_is_not_ready() -> None:
    assert not wait_for_stack.probe_ct("http://127.0.0.1:1/ocx-test/ct/v1/get-roots")
