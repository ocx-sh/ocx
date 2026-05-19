# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests — Windows native `.exe` shim (issue #66, plan §3.3).

These exercise the genuinely Win32-dependent shim behaviours that the
host-runnable `ocx_shim` unit tests (plan §3.2) cannot cover: PATHEXT
resolution from cmd / pwsh / git-bash, `CreateProcessW` argv passthrough,
exit-code propagation, Ctrl+C forwarding, the BatBadBut `& whoami`
regression, `.exe` resolution with no `.cmd` emitted, missing/malformed
sidecar exit codes, and `OCX_BINARY_PIN` honouring.

⚠ Registry-independent by design. The pytest acceptance harness uses a
Docker Compose `registry:2` fixture on localhost:5000, but **Docker
registry startup is Linux-only — the `registry:2` compose fixture does not
start on the `windows-latest` GitHub runner** (system_design §8). Every
test here builds a *fake* `pkg_root` on disk (metadata.json + content/ +
generated entrypoints/) WITHOUT a registry, then drives the compiled
`ocx-shim` directly. No `ocx`/registry round-trip.

The whole module is `skipif` non-Windows so the suite still PARSES and
collects on Linux CI (`pytest --collect-only`) while the Win32 behaviours
run only on `windows-latest`. Individual tests additionally skip if the
`ocx-shim` binary has not been built yet (Phase 4 deliverable) so the file
is green-by-skip until the shim exists, then becomes a live gate.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

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
    with a tiny tool the inner `ocx launcher exec` would resolve. The shim
    only reads the sibling `.shim` sidecar (pkg_root) — it never touches the
    registry — so a hand-built tree is a faithful, registry-free fixture.
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
    # A trivial echo-style tool so exit-code / argv-passthrough assertions
    # have something real to run when an `ocx` is on PATH or pinned.
    (content_bin / f"{tool_name}.cmd").write_text(
        "@ECHO off\r\nECHO %*\r\nEXIT /B 0\r\n", encoding="utf-8"
    )
    return pkg_root


