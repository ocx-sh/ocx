# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests — Windows native `.exe` shim (issue #66, plan §3.3).

These exercise the genuinely Win32-dependent shim behaviours that the
host-runnable `ocx_shim` unit tests (plan §3.2) cannot cover: PATHEXT
resolution from cmd / pwsh / git-bash, `CreateProcessW` argv passthrough,
exit-code propagation, Ctrl+C forwarding, the BatBadBut `& whoami`
regression, `.exe` resolution with no extensionless script or `.cmd` emitted,
missing/malformed record exit codes, `OCX_BINARY_PIN` honouring and the
program search that never starts a planted `ocx.exe` (V-L11, V-L38).

⚠ Registry-independent by design. The pytest acceptance harness uses a
Docker Compose `registry:2` fixture on localhost:5000, but **Docker
registry startup is Linux-only — the `registry:2` compose fixture does not
start on the `windows-latest` GitHub runner** (system_design §8). Every
test here builds a *fake* `pkg_root` on disk (metadata.json + content/ +
launchers/<G>/NAME.exe + NAME.launch) WITHOUT a registry, then drives the
compiled `ocx-shim` directly. The `ocx` it starts is a compiled argv recorder
(`launcher_fixtures.build_recorder`): a batch file is refused on every rung.

⚠ Every test here executes a **locally built** `ocx-shim` — `_find_shim_binary`
resolves `OCX_SHIM_BINARY` or a `target/` build, and CI sets the former to
`target/debug/ocx-shim.exe`. None of them touches
`crates/ocx_store/src/shims/ocx-shim-*.exe`, the committed blob `include_bytes!`
ships and a home launcher is a byte copy of. That blob is executed only
by `test_trampoline_exec.py`'s Windows cases, which deliberately do NOT use
`OCX_SHIM_BINARY` or the `shim_entrypoint` fixture below. Do not "unify" the two
fixtures: folding them would silently un-test the shipped artefact again.

The whole module is `skipif` non-Windows so the suite still PARSES and
collects on Linux CI (`pytest --collect-only`) while the Win32 behaviours
run only on `windows-latest`. Individual tests additionally skip if the
`ocx-shim` binary has not been built yet (Phase 4 deliverable) so the file
is green-by-skip until the shim exists, then becomes a live gate.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import bash_executable
from src.launcher_fixtures import build_recorder, recorder_copy

# Parse on Linux, run only on Windows: a module-level skip keeps collection
# clean on the Linux leg (system_design §8 — registry fixture is Linux-only,
# so these must never depend on it and never execute off Windows).
pytestmark = pytest.mark.skipif(
    sys.platform != "win32",
    reason="Windows native shim behaviour; Win32-only (registry-independent, runs on windows-latest)",
)


# ---------------------------------------------------------------------------
# Registry-independent fixture: a fake pkg_root + shim entrypoint on disk
# ---------------------------------------------------------------------------


def _find_shim_binary() -> Path | None:
    """Locate the compiled `ocx-shim` executable.

    Built by `cargo build -p ocx_shim` (Phase 4). Returned `None` (→ skip)
    until it exists, so this file is green-by-skip pre-implementation and a
    live gate afterwards.
    """
    repo_root = Path(__file__).resolve().parents[2]
    candidates = [
        repo_root / "target" / "release" / "ocx-shim.exe",
        repo_root / "target" / "debug" / "ocx-shim.exe",
    ]
    env_override = os.environ.get("OCX_SHIM_BINARY")
    if env_override:
        candidates.insert(0, Path(env_override))
    for cand in candidates:
        if cand.is_file():
            return cand
    return None


