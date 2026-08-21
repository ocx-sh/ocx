# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stdlib fake OCI registry endpoints that answer wrongly on purpose.

`registry:2` and `zot` cannot be made to misbehave: both are content-addressed
and refuse to serve bytes that are not the manifest they claim to be. The
failure ocx-sh/ocx#327 reported needs exactly that — a host that speaks enough
of the distribution API to be reached, then answers every manifest request with
an HTML page.

Per-test instance bound to an ephemeral loopback port, zero real network. Same
shape as `test/tests/fake_forge.py`.
"""
from __future__ import annotations

import http.server
import json
import re
import threading
import urllib.parse

_MANIFEST_RE = re.compile(r"^/v2/(?P<repository>.+)/manifests/(?P<reference>[^/]+)$")
_TAGS_RE = re.compile(r"^/v2/(?P<repository>.+)/tags/list$")

# What a captive portal, an SSO gateway, or a tenant that no longer serves the
# registry answers with — 200, HTML, no digest header.
_PORTAL = (
    "<!DOCTYPE html><html><head><title>Sign in</title></head>"
    "<body><h1>Sign in to continue</h1></body></html>"
)


class _Handler(http.server.BaseHTTPRequestHandler):
    server: HtmlMirror  # narrows the inherited Any-typed attribute

    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 (stdlib signature)
        pass  # quiet test output — assertions read `server.requests` instead

    def _reply(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _reply_json(self, status: int, payload: object) -> None:
        self._reply(status, json.dumps(payload).encode(), "application/json")

    def _dispatch(self) -> None:
        path = urllib.parse.urlsplit(self.path).path
        self.server.record(self.command, path)

        # The anonymous auth probe: 200 means "no challenge, pull away".
        if path in ("/v2", "/v2/"):
            self._reply_json(200, {})
            return

        match = _TAGS_RE.fullmatch(path)
        if match:
            self._reply_json(200, {"name": match.group("repository"), "tags": self.server.tags})
            return

        if _MANIFEST_RE.fullmatch(path):
            # The defect under test: 200 with a body that cannot be a manifest,
            # and no Docker-Content-Digest header, so a client that does not read
            # the content type carries the HTML into digest verification.
            self._reply(200, _PORTAL.encode(), "text/html; charset=utf-8")
            return

        self._reply_json(404, {"errors": [{"code": "NOT_FOUND", "message": "not found", "detail": None}]})

    def do_GET(self) -> None:  # noqa: N802 (stdlib API)
        self._dispatch()

    def do_HEAD(self) -> None:  # noqa: N802 (stdlib API)
        self._dispatch()


class HtmlMirror(http.server.ThreadingHTTPServer):
    """A host that answers every manifest request with an HTML portal page."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.requests: list[tuple[str, str]] = []
        self.tags: list[str] = ["1.0.0"]
        super().__init__(("127.0.0.1", 0), _Handler)

    @property
    def authority(self) -> str:
        host, port = self.server_address[:2]
        return f"{host}:{port}"

    @property
    def base_url(self) -> str:
        return f"http://{self.authority}"

    def record(self, method: str, path: str) -> None:
        with self.lock:
            self.requests.append((method, path))