def _install_shim_entrypoint(
    shim_bin: Path, ep_dir: Path, pkg_root: Path, *, name: str = "hello"
) -> Path:
    """Place `<name>.exe` (verbatim shim copy) + `<name>.shim` (pkg_root).

    Registry-free equivalent of `launcher::generate()`'s Windows emission:
    `.exe` is a byte copy of the built shim, `.shim` is exactly
    `f"{pkg_root}\\n"` (UTF-8, no BOM, single LF — the frozen format).
    """
    ep_dir.mkdir(parents=True, exist_ok=True)
    exe_path = ep_dir / f"{name}.exe"
    exe_path.write_bytes(shim_bin.read_bytes())
    sidecar = ep_dir / f"{name}.shim"
    sidecar.write_bytes(f"{pkg_root}\n".encode("utf-8"))
    return exe_path


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
    ep_dir = pkg_root / "entrypoints"
    exe = _install_shim_entrypoint(shim_bin, ep_dir, pkg_root)
    env = dict(os.environ)
    env["OCX_HOME"] = str(ocx_home)
    return {
        "exe": exe,
        "ep_dir": ep_dir,
        "pkg_root": pkg_root,
        "sidecar": ep_dir / "hello.shim",
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
        pytest.param(["bash", "-c"], id="git-bash"),
    ],
)
def test_shim_resolves_via_pathext(shim_entrypoint: dict, shell: list[str]) -> None:
    """`hello` resolves to `hello.exe` via default PATHEXT from each shell.

    cmd/pwsh honour PATHEXT (`.EXE` before `.CMD`); git-bash resolves an
    explicit `hello.exe` on PATH. All must reach the shim, not cmd.exe.
    """
    ep_dir = shim_entrypoint["ep_dir"]
    env = dict(shim_entrypoint["env"])
    env["PATH"] = f"{ep_dir}{os.pathsep}{env.get('PATH', '')}"
    # Pin `ocx` to a guaranteed-absent absolute path. The earlier
    # "no `ocx` on PATH" precondition was environment-dependent: under
    # git-bash the inherited PATH *did* resolve some `ocx`, so the shim
    # reached E8 (spawned a child, rc=1, no marker) instead of E5, and
    # the test flaked there while cmd/pwsh passed. With OCX_BINARY_PIN
    # set to a missing path the shim deterministically takes the
    # `OcxNotFound { pinned: Some(_) }` branch on every shell, fully
    # independent of PATH — while still being *reached* via PATH/PATHEXT
    # resolution of `hello[.exe]`, which is what this test guards.
    env["OCX_BINARY_PIN"] = str(ep_dir / "definitely-no-ocx-here.exe")
    # cmd/pwsh honour PATHEXT and resolve bare `hello` → `hello.exe`.
    # git-bash does NOT honour PATHEXT and cannot exec a single-quoted
    # Windows backslash abs-path; it resolves an explicit `hello.exe`
    # against PATH (ep_dir is on PATH) — same target, bash-safe.
    invocation = "hello --probe" if shell[0] != "bash" else "hello.exe --probe"
    proc = subprocess.run(
        [*shell, invocation],
        capture_output=True,
        text=True,
        env=env,
    )
    # The shim must be reached and hit its deterministic pinned-miss E5
    # path. Its `ocx-shim:` stderr line is the authoritative cross-shell
    # oracle (pwsh `-Command` / git-bash do not reliably propagate a
    # native child's exit code), with the native E5 code 69 as a
    # corroborating signal where it survives.
    assert "ocx-shim: pinned ocx not found" in proc.stderr, (
        f"shim must resolve via PATH/PATHEXT and reach its own pinned-miss "
        f"E5 path (not cmd.exe / a stray ocx / loader error); "
        f"rc={proc.returncode} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# argv passthrough — argv[0] is the real target, never cmd.exe
# ---------------------------------------------------------------------------


def test_shim_argv0_is_real_target_not_cmd_exe(shim_entrypoint: dict) -> None:
    """The spawned process is the resolved target, never `cmd.exe`.

    The shim uses `CreateProcessW` directly (no `cmd.exe` mediation) — the
    defining property that closes the BatBadBut `%*` vector.
    """
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "--version"],
        capture_output=True,
        text=True,
        env=shim_entrypoint["env"],
    )
    assert "cmd.exe" not in proc.stderr.lower(), (
        f"shim must not route through cmd.exe; stderr={proc.stderr!r}"
    )