def _make_fake_pkg_root(base: Path, *, tool_name: str = "hello") -> Path:
    """Build a fake `pkg_root` on disk WITHOUT a registry.

    Layout mirrors an assembled package: `metadata.json` + `content/bin/`
    with a tiny tool. The launcher only reads its sibling `.launch` record and
    builds a frozen command line for `ocx`, so a hand-built tree is a faithful,
    registry-free fixture.
    """
    pkg_root = base / "packages" / "ocx.sh" / "sha256" / "ab" / "cd"
    content_bin = pkg_root / "content" / "bin"
    content_bin.mkdir(parents=True, exist_ok=True)
    metadata = {
        "schemaVersion": 1,
        "entrypoints": {tool_name: {}},
        "env": [
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
            }
        ],
    }
    (pkg_root / "metadata.json").write_text(json.dumps(metadata), encoding="utf-8")
    (content_bin / f"{tool_name}.cmd").write_text(
        "@ECHO off\r\nECHO %*\r\nEXIT /B 0\r\n", encoding="utf-8"
    )
    return pkg_root


def _install_shim_entrypoint(
    shim_bin: Path, ep_dir: Path, pkg_root: Path, *, name: str = "hello"
) -> Path:
    """Place `<name>.exe` (verbatim shim copy) + `<name>.launch` (kind `package`).

    Registry-free equivalent of the home launcher a package's first use
    publishes: the `.exe` is a byte copy of the built launcher, the record is
    the one-line JSON the launcher decodes, and nothing else sits beside them.
    """
    ep_dir.mkdir(parents=True, exist_ok=True)
    exe_path = ep_dir / f"{name}.exe"
    exe_path.write_bytes(shim_bin.read_bytes())
    record = {"kind": "package", "schema_version": 1, "package_root": str(pkg_root)}
    (ep_dir / f"{name}.launch").write_bytes((json.dumps(record) + "\n").encode())
    return exe_path


def _holds_ocx(directory: Path) -> bool:
    """Whether `directory` holds anything a bare `ocx` would resolve to."""
    return any(
        (directory / f"ocx{ext}").is_file() for ext in (".exe", ".com", ".cmd", ".bat")
    )


