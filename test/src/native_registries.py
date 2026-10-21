#!/usr/bin/env python3
"""Start / stop the suite's zot registries natively: `native_registries.py start|stop`.

Docker-free twin of compose's `registry`, `target-registry` and `prod-registry`;
`zot` comes from `ocx.toml`. `mirror-registry` (registry:2) has no native build,
so runs set `OCX_TEST_NATIVE_SERVICES=1` and its fixtures skip. Ports follow
`OCX_TEST_{REGISTRY,TARGET,PROD}_PORT`; state lives in `OCX_TEST_NATIVE_DIR`
(default `<tmp>/ocx-native-registries-<registry port>`).
"""

from __future__ import annotations

import csv
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from src.helpers import registry_accepts_writes, registry_is_reachable

ZOT_CONFIG = Path(__file__).resolve().parent.parent / "zot-config.json"
INSTANCES = (
    ("registry", "OCX_TEST_REGISTRY_PORT", "5000"),
    ("target", "OCX_TEST_TARGET_PORT", "5003"),
    ("prod", "OCX_TEST_PROD_PORT", "5004"),
)


def _state_dir() -> Path:
    registry_port = os.environ.get("OCX_TEST_REGISTRY_PORT", "5000")
    default = Path(tempfile.gettempdir()) / f"ocx-native-registries-{registry_port}"
    return Path(os.environ.get("OCX_TEST_NATIVE_DIR", str(default)))


def _alive(pid: int) -> bool:
    if sys.platform == "win32":
        out = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"], capture_output=True, text=True, check=False
        ).stdout
        # CSV row: "zot.exe","1234",...; compare the PID column exactly (pid 12 must not match 1234).
        return any(len(row) > 1 and row[1] == str(pid) for row in csv.reader(out.splitlines()))
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def _is_zot(pid: int) -> bool:
    """True when `pid` is a zot process: a reused pidfile pid must never be killed."""
    if sys.platform == "win32":
        out = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"], capture_output=True, text=True, check=False
        ).stdout
        return any(
            len(row) > 1 and row[1] == str(pid) and row[0].lower().startswith("zot")
            for row in csv.reader(out.splitlines())
        )
    out = subprocess.run(["ps", "-p", str(pid), "-o", "comm="], capture_output=True, text=True, check=False).stdout
    return Path(out.strip()).name.startswith("zot")


def _kill(pid: int) -> None:
    if sys.platform == "win32":
        # /T: zot has no children today, but a stray one would keep the store locked.
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)], capture_output=True, check=False)
        return
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        return
    for _ in range(50):
        if not _alive(pid):
            return
        time.sleep(0.1)
    os.kill(pid, signal.SIGKILL)


def stop() -> None:
    state = _state_dir()
    for name, _, _ in INSTANCES:
        pidfile = state / name / "pid"
        if pidfile.is_file():
            pid = int(pidfile.read_text(encoding="utf-8"))
            if _alive(pid) and _is_zot(pid):
                _kill(pid)
    # Fresh store per run: the stores go with the processes.
    shutil.rmtree(state, ignore_errors=True)


def start() -> None:
    zot = shutil.which("zot")
    if zot is None:
        sys.exit("native_registries: `zot` is not on PATH (project toolchain: `ocx exec -- ...`)")
    state = _state_dir()
    stop()  # leftovers from a crashed run of this same state dir
    ports = [(name, os.environ.get(var, default)) for name, var, default in INSTANCES]
    for name, port in ports:
        if registry_is_reachable(f"127.0.0.1:{port}"):
            sys.exit(f"native_registries: port {port} ({name}) is already served; refusing to start")

    base = json.loads(ZOT_CONFIG.read_text(encoding="utf-8"))
    started: list[tuple[str, str]] = []
    for name, port in ports:
        root = state / name
        (root / "store").mkdir(parents=True)
        config = json.loads(json.dumps(base))  # dedupe stays as in zot-config.json
        config["storage"]["rootDirectory"] = str(root / "store")
        config["http"].update(address="127.0.0.1", port=port)
        (root / "config.json").write_text(json.dumps(config), encoding="utf-8")
        flags = (
            subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
            if sys.platform == "win32"
            else 0
        )
        with (root / "zot.log").open("wb") as log:
            proc = subprocess.Popen(
                [zot, "serve", str(root / "config.json")],
                stdout=log,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                creationflags=flags,
                start_new_session=sys.platform != "win32",
            )
        (root / "pid").write_text(str(proc.pid), encoding="utf-8")
        started.append((name, port))

    deadline = time.monotonic() + 60
    for name, port in started:
        address = f"127.0.0.1:{port}"
        while not (registry_is_reachable(address) and registry_accepts_writes(address)):
            if time.monotonic() > deadline:
                log = (state / name / "zot.log").read_text(encoding="utf-8", errors="replace")
                stop()
                sys.exit(f"native_registries: {name} at {address} not writable in 60 s:\n{log[-2000:]}")
            time.sleep(0.25)
        print(f"native_registries: {name} up at {address}")


if __name__ == "__main__":
    if sys.argv[1:] == ["start"]:
        start()
    elif sys.argv[1:] == ["stop"]:
        stop()
    else:
        sys.exit(__doc__)
