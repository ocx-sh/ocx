"""The suite's zot registries: one set per pytest session, on kernel-chosen ports.

``test/conftest.py::pytest_configure`` calls :func:`start` in the xdist controller
(workers inherit the exported ``OCX_TEST_*_PORT`` variables and never start zot),
``pytest_unconfigure`` calls :meth:`Stack.stop`. Each instance runs ``port: "0"``:
zot binds a free port and logs it, which is the only place the number exists.

A killed controller must not leave zot behind. POSIX: a watchdog child holds the
read end of a pipe whose write end only the controller holds, and kills zot on
EOF. Windows: zot is assigned to a Job Object with KILL_ON_JOB_CLOSE, closed with
the controller's last handle. ``preexec_fn`` is out: it is unsafe in a process
with threads.
"""

from __future__ import annotations

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


def zot_config(store: Path, port: int | None = None) -> dict:
    # dedupe stays on: cross-repository mount reads zot's dedupe cache, and
    # `?mount=&from=` answers 202 instead of 201 without it.
    return {
        "distSpecVersion": "1.1.1",
        "storage": {"rootDirectory": str(store), "dedupe": True},
        "http": {"address": "127.0.0.1", "port": str(port or 0)},
        "log": {"level": "info"},  # the chosen port is logged at info
    }


def resolve_zot() -> str:
    found = os.environ.get(ZOT_ENV) or shutil.which("zot")
    if not found:
        raise ZotError(
            f"zot is not on PATH and {ZOT_ENV} is unset; run under `ocx exec zot -- ...`"
        )
    return found


def _serves_v2(port: int) -> bool:
    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/v2/", timeout=2
        ) as response:
            return response.status == 200
    except urllib.error.URLError, OSError:
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

    def adopt(self, proc: subprocess.Popen) -> None:
        os.write(self._write_end, f"{proc.pid}\n".encode())

    def release(self) -> None:
        """Orderly end: the watchdog exits without killing anything."""
        os.write(self._write_end, _DONE)
        os.close(self._write_end)
        self._watchdog.wait()

    def spawn_kwargs(self) -> dict:
        return {"start_new_session": True}  # own group, so the watchdog can killpg it


class _WindowsGuard:
    _KILL_ON_JOB_CLOSE = 0x2000
    _EXTENDED_LIMIT_INFORMATION = 9
    _SET_QUOTA_AND_TERMINATE = 0x0100 | 0x0001

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
        k32.OpenProcess.restype = wintypes.HANDLE
        k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        k32.SetInformationJobObject.argtypes = [
            wintypes.HANDLE,
            ctypes.c_int,
            wintypes.LPVOID,
            wintypes.DWORD,
        ]
        k32.CloseHandle.argtypes = [wintypes.HANDLE]
        self._k32, self._ctypes = k32, ctypes
        self._root = root
        self._job = k32.CreateJobObjectW(None, None)
        if not self._job:
            raise ZotError(f"CreateJobObjectW failed: {ctypes.get_last_error()}")
        info = Extended()
        info.BasicLimitInformation.LimitFlags = self._KILL_ON_JOB_CLOSE
        if not k32.SetInformationJobObject(
            self._job,
            self._EXTENDED_LIMIT_INFORMATION,
            ctypes.byref(info),
            ctypes.sizeof(info),
        ):
            raise ZotError(f"SetInformationJobObject failed: {ctypes.get_last_error()}")

    def adopt(self, proc: subprocess.Popen) -> None:
        k32 = self._k32
        handle = k32.OpenProcess(self._SET_QUOTA_AND_TERMINATE, False, proc.pid)
        if not handle:
            raise ZotError(
                f"OpenProcess({proc.pid}) failed: {self._ctypes.get_last_error()}"
            )
        try:
            if not k32.AssignProcessToJobObject(self._job, handle):
                raise ZotError(
                    f"AssignProcessToJobObject failed: {self._ctypes.get_last_error()}"
                )
        finally:
            k32.CloseHandle(handle)

    def release(self) -> None:
        self._k32.CloseHandle(
            self._job
        )  # processes are already stopped; nothing is killed here

    def spawn_kwargs(self) -> dict:
        return {
            "creationflags": subprocess.DETACHED_PROCESS
            | subprocess.CREATE_NEW_PROCESS_GROUP
        }  # type: ignore[attr-defined]


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
) -> Stack:
    """Start one zot per name and block until each answers ``GET /v2/`` with 200.

    ``fixed_ports`` pins a name to a port, for the one caller whose output embeds
    the registry host (``tests/fixtures/golden/generate.py``); the suite never does.

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
                stack.ports[name] = pinned
            config.write_text(
                json.dumps(zot_config(root / name / "store", pinned)), encoding="utf-8"
            )
            logs[name] = root / name / "zot.log"
            with logs[name].open("wb") as log:
                proc = subprocess.Popen(
                    [binary, "serve", str(config)],
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    stdin=subprocess.DEVNULL,
                    **guard.spawn_kwargs(),
                )
            stack._procs.append(proc)
            guard.adopt(proc)
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
