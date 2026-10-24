"""The suite's zot registries: one set per pytest session, on kernel-chosen ports.

``test/conftest.py::pytest_configure`` calls :func:`start` in the xdist controller (workers inherit the
exported ``OCX_TEST_*_PORT`` variables); ``pytest_unconfigure`` calls :meth:`Stack.stop`. Each instance
runs ``port: "0"`` and logs the port it bound, the only place the number exists.

A killed controller leaves no zot, at any instant: on POSIX zot starts only after the watchdog child has its
pid (see ``_LAUNCH``); on Windows the controller joins a KILL_ON_JOB_CLOSE Job Object before it spawns.
"""

from __future__ import annotations

import http.client
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO

#: Instance name -> the ``OCX_TEST_<NAME>_PORT`` variable that carries its port.
INSTANCES = ("registry", "target", "prod")
READY_TIMEOUT_SECONDS = 60.0
#: Overrides the ``zot`` lookup on PATH; the Bazel runner points it at a runfile.
ZOT_ENV = "OCX_TEST_ZOT"

_PORT_MESSAGE = "kernel chosen port"
_DONE = b"done\n"

# A line per zot pid, then EOF. EOF without "done" means the controller died:
# kill every group, then take the stores with it. Imports nothing from the suite.
_WATCHDOG = r"""
import os, shutil, signal, sys, time
pids, done = [], False
for line in sys.stdin:
    if line.strip() == "done":
        done = True
    elif line.strip().isdigit():
        pids.append(int(line))
if not done:
    for pid in pids:
        try:
            os.killpg(pid, signal.SIGKILL)
        except OSError:
            pass
    time.sleep(0.5)
    shutil.rmtree(sys.argv[1], ignore_errors=True)
"""


# Execs zot once the controller says "g" (after the watchdog has the pid). EOF first: the controller died.
_LAUNCH = r"""
import os, sys
if sys.stdin.buffer.read(1) != b"g":
    sys.exit(1)
os.execv(sys.argv[1], sys.argv[1:])
"""


class ZotError(RuntimeError):
    """zot could not be started or never became ready; the message names the log."""


def parse_chosen_port(log_text: str) -> int | None:
    """The port zot announces when it is told to listen on ``0``, or None yet."""
    for line in log_text.splitlines():
        if _PORT_MESSAGE not in line:
            continue
        try:
            port = json.loads(line).get("port")
        except ValueError:
            continue
        if isinstance(port, int):
            return port
    return None


def zot_config(
    store: Path, port: int | None = None, address: str = "127.0.0.1"
) -> dict:
    # dedupe stays on: cross-repository mount reads zot's dedupe cache, and
    # `?mount=&from=` answers 202 instead of 201 without it.
    return {
        "distSpecVersion": "1.1.1",
        "storage": {"rootDirectory": str(store), "dedupe": True},
        "http": {"address": address, "port": str(port or 0)},
        "log": {"level": "info"},  # the chosen port is logged at info
    }


def resolve_zot() -> str:
    found = os.environ.get(ZOT_ENV) or shutil.which("zot")
    if not found:
        raise ZotError(
            f"zot is not on PATH and {ZOT_ENV} is unset; run under `ocx exec zot -- ...`"
        )
    return found


# An ambient HTTP_PROXY must not carry the loopback probe somewhere else.
_DIRECT = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def _serves_v2(port: int) -> bool:
    try:
        with _DIRECT.open(f"http://127.0.0.1:{port}/v2/", timeout=2) as response:
            return response.status == 200
    # URLError covers refused and non-2xx; the others escape urllib while zot is half up.
    except (
        urllib.error.URLError,
        ConnectionError,
        TimeoutError,
        http.client.HTTPException,
    ):
        return False


