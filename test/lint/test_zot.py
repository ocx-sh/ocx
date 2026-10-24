# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`src/zot.py`: kernel-chosen ports, the readiness timeout and the crash-safe kill.

No real zot: a fake that prints zot's own "kernel chosen port" line and serves
`GET /v2/`, so the whole start -> ready -> stop path runs in under a second.
"""

from __future__ import annotations

import contextlib
import http.server
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import textwrap
import threading
import time
import urllib.request
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import zot

TEST_ROOT = Path(__file__).resolve().parent.parent

# Liveness reads /proc and the fake zot is a shebang script: the POSIX tests carry this.
needs_proc = pytest.mark.skipif(not Path("/proc/self").exists(), reason="needs /proc")

# zot v2.1 writes this line, verbatim apart from the time and caller fields.
REAL_LINE = (
    '{"time":"2026-10-22T05:55:32.398557029+02:00","level":"info","message":"port is unspecified, '
    'listening on kernel chosen port","port":40795,"address":"127.0.0.1","caller":"zotregistry.dev/zot/v2/pkg/api/listener.go:139"}'
)


def write_fake_zot(script: Path) -> None:
    """A script that answers `zot serve <config>`; FAKE_ZOT_MODE picks ok | silent | exit."""
    script.write_text(
        textwrap.dedent(
            f"""\
            #!{sys.executable}
            import http.server, json, os, sys, time
            mode = os.environ.get("FAKE_ZOT_MODE", "ok")
            args = sys.argv[sys.argv.index("serve"):]
            configured = int(json.load(open(args[1]))["http"]["port"])
            if mode == "exit":
                sys.exit(3)
            if mode == "silent":
                time.sleep(600)
            with open(os.path.join(os.path.dirname(args[1]), "pid"), "w") as fh:
                fh.write(str(os.getpid()))
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


