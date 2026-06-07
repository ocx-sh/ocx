# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package test --script`` (embedded Starlark runner).

Specification-phase tests: written from the design record
(``plan_package_test_scripting.md`` UX scenarios U1-U23 + Component Contracts
C1-C9 + Error Taxonomy + ``adr_package_test_scripting.md``), NOT from the
implementation. They drive the real ``ocx`` binary against a real materialized
package and MUST fail against the current ``unimplemented!()`` script branch
(the binary lacks the behaviour). Each test maps explicitly to a scenario ID in
its name/docstring.

Exit codes (``crates/ocx_lib/src/cli/exit_code.rs`` + ADR Exit Code Scheme):
  0  = Success      (script passed)
  1  = Failure      (assertion fail / host-fn fail / timeout / re-entrant ocx)
  64 = UsageError   (--script + cmd both; neither; missing script path)
  65 = DataError    (syntax / arity / type / invalid regex)
  74 = IoError      (--script - stdin read failure; scratch I/O)

There is NO exit code 2 (the enum has none; engine-internal → 1).

TOCTOU is NOT gated here (inherently racy) — left to /security-auditor per the
plan's TOCTOU note. Symlink-escape (U13/U14) IS gated: deterministic.

Linux-only marker: the scripted-test CI *leg* is linux-only (ADR Testbed & CI /
plan Step 4.7-4.9) while the approach is being validated. The pytest harness
itself is cross-platform, but the symlink-escape scenarios (U13/U14) rely on
POSIX ``ln -s`` behaviour, so the whole module is gated to non-Windows.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

from src import OcxRunner, current_platform
from src.helpers import make_package
from src.runner import PackageInfo

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="scripted-test leg is linux/macOS only while the approach is validated "
    "(ADR Testbed & CI); symlink-escape scenarios need POSIX `ln -s`",
)

_PLATFORM = current_platform()

# A deterministic version string the test tool prints, so assertions like
# `expect.contains(r.stdout, "v3")` are stable and version-agnostic.
_TOOL_VERSION = "v3.7.0"


# ---------------------------------------------------------------------------
# Package + script fixtures (DAMP — self-contained per quality-core.md)
# ---------------------------------------------------------------------------

_SHTOOL_SCRIPT = (
    "#!/bin/sh\n"
    'case "$1" in\n'
    f'  --version) echo "{_TOOL_VERSION}" ;;\n'
    '  --badflag) echo "unknown flag" 1>&2 ; exit 3 ;;\n'
    # ~11 MiB of 'x' to exceed the 10 MiB capture cap.
    '  --spew) yes x | head -c 11534336 ;;\n'
    '  mklink) ln -s / "$2" ;;\n'
    '  --echo-env) eval "echo \\$$2" ;;\n'
    '  *) : ;;\n'
    "esac\n"
)
"""Deterministic ``shtool`` shell script body used by all scripted-test cases.

Modes:
  ``--version``        → prints ``v3.7.0`` to stdout, exit 0
  ``--badflag``        → prints to stderr, exit 3 (non-zero)
  ``--spew``           → emits > 10 MiB to stdout (truncation cap test)
  ``mklink DST``       → ``ln -s / DST`` inside CWD (symlink-escape setup)
  ``--echo-env VAR``   → prints the child env value of VAR
  (no args)            → exit 0
"""


@pytest.fixture()
def script_test_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[Path, Path, PackageInfo]:
    """Publish the ``shtool`` test package and return ``(bundle, metadata, pkg)``.

    Delegates build/push/index to ``make_package()`` via the ``bin_scripts``
    extension, so there is a single construction path for all scripted-test
    cases.  Function-scoped (default) for test isolation; ``unique_repo`` and
    ``tmp_path`` are also function-scoped.
    """
    tag = "1.0.0"
    home_key = unique_repo.upper().replace("-", "_") + "_HOME"
    pkg = make_package(
        ocx,
        unique_repo,
        tag,
        tmp_path,
        bins=["shtool"],
        bin_scripts={"shtool": _SHTOOL_SCRIPT},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            },
            {
                "key": home_key,
                "type": "constant",
                "value": "${installPath}",
                "visibility": "public",
            },
        ],
        # cascade=False: no cross-tag indexing needed; index by short ref only.
        cascade=False,
    )
    bundle = tmp_path / f"bundle-{unique_repo}-{tag}.tar.xz"
    metadata_path = tmp_path / f"metadata-{unique_repo}-{tag}.json"
    return bundle, metadata_path, pkg