class _PosixGuard:
    def __init__(self, root: Path) -> None:
        read_end, self._write_end = os.pipe()
        self._watchdog = subprocess.Popen(
            [sys.executable, "-c", _WATCHDOG, str(root)],
            stdin=read_end,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,  # survives a signal sent to the controller's group
        )
        os.close(read_end)

    def spawn(self, argv: list[str], log: IO[bytes]) -> subprocess.Popen:
        """Start ``argv`` in a group of its own (so the watchdog can killpg it) after the watchdog knows its pid."""
        proc = subprocess.Popen(
            [sys.executable, "-c", _LAUNCH, *argv],
            stdin=subprocess.PIPE,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,  # never preexec_fn: it can deadlock in a threaded controller
        )
        os.write(self._write_end, f"{proc.pid}\n".encode())
        try:
            proc.stdin.write(b"g")
            proc.stdin.close()
        except BrokenPipeError:
            pass  # the launcher is already gone; the readiness loop reports its exit status
        return proc

    def release(self) -> None:
        """Orderly end: the watchdog exits without killing anything."""
        os.write(self._write_end, _DONE)
        os.close(self._write_end)
        self._watchdog.wait()


class _WindowsGuard:
    """Puts the controller itself in a KILL_ON_JOB_CLOSE job, so every child is born inside it.

    Assigning zot after ``Popen`` leaves an instant in which a killed controller strands it.
    """

    _KILL_ON_JOB_CLOSE = 0x2000
    _EXTENDED_LIMIT_INFORMATION = 9

    def __init__(self, root: Path) -> None:
        import ctypes
        from ctypes import wintypes

        class Basic(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_int64),
                ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class Extended(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", Basic),
                ("IoInfo", ctypes.c_uint64 * 6),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.CreateJobObjectW.restype = wintypes.HANDLE
        k32.CreateJobObjectW.argtypes = [wintypes.LPVOID, wintypes.LPCWSTR]
        k32.GetCurrentProcess.restype = wintypes.HANDLE
        k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        k32.SetInformationJobObject.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            wintypes.LPVOID,
            wintypes.DWORD,
        ]
        k32.CloseHandle.argtypes = [wintypes.HANDLE]
        self._k32, self._ctypes, self._extended = k32, ctypes, Extended
        self._root = root
        self._job = k32.CreateJobObjectW(None, None)
        if not self._job:
            raise ZotError(f"CreateJobObjectW failed: {ctypes.get_last_error()}")
        if not self._set_flags(self._KILL_ON_JOB_CLOSE):
            raise ZotError(f"SetInformationJobObject failed: {ctypes.get_last_error()}")
        if not k32.AssignProcessToJobObject(self._job, k32.GetCurrentProcess()):
            raise ZotError(
                f"AssignProcessToJobObject(controller) failed: {ctypes.get_last_error()}"
            )

    def _set_flags(self, flags: int) -> bool:
        info = self._extended()
        info.BasicLimitInformation.LimitFlags = flags
        return bool(
            self._k32.SetInformationJobObject(
                self._job,
                self._EXTENDED_LIMIT_INFORMATION,
                self._ctypes.byref(info),
                self._ctypes.sizeof(info),
            )
        )

    def spawn(self, argv: list[str], log: IO[bytes]) -> subprocess.Popen:
        return subprocess.Popen(
            argv,
            stdout=log,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            creationflags=subprocess.DETACHED_PROCESS
            | subprocess.CREATE_NEW_PROCESS_GROUP,  # type: ignore[attr-defined]
        )

    def release(self) -> None:
        """Orderly end: the flag goes first, or closing the last handle would kill the controller too."""
        if self._set_flags(0):
            self._k32.CloseHandle(self._job)


_Guard = _WindowsGuard if sys.platform == "win32" else _PosixGuard


