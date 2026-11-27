# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Fixtures for starting a home launcher against a stub ``ocx``.

A home launcher (``package``, ``deferred`` or ``toolchain`` kind) builds a frozen argv for
``ocx`` and replaces itself with it. The stub is that ``ocx``: it prints nothing, appends one
JSON line per start to a log (argv, ``__OCX_LAUNCHER_DEPTH``, ``OCX_GLOBAL``, ``OCX_PROJECT``)
and exits 0. POSIX only (``write_stub_ocx``: a ``/bin/sh`` script); Windows refuses a batch
file, so ``build_recorder`` compiles a native ``.exe`` there.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
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
        return self.launcher.with_suffix(".launch")


def install_entrypoint_package(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    name: str = "hello",
    *,
    entrypoint: dict | None = None,
) -> InstalledLauncher:
    """Publish and install a package with one entry point; return its launcher file.

    ``entrypoint`` is the entry point's value object (``command``, ``args``); omitted, it
    dispatches to the package binary of the same name.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        repo,
        tmp_path,
        entrypoints={name: entrypoint} if entrypoint is not None else [name],
        bins=[name],
    )
    ocx.plain("package", "install", "--select", pkg.short)
    root = Path(ocx.json("package", "which", pkg.short)["paths"][pkg.short]["path"])
    launcher = only_generation(root / "launchers") / (
        name + ".exe" if sys.platform == "win32" else name
    )
    assert launcher.is_file(), f"the package must publish a launcher at {launcher}"
    return InstalledLauncher(launcher=launcher, package_root=root)


_RECORDER_RS = r'''
use std::io::Write;
fn q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
fn main() {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| q(&a.to_string_lossy())).collect();
    let env: Vec<String> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().to_uppercase(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| k.starts_with("OCX_") || k.starts_with("__OCX_"))
        .map(|(k, v)| format!("{}:{}", q(&k), q(&v)))
        .collect();
    let log = std::env::current_exe().unwrap().with_extension("log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
    writeln!(f, "{{\"argv\":[{}],\"env\":{{{}}}}}", argv.join(","), env.join(",")).unwrap();
    let code = std::env::var("RECORDER_EXIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    std::process::exit(code);
}
'''


# The first `rustc` call may make the `rustup` proxy install a toolchain, and two installs into one
# `RUSTUP_HOME` collide, so one process at a time compiles; the rest wait for its result.
_BUILD_WAIT = 1500  # seconds a waiter gives the builder: a cold toolchain install plus the compile
_BUILD_STALE = 1200  # a lock file older than this belongs to a builder that died


def _wait_for_exclusive_build(root: Path, exe: Path) -> bool:
    """Take ``root/build.lock`` (exclusive create); ``False`` when ``exe`` appeared while waiting."""
    lock = root / "build.lock"
    deadline = time.monotonic() + _BUILD_WAIT
    while not exe.is_file():
        try:
            os.close(os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY))
            return True
        except FileExistsError:
            with contextlib.suppress(OSError):
                if time.time() - lock.stat().st_mtime > _BUILD_STALE:
                    lock.unlink()
                    continue
        assert time.monotonic() < deadline, f"another process holds {lock} and never published {exe}"
        time.sleep(0.5)
    return False


def build_recorder() -> Path:
    """Compile the argv recorder once per machine; return the shared ``recorder.exe``, the Windows stand-in for ``write_stub_ocx``.

    A start appends one JSON line (``argv`` and the upper-cased ``OCX_*`` / ``__OCX_*`` environment) to
    ``<exe>.log`` beside the file it ran as, then exits with ``$RECORDER_EXIT`` (default 0): copy it with
    ``recorder_copy``, never run it in place. Needs ``rustc`` on ``PATH``.

    The build sits in a source-hash directory under the temp directory, so every xdist worker and module
    shares it, and one process compiles at a time (a rustup toolchain install must not run twice at once).
    The compile writes a private name and publishes with one rename.
    """
    rustc = shutil.which("rustc")
    assert rustc, "the Windows argv recorder is compiled with rustc, which is not on PATH"
    key = hashlib.sha256(_RECORDER_RS.encode()).hexdigest()[:16]
    root = Path(tempfile.gettempdir()) / f"ocx-recorder-{key}"
    exe = root / "recorder.exe"
    if exe.is_file():
        return exe
    root.mkdir(parents=True, exist_ok=True)
    if not _wait_for_exclusive_build(root, exe):
        return exe
    source = root / f"recorder-{os.getpid()}.rs"
    staged = root / f"recorder-{os.getpid()}.exe.tmp"
    try:
        if exe.is_file():
            return exe
        source.write_text(_RECORDER_RS, encoding="utf-8")
        built = subprocess.run(
            [rustc, "--edition", "2021", "-O", "--crate-type", "bin", "-o", str(staged), str(source)],
            capture_output=True,
            text=True,
            timeout=_BUILD_STALE - 60,
            check=False,
        )
        assert built.returncode == 0, f"compiling the recorder failed:\n{built.stderr}"
        os.replace(staged, exe)
        return exe
    finally:
        source.unlink(missing_ok=True)
        staged.unlink(missing_ok=True)
        (root / "build.lock").unlink(missing_ok=True)


def recorder_copy(recorder: Path, directory: Path, name: str = "ocx.exe") -> tuple[Path, Path]:
    """Copy the recorder to ``directory/name``; return ``(exe, its log)``."""
    directory.mkdir(parents=True, exist_ok=True)
    exe = directory / name
    shutil.copy2(recorder, exe)
    return exe, exe.with_suffix(".log")
