"""Self-test of the static index fixture (test/src/static_index.py): leaving `running()` joins in-flight handlers."""

from __future__ import annotations

import socket
import threading
from pathlib import Path

from src import static_index


def test_running_joins_a_handler_still_dribbling_a_body(tmp_path: Path) -> None:
    (tmp_path / "body.json").write_bytes(b"x" * 40)
    baseline = set(threading.enumerate())
    with static_index.running(tmp_path) as server:
        server.slow_bodies["/body.json"] = (4, 0.3)
        client = socket.create_connection(server.server_address, timeout=5)
        client.sendall(b"GET /body.json HTTP/1.1\r\nHost: x\r\n\r\n")
        assert client.recv(1)  # the handler is now mid-body
    # `running()` exited while the client was still connected.
    client.close()
    leaked = [t for t in threading.enumerate() if t not in baseline and t.is_alive()]
    assert leaked == []
