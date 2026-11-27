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

import json
import re
import shutil
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


def build_recorder(directory: Path) -> Path:
    """Compile the argv recorder to ``directory/recorder.exe``; the Windows stand-in for ``write_stub_ocx``.

    Each start appends one JSON line (``argv``, and the ``OCX_*`` / ``__OCX_*`` environment
    with upper-cased names) to ``<exe>.log`` beside the file it ran as, so copies of one build
    keep separate logs, then exits with ``$RECORDER_EXIT`` (default 0). Needs ``rustc`` on ``PATH``; absent, the test fails naming it.
    """
    rustc = shutil.which("rustc")
    assert rustc, "the Windows argv recorder is compiled with rustc, which is not on PATH"
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "recorder.rs"
    source.write_text(_RECORDER_RS, encoding="utf-8")
    exe = directory / "recorder.exe"
    built = subprocess.run(
        [rustc, "--edition", "2021", "-O", "-o", str(exe), str(source)],
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    assert built.returncode == 0, f"compiling the recorder failed:\n{built.stderr}"
    return exe


def recorder_copy(recorder: Path, directory: Path, name: str = "ocx.exe") -> tuple[Path, Path]:
    """Copy the recorder to ``directory/name``; return ``(exe, its log)``."""
    directory.mkdir(parents=True, exist_ok=True)
    exe = directory / name
    shutil.copy2(recorder, exe)
    return exe, exe.with_suffix(".log")
