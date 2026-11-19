# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The OpenTelemetry collector the telemetry acceptance tests export to, or a failure that says why.

The binary is pinned as `otelcol` in `[group.test.tools]` of the root `ocx.toml`.
Resolution order: `OCX_TEST_OTELCOL`, then that toolchain entry. With neither, the
test fails: a skipped export proof and a passing one are the same green, so a leg
with no collector must say so with `OCX_TEST_NO_OTELCOL=1`, and the skip then names
that variable.
"""

from __future__ import annotations

import contextlib
import json
import os
import socket
import subprocess
import sys
import time
from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass, field
from pathlib import Path

import pytest
from google.protobuf import json_format
from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import (
    ExportLogsServiceRequest,
)

from src import previous_release
from src.zot import _Guard

PATH_ENV = "OCX_TEST_OTELCOL"
OPT_OUT_ENV = "OCX_TEST_NO_OTELCOL"
GROUP = "test"

_NAME = "otelcol.exe" if os.name == "nt" else "otelcol"
_STARTUP_TIMEOUT_SECONDS = 30.0
_STOP_GRACE_SECONDS = 10.0
_LOG_TAIL_LINES = 20


def toolchain_binary() -> Path | None:
    """The `otelcol` the project toolchain's `test` group composes onto `PATH`, or None."""
    return previous_release.toolchain_binary(_NAME)


def resolve(
    environ: Mapping[str, str] | None = None,
    *,
    toolchain: Callable[[], Path | None] = toolchain_binary,
) -> Path:
    """The `otelcol` path. Fails when there is none, unless the leg opted out."""
    env = os.environ if environ is None else environ
    configured = env.get(PATH_ENV)
    if configured:
        path = Path(configured)
        if not os.access(path, os.X_OK):
            pytest.fail(f"{PATH_ENV}={configured} is not an executable file")
        return path
    lookup_failure = ""
    try:
        found = toolchain()
    except previous_release.LookupFailure as exc:
        found, lookup_failure = None, f"\nThe toolchain lookup failed: {exc}"
    if found is not None:
        return found
    if env.get(OPT_OUT_ENV) == "1":
        pytest.skip(f"no otelcol, and {OPT_OUT_ENV}=1 says this leg has none")
    pytest.fail(
        f"no otelcol: {PATH_ENV} is unset and the `otelcol` pin in `[group.{GROUP}.tools]` of ocx.toml "
        f"has no materialised binary. Run `ocx pull -g {GROUP}` at the repository root, point {PATH_ENV} at "
        f"a collector binary, or set {OPT_OUT_ENV}=1 on a leg that carries no `{GROUP}` group."
        + lookup_failure
    )


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True
    except OSError:
        return False


def _config(port: int, output: Path) -> str:
    # `metrics.level: none` keeps the collector off its default internal-metrics port 8888, which a second
    # collector (another test, another worker) would collide on. json.dumps quotes a Windows path validly.
    return f"""receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:{port}
exporters:
  file:
    path: {json.dumps(str(output))}
    format: json
    flush_interval: 100ms
service:
  telemetry:
    metrics:
      level: none
    logs:
      level: error
  pipelines:
    logs:
      receivers: [otlp]
      exporters: [file]
"""


@dataclass
class Collector:
    endpoint: str  # base URL; the exporter appends /v1/logs
    output: Path
    log: Path
    _proc: subprocess.Popen = field(repr=False)

    def log_tail(self) -> str:
        lines = self.log.read_text(encoding="utf-8", errors="replace").splitlines()
        return "\n".join(lines[-_LOG_TAIL_LINES:])

    def read_exports(self, deadline_s: float = 30) -> list[ExportLogsServiceRequest]:
        """Every request the file exporter has written, once at least one line exists.

        Fails when none arrives by the deadline. Each line is parsed with the protobuf JSON
        mapping, so a key the schema does not know is ignored and one it does is typed.
        """
        deadline = time.monotonic() + deadline_s
        while True:
            text = (
                self.output.read_text(encoding="utf-8") if self.output.exists() else ""
            )
            lines = [line for line in text.splitlines() if line.strip()]
            if lines:
                return [
                    json_format.Parse(
                        line, ExportLogsServiceRequest(), ignore_unknown_fields=True
                    )
                    for line in lines
                ]
            if self._proc.poll() is not None:
                pytest.fail(
                    f"otelcol exited with {self._proc.returncode} before exporting anything:\n{self.log_tail()}"
                )
            if time.monotonic() > deadline:
                pytest.fail(
                    f"otelcol wrote no export to {self.output} within {deadline_s:g}s:\n{self.log_tail()}"
                )
            time.sleep(0.05)


@contextlib.contextmanager
def run_collector(tmp_path: Path, binary: Path | None = None) -> Iterator[Collector]:
    """Start a collector on a free loopback port, with its output file under `tmp_path`; stop it on exit."""
    exe = binary or resolve()
    root = tmp_path / "otelcol"
    root.mkdir(parents=True, exist_ok=False)
    port = _free_port()
    output, log, config = (
        root / "export.jsonl",
        root / "otelcol.log",
        root / "config.yaml",
    )
    config.write_text(_config(port, output), encoding="utf-8")
    argv = [str(exe), "--config", str(config)]
    # Off Windows the zot guard's watchdog kills the process group if pytest dies; on Windows zot's job-object
    # guard would re-assign the controller per test, so the collector is only stopped in the `finally`.
    guard = _Guard(root) if sys.platform != "win32" else None
    with log.open("wb") as log_file:
        if guard is not None:
            proc = guard.spawn(argv, log_file)
        else:
            proc = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=log_file,
                stderr=subprocess.STDOUT,
                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,  # type: ignore[attr-defined]
            )
    collector = Collector(f"http://127.0.0.1:{port}", output, log, proc)
    try:
        deadline = time.monotonic() + _STARTUP_TIMEOUT_SECONDS
        while not _accepts(port):
            if proc.poll() is not None:
                pytest.fail(
                    f"otelcol exited with {proc.returncode} before it listened:\n{collector.log_tail()}"
                )
            if time.monotonic() > deadline:
                pytest.fail(
                    f"otelcol did not accept connections on {port} within {_STARTUP_TIMEOUT_SECONDS:g}s:\n{collector.log_tail()}"
                )
            time.sleep(0.05)
        yield collector
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=_STOP_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        if guard is not None:
            guard.release()
