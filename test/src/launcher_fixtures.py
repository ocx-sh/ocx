# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Fixtures for starting a home launcher against a stub ``ocx``.

A home launcher (``package``, ``deferred`` or ``toolchain`` kind) builds a
frozen argv for ``ocx`` and replaces itself with it. The stub here is the
``ocx`` it starts: it prints nothing, appends one JSON line per start to a log
(argv, ``__OCX_LAUNCHER_DEPTH``, ``OCX_GLOBAL``, ``OCX_PROJECT``) and exits 0,
so a test asserts the argv and environment the launcher built without running
a real ``ocx``.

POSIX only: the stub is a ``/bin/sh`` script that execs the test interpreter.
Windows cannot use it because a batch file is refused by the launcher.
"""

from __future__ import annotations

import json
import re
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner

DEPTH_VAR = "__OCX_LAUNCHER_DEPTH"

_GENERATION = re.compile(r"[0-9a-f]{16}")

_STUB_PY = """\
import json, os, sys
with open(sys.argv[1], "a", encoding="utf-8") as log:
    log.write(json.dumps({
        "argv": sys.argv[2:],
        "depth": os.environ.get("__OCX_LAUNCHER_DEPTH"),
        "global": os.environ.get("OCX_GLOBAL"),
        "project": os.environ.get("OCX_PROJECT"),
    }) + "\\n")
"""


def write_stub_ocx(directory: Path, name: str = "ocx") -> tuple[Path, Path]:
    """Write the stub ``ocx`` as ``directory/name``; return ``(stub, log)``."""
    directory.mkdir(parents=True, exist_ok=True)
    log = directory / "stub-calls.jsonl"
    (directory / "_stub.py").write_text(_STUB_PY, encoding="utf-8")
    stub = directory / name
    stub.write_text(
        f'#!/bin/sh\nexec "{sys.executable}" "{directory / "_stub.py"}" "{log}" "$@"\n',
        encoding="utf-8",
    )
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
    return stub, log


def stub_calls(log: Path) -> list[dict]:
    """Every start the stub recorded, in order; empty when it never ran."""
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


def launcher_env(ocx: OcxRunner, **extra: str | None) -> dict[str, str]:
    """The runner env without a depth counter; ``extra`` sets a variable, ``None`` removes it."""
    env = dict(ocx.env)
    env.pop(DEPTH_VAR, None)
    for key, value in extra.items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return env


def run_launcher(
    launcher: Path, *args: str, env: dict[str, str], cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    """Start a launcher file directly, bounded so a loop fails instead of hanging."""
    return subprocess.run(
        [str(launcher), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )


def only_generation(parent: Path) -> Path:
    """The single ``<G>`` (16 hex) child of a ``launchers/`` directory."""
    children = sorted(parent.iterdir()) if parent.is_dir() else []
    assert len(children) == 1 and _GENERATION.fullmatch(children[0].name), (
        f"{parent} must hold exactly one 16-hex generation directory; got {[c.name for c in children]}"
    )
    return children[0]


@dataclass(frozen=True)
class InstalledLauncher:
    launcher: Path
    package_root: Path

    @property
    def record(self) -> Path:
        return self.launcher.with_name(self.launcher.name + ".launch")


def install_entrypoint_package(
    ocx: OcxRunner, repo: str, tmp_path: Path, name: str = "hello"
) -> InstalledLauncher:
    """Publish and install a package with one entry point; return its launcher file."""
    pkg = make_package_with_entrypoints(
        ocx, repo, tmp_path, entrypoints=[name], bins=[name]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    root = Path(ocx.json("package", "which", pkg.short)["paths"][pkg.short]["path"])
    launcher = only_generation(root / "launchers") / name
    assert launcher.is_file(), f"the package must publish a launcher at {launcher}"
    return InstalledLauncher(launcher=launcher, package_root=root)