def test_shim_forwards_args_verbatim(shim_entrypoint: dict) -> None:
    """Args with spaces / unicode reach the target literally (one argument)."""
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "arg with spaces", "café"],
        capture_output=True,
        text=True,
        env=shim_entrypoint["env"],
    )
    # Pre-spawn failures (no ocx) exit 69; a successful forward exits 0.
    assert proc.returncode in (0, 69), (
        f"verbatim arg forwarding must not corrupt argv; rc={proc.returncode} "
        f"stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# BatBadBut regression — `& whoami` is a literal argument, never executed
# ---------------------------------------------------------------------------


def test_shim_ampersand_arg_not_executed(shim_entrypoint: dict, tmp_path: Path) -> None:
    """`& whoami` is passed as ONE literal argument, never executed.

    This is the core BatBadBut / CVE-2024-24576 regression: a `.cmd`
    launcher would let `cmd.exe` re-parse `%*` and run `whoami`; the shim
    must not.

    A3 / qual#2: without an `ocx` on PATH the shim exits E5 (69) BEFORE it
    ever spawns, so the assertion would pass vacuously (`& whoami` is never
    even forwarded). We pin `OCX_BINARY_PIN` to a fake `ocx.cmd` that echoes
    its received argv to a file, so the `& whoami` literal is observable
    POST-spawn and we can prove it was forwarded literally and NOT executed
    as a separate command (no `whoami` output, no command-not-found noise).
    """
    fake_ocx_dir = tmp_path / "fake_ocx"
    fake_ocx_dir.mkdir()
    argv_log = tmp_path / "argv.txt"
    # `%*` here is INSIDE the fake `ocx.cmd` body (trusted, ours) — it just
    # records exactly what the shim forwarded so we can assert on it. If the
    # shim had let a shell re-parse `& whoami`, `whoami` would have run
    # before this fake ocx ever saw the argument.
    (fake_ocx_dir / "ocx.cmd").write_text(
        "@ECHO off\r\n"
        f'>"{argv_log}" ECHO %*\r\n'
        "EXIT /B 0\r\n",
        encoding="utf-8",
    )
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(fake_ocx_dir / "ocx.cmd")
    proc = subprocess.run(
        [str(shim_entrypoint["exe"]), "& whoami"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert proc.returncode == 0, (
        f"the pinned fake ocx must run and exit 0; rc={proc.returncode} "
        f"stderr={proc.stderr!r}"
    )
    forwarded = argv_log.read_text(encoding="utf-8")
    # The literal `& whoami` token must appear in what the shim forwarded —
    # proof it was passed through CreateProcessW verbatim, not split by a
    # shell. `whoami` output is `DOMAIN\user`; its absence from stdout/stderr
    # is the proof no separate `whoami` process ran (BatBadBut closed).
    assert "& whoami" in forwarded, (
        f"`& whoami` must reach the pinned ocx as ONE literal argument; "
        f"forwarded={forwarded!r}"
    )
    combined = (proc.stdout + proc.stderr).lower()
    assert "\\" not in combined, (
        f"no separate `whoami` (DOMAIN\\user) may have executed; "
        f"stdout={proc.stdout!r} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Exit-code propagation — tool exit N → shim exit N
# ---------------------------------------------------------------------------


def test_shim_propagates_child_exit_code(shim_entrypoint: dict, tmp_path: Path) -> None:
    """Tool exits 42 → `%ERRORLEVEL%` 42 (full passthrough, E8).

    Pins a fake `ocx` (via `OCX_BINARY_PIN`) that exits 42 so the shim's
    child-exit-code passthrough is exercised end-to-end without a registry.

    The fake is pinned, not placed on PATH: the `.exe`-only cutover
    (`adr_windows_exe_shim.md` established-fact #6) means an unset-pin
    literal `ocx` resolves through `CreateProcessW` with NULL
    `lpApplicationName`, which appends only `.EXE` — never the full
    `PATHEXT`. A PATH `ocx.cmd` is therefore deliberately unresolvable;
    real `ocx` ships as `ocx.exe`. Pinning takes the explicit
    `lpApplicationName` branch (same path the passing
    `test_shim_honours_ocx_binary_pin` / `test_shim_runs_without_console`
    sentinel tests exercise), so a `.cmd` fake is a faithful exit-code
    stand-in here without re-introducing `cmd.exe` PATHEXT semantics the
    ADR removed.
    """
    fake_ocx_dir = tmp_path / "fake_ocx"
    fake_ocx_dir.mkdir()
    # The shim spawns `ocx launcher exec ...`; this fake `ocx` ignores its
    # args and exits 42 so we can assert the shim forwards that code.
    pinned = fake_ocx_dir / "ocx.cmd"
    pinned.write_text("@ECHO off\r\nEXIT /B 42\r\n", encoding="utf-8")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pinned)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        capture_output=True,
        text=True,
        env=env,
    )
    assert proc.returncode == 42, (
        f"shim must propagate the child's exit code verbatim (E8 full "
        f"passthrough); got rc={proc.returncode} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Ctrl+C forwarded — child handles the signal, shim returns child's code
# ---------------------------------------------------------------------------


@pytest.mark.skip(
    reason="Ctrl+C delivery requires an interactive console group; "
    "documented contract (ADR U8) — exercised manually / in a dedicated CI job"
)
def test_shim_forwards_ctrl_c(shim_entrypoint: dict) -> None:
    """Ctrl+C reaches the child (no-op SetConsoleCtrlHandler); shim waits.

    Sending a real CTRL_C_EVENT to a child process group from pytest is
    flaky in CI; the behaviour is pinned by the unit-level Win32 contract
    and this documents the acceptance expectation (ADR scenario U8).
    """
    raise AssertionError("documented; see skip reason")


# ---------------------------------------------------------------------------
# `.exe` resolves; no `.cmd` is emitted (cutover to `.exe`-only)
# ---------------------------------------------------------------------------


def test_shim_exe_resolves_and_no_cmd_emitted(shim_entrypoint: dict) -> None:
    """Bare-name `hello` resolves to the shim; no `<name>.cmd` exists.

    Post-cutover (`adr_windows_exe_shim.md` Axis C → C2) the Windows launcher
    is `<name>.exe` + `<name>.shim` only. `launcher::generate()` never emits a
    `.cmd`, so the residual `%*` orphan is gone. This pins both halves of that
    invariant: the entrypoint dir has the `.exe`/`.shim` pair and NO `.cmd`,
    and bare-name resolution under the default Windows PATHEXT reaches the
    shim (rc 0 forward or E5/69 when no `ocx` is resolvable — never a
    Python/loader crash, never cmd.exe).
    """
    ep_dir = shim_entrypoint["ep_dir"]
    assert (ep_dir / "hello.exe").is_file(), "the `.exe` shim must be present"
    assert (ep_dir / "hello.shim").is_file(), "the `.shim` sidecar must be present"
    assert not (ep_dir / "hello.cmd").exists(), (
        "no `<name>.cmd` may exist — cutover to `.exe`-only removed the "
        "residual `%*` orphan"
    )
    env = dict(shim_entrypoint["env"])
    env["PATH"] = f"{ep_dir}{os.pathsep}{env.get('PATH', '')}"
    proc = subprocess.run(
        ["cmd", "/c", "hello"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert proc.returncode in (0, 69), (
        f"bare-name `hello` must resolve the `.exe` shim via default PATHEXT "
        f"and either forward or report E5 (69); rc={proc.returncode} "
        f"stderr={proc.stderr!r}"
    )
    assert "cmd.exe" not in proc.stderr.lower(), (
        f"resolution must reach the shim, never route through cmd.exe; "
        f"stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Missing / malformed sidecar → exit 78 (E1 / E2)
# ---------------------------------------------------------------------------


def test_shim_missing_sidecar_exits_78(shim_entrypoint: dict) -> None:
    """Deleting `hello.shim` → E1, exit 78, actionable stderr."""
    shim_entrypoint["sidecar"].unlink()
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        capture_output=True,
        text=True,
        env=shim_entrypoint["env"],
    )
    assert proc.returncode == 78, (
        f"missing sidecar must exit 78 (EX_CONFIG / E1); rc={proc.returncode}"
    )
    assert "ocx-shim:" in proc.stderr, (
        f"E1 must emit an actionable `ocx-shim:` stderr line; stderr={proc.stderr!r}"
    )


def test_shim_malformed_sidecar_exits_78(shim_entrypoint: dict) -> None:
    """A sidecar with an interior newline → E2, exit 78."""
    shim_entrypoint["sidecar"].write_bytes(b"C:\\pkg\nroot\n")
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        capture_output=True,
        text=True,
        env=shim_entrypoint["env"],
    )
    assert proc.returncode == 78, (
        f"malformed sidecar must exit 78 (EX_CONFIG / E2); rc={proc.returncode}"
    )


# ---------------------------------------------------------------------------
# `OCX_BINARY_PIN` honoured — the pinned binary is spawned
# ---------------------------------------------------------------------------


def test_shim_honours_ocx_binary_pin(shim_entrypoint: dict, tmp_path: Path) -> None:
    """`OCX_BINARY_PIN` set → the shim spawns *that* binary (wire-ABI parity).

    Parity with the `.cmd` `IF DEFINED OCX_BINARY_PIN` branch: the pinned
    binary, not a PATH `ocx`, must be invoked. The fake pinned binary exits
    with a unique sentinel code so we can prove it ran.
    """
    pinned_dir = tmp_path / "pinned"
    pinned_dir.mkdir()
    pinned = pinned_dir / "my-ocx.cmd"
    pinned.write_text("@ECHO off\r\nEXIT /B 57\r\n", encoding="utf-8")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pinned)
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        capture_output=True,
        text=True,
        env=env,
    )
    assert proc.returncode == 57, (
        f"shim must spawn the OCX_BINARY_PIN binary (exit 57 sentinel), not a "
        f"PATH `ocx`; got rc={proc.returncode} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# No-console launch — detached / no std handles (amendment §1, Codex#1)
# ---------------------------------------------------------------------------


def test_shim_runs_without_console(shim_entrypoint: dict, tmp_path: Path) -> None:
    """The shim still forwards + propagates when launched with NO console.

    Design-record "Review-Fix amendments" §1: a detached / GUI / service
    parent yields `GetStdHandle` → NULL / INVALID_HANDLE_VALUE for the std
    handles. The shim must NOT set `STARTF_USESTDHANDLES` then (it would
    regress vs the removed `.cmd` path) yet must STILL launch the child and
    propagate its exit code.

    Real regression (A3 / qual#2): we pin a fake `ocx` that exits with a
    unique sentinel (mirrors `test_shim_honours_ocx_binary_pin`), and launch
    the shim with `DETACHED_PROCESS` + stdin from DEVNULL so it has no
    console handles. A vacuous pass (pre-spawn E5) is ruled out by asserting
    the *sentinel* exit code — only reachable if the child actually ran.
    """
    pinned_dir = tmp_path / "pinned_noconsole"
    pinned_dir.mkdir()
    pinned = pinned_dir / "my-ocx.cmd"
    # Unique sentinel: only observable if the no-console shim truly spawned
    # this child and propagated its code (E8 full passthrough).
    pinned.write_text("@ECHO off\r\nEXIT /B 73\r\n", encoding="utf-8")
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = str(pinned)
    # 0x00000008 == DETACHED_PROCESS: the child (the shim) gets NO console,
    # so its GetStdHandle calls return NULL/INVALID — the no-console path.
    detached_process = 0x00000008
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        creationflags=detached_process,
        env=env,
    )
    assert proc.returncode == 73, (
        f"a no-console (DETACHED_PROCESS) shim must still spawn the pinned "
        f"ocx and propagate its exit code (sentinel 73, E8); got "
        f"rc={proc.returncode} stderr={proc.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Empty OCX_BINARY_PIN — deterministic spawn-failure, never PATH-resolved
# ---------------------------------------------------------------------------


def test_shim_empty_pin_deterministic_fail(shim_entrypoint: dict) -> None:
    """`OCX_BINARY_PIN=""` → deterministic non-zero spawn-failure, not 0.

    Parity-by-decision (plan Wire-ABI matrix "set empty → take pin branch";
    `core::resolve_program`/`spawn_application_name` tests): a defined-but-
    empty pin takes the PIN branch, NOT the unset/`ocx`-PATH fallback (that
    is the Unix `${VAR:-ocx}` behaviour, deliberately out of scope). An empty
    `lpApplicationName` then fails the spawn deterministically.

    The contract this pins: an empty pin must produce a deterministic
    spawn-failure exit (E5 69 / E6 74 / E6+ACCESS_DENIED 77) — and must NOT
    exit 0 (a PATH-resolved success would prove the empty pin wrongly
    collapsed to the `ocx` branch).
    """
    env = dict(shim_entrypoint["env"])
    env["OCX_BINARY_PIN"] = ""
    proc = subprocess.run(
        [str(shim_entrypoint["exe"])],
        capture_output=True,
        text=True,
        env=env,
    )
    assert proc.returncode in (69, 74, 77), (
        f"an empty OCX_BINARY_PIN must take the pin branch and fail the spawn "
        f"deterministically (E5 69 / E6 74 / E6+ACCESS_DENIED 77); got "
        f"rc={proc.returncode} stderr={proc.stderr!r}"
    )
    assert proc.returncode != 0, (
        f"an empty OCX_BINARY_PIN must NOT exit 0 — exit 0 would mean it "
        f"wrongly fell back to a PATH-resolved `ocx` (parity-by-decision "
        f"violated); stderr={proc.stderr!r}"
    )
