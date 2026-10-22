# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`src/zot.py`: kernel-chosen ports, the readiness timeout and the crash-safe kill.

No real zot: a fake that prints zot's own "kernel chosen port" line and serves
`GET /v2/`, so the whole start -> ready -> stop path runs in under a second.
"""

from __future__ import annotations

import json
import shutil
import socket
import subprocess
import sys
import textwrap
import time
import urllib.request
from pathlib import Path

import pytest

from src import zot

TEST_ROOT = Path(__file__).resolve().parent.parent

# /proc is the liveness detector and the fake zot is a shebang script.
pytestmark = pytest.mark.skipif(not Path("/proc/self").exists(), reason="needs /proc")

# zot v2.1 writes this line, verbatim apart from the time and caller fields.
REAL_LINE = (
    '{"time":"2026-10-22T05:55:32.398557029+02:00","level":"info","message":"port is unspecified, '
    'listening on kernel chosen port","port":40795,"address":"127.0.0.1","caller":"zotregistry.dev/zot/v2/pkg/api/listener.go:139"}'
)


@pytest.fixture
def fake_zot(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    """An executable `serve <config>`; FAKE_ZOT_MODE picks ok | silent | exit."""
    script = tmp_path / "fake-zot"
    script.write_text(
        textwrap.dedent(
            f"""\
            #!{sys.executable}
            import http.server, json, os, sys, time
            mode = os.environ.get("FAKE_ZOT_MODE", "ok")
            assert sys.argv[1] == "serve"
            configured = int(json.load(open(sys.argv[2]))["http"]["port"])
            if mode == "exit":
                sys.exit(3)
            if mode == "silent":
                time.sleep(600)
            class Handler(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    self.send_response(200 if self.path == "/v2/" else 404)
                    self.end_headers()
                def log_message(self, *args):
                    pass
            server = http.server.ThreadingHTTPServer(("127.0.0.1", configured), Handler)
            if not configured:
                print(json.dumps({{"level": "info", "message": "port is unspecified, listening on kernel chosen port",
                                  "port": server.server_port}}), flush=True)
            server.serve_forever()
            """
        ),
        encoding="utf-8",
    )
    script.chmod(0o755)
    monkeypatch.delenv("FAKE_ZOT_MODE", raising=False)
    return str(script)


def alive(pid: int) -> bool:
    try:
        state = (
            Path(f"/proc/{pid}/stat")
            .read_text(encoding="utf-8")
            .rsplit(")", 1)[1]
            .split()[0]
        )
    except FileNotFoundError:
        return False
    return state != "Z"  # a zombie is dead, only not yet reaped


def wait_until(condition, seconds: float = 10.0) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if condition():
            return True
        time.sleep(0.05)
    return condition()


def test_the_chosen_port_is_read_from_zots_own_log_line() -> None:
    assert zot.parse_chosen_port("garbage\n" + REAL_LINE + "\n") == 40795
    assert zot.parse_chosen_port('{"message":"kernel chosen port","port":"x"}') is None
    assert (
        zot.parse_chosen_port('{"level":"info","message":"jwt bearer authentication"}')
        is None
    )
    assert zot.parse_chosen_port("") is None


def test_a_started_stack_answers_on_distinct_ports_and_stop_removes_it(
    fake_zot: str,
) -> None:
    stack = zot.start(fake_zot, timeout=20)
    try:
        assert sorted(stack.ports) == ["prod", "registry", "target"]
        assert len(set(stack.ports.values())) == 3
        for port in stack.ports.values():
            assert (
                urllib.request.urlopen(f"http://127.0.0.1:{port}/v2/", timeout=5).status
                == 200
            )
        env = stack.env()
        assert env["OCX_TEST_REGISTRY_PORT"] == str(stack.ports["registry"])
        assert env["OCX_TEST_TARGET_PORT"] == str(stack.ports["target"])
        assert env["OCX_TEST_PROD_PORT"] == str(stack.ports["prod"])
        assert env["OCX_INSECURE_REGISTRIES"] == f"localhost:{stack.ports['registry']}"
        pids = stack.pids
        assert all(alive(pid) for pid in pids), (
            "detector must see the running zots before stop"
        )
    finally:
        stack.stop()
    assert not any(alive(pid) for pid in pids)
    assert not stack.root.exists()


def failed_start_root(failure: pytest.ExceptionInfo[zot.ZotError]) -> Path:
    """The kept store root a failed start names in its message (its log is inside)."""
    log = Path(str(failure.value).rsplit("log: ", 1)[1])
    assert log.name == "zot.log" and log.exists(), str(failure.value)
    return log.parents[1]


def test_a_zot_that_never_listens_fails_naming_its_log_and_is_not_left_running(
    fake_zot: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_ZOT_MODE", "silent")
    started = time.monotonic()
    with pytest.raises(zot.ZotError, match=r"within 1\.5 s") as failure:
        zot.start(fake_zot, names=("registry",), timeout=1.5)
    assert time.monotonic() - started < 15
    root = failed_start_root(failure)
    try:
        assert wait_until(lambda: not _zots_under(root)), (
            "the timed-out zot was left running"
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)


def _zots_under(root: Path) -> list[str]:
    """Pids of live processes whose argv carries `root` (the fake zot's config path)."""
    found = []
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                if str(root) in (entry / "cmdline").read_text(
                    encoding="utf-8", errors="replace"
                ):
                    found.append(entry.name)
            except OSError:
                pass
    return found


def test_a_zot_that_exits_early_fails_at_once_not_after_the_timeout(
    fake_zot: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_ZOT_MODE", "exit")
    started = time.monotonic()
    with pytest.raises(zot.ZotError, match="exited with 3") as failure:
        zot.start(fake_zot, names=("registry",), timeout=30)
    assert time.monotonic() - started < 10
    shutil.rmtree(failed_start_root(failure), ignore_errors=True)


def test_a_missing_zot_binary_is_named(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(zot.ZOT_ENV, raising=False)
    monkeypatch.setenv("PATH", "")
    with pytest.raises(zot.ZotError, match=zot.ZOT_ENV):
        zot.resolve_zot()


def test_sigkill_of_the_controller_leaves_no_zot_and_no_store(fake_zot: str) -> None:
    controller = subprocess.Popen(
        [
            sys.executable,
            "-c",
            (
                "import json, time\n"
                "from src import zot\n"
                f"stack = zot.start({fake_zot!r}, timeout=20)\n"
                "print(json.dumps({'pids': stack.pids, 'root': str(stack.root)}), flush=True)\n"
                "time.sleep(600)\n"
            ),
        ],
        cwd=TEST_ROOT,
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        announced = json.loads(controller.stdout.readline())
        pids, root = announced["pids"], Path(announced["root"])
        assert len(pids) == 3 and all(alive(pid) for pid in pids), (
            "the detector sees the zots while the controller lives"
        )
        assert root.exists()
        controller.kill()  # SIGKILL: no atexit, no finally, no unconfigure
        controller.wait()
        assert wait_until(lambda: not any(alive(pid) for pid in pids)), (
            "zot outlived its killed controller"
        )
        assert wait_until(lambda: not root.exists()), (
            "the stores outlived their killed controller"
        )
    finally:
        controller.kill()
        controller.wait()
        if controller.stdout:
            controller.stdout.close()


def test_a_pinned_port_is_configured_and_served(fake_zot: str) -> None:
    assert zot.zot_config(Path("/s"))["http"]["port"] == "0"
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        free = probe.getsockname()[1]
    stack = zot.start(fake_zot, names=("registry",), fixed_ports={"registry": free})
    try:
        assert stack.ports == {"registry": free}
        assert zot._serves_v2(free)
    finally:
        stack.stop()