@dataclass
class Stack:
    root: Path
    ports: dict[str, int]
    _procs: list[subprocess.Popen] = field(repr=False, default_factory=list)
    _guard: _PosixGuard | _WindowsGuard | None = field(repr=False, default=None)

    @property
    def pids(self) -> list[int]:
        return [proc.pid for proc in self._procs]

    def env(self) -> dict[str, str]:
        """The variables workers read: one port per instance, and the insecure list."""
        env = {
            f"OCX_TEST_{name.upper()}_PORT": str(port)
            for name, port in self.ports.items()
        }
        env["OCX_INSECURE_REGISTRIES"] = f"localhost:{self.ports['registry']}"
        return env

    def stop(self, *, keep_root: bool = False) -> None:
        for proc in self._procs:
            if proc.poll() is None:
                proc.terminate()
        for proc in self._procs:
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        if self._guard is not None:
            self._guard.release()
            self._guard = None
        if not keep_root:
            shutil.rmtree(self.root, ignore_errors=True)


def start(
    zot: str | None = None,
    *,
    names: tuple[str, ...] = INSTANCES,
    timeout: float = READY_TIMEOUT_SECONDS,
    fixed_ports: dict[str, int] | None = None,
    address: str = "127.0.0.1",
) -> Stack:
    """Start one zot per name and block until each answers ``GET /v2/`` with 200.

    ``fixed_ports`` pins a name to a port, for callers that need a stable one (the golden
    generator, the bench, the manual rigs); the suite never does. A pinned port that
    already answers ``GET /v2/`` is refused: it would pass for ours. ``address`` is the
    listen address, widened only for a client in a container.

    Raises ZotError after ``timeout`` seconds, naming the instance's log; the stores
    are kept in that case so the log can be read.
    """
    binary = zot or resolve_zot()
    root = Path(tempfile.mkdtemp(prefix="ocx-zot-"))
    stack = Stack(root=root, ports={})
    logs: dict[str, Path] = {}
    try:
        stack._guard = guard = _Guard(root)
        for name in names:
            (root / name / "store").mkdir(parents=True)
            config = root / name / "config.json"
            pinned = (fixed_ports or {}).get(name)
            if pinned:
                if _serves_v2(pinned):
                    raise ZotError(
                        f"port {pinned} for `{name}` already answers GET /v2/; stop that registry first"
                    )
                stack.ports[name] = pinned
            config.write_text(
                json.dumps(zot_config(root / name / "store", pinned, address)),
                encoding="utf-8",
            )
            logs[name] = root / name / "zot.log"
            with logs[name].open("wb") as log:
                proc = guard.spawn([binary, "serve", str(config)], log)
            stack._procs.append(proc)
        deadline = time.monotonic() + timeout
        pending = dict(zip(names, stack._procs, strict=True))
        while pending:
            for name, proc in list(pending.items()):
                if proc.poll() is not None:
                    raise ZotError(
                        f"zot `{name}` exited with {proc.returncode} before it listened; log: {logs[name]}"
                    )
                port = stack.ports.get(name) or parse_chosen_port(
                    logs[name].read_text(encoding="utf-8", errors="replace")
                )
                if port is not None:
                    stack.ports[name] = port
                    if _serves_v2(port):
                        del pending[name]
            if pending and time.monotonic() > deadline:
                name = next(iter(pending))
                raise ZotError(
                    f"zot `{name}` did not answer GET /v2/ with 200 within {timeout:g} s; log: {logs[name]}"
                )
            if pending:
                time.sleep(0.1)
    except BaseException:
        stack.stop(keep_root=bool(stack._procs))
        raise
    return stack


if __name__ == "__main__":
    # `uv run python -m src.zot [PORT]`: the registry the manual rigs under test/manual/ talk to.
    _port = int(
        sys.argv[1]
        if len(sys.argv) > 1
        else os.environ.get("OCX_TEST_REGISTRY_PORT", "5000")
    )
    _stack = start(names=("registry",), fixed_ports={"registry": _port})
    print(
        f"zot registry on localhost:{_port}; Ctrl-C stops it and drops its data",
        flush=True,
    )
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        _stack.stop()