def _write_script(tmp_path: Path, name: str, body: str) -> Path:
    path = tmp_path / name
    path.write_text(body)
    return path


def _run_script(
    ocx: OcxRunner,
    bundle: Path,
    metadata_path: Path,
    pkg: PackageInfo,
    *,
    script: str | Path,
    extra_args: tuple[str, ...] = (),
    stdin: str | None = None,
    fmt: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Invoke ``ocx package test --script`` against the materialized package.

    ``script`` is a path, or the literal ``"-"`` for the stdin-source form
    (U18). ``stdin`` (independent of ``--script -``) feeds the parent process
    stdin; for ``--script -`` it carries the script SOURCE.
    """
    cmd = [str(ocx.binary)]
    if fmt:
        cmd += ["--format", fmt]
    cmd += [
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg.short,
        str(bundle),
        *extra_args,
        "--script", str(script),
    ]
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=ocx.env,
        input=stdin,
        timeout=120.0,
    )


# ---------------------------------------------------------------------------
# U1 — happy path, hermetic tool package
# ---------------------------------------------------------------------------


def test_u1_happy_path_script_passes(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U1: run a tool, expect.ok + expect.contains + ocx.env compose check → exit 0."""
    bundle, meta, pkg = script_test_package
    home_key = unique_repo.upper().replace("-", "_") + "_HOME"
    smoke = _write_script(
        tmp_path,
        "smoke.star",
        f'r = ocx.run("shtool", "--version")\n'
        f"expect.ok(r)\n"
        f'expect.contains(r.stdout, "v3")\n'
        f'expect.true(ocx.env("{home_key}") != None)\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U1: expected exit 0, got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U2 / U3 — clap mutual-exclusion + required-unless-present (exit 64)
# ---------------------------------------------------------------------------


def test_u2_script_and_trailing_command_conflict(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U2: --script AND a trailing command → clap conflict, exit 64."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(tmp_path, "s.star", 'r = ocx.run("shtool", "--version")\n')

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(meta),
        "--script", str(smoke),
        "-i", pkg.short,
        str(bundle),
        "--",
        "shtool", "--version",
        check=False,
    )

    assert result.returncode == 64, (
        f"U2: both forms supplied must exit 64 (clap conflicts_with), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_u3_neither_script_nor_command(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U3: neither --script nor a trailing command → exit 64.

    Distinct from U2: this exercises `required_unless_present = "script"` on
    the `command` field, NOT `conflicts_with` (plan C1 Fix#10/#12).
    """
    bundle, meta, pkg = script_test_package

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(meta),
        "-i", pkg.short,
        str(bundle),
        check=False,
    )

    assert result.returncode == 64, (
        f"U3: neither form supplied must exit 64 (required_unless_present), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U4 — missing script path (exit 64, NOT 74)
# ---------------------------------------------------------------------------


def test_u4_missing_script_file(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U4: --script /no/such.star → exit 64; the message names the path."""
    bundle, meta, pkg = script_test_package
    missing = tmp_path / "no" / "such.star"

    result = _run_script(ocx, bundle, meta, pkg, script=missing)

    assert result.returncode == 64, (
        f"U4: missing script path must exit 64 (UsageError, NOT 74), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert "such.star" in result.stderr, (
        f"U4: error must name the missing path, stderr: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# U5 — assertion failure (exit 1, message includes failing assertion)
# ---------------------------------------------------------------------------


def test_u5_assertion_failure(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U5: a failing assertion → exit 1 (NEVER Err); failure surfaced."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "fail.star",
        'r = ocx.run("shtool", "--version")\n'
        'expect.contains(r.stdout, "v9")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U5: assertion failure must exit 1, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U6 — syntax error in script (exit 65)
# ---------------------------------------------------------------------------


def test_u6_syntax_error(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U6: a parser error in the script → exit 65 (DataError)."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(tmp_path, "broken.star", "def (:\n")

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 65, (
        f"U6: syntax error must exit 65 (DataError), got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U7 / U8 — lexical sandbox escapes (exit 1, nothing leaked/written)
# ---------------------------------------------------------------------------


def test_u7_sandbox_escape_read(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U7: ocx.read_file("../../etc/passwd") → exit 1; no host content emitted."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "escread.star",
        'c = ocx.read_file("../../../../../../etc/passwd")\n'
        "print(c)\n",
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U7: lexical read escape must exit 1 (Failed), got {result.returncode}"
    )
    assert "root:" not in result.stdout, (
        "U7: host /etc/passwd content must NOT be emitted on a guard rejection"
    )


def test_u8_sandbox_escape_write(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U8: ocx.write_file outside scratch → exit 1; nothing written outside."""
    bundle, meta, pkg = script_test_package
    target = tmp_path / "escape_marker"
    smoke = _write_script(
        tmp_path,
        "escwrite.star",
        f'ocx.write_file("../../../../../../../../{target}", "leaked")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U8: lexical write escape must exit 1 (Failed), got {result.returncode}"
    )
    assert not target.exists(), (
        "U8: nothing must be written outside the scratch root"
    )


# ---------------------------------------------------------------------------
# U9 — non-zero ocx.run does NOT auto-fail
# ---------------------------------------------------------------------------


def test_u9_nonzero_run_does_not_autofail(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U9: a non-zero child + script asserts the non-zero itself → exit 0.

    Asserts `r.exit_code != 0` (NOT `== 3` — the exact value is fragile; the
    ADR only requires "non-zero does not auto-fail").
    """
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "nonzero.star",
        'r = ocx.run("shtool", "--badflag")\n'
        "expect.true(r.exit_code != 0)\n",
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U9: non-zero ocx.run must not auto-fail; script asserted it → exit 0, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U10 — --keep preserves scratch for debugging
# ---------------------------------------------------------------------------


def test_u10_keep_preserves_scratch_on_failure(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U10: U5 with --keep → exit 1, roots preserved, path printed to stderr."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "keepfail.star",
        'r = ocx.run("shtool", "--version")\n'
        'expect.contains(r.stdout, "v9")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke, extra_args=("--keep",))

    assert result.returncode == 1, (
        f"U10: kept failing run must exit 1, got {result.returncode}"
    )
    assert "kept at " in result.stderr, (
        f"U10: kept path must be printed to stderr, got: {result.stderr!r}"
    )
    m = re.search(r"kept at (\S+)", result.stderr)
    assert m is not None, f"U10: no kept path in stderr: {result.stderr!r}"
    assert Path(m.group(1)).exists(), (
        "U10: --keep must preserve the package/scratch tree for debugging"
    )


# ---------------------------------------------------------------------------
# U11 — empty script (exit 0)
# ---------------------------------------------------------------------------


def test_u11_empty_script(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U11: an empty script → exit 0 (no statements, no assertions = success)."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(tmp_path, "empty.star", "")

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U11: empty script must exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U12 — output truncation cap
# ---------------------------------------------------------------------------


def test_u12_output_truncation_flag(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U12: a child emitting > cap bytes → r.truncated is True; exit 0."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "trunc.star",
        'r = ocx.run("shtool", "--spew")\n'
        "expect.true(r.truncated)\n",
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U12: truncation asserted by script must exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U13 / U14 — symlink-escape (Codex C1): read + cwd
# ---------------------------------------------------------------------------


def test_u13_sandbox_escape_symlink_on_read(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U13: package binary creates scratch/esc -> /, then read through it → exit 1.

    Also covers the ocx.exists variant (read side is NOT exempt from the C1
    symlink re-check).
    """
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "symread.star",
        'ocx.run("shtool", "mklink", ocx.scratch_root + "/esc")\n'
        'c = ocx.read_file("esc/etc/passwd")\n'
        "print(c)\n"
        'b = ocx.exists("esc/etc/passwd")\n'
        "print(b)\n",
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U13: symlink-escape on read must exit 1 (Failed), got {result.returncode}"
    )
    assert "root:" not in result.stdout, (
        "U13: no host content may be emitted through a scratch symlink"
    )


def test_u14_sandbox_escape_symlink_as_cwd(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U14: scratch/esc -> /, then ocx.run(cwd="esc") → exit 1; nothing spawned."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "symcwd.star",
        'ocx.run("shtool", "mklink", ocx.scratch_root + "/esc")\n'
        'ocx.run("shtool", "--version", cwd="esc")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U14: symlink as cwd must exit 1 (Failed), got {result.returncode}"
    )


# ---------------------------------------------------------------------------
# U15 / U16 — env overlay policy (Codex C3)
# ---------------------------------------------------------------------------


def test_u15_env_overlay_reserved_key_rejected(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U15: ocx.run(env={"PATH": "/evil"}) → exit 1; reserved key rejected."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "reserved.star",
        'ocx.run("shtool", "--version", env={"PATH": "/evil"})\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U15: reserved-key overlay must exit 1 (Failed), got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_u16_env_overlay_non_reserved_and_resolution_invariance(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U16: env={"FOO":"bar"} overlays the child but does NOT change resolution.

    The program still resolves from the composed PATH (overlay applied AFTER
    resolution). Asserting both: FOO is visible to the child AND shtool still
    resolves and runs.
    """
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "overlay.star",
        'r = ocx.run("shtool", "--echo-env", "FOO", env={"FOO": "bar"})\n'
        "expect.ok(r)\n"
        'expect.contains(r.stdout, "bar")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U16: non-reserved overlay must succeed (resolution unchanged), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U17 — child killed on parent termination (Codex C4-parity)
# ---------------------------------------------------------------------------


def test_u17_child_killed_on_parent_sigterm(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U17: long-running ocx.run child; SIGTERM the parent → child dies (no orphan).

    Best-effort signal-forwarding parity with child_process::spawn_and_wait.
    """
    import uuid

    bundle, meta, pkg = script_test_package
    # A long sleep via the package shell; the marker file lets us detect orphans.
    # A unique sentinel arg makes `pgrep` exact: a bare `sleep 120` could
    # false-positive on an unrelated host process; the UUID cannot.
    sentinel = f"u17-{uuid.uuid4()}"
    marker = tmp_path / "u17_child_alive"
    smoke = _write_script(
        tmp_path,
        "longrun.star",
        f'ocx.run("sh", "-c", "touch {marker}; sleep 120 {sentinel}")\n',
    )

    proc = subprocess.Popen(
        [
            str(ocx.binary),
            "package", "test",
            "-p", _PLATFORM,
            "-m", str(meta),
            "-i", pkg.short,
            str(bundle),
            "--script", str(smoke),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=ocx.env,
        text=True,
    )
    # Wait until the child has actually started, then terminate the parent.
    deadline = 30.0
    waited = 0.0
    while not marker.exists() and waited < deadline:
        import time

        time.sleep(0.25)
        waited += 0.25
    proc.terminate()
    try:
        proc.wait(timeout=20.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=10.0)
        pytest.fail("U17: parent ocx did not exit promptly after SIGTERM")

    # If the grandchild sleep survived, it would still be running well past the
    # parent's exit. Give the kill path a moment, then assert no live `sleep 120`.
    import time

    time.sleep(2.0)
    pgrep = subprocess.run(
        ["pgrep", "-f", sentinel], capture_output=True, text=True
    )
    assert pgrep.returncode != 0, (
        "U17: child must be killed on parent termination (no orphaned "
        f"`sleep 120 {sentinel}`); pgrep found: {pgrep.stdout!r}"
    )


# ---------------------------------------------------------------------------
# U18 — script source via stdin (R1)
# ---------------------------------------------------------------------------


def test_u18_script_source_via_stdin(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U18: `--script -` reads the script SOURCE from stdin → exit 0."""
    bundle, meta, pkg = script_test_package
    source = 'r = ocx.run("shtool", "--version")\nexpect.ok(r)\n'

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=source)

    assert result.returncode == 0, (
        f"U18: `--script -` (stdin source) must exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_u18_stdin_read_failure_is_io_error_74(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U18 (R1 edge): `--script -` with the stdin stream broken → exit 74.

    Distinct from a missing path (64): reading a supplied stream that errors is
    I/O (IoError), not bad usage. We close the parent stdin immediately so the
    runner's read of `-` fails.
    """
    bundle, meta, pkg = script_test_package

    # `stdin=DEVNULL` delivers an immediate EOF / zero bytes — the
    # broken / never-delivered stdin-source case (LDR-8). A zero-byte
    # `--script -` stream is IoError (74), distinct from an explicitly empty
    # script *file* (U11 → Passed). DEVNULL is used instead of
    # `Popen(...).stdin.close()` because Python 3.14's
    # `subprocess.communicate` raises `ValueError` when the stdin pipe was
    # explicitly closed first — a harness incompatibility unrelated to ocx.
    proc = subprocess.run(
        [
            str(ocx.binary),
            "package", "test",
            "-p", _PLATFORM,
            "-m", str(meta),
            "-i", pkg.short,
            str(bundle),
            "--script", "-",
        ],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        env=ocx.env,
        text=True,
        timeout=120.0,
    )

    assert proc.returncode == 74, (
        f"U18: broken/empty stdin script stream must exit 74 (IoError, NOT 64), "
        f"got {proc.returncode}\nstderr: {proc.stderr}"
    )


# ---------------------------------------------------------------------------
# U19 / U20 — ocx.run varargs: zero-arg Failed + list splat (R2)
# ---------------------------------------------------------------------------


def test_u19_run_zero_args_is_failed(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U19: ocx.run() with zero positional args → exit 1 (Failed)."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(tmp_path, "zeroarg.star", "ocx.run()\n")

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"U19: ocx.run() with zero args must exit 1 (Failed), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_u20_run_splat_of_list_var(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U20: ocx.run(*cmd) splat of a list variable works → exit 0."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "splat.star",
        'cmd = ["shtool", "--version"]\n'
        "r = ocx.run(*cmd)\n"
        "expect.ok(r)\n",
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 0, (
        f"U20: list splat into ocx.run must work → exit 0, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# U21 / U22 — --format json result envelope (R3)
# ---------------------------------------------------------------------------


def test_u21_format_json_envelope_passed(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U21: U1 with --format json → exit 0; stdout is the ScriptRunReport envelope.

    Asserts the stable FIELD SHAPE (status present), never prose.
    """
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "jsonok.star",
        'r = ocx.run("shtool", "--version")\nexpect.ok(r)\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke, fmt="json")

    assert result.returncode == 0, (
        f"U21: passing run with --format json must exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert "status" in payload, (
        f"U21: JSON envelope must carry a `status` field, got keys: {list(payload)}"
    )
    assert payload["status"] == "passed", (
        f"U21: status must be `passed`, got {payload.get('status')!r}"
    )


def test_u22_format_json_envelope_assertion_failure(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """U22: U5 with --format json → exit 1; structured assertion record present.

    Field presence/shape is stable; the human-readable prose is NOT asserted
    verbatim (only message-field presence).
    """
    bundle, meta, pkg = script_test_package
    smoke = _write_script(
        tmp_path,
        "jsonfail.star",
        'r = ocx.run("shtool", "--version")\n'
        'expect.contains(r.stdout, "v9")\n',
    )

    result = _run_script(ocx, bundle, meta, pkg, script=smoke, fmt="json")

    assert result.returncode == 1, (
        f"U22: failing run must exit 1 (exit code authoritative), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "failed", (
        f"U22: status must be `failed`, got {payload.get('status')!r}"
    )
    assert payload.get("assertion") is not None, (
        "U22: a failed-on-assertion envelope must carry a structured "
        "`assertion` record (field presence is the stable contract)"
    )
    assert "kind" in payload["assertion"], (
        "U22: the assertion record must carry a `kind` field"
    )
    assert "message" in payload["assertion"], (
        "U22: the assertion record must carry a `message` field "
        "(presence stable; prose NOT asserted)"
    )


# ---------------------------------------------------------------------------
# Extra clap edge — re-entrant ocx refusal (Error Taxonomy / Fix#6)
# ---------------------------------------------------------------------------


def test_reentrant_ocx_refused(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """ocx.run("ocx", ...) → exit 1 (re-entrancy refused in v1, Fix#6)."""
    bundle, meta, pkg = script_test_package
    smoke = _write_script(tmp_path, "reentrant.star", 'ocx.run("ocx", "--version")\n')

    result = _run_script(ocx, bundle, meta, pkg, script=smoke)

    assert result.returncode == 1, (
        f"re-entrant `ocx` must be refused (Failed, exit 1), "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Typed host API — Platform / OperatingSystem / Architecture / RunResult
#
# Covers the typed Starlark surface introduced by the typed-host-API plan:
#
#   - ocx.target_platform returns a typed Platform value with attributes
#     `is_any`, `os`, `arch` (not a dict, not strings).
#   - ocx.os.* / ocx.arch.* namespaces carry typed enum constants whose
#     equality compares the discriminant, not a string.
#   - ocx.run(...) returns a typed RunResult value whose `exit_code` is an
#     int (not a stringly-typed struct).
#   - Cross-type wall: an OS constant must NOT compare equal to a string,
#     nor to an arch constant. Same wall holds in the other direction.
#
# These cases author their own minimal .star inline via the `--script -`
# stdin form — no on-disk .star files are added.
# ---------------------------------------------------------------------------


def _split_platform(platform: str) -> tuple[str, str]:
    """Returns the (os, arch) pair from a `current_platform()` string.

    `current_platform()` returns either `"any"` or `"<os>/<arch>"`. Tests in
    this group only run when a concrete platform is set so the assertion
    target is deterministic.
    """
    parts = platform.split("/")
    assert len(parts) >= 2, f"expected os/arch platform, got '{platform}'"
    return parts[0], parts[1]


_PASCAL_OS = {"linux": "Linux", "darwin": "Darwin", "windows": "Windows"}
_PASCAL_ARCH = {"amd64": "Amd64", "arm64": "Arm64"}


def test_target_platform_is_typed_value_with_attrs(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """ocx.target_platform returns a typed Platform value with `is_any`,
    `os`, `arch`; `os` compares equal to the matching ocx.os.* constant."""
    bundle, meta, pkg = script_test_package
    os_str, arch_str = _split_platform(_PLATFORM)
    pascal_os = _PASCAL_OS[os_str]
    pascal_arch = _PASCAL_ARCH[arch_str]
    body = (
        "p = ocx.target_platform\n"
        "expect.false(p.is_any)\n"
        f"expect.eq(p.os, ocx.os.{pascal_os})\n"
        f"expect.eq(p.arch, ocx.arch.{pascal_arch})\n"
        # str(p.os) is the lowercase OCI string — round-trip with the -p flag.
        f'expect.eq(str(p.os), "{os_str}")\n'
        f'expect.eq(str(p.arch), "{arch_str}")\n'
    )

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=body)

    assert result.returncode == 0, (
        f"typed platform attrs: expected exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_package_and_scratch_roots_are_path_attributes(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """ocx.package_root / ocx.scratch_root are per-run path ATTRIBUTES (no
    parens) — non-empty strings, and the two roots differ."""
    bundle, meta, pkg = script_test_package
    body = (
        "pr = ocx.package_root\n"
        "sr = ocx.scratch_root\n"
        'expect.eq(type(pr), "string")\n'
        'expect.eq(type(sr), "string")\n'
        "expect.true(len(pr) > 0)\n"
        "expect.true(len(sr) > 0)\n"
        "expect.ne(pr, sr)\n"
    )

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=body)

    assert result.returncode == 0, (
        f"root attributes: expected exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_root_method_form_is_removed(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """The old method form `ocx.package_root()` is gone — calling the path
    attribute as a function is a type error, so the script exits non-zero."""
    bundle, meta, pkg = script_test_package
    body = "x = ocx.package_root()\n"

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=body)

    assert result.returncode != 0, (
        "calling the package_root attribute as a method must fail "
        f"(got exit 0)\nstderr: {result.stderr}"
    )


def test_run_result_exit_code_is_typed_int(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """RunResult's `exit_code` attribute is a Starlark int (typed value with
    declared shape — not a stringly-typed AllocStruct field)."""
    bundle, meta, pkg = script_test_package
    body = (
        'r = ocx.run("shtool", "--version")\n'
        "expect.ok(r)\n"
        'expect.eq(type(r.exit_code), "int")\n'
        'expect.eq(type(r.stdout), "string")\n'
        'expect.eq(type(r.stderr), "string")\n'
        'expect.eq(type(r.duration_ms), "int")\n'
        'expect.eq(type(r.truncated), "bool")\n'
    )

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=body)

    assert result.returncode == 0, (
        f"RunResult type tags: expected exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_os_arch_cross_type_wall(
    script_test_package: tuple[Path, Path, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """ocx.os.* constants do NOT compare equal to strings or to arch
    constants — typed values, not stringly-typed dict keys."""
    bundle, meta, pkg = script_test_package
    body = (
        'expect.ne(ocx.os.Linux, "linux")\n'
        "expect.ne(ocx.os.Linux, ocx.arch.Amd64)\n"
        'expect.ne(ocx.arch.Amd64, "amd64")\n'
        # Same OS variant on both sides IS equal — the wall is between types,
        # not between identical typed values.
        "expect.eq(ocx.os.Linux, ocx.os.Linux)\n"
        "expect.eq(ocx.arch.Amd64, ocx.arch.Amd64)\n"
    )

    result = _run_script(ocx, bundle, meta, pkg, script="-", stdin=body)

    assert result.returncode == 0, (
        f"cross-type wall: expected exit 0, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