@pytest.fixture
def fake_zot(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    """An executable `serve <config>` (POSIX: the shebang runs it)."""
    script = tmp_path / "fake-zot"
    write_fake_zot(script)
    script.chmod(0o755)
    monkeypatch.delenv("FAKE_ZOT_MODE", raising=False)
    return str(script)


def _windows_alive(pid: int) -> bool:
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        error = ctypes.get_last_error()
        if error == 87:  # ERROR_INVALID_PARAMETER: no such pid
            return False
        # Any other failure says nothing about the process: reading it as "gone" would pass vacuously.
        raise ctypes.WinError(error)
    try:
        code = wintypes.DWORD()
        if not k32.GetExitCodeProcess(handle, ctypes.byref(code)):
            raise ctypes.WinError(ctypes.get_last_error())
        return code.value == 259  # STILL_ACTIVE
    finally:
        k32.CloseHandle(handle)


def alive(pid: int) -> bool:
    if sys.platform == "win32":
        return _windows_alive(pid)
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


@needs_proc
def test_the_chosen_port_is_read_from_zots_own_log_line() -> None:
    assert zot.parse_chosen_port("garbage\n" + REAL_LINE + "\n") == 40795
    assert zot.parse_chosen_port('{"message":"kernel chosen port","port":"x"}') is None
    assert (
        zot.parse_chosen_port('{"level":"info","message":"jwt bearer authentication"}')
        is None
    )
    assert zot.parse_chosen_port("") is None


@needs_proc
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


@needs_proc
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


@needs_proc
def test_a_zot_that_exits_early_fails_at_once_not_after_the_timeout(
    fake_zot: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_ZOT_MODE", "exit")
    started = time.monotonic()
    with pytest.raises(zot.ZotError, match="exited with 3") as failure:
        zot.start(fake_zot, names=("registry",), timeout=30)
    assert time.monotonic() - started < 10
    shutil.rmtree(failed_start_root(failure), ignore_errors=True)


@needs_proc
def test_a_missing_zot_binary_is_named(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(zot.ZOT_ENV, raising=False)
    monkeypatch.setenv("PATH", "")
    with pytest.raises(zot.ZotError, match=zot.ZOT_ENV):
        zot.resolve_zot()


@needs_proc
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


@needs_proc
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


@needs_proc
def test_a_controller_killed_between_spawn_and_adoption_leaves_no_zot_and_no_store(
    fake_zot: str, tmp_path: Path
) -> None:
    """The pid line the watchdog is sent after the spawn is where the controller dies."""
    controller = (
        "import glob, json, os, signal\n"
        "from src import zot\n"
        "real = os.write\n"
        "def die(fd, data):\n"
        "    if not data.strip().isdigit():\n"
        "        return real(fd, data)\n"
        "    root = glob.glob(os.path.join(os.environ['TMPDIR'], 'ocx-zot-*'))[0]\n"
        "    print(json.dumps({'pid': int(data), 'root': root}), flush=True)\n"
        "    os.kill(os.getpid(), signal.SIGKILL)\n"
        "os.write = die\n"
        f"zot.start({fake_zot!r}, names=('registry',), timeout=20)\n"
    )
    env = dict(os.environ)
    env["TMPDIR"] = str(tmp_path)
    done = subprocess.run(
        [sys.executable, "-c", controller],
        cwd=TEST_ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert done.returncode == -signal.SIGKILL, done.stderr
    announced = json.loads(done.stdout.splitlines()[0])
    pid, root = announced["pid"], Path(announced["root"])
    assert wait_until(lambda: not alive(pid)), (
        "the spawned process outlived its killed controller"
    )
    assert wait_until(lambda: not _zots_under(root)), "zot started without a guard"
    assert wait_until(lambda: not root.exists()), "the stores outlived the controller"


@needs_proc
def test_the_readiness_probe_ignores_an_ambient_proxy(fake_zot: str) -> None:
    stack = zot.start(fake_zot, names=("registry",))
    try:
        env = dict(os.environ)
        for name in ("NO_PROXY", "no_proxy"):
            env.pop(name, None)
        env["HTTP_PROXY"] = env["http_proxy"] = "http://127.0.0.1:9"
        # A subprocess: urllib builds its default opener, proxies included, once per process.
        probe = subprocess.run(
            [
                sys.executable,
                "-c",
                f"from src import zot; print(zot._serves_v2({stack.ports['registry']}))",
            ],
            cwd=TEST_ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        assert probe.stdout.strip() == "True", probe.stderr
    finally:
        stack.stop()


@contextlib.contextmanager
def answers_with(status: int) -> Iterator[int]:
    """The port of a local server that answers every GET with `status`."""

    class Registry(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.send_response(status)
            self.end_headers()

        def log_message(self, *args) -> None:
            pass

    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Registry) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            yield server.server_port
        finally:
            server.shutdown()


@needs_proc
def test_a_pinned_port_that_already_answers_is_refused(fake_zot: str) -> None:
    with (
        answers_with(200) as foreign,
        pytest.raises(zot.ZotError, match="already answers"),
    ):
        zot.start(fake_zot, names=("registry",), fixed_ports={"registry": foreign})


@pytest.mark.parametrize(
    ("status", "serving"), [(200, True), (204, False), (401, False), (404, False)]
)
def test_only_a_200_on_v2_counts_as_serving_it(status: int, serving: bool) -> None:
    """204 is the case urllib lets through as a success, so only the `== 200` compare rejects it."""
    with answers_with(status) as port:
        assert zot._serves_v2(port) is serving


def test_a_port_nothing_listens_on_is_not_serving_v2() -> None:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        closed = probe.getsockname()[1]
    assert zot._serves_v2(closed) is False


@needs_proc
def test_the_listen_address_is_loopback_unless_widened() -> None:
    assert zot.zot_config(Path("/s"))["http"]["address"] == "127.0.0.1"
    assert zot.zot_config(Path("/s"), address="0.0.0.0")["http"]["address"] == "0.0.0.0"


@needs_proc
def test_the_module_serves_one_registry_on_a_given_port_until_killed(
    fake_zot: str, tmp_path: Path
) -> None:
    """`python -m src.zot PORT`, the registry the manual rigs talk to."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        free = probe.getsockname()[1]
    env = dict(os.environ)
    env[zot.ZOT_ENV] = fake_zot
    env["TMPDIR"] = str(tmp_path)
    server = subprocess.Popen(
        [sys.executable, "-m", "src.zot", str(free)],
        cwd=TEST_ROOT,
        env=env,
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        assert f"localhost:{free}" in server.stdout.readline()
        assert (
            urllib.request.urlopen(f"http://127.0.0.1:{free}/v2/", timeout=5).status
            == 200
        )
        root = next(tmp_path.glob("ocx-zot-*"))
        server.kill()  # SIGKILL: the guard, not the module's own cleanup, must leave nothing
        server.wait()
        assert wait_until(lambda: not _zots_under(root)), "zot outlived the module"
        assert wait_until(lambda: not root.exists())
    finally:
        server.kill()
        server.wait()
        server.stdout.close()


if sys.platform == "win32":
    # Defined only on Windows, never skipped: a skip on Linux counts against test/LINT_SKIP_CEILING (0).
    def test_terminating_the_controller_takes_every_zot_with_it_on_windows(
        tmp_path: Path,
    ) -> None:
        """The Windows twin of the SIGKILL test above: the job object, not a watchdog, ends the zots."""
        # `start()` runs `<binary> serve <config>`, so python runs the script named `serve` in its cwd.
        write_fake_zot(tmp_path / "serve")
        # The base interpreter, not the venv launcher: the launcher holds its child in a
        # kill-on-close job of its own, which ends the zots even with the guard's job removed.
        base_python = getattr(sys, "_base_executable", sys.executable)
        # zot.py is stdlib-only; `src/__init__.py` imports what the base interpreter lacks.
        controller_source = (
            "import importlib.util, json, os, sys, time\n"
            f"spec = importlib.util.spec_from_file_location('zot', {str(TEST_ROOT / 'src' / 'zot.py')!r})\n"
            "zot = importlib.util.module_from_spec(spec)\n"
            "sys.modules['zot'] = zot\n"
            "spec.loader.exec_module(zot)\n"
            f"os.chdir({str(tmp_path)!r})\n"
            "stack = zot.start(sys.executable, timeout=60)\n"
            "print(json.dumps({'self': os.getpid(), 'pids': stack.pids, 'root': str(stack.root)}), flush=True)\n"
            "time.sleep(600)\n"
        )
        env = dict(os.environ)
        # The stores land under tmp_path; the Windows guard leaves them, as it only kills.
        env["TMPDIR"] = env["TEMP"] = env["TMP"] = str(tmp_path)
        env.pop("FAKE_ZOT_MODE", None)
        launcher = subprocess.Popen(
            [base_python, "-c", controller_source],
            cwd=TEST_ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            encoding="utf-8",
            errors="replace",
        )
        pids: list[int] = []
        try:
            line = launcher.stdout.readline()
            assert line, (
                f"the controller exited before announcing its zots: {launcher.stderr.read()}"
            )
            announced = json.loads(line)
            # The fakes' own pid files name the serving processes, whatever `Popen` returned.
            fake_pids = [
                int(pid_file.read_text(encoding="utf-8"))
                for pid_file in Path(announced["root"]).glob("*/pid")
            ]
            assert len(announced["pids"]) == 3 and len(fake_pids) == 3
            pids = sorted({announced["self"], *announced["pids"], *fake_pids})
            assert all(alive(pid) for pid in pids), (
                "the detector sees the controller and its zots while it lives"
            )
            # On Windows this is TerminateProcess: no atexit, no finally.
            os.kill(announced["self"], signal.SIGTERM)
            wait_until(lambda: not any(alive(pid) for pid in pids))
            assert [pid for pid in pids if alive(pid)] == [], (
                "a process outlived the terminated controller"
            )
        finally:
            for pid in pids:
                if alive(pid):  # a red must not leak processes
                    with contextlib.suppress(OSError):
                        os.kill(pid, signal.SIGTERM)
            launcher.kill()
            launcher.wait()
            launcher.stdout.close()
            launcher.stderr.close()
