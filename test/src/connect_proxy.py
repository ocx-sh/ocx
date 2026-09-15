# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stdlib TLS-terminating ``CONNECT`` proxy fixture for the extra-CA-roots
acceptance tests (ocx#467, `test/tests/test_extra_ca_certs.py`).

The corporate shape ocx#448 is about: an egress proxy that answers every
``CONNECT host:port`` with ``200 Connection established`` and then speaks TLS
to the client **itself**, presenting a leaf for ``host`` signed by the corp CA
(``terminator_pki``), and relays the decrypted bytes to the real origin
(``aliases[authority]``). A client that trusts the corp root sees a normal
HTTPS origin; one that does not fails the handshake with ``UnknownIssuer`` —
which is the red/green pair every row in the test module is built on.

``ocx`` reaches this proxy through ``HTTPS_PROXY``. As in
``src.forward_proxy``, ``url`` is spelled with the HOSTNAME ``localhost``,
never ``127.0.0.1``: reqwest's proxy matcher resolves an IP-literal proxy
without the custom DNS-resolver hook the SSRF guard installs, so an IP-dialed
proxy would prove nothing about that hook.

``front_tls_pki`` wraps the LISTENER too (``url`` becomes ``https://``): the
``HTTPS_PROXY=https://…`` row, where the client TLS-connects to the proxy and
then runs the tunnel's TLS inside that session. The tunnel side is therefore
never ``wrap_socket``-ed onto ``self.connection`` directly — that detaches the
fd and would bypass an outer TLS layer — but onto one end of a
``socket.socketpair()`` whose other end is byte-relayed to the client; the
same relay serves the plain and the TLS-fronted listener alike, so the row
that differs is only the listener wrap.
"""

from __future__ import annotations

import contextlib
import http.server
import socket
import ssl
import tempfile
import threading
import time
from collections.abc import Iterator
from pathlib import Path

from src.tls_index import MintedPki

# Loopback traffic only; generous enough that a slow CI runner never trips it,
# tight enough that a tunnel whose client vanished cannot hold a handler
# thread for the rest of the session.
_RELAY_TIMEOUT_SECONDS = 30.0
_CHUNK = 65536


def _server_context(pki: MintedPki, stage: Path) -> ssl.SSLContext:
    """A server-side context presenting ``pki``'s leaf. ``load_cert_chain``
    reads files, never bytes, so the leaf is staged under ``stage``."""
    stage.mkdir(parents=True, exist_ok=True)
    cert_path = stage / "leaf.pem"
    key_path = stage / "leaf.key"
    cert_path.write_bytes(pki.leaf_cert_pem)
    key_path.write_bytes(pki.leaf_key_pem)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(str(cert_path), str(key_path))
    return context


def _pump(source: socket.socket, sink: socket.socket) -> None:
    """Copies bytes ``source`` → ``sink`` until EOF or any socket error, then
    half-closes a plain ``sink`` so its reader sees the EOF too. Never closes:
    the owner of both sockets does that once both directions have returned.

    A TLS ``sink`` is never half-closed: ``SSLSocket.shutdown`` drops the SSL
    object out from under the thread still reading the other direction, whose
    next ``recv`` would then hand raw ciphertext on as plaintext. Its peer's
    own close (or the socket timeout) ends that direction instead.
    """
    try:
        while True:
            data = source.recv(_CHUNK)
            if not data:
                return
            sink.sendall(data)
    except OSError:  # ssl.SSLError and socket.timeout are both OSError
        return
    finally:
        if not isinstance(sink, ssl.SSLSocket):
            with contextlib.suppress(OSError):
                sink.shutdown(socket.SHUT_WR)


def _relay(a: socket.socket, b: socket.socket) -> None:
    """Full-duplex byte relay between ``a`` and ``b`` — one thread per
    direction — returning once both directions have hit EOF or failed."""
    forward = threading.Thread(target=_pump, args=(a, b), daemon=True)
    forward.start()
    _pump(b, a)
    forward.join()


class _Handler(http.server.BaseHTTPRequestHandler):
    server: ConnectProxy  # narrows the inherited Any-typed attribute

    # Unbuffered request reads: a buffered ``rfile`` could swallow the first
    # tunnel bytes (the client's ClientHello) along with the CONNECT headers.
    rbufsize = 0
    timeout = _RELAY_TIMEOUT_SECONDS

    def log_message(self, format: str, *args: object) -> None:
        pass  # quiet test output — assertions read `server.tunnels` instead

    def do_CONNECT(self) -> None:
        """Records the authority, dials its alias, answers 200, then
        TLS-terminates the tunnel with ``terminator_pki`` and relays the
        plaintext to the origin until either side closes."""
        authority = self.path
        self.server.tunnels.append(authority)
        # One CONNECT per connection: the tunnel IS the connection from here.
        self.close_connection = True

        alias = self.server.aliases.get(authority)
        if alias is None:
            self.send_error(502, f"CONNECT {authority}: no alias for that authority")
            return
        try:
            upstream = socket.create_connection(alias, timeout=_RELAY_TIMEOUT_SECONDS)
        except OSError as exc:
            self.send_error(502, f"CONNECT {authority}: could not reach {alias}: {exc}")
            return

        self.send_response(200, "Connection established")
        self.end_headers()

        # `client_end` is what the client's bytes arrive on (relayed from the
        # listener connection, plain or TLS-fronted); `tunnel_end` is where
        # the tunnel's own TLS is terminated. See the module docstring.
        client_end, tunnel_end = socket.socketpair()
        client_end.settimeout(_RELAY_TIMEOUT_SECONDS)
        tunnel_end.settimeout(_RELAY_TIMEOUT_SECONDS)
        front = threading.Thread(
            target=_relay, args=(self.connection, client_end), daemon=True
        )
        front.start()
        try:
            try:
                terminated = self.server.terminator_context.wrap_socket(
                    tunnel_end, server_side=True
                )
            except OSError:
                # The client refused our leaf (no trusted root): the red half
                # of every red/green pair. Nothing reached the origin.
                self.server.refused_handshakes.append(authority)
                return
            with terminated:
                _relay(terminated, upstream)
        finally:
            upstream.close()
            tunnel_end.close()
            front.join()
            client_end.close()


class ConnectProxy(http.server.ThreadingHTTPServer):
    """A TLS-terminating ``CONNECT`` proxy on an ephemeral loopback port.

    ``aliases`` (``"host:port"`` -> ``(dial_host, dial_port)``) is the routing
    table: a CONNECT authority present in it is relayed, in plaintext, to the
    mapped origin; any other authority answers 502 — a terminating proxy has
    no business relaying bytes it cannot read to an origin it was not told
    about. ``tunnels`` records every authority in CONNECT order, appended
    before anything is dialed, so a refused handshake still leaves evidence;
    ``refused_handshakes`` records the authorities whose client rejected the
    terminator's leaf.
    """

    def __init__(
        self,
        aliases: dict[str, tuple[str, int]],
        terminator_pki: MintedPki,
        front_tls_pki: MintedPki | None = None,
    ) -> None:
        self.aliases = aliases
        self.tunnels: list[str] = []
        self.refused_handshakes: list[str] = []
        self._tls_dir = tempfile.TemporaryDirectory(prefix="ocx-connect-proxy-")
        stage = Path(self._tls_dir.name)
        self.terminator_context = _server_context(terminator_pki, stage / "tunnel")
        self.front_tls = front_tls_pki is not None
        super().__init__(("127.0.0.1", 0), _Handler)
        if front_tls_pki is not None:
            self.socket = _server_context(front_tls_pki, stage / "front").wrap_socket(
                self.socket, server_side=True
            )

    def server_close(self) -> None:
        super().server_close()
        self._tls_dir.cleanup()

    @property
    def url(self) -> str:
        """``"http(s)://localhost:<port>"`` — HOSTNAME form, see module docstring."""
        _, port = self.server_address[:2]
        scheme = "https" if self.front_tls else "http"
        return f"{scheme}://localhost:{port}"


@contextlib.contextmanager
def running(
    aliases: dict[str, tuple[str, int]],
    terminator_pki: MintedPki,
    front_tls_pki: MintedPki | None = None,
) -> Iterator[ConnectProxy]:
    """Starts a ``ConnectProxy`` on a background thread, waits for it to
    accept, and tears it down on exit. Mirrors ``forward_proxy.running``.

    The readiness probe is a plain TCP connect that closes immediately; on a
    TLS-fronted listener the server's handshake on it fails and is swallowed
    by ``socketserver``, which is exactly the signal that the listener is up.
    """
    server = ConnectProxy(aliases, terminator_pki, front_tls_pki)
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
            raise RuntimeError("CONNECT proxy fixture did not become reachable")
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