@pytest.fixture(scope="module")
def recorder_exe(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A native argv recorder, compiled once (see `launcher_fixtures.build_recorder`)."""
    return build_recorder(tmp_path_factory.mktemp("recorder"))


def _recorded(pin: Path) -> list[dict]:
    """Every start the recorder at `pin` logged, in order; empty when it never ran."""
    log = pin.with_suffix(".log")
    if not log.is_file():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


@pytest.fixture()
def shim_entrypoint(tmp_path: Path) -> dict:
    """A built, registry-independent shim entrypoint ready to invoke.

    Skips (not fails) until `ocx-shim` is built so the suite is green on a
    fresh tree and a live gate once Phase 4 lands.
    """
    shim_bin = _find_shim_binary()
    if shim_bin is None:
        pytest.skip("ocx-shim binary not built yet (Phase 4 deliverable)")
    ocx_home = tmp_path / "ocx_home"
    pkg_root = _make_fake_pkg_root(ocx_home)
    ep_dir = pkg_root / "launchers" / "0123456789abcdef"
    exe = _install_shim_entrypoint(shim_bin, ep_dir, pkg_root)
    env = dict(os.environ)
    env["OCX_HOME"] = str(ocx_home)
    # No ambient ocx. Unpinned, the shim spawns a bare `ocx` through the
    # CreateProcessW search, so an ocx the host or runner carries on PATH
    # (setup-ocx puts one there) would decide each outcome - and a released
    # ocx exits 65 on this fixture's hand-written metadata. A test that wants
    # a forward pins its own recorder via OCX_BINARY_PIN.
    env.pop("OCX_BINARY_PIN", None)
    env["PATH"] = os.pathsep.join(
        d
        for d in env.get("PATH", "").split(os.pathsep)
        if d and not _holds_ocx(Path(d))
    )
    return {
        "exe": exe,
        "ep_dir": ep_dir,
        "pkg_root": pkg_root,
        "record": ep_dir / "hello.launch",
        "env": env,
    }


# ---------------------------------------------------------------------------
# Resolution from cmd / pwsh / git-bash with default PATHEXT
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "shell",
    [
        pytest.param(["cmd", "/c"], id="cmd"),
        pytest.param(["pwsh", "-NoProfile", "-Command"], id="pwsh"),
        # MSYS2 and Git Bash run `NAME.exe` when `NAME.exe` is typed. The bash
        # binary is resolved to Git-for-Windows bash in the body (a bare
        # "bash" is the WSL launcher on the runner - see bash_executable).
        pytest.param(["bash", "-c"], id="git-bash"),
    ],
)
def test_launcher_starts_from_each_shell_with_only_exe_and_launch_on_disk(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path, shell: list[str]
) -> None:
    """V-L38: `NAME` starts from cmd, PowerShell and Git Bash; the directory holds `.exe` and `.launch` only."""
    cmd = list(shell)
    if cmd[0] == "bash":
        gb = bash_executable()
        if gb is None:
            pytest.skip("bash.exe not found: Git-for-Windows bash is absent (bare bash is the WSL launcher)")
        cmd[0] = gb
    elif shutil.which(cmd[0]) is None:
        pytest.skip(f"{cmd[0]} not installed")
    ep_dir = shim_entrypoint["ep_dir"]
    assert sorted(p.name for p in ep_dir.iterdir()) == ["hello.exe", "hello.launch"], (
        "a Windows home holds NAME.exe and NAME.launch only: no extensionless script, `.shim`, `.exec` or `.cmd`"
    )
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env["PATH"] = f"{ep_dir}{os.pathsep}{env.get('PATH', '')}"
    env["OCX_BINARY_PIN"] = str(pin)
    # cmd/pwsh resolve bare `hello` through PATHEXT; git-bash does not honour PATHEXT, so it
    # types the `.exe` it resolves on PATH - same file.
    invocation = "hello.exe --probe" if shell[0] == "bash" else "hello --probe"
    proc = subprocess.run(
        [*cmd, invocation], capture_output=True, text=True, env=env, check=False
    )
    started = _recorded(pin)
    assert [call["argv"] for call in started] == [
        ["launcher", "exec", str(shim_entrypoint["pkg_root"]), "--", "hello", "--probe"]
    ], (
        f"`hello` must resolve to the launcher and start the pinned ocx once; "
        f"rc={proc.returncode} stderr={proc.stderr!r} stdout={proc.stdout!r}"
    )


# ---------------------------------------------------------------------------
# argv passthrough - argv[0] is the real target, never cmd.exe
# ---------------------------------------------------------------------------


def test_launcher_argv0_is_real_target_not_cmd_exe(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """The spawned process is the pinned program itself, never `cmd.exe` (CreateProcessW, no shell)."""
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pin)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "--version"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert proc.returncode == 0, f"rc={proc.returncode} stderr={proc.stderr!r}"
    assert "cmd.exe" not in proc.stderr.lower(), f"stderr={proc.stderr!r}"
    assert len(_recorded(pin)) == 1, "the pinned program itself must have run"


def test_launcher_forwards_args_verbatim(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """Args with spaces and unicode reach the program as single, unchanged arguments."""
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pin)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "arg with spaces", "caf\u00e9"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert proc.returncode == 0, f"rc={proc.returncode} stderr={proc.stderr!r}"
    (call,) = _recorded(pin)
    assert call["argv"] == [
        "launcher", "exec", str(shim_entrypoint["pkg_root"]), "--", "hello",
        "arg with spaces", "caf\u00e9",
    ], "the frozen `launcher exec` line, user args verbatim"


# ---------------------------------------------------------------------------
# BatBadBut regression - `& whoami` is a literal argument, never executed
# ---------------------------------------------------------------------------


def test_launcher_ampersand_arg_not_executed(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """`& whoami` is passed as ONE literal argument, never executed (CVE-2024-24576 class).

    The recorder logs the argv it received, so the literal is observable after the
    spawn; a shell that re-parsed it would have run `whoami` first.
    """
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pin)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "& whoami"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert proc.returncode == 0, f"rc={proc.returncode} stderr={proc.stderr!r}"
    (call,) = _recorded(pin)
    assert call["argv"][-1] == "& whoami", f"one literal argument; got {call['argv']!r}"
    combined = (proc.stdout + proc.stderr).lower()
    assert "\\" not in combined, (
        f"no separate `whoami` (DOMAIN\\user) may have executed; stdout={proc.stdout!r} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Exit-code propagation - program exit N -> launcher exit N
# ---------------------------------------------------------------------------


def test_launcher_propagates_child_exit_code(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """The program exits 42 -> the launcher exits 42 (full passthrough)."""
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env.update({"OCX_BINARY_PIN": str(pin), "RECORDER_EXIT": "42"})
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True, env=env, check=False
    )
    assert proc.returncode == 42, f"rc={proc.returncode} stderr={proc.stderr!r}"


# ---------------------------------------------------------------------------
# Ctrl+C forwarded - child handles the signal, launcher returns child's code
# ---------------------------------------------------------------------------


@pytest.mark.skip(
    reason="Ctrl+C delivery requires an interactive console group; "
    "documented contract (ADR U8) - exercised manually / in a dedicated CI job"
)
def test_launcher_forwards_ctrl_c(shim_entrypoint: dict) -> None:
    """Ctrl+C reaches the child (no-op SetConsoleCtrlHandler); the launcher waits."""
    raise AssertionError("documented; see skip reason")


# ---------------------------------------------------------------------------
# Program search (V-L11) and listing (V-L38)
# ---------------------------------------------------------------------------


def test_launcher_directory_holds_exe_and_launch_only_and_bare_name_reaches_the_launcher(
    shim_entrypoint: dict,
) -> None:
    """V-L38: no extensionless script, `.shim`, `.exec` or `.cmd`; bare `hello` still reaches the launcher.

    With no ocx on PATH and no pin the launcher's PATH walk finds none and exits 78
    "no ocx found on PATH" (rung 3): reaching that message proves the `.exe` ran.
    """
    ep_dir = shim_entrypoint["ep_dir"]
    assert sorted(p.name for p in ep_dir.iterdir()) == ["hello.exe", "hello.launch"]
    env = dict(shim_entrypoint["env"])
    env["PATH"] = f"{ep_dir}{os.pathsep}{env.get('PATH', '')}"
    proc = subprocess.run(
        ["cmd", "/c", "hello"], capture_output=True, text=True, env=env, check=False
    )
    assert proc.returncode == 78 and "no ocx found on PATH" in proc.stderr, (
        f"rc={proc.returncode} stderr={proc.stderr!r}"
    )
    assert "cmd.exe" not in proc.stderr.lower()


def test_launcher_never_starts_a_planted_ocx_exe_in_cwd_or_its_own_directory(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """V-L11: an `ocx.exe` in the working directory and one beside the launcher are never started.

    Both plants are recorders; the real `ocx` (a third recorder) sits on PATH. Only that
    one may log a start, because `lpApplicationName` is always set and PATH entries that
    are the launcher's own directory are skipped.
    """
    cwd = tmp_path / "cwd"
    plant_cwd, _ = recorder_copy(recorder_exe, cwd, "ocx.exe")
    plant_own, _ = recorder_copy(recorder_exe, shim_entrypoint["ep_dir"], "ocx.exe")
    real, _ = recorder_copy(recorder_exe, tmp_path / "real_ocx_dir", "ocx.exe")
    env = dict(shim_entrypoint["env"])
    env["PATH"] = os.pathsep.join(
        [str(shim_entrypoint["ep_dir"]), str(real.parent), env["PATH"]]
    )
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "x"],
        cwd=cwd, capture_output=True, text=True, env=env, check=False,
    )
    assert proc.returncode == 0, f"rc={proc.returncode} stderr={proc.stderr!r}"
    assert len(_recorded(real)) == 1, "the real ocx on PATH must run"
    assert _recorded(plant_cwd) == [], "an ocx.exe in the working directory must never start"
    assert _recorded(plant_own) == [], "an ocx.exe beside the launcher must never start"


# ---------------------------------------------------------------------------
# Missing / malformed record -> exit 78
# ---------------------------------------------------------------------------


def test_launcher_missing_record_exits_78(shim_entrypoint: dict) -> None:
    """Deleting `hello.launch` -> exit 78 naming the missing record."""
    shim_entrypoint["record"].unlink()
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True,
        env=shim_entrypoint["env"], check=False,
    )
    assert proc.returncode == 78, f"rc={proc.returncode} stderr={proc.stderr!r}"
    assert "hello.launch" in proc.stderr, f"stderr={proc.stderr!r}"


def test_launcher_malformed_record_exits_78(shim_entrypoint: dict) -> None:
    """A record that is not JSON -> exit 78."""
    shim_entrypoint["record"].write_bytes(b"C:\\pkg\nroot\n")
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True,
        env=shim_entrypoint["env"], check=False,
    )
    assert proc.returncode == 78, f"rc={proc.returncode} stderr={proc.stderr!r}"


# ---------------------------------------------------------------------------
# `OCX_BINARY_PIN` honoured - the pinned program is spawned
# ---------------------------------------------------------------------------


def test_launcher_honours_ocx_binary_pin(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """A pinned program that is NOT on PATH runs: its sentinel exit code comes back (rung 1)."""
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "my-ocx.exe")
    env = dict(shim_entrypoint["env"])
    env.update({"OCX_BINARY_PIN": str(pin), "RECORDER_EXIT": "57"})
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True, env=env, check=False
    )
    assert proc.returncode == 57, f"rc={proc.returncode} stderr={proc.stderr!r}"


def test_launcher_refuses_a_batch_file_pin_with_78(
    shim_entrypoint: dict, tmp_path: Path
) -> None:
    """a pin naming a batch file is refused with 78 on every rung, never started."""
    marker = tmp_path / "ran.txt"
    batch = tmp_path / "pinned" / "ocx.cmd"
    batch.parent.mkdir()
    batch.write_text(f'@ECHO off\r\n>"{marker}" ECHO ran\r\n', encoding="utf-8")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(batch)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True, env=env, check=False
    )
    assert proc.returncode == 78, f"rc={proc.returncode} stderr={proc.stderr!r}"
    assert not marker.exists(), "a batch program must never be started"


# ---------------------------------------------------------------------------
# No-console launch - detached / no std handles
# ---------------------------------------------------------------------------


def test_launcher_runs_without_console(
    shim_entrypoint: dict, recorder_exe: Path, tmp_path: Path
) -> None:
    """The launcher still starts the program and propagates its code with NO console (DETACHED_PROCESS).

    The sentinel 73 is only reachable if the child actually ran.
    """
    pin, _ = recorder_copy(recorder_exe, tmp_path / "pinned", "my-ocx.exe")
    env = dict(shim_entrypoint["env"])
    env.update({"OCX_BINARY_PIN": str(pin), "RECORDER_EXIT": "73"})
    detached_process = 0x00000008
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        stdin=subprocess.DEVNULL, capture_output=True,
        creationflags=detached_process, env=env, check=False,
    )
    assert proc.returncode == 73, f"rc={proc.returncode} stderr={proc.stderr!r}"


# ---------------------------------------------------------------------------
# Empty OCX_BINARY_PIN - an empty pin is no pin (rung 1 is "when non-empty")
# ---------------------------------------------------------------------------


def test_launcher_empty_pin_falls_through_to_the_path_walk(shim_entrypoint: dict) -> None:
    """`OCX_BINARY_PIN=""` is treated as unset: with no ocx on PATH the walk fails with 78."""
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = ""
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])], capture_output=True, text=True, env=env, check=False
    )
    assert proc.returncode == 78 and "no ocx found on PATH" in proc.stderr, (
        f"rc={proc.returncode} stderr={proc.stderr!r}"
    )
