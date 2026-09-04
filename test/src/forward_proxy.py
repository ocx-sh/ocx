"""Stdlib absolute-form HTTP forward proxy fixture for the SSRF-guard-under-proxy
acceptance tests (`test/tests/test_proxy_registry.py`).

`ocx` reaches this proxy through `HTTP_PROXY`/`HTTPS_PROXY`. `url` is
deliberately spelled with the HOSTNAME `localhost`, never the loopback IP
literal `127.0.0.1`: reqwest's proxy matcher resolves an IP-literal proxy
address without ever calling the custom DNS resolver hook the SSRF guard
installs, so a proxy dialed by IP would prove nothing about the hook's
behaviour under a name-based proxy.

`aliases` maps a `host:port` authority the ocx PROCESS cannot resolve (the
guarded registry name it addresses through the proxy, e.g. `PHANTOM_REGISTRY`)
to the `(host, port)` the PROXY itself dials in its place. This is the
corporate-network shape #407 is about: only the proxy can resolve the name, so
the client never gets to try.

Every request this fixture accepts must be in the **absolute-form** an
HTTP-proxy client sends (`GET http://host:port/path HTTP/1.1` — RFC 9112
§3.2.2), never the **origin-form** a client sends straight to a destination
(`GET /path HTTP/1.1`): an origin-form request answers 400. Only `GET`/`HEAD`
are implemented; any other method answers 501.
"""

from __future__ import annotations

import contextlib
import dataclasses
import http.client
import http.server
import socket
import threading
import time
import urllib.parse
from collections.abc import Iterator

PHANTOM_REGISTRY = "no-such-registry.invalid:5000"  # RFC 6761 §2.4: never resolves

# Headers that describe a single hop, never meaningful to replay on the next
# one. `host` rides along here too: the request's original `Host` (if any) is
# dropped by this same filter and replaced explicitly with the authority the
# proxy was asked to dial, so it is never copied *and* re-set.
_HOP_BY_HOP = frozenset(
    {
        "proxy-connection",
        "connection",
        "keep-alive",
        "transfer-encoding",
        "upgrade",
        "proxy-authorization",
        "host",
    }
)

# Loopback traffic only (registries and the static-index fixture) — generous
# enough that a slow CI runner never trips it, no retry ladder needed.
_UPSTREAM_TIMEOUT_SECONDS = 10.0


@dataclasses.dataclass(slots=True)
class ProxiedRequest:
    """One request this fixture served, as evidence for acceptance assertions."""

    request_line: str  # verbatim, e.g. 'GET http://host:5000/v2/ HTTP/1.1'
    method: str
    absolute_uri: str
    forwarded_to: str  # "<host>:<port>" after alias resolution


class _Handler(http.server.BaseHTTPRequestHandler):
    server: ForwardProxy  # narrows the inherited Any-typed attribute

    def log_message(self, format: str, *args: object) -> None:
        pass  # quiet test output — assertions read `server.requests` instead

    def do_GET(self) -> None:
        self._proxy()

    def do_HEAD(self) -> None:
        self._proxy()

    def _proxy(self) -> None:
        """Forwards an absolute-form GET/HEAD to its authority (aliased, or
        dialed as written when unaliased), recording the attempt before
        dialing so a failed upstream connection still leaves evidence.
        """
        target = urllib.parse.urlsplit(self.path)
        if not target.netloc:
            self.send_error(
                400,
                "origin-form request; this fixture only serves as a forward proxy",
            )
            return

        authority = target.netloc
        if authority in self.server.aliases:
            dial_host, dial_port = self.server.aliases[authority]
        else:
            # No alias: dial the authority exactly as the request named it —
            # this is what lets the same proxied run also carry the local
            # `index_server` fixture's own traffic, which is never aliased.
            dial_host = target.hostname or ""
            dial_port = target.port if target.port is not None else 80

        self.server.requests.append(
            ProxiedRequest(
                request_line=self.requestline,
                method=self.command,
                absolute_uri=self.path,
                forwarded_to=f"{dial_host}:{dial_port}",
            )
        )

        path_and_query = target.path or "/"
        if target.query:
            path_and_query += "?" + target.query

        connection = http.client.HTTPConnection(
            dial_host, dial_port, timeout=_UPSTREAM_TIMEOUT_SECONDS
        )
        try:
            connection.putrequest(
                self.command, path_and_query, skip_host=True, skip_accept_encoding=True
            )
            for key, value in self.headers.items():
                if key.lower() in _HOP_BY_HOP:
                    continue
                connection.putheader(key, value)
            connection.putheader("Host", authority)
            connection.putheader("Connection", "close")
            connection.endheaders()
            upstream = connection.getresponse()
        except OSError as exc:
            connection.close()
            self.send_error(
                502, f"forward proxy could not reach {dial_host}:{dial_port}: {exc}"
            )
            return

        body = upstream.read()
        self.send_response(upstream.status, upstream.reason)
        for key, value in upstream.getheaders():
            # `send_response()` already emits its own `Server` and `Date`
            # headers, so relaying the upstream's copies would duplicate both.
            if key.lower() in _HOP_BY_HOP or key.lower() in ("date", "server"):
                continue
            self.send_header(key, value)
        self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        connection.close()


class ForwardProxy(http.server.ThreadingHTTPServer):
    """An absolute-form HTTP forward proxy on an ephemeral loopback port.

    `aliases` (`"host:port"` -> `(dial_host, dial_port)`) is the routing table:
    an absolute-URI authority present in `aliases` is dialed at the mapped
    `(host, port)` instead of its own; an authority absent from `aliases` is
    dialed as written, exactly as a real forward proxy would — this is what
    lets the same proxied run also carry the local `index_server` fixture's
    own traffic, which no test ever adds to `aliases`.
    """

    def __init__(self, aliases: dict[str, tuple[str, int]]) -> None:
        self.aliases = aliases
        self.requests: list[ProxiedRequest] = []
        super().__init__(("127.0.0.1", 0), _Handler)

    @property
    def url(self) -> str:
        """`"http://localhost:<port>"` — HOSTNAME form, see module docstring."""
        _, port = self.server_address[:2]
        return f"http://localhost:{port}"

    def authorities(self) -> list[str]:
        """Distinct absolute-URI authorities served, in first-seen order."""
        return list(
            dict.fromkeys(
                urllib.parse.urlsplit(request.absolute_uri).netloc
                for request in self.requests
            )
        )


@contextlib.contextmanager
def running(aliases: dict[str, tuple[str, int]]) -> Iterator[ForwardProxy]:
    """Starts a `ForwardProxy` on a background thread, waits for it to accept,
    and tears it down on exit. Mirrors `static_index.running`.
    """
    server = ForwardProxy(aliases)
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
            raise RuntimeError("forward proxy fixture did not become reachable")
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
