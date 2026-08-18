# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A raw TCP forwarder whose address stays put after the endpoint dies.

The trust-root cache is keyed by the Rekor URL's *authority*, so "offline verify
made no Rekor request" can only be proven at one address that is live while the
cache warms and dead afterwards. The real Rekor is session-scoped and shared, so
it cannot be stopped mid-test, and swapping in a different dead URL would change
the cache key — the verify would then miss the cache for the wrong reason and
the test would pass without proving anything.

Bytes are relayed unparsed: nothing here needs to understand HTTP, and a parser
would be a second implementation of one.
"""

from __future__ import annotations

import socket
import threading


class TcpRelay:
    """Forwards 127.0.0.1:<ephemeral> to ``upstream`` until :meth:`close`."""

    def __init__(self, upstream_host: str, upstream_port: int) -> None:
        self._upstream = (upstream_host, upstream_port)
        self._server = socket.create_server(("127.0.0.1", 0))
        # Polled rather than blocking, so `close` can join the accept loop before
        # closing the socket. Closing it under a blocked `accept` leaves the
        # kernel's file description alive — the port then keeps accepting and the
        # "endpoint is dead" evidence every caller relies on is silently false.
        self._server.settimeout(0.1)
        self.port: int = self._server.getsockname()[1]
        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def close(self) -> None:
        """Stop listening. Later connections to :attr:`url` are refused."""
        self._stopped.set()
        self._thread.join(timeout=5)
        assert not self._thread.is_alive(), "accept loop did not stop; the port may still accept"
        self._server.close()

    def __enter__(self) -> TcpRelay:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def _serve(self) -> None:
        while not self._stopped.is_set():
            try:
                client, _ = self._server.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            threading.Thread(target=self._bridge, args=(client,), daemon=True).start()

    def _bridge(self, client: socket.socket) -> None:
        try:
            upstream = socket.create_connection(self._upstream)
        except OSError:
            client.close()
            return
        for source, sink in ((client, upstream), (upstream, client)):
            threading.Thread(target=self._pump, args=(source, sink), daemon=True).start()

    @staticmethod
    def _pump(source: socket.socket, sink: socket.socket) -> None:
        try:
            while chunk := source.recv(65536):
                sink.sendall(chunk)
        except OSError:
            pass
        finally:
            # Half-close so the peer sees EOF instead of waiting on a body that
            # is never coming; the other direction closes on its own pump.
            try:
                sink.shutdown(socket.SHUT_WR)
            except OSError:
                pass
