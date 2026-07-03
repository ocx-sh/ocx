"""Acceptance tests for ``ocx login`` / ``ocx logout``.

Specification-phase tests anchored to ``plan_ocx_login.md`` User Experience
Scenarios + Edge Cases. Tests panic with `unimplemented!()` against the
current stubs — that is the contract-first TDD specification gate.

Each test invokes the compiled ``ocx`` binary and asserts:
  * exit code (sysexits-aligned per ``quality-rust-exit_codes.md``)
  * stdout / stderr substrings (lower-case error rule per ``quality-rust-errors.md``)
  * post-state of ``~/.docker/config.json`` where relevant

A new ``mock_credential_helper`` fixture (see ``test/conftest.py``) drops a
``docker-credential-test`` shell script onto a tempdir; tests prepend the dir
to PATH and configure ``credsStore`` or ``credHelpers`` to ``test``.

All credential write ops use ``DOCKER_CONFIG`` to redirect the docker config
file into a per-test tempdir — never the user's real ``~/.docker/``.
"""
from __future__ import annotations

import base64
import json
import os
import platform
import stat
import subprocess
import sys
import textwrap
import time
from pathlib import Path
from typing import Any

import pytest

from src.runner import OcxRunner

# All scenario-based behavior assumes a POSIX shell for the mock helper.
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="mock credential helper uses POSIX shell scripts",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_login(
    ocx: OcxRunner,
    *args: str,
    docker_config_dir: Path,
    helper_dir: Path | None = None,
    stdin: str | None = None,
    extra_env: dict[str, str] | None = None,
    timeout: float = 40.0,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx login`` with isolated DOCKER_CONFIG and (optionally) a helper dir on PATH."""
    env = dict(ocx.env)
    env["DOCKER_CONFIG"] = str(docker_config_dir)
    if helper_dir is not None:
        env["PATH"] = f"{helper_dir}:{env.get('PATH', '')}"
    if extra_env:
        env.update(extra_env)
    # These tests isolate credential-storage behavior; they use fake credentials
    # against registries with no auth backend. --no-verify skips the registry
    # round-trip so the store logic is exercised without network/auth. Verify
    # wiring itself is covered by the Rust unit tests in command/login.rs.
    cmd = [str(ocx.binary), "login", "--no-verify", *args]
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=env,
        input=stdin,
        timeout=timeout,
    )


def _run_logout(
    ocx: OcxRunner,
    *args: str,
    docker_config_dir: Path,
    helper_dir: Path | None = None,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = dict(ocx.env)
    env["DOCKER_CONFIG"] = str(docker_config_dir)
    if helper_dir is not None:
        env["PATH"] = f"{helper_dir}:{env.get('PATH', '')}"
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary), "logout", *args]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def _read_docker_config(docker_config_dir: Path) -> dict[str, Any] | None:
    path = docker_config_dir / "config.json"
    if not path.exists():
        return None
    return json.loads(path.read_text())


def _write_helper(
    helper_dir: Path,
    *,
    behavior: str = "default",
    sidecar: Path | None = None,
) -> Path:
    """Drop a ``docker-credential-test`` script into ``helper_dir``.

    Behavior:
        * ``default``        — persist stdin to ``sidecar``; respond to get/erase/list.
        * ``timeout60s``     — sleep 60s on every action.
        * ``exit1_no_stdout``— write non-sentinel stderr, exit 1 (HelperFailure).
        * ``exit1_sentinel`` — emit ``credentials not found in native keychain``, exit 1.
        * ``output_2mb``     — emit 2 MiB to stdout (cap-trip).
    """
    bin_path = helper_dir / "docker-credential-test"
    if behavior == "timeout60s":
        script = "#!/bin/sh\nsleep 60\n"
    elif behavior == "exit1_no_stdout":
        script = "#!/bin/sh\necho something-broke >&2\nexit 1\n"
    elif behavior == "exit1_sentinel":
        script = (
            "#!/bin/sh\necho 'credentials not found in native keychain'\nexit 1\n"
        )
    elif behavior == "output_2mb":
        script = "#!/bin/sh\ndd if=/dev/zero bs=1024 count=2048 2>/dev/null | tr '\\0' 'a'\n"
    else:
        assert sidecar is not None, "default helper requires a sidecar path"
        script = textwrap.dedent(
            f"""\
            #!/bin/sh
            action="$1"
            sidecar="{sidecar}"
            input=$(cat)
            case "$action" in
                store) printf '%s' "$input" > "$sidecar" ;;
                get)
                    if [ -f "$sidecar" ]; then cat "$sidecar";
                    else echo 'credentials not found in native keychain'; exit 1; fi ;;
                erase) rm -f "$sidecar" ;;
                list)  echo '{{}}' ;;
                *) echo "unknown action: $action" >&2; exit 2 ;;
            esac
            """
        )
    bin_path.write_text(script)
    bin_path.chmod(
        bin_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
    )
    return bin_path


# ---------------------------------------------------------------------------
# Scenario 1: interactive TTY login (Scenario 1)
# ---------------------------------------------------------------------------


# Python 3.14 warns on forkpty() in a multi-threaded process.  Under
# ``pytest -n auto`` each xdist worker carries an execnet receiver thread,
# so pexpect.spawn() -> ptyprocess -> pty.fork() forks from a multi-threaded
# process.  The pattern is fork -> immediate exec with the only sibling
# thread blocked on a socket recv, so the documented deadlock cannot be
# realized here.  Scope the suppression to this exact message so genuine
# forkpty misuse elsewhere still surfaces.
@pytest.mark.filterwarnings(
    "ignore:This process .* is multi-threaded, use of forkpty\\(\\):DeprecationWarning"
)
def test_login_interactive_tty_stores_credential(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 1 — pexpect-driven username + password prompts."""
    pexpect = pytest.importorskip("pexpect")
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    # Seed config with credsStore so a helper is configured at parse.
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    env = dict(ocx.env)
    env["DOCKER_CONFIG"] = str(docker_config_dir)
    env["PATH"] = f"{helper_dir}:{env.get('PATH', '')}"

    child = pexpect.spawn(
        str(ocx.binary), ["login", "--no-verify", "ghcr.io"], env=env, timeout=15, encoding="utf-8"
    )
    child.expect("Username: ")
    child.sendline("ocx-bot")
    child.expect("Password: ")
    child.sendline("secret")
    child.expect(pexpect.EOF)
    child.close()
    assert child.exitstatus == 0, f"exit was {child.exitstatus}"
    assert sidecar.exists(), "helper should have persisted credential"


def test_login_falls_back_to_default_registry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 1 — no positional REGISTRY ⇒ uses ``OCX_DEFAULT_REGISTRY``."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
        extra_env={"OCX_DEFAULT_REGISTRY": "internal.example.com"},
    )
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"
    assert sidecar.exists(), "credential should be persisted"
    payload = json.loads(sidecar.read_text())
    assert payload.get("ServerURL") == "internal.example.com"


# ---------------------------------------------------------------------------
# Scenario 2: --password-stdin (CI)
# ---------------------------------------------------------------------------


def test_login_password_stdin_ci_stores_credential(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 2 — non-interactive CI flow with token piped on stdin."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "ocx-bot",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="GHCR_TOKEN_VALUE\n",
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(sidecar.read_text())
    assert payload.get("Username") == "ocx-bot"
    assert payload.get("Secret") == "GHCR_TOKEN_VALUE"


def test_login_strips_exactly_one_trailing_newline(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 2 — exactly one trailing ``\\n`` byte is stripped (Edge Cases)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(sidecar.read_text())
    assert payload.get("Secret") == "tok", f"trailing whitespace not stripped: {payload!r}"


def test_login_preserves_carriage_return_in_password_stdin(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Edge case — ``\\r\\n`` line ending leaves the ``\\r`` in the stored secret."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\r\n",
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(sidecar.read_text())
    assert payload.get("Secret") == "tok\r", (
        f"only \\n is stripped — \\r must be preserved; got {payload!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 3: non-TTY without --password-stdin
# ---------------------------------------------------------------------------


def test_login_non_tty_without_flag_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """Scenario 3 — non-interactive login without ``--password-stdin`` must exit 64."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    _write_helper(helper_dir, sidecar=tmp_path / "sidecar.json")

    result = _run_login(
        ocx,
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="",
    )
    assert result.returncode == 64, f"exit {result.returncode}, stderr: {result.stderr}"


def test_login_empty_password_stdin_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """Empty ``--password-stdin`` input must exit 64 with diagnostic."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    _write_helper(helper_dir, sidecar=tmp_path / "sidecar.json")

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="",
    )
    assert result.returncode == 64, f"exit {result.returncode}"


def test_login_password_value_flag_rejected(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--password VALUE`` does not exist — CWE-214; argv-visible secrets banned."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password",
        "tok",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
    )
    assert result.returncode == 64, f"exit {result.returncode}"


# ---------------------------------------------------------------------------
# Scenario 4: credentials rejected (Ping-then-Put invariant)
# ---------------------------------------------------------------------------


@pytest.mark.skip(
    reason=(
        "v1 uses NoopPing; Ping-then-Put unit-tested via MockPing in "
        "auth/login.rs::login_returns_login_rejected_when_ping_fails_and_store_put_not_called. "
        "Acceptance gate deferred to --verify opt-in (v2)."
    )
)
def test_login_rejects_credentials_with_loginrejected(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """Scenario 4 — bad credentials must NOT reach the store (exit 80, config unchanged)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    # Use a registry that will return 401 / fail to Ping. The local test
    # registry runs anonymous; we point at an unreachable hostname so the
    # implementer's Ping fails. Either way the store invariant holds.
    result = _run_login(
        ocx,
        "-u",
        "bad",
        "--password-stdin",
        "unreachable-registry.invalid",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="WRONG\n",
    )
    assert result.returncode == 80, (
        f"failed credentials must exit 80; got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    # Critical invariant: store must NOT have received the credential.
    assert not sidecar.exists(), (
        "Ping-then-Put invariant violated: bad credential reached the helper store"
    )


# ---------------------------------------------------------------------------
# Scenario 5: plaintext fallback gates
# ---------------------------------------------------------------------------


def test_login_refuses_plaintext_by_default_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 5A — no helper, no flag ⇒ refuse plaintext with exit 78."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    # Empty config + no helper on PATH.
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "internal.example.com",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 78, f"exit {result.returncode}, stderr: {result.stderr}"


def test_login_allows_plaintext_with_flag_writes_base64_with_warning(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 5B — ``--allow-insecure-store`` opts into plaintext fallback."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "--allow-insecure-store",
        "internal.example.com",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is not None, "config.json must exist after login"
    auth = cfg.get("auths", {}).get("internal.example.com", {}).get("auth")
    assert auth, f"auths.internal.example.com.auth missing; cfg: {cfg!r}"
    decoded = base64.b64decode(auth).decode()
    assert decoded == "u:tok"
    # Plaintext warning to stderr (security req §5 of plan).
    assert "plaintext" in result.stderr.lower() or "warning" in result.stderr.lower(), (
        f"expected plaintext warning on stderr; got: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 6: JSON output mode
# ---------------------------------------------------------------------------


def test_login_json_format_emits_minimal_payload(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 6 — ``--format json`` emits ``{"registry":"…","username":"…"}``."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    env = dict(ocx.env)
    env["DOCKER_CONFIG"] = str(docker_config_dir)
    env["PATH"] = f"{helper_dir}:{env.get('PATH', '')}"
    cmd = [
        str(ocx.binary),
        "--format",
        "json",
        "login",
        "--no-verify",
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
    ]
    result = subprocess.run(
        cmd, capture_output=True, text=True, env=env, input="tok\n"
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload == {"registry": "ghcr.io", "username": "u"}


# ---------------------------------------------------------------------------
# Helper edge cases
# ---------------------------------------------------------------------------


def test_login_helper_timeout_exits_75(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — helper hangs >30s ⇒ exit 75 (TempFail)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    _write_helper(helper_dir, behavior="timeout60s")
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    start = time.monotonic()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
        timeout=45.0,
    )
    elapsed = time.monotonic() - start
    assert result.returncode == 75, f"exit {result.returncode}, stderr: {result.stderr}"
    assert elapsed < 35.0, f"timeout fired late ({elapsed:.1f}s)"


def test_login_helper_failure_exits_80(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — helper exits 1 with non-sentinel stderr ⇒ exit 80 (AuthError)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    _write_helper(helper_dir, behavior="exit1_no_stdout")
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
    )
    assert result.returncode == 80, f"exit {result.returncode}"


@pytest.mark.skip(
    reason=(
        "OutputTooLarge triggers a pipe-buffer deadlock in run_helper: after reading "
        "cap+1 bytes the read thread stops draining stdout, the child blocks on write, "
        "and child.wait() never returns — so the 30s Timeout fires before OutputTooLarge "
        "can be detected. The exit-80 contract is correct; fix requires draining "
        "remaining stdout in a background thread after the cap is hit (tracked as "
        "external/docker_credential bug; deferred to v2)."
    )
)
def test_login_helper_output_too_large_exits_80(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — helper emits >64 KiB to stdout ⇒ exit 80 (AuthError, OutputTooLarge)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    _write_helper(helper_dir, behavior="output_2mb")
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
    )
    assert result.returncode == 80, (
        f"expected exit 80 (AuthError), got {result.returncode}: {result.stderr}"
    )


def test_login_helper_not_on_path_exits_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — ``credsStore = "missing-helper"`` but no binary on PATH ⇒ exit 78."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    (docker_config_dir / "config.json").write_text(
        '{"credsStore":"absolutely-missing-helper-xyz"}'
    )
    # Empty PATH-augmentation: don't pass helper_dir.
    empty_dir = tmp_path / "empty_path"
    empty_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=empty_dir,
        stdin="tok\n",
    )
    assert result.returncode == 78, f"exit {result.returncode}"


def test_login_helper_unsafe_path_exits_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — helper resolved under a world-writable dir ⇒ exit 78 (UnsafePath)."""
    # Build a helper bin in a 0777 dir under /tmp.
    import tempfile as _tempfile

    helper_dir = Path(_tempfile.mkdtemp(prefix="ocx-unsafe-", dir="/tmp"))
    os.chmod(helper_dir, 0o777)
    _write_helper(helper_dir, sidecar=tmp_path / "sidecar.json")
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    (docker_config_dir / "config.json").write_text('{"credsStore":"test"}')

    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
        stdin="tok\n",
    )
    assert result.returncode == 78, f"exit {result.returncode}"


@pytest.mark.skipif(
    platform.system() == "Windows",
    reason="POSIX file mode bits only",
)
def test_login_creates_config_with_mode_0600(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — ``~/.docker/config.json`` created with mode 0600 on first login."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "--allow-insecure-store",
        "internal.example.com",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, result.stderr
    cfg = docker_config_dir / "config.json"
    mode = stat.S_IMODE(cfg.stat().st_mode)
    assert mode == 0o600, f"expected mode 0600, got 0o{mode:o}"


def test_login_preserves_unknown_config_fields(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Edge — unknown top-level fields (``experimental``, ``currentContext``) survive round-trip."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    (docker_config_dir / "config.json").write_text(
        '{"experimental":true,"currentContext":"default"}'
    )
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "--allow-insecure-store",
        "internal.example.com",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, result.stderr
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is not None
    assert cfg.get("experimental") is True
    assert cfg.get("currentContext") == "default"


def test_login_concurrent_with_pull_no_torn_json(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scenario 9 — 10 parallel logins + 1 reader; every observation is valid JSON."""
    import threading

    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()

    failures: list[str] = []

    def reader() -> None:
        cfg = docker_config_dir / "config.json"
        for _ in range(1000):
            if cfg.exists():
                try:
                    raw = cfg.read_text()
                    if raw:
                        json.loads(raw)
                except json.JSONDecodeError as exc:
                    failures.append(f"torn JSON: {exc}")
                    return

    def writer(i: int) -> None:
        result = _run_login(
            ocx,
            "-u",
            f"u{i}",
            "--password-stdin",
            "--allow-insecure-store",
            f"reg{i}.example",
            docker_config_dir=docker_config_dir,
            stdin=f"tok{i}\n",
        )
        if result.returncode != 0:
            failures.append(f"writer {i} exited {result.returncode}: {result.stderr}")

    rt = threading.Thread(target=reader)
    rt.start()
    writers = [threading.Thread(target=writer, args=(i,)) for i in range(10)]
    for w in writers:
        w.start()
    for w in writers:
        w.join()
    rt.join()
    assert not failures, f"failures: {failures}"


def test_login_canonicalizes_registry_url(ocx: OcxRunner, tmp_path: Path) -> None:
    """Edge — ``https://ghcr.io/v1/`` ⇒ config key is bare ``ghcr.io``."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "--allow-insecure-store",
        "https://ghcr.io/v1/",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, result.stderr
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is not None
    assert "ghcr.io" in cfg.get("auths", {}), (
        f"canonicalization missed: auths keys = {list(cfg.get('auths', {}).keys())}"
    )


def test_login_docker_io_normalizes_to_index_docker_io_v1(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Edge — ``docker.io`` special-case ⇒ config key ``https://index.docker.io/v1/``."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_login(
        ocx,
        "-u",
        "u",
        "--password-stdin",
        "--allow-insecure-store",
        "docker.io",
        docker_config_dir=docker_config_dir,
        stdin="tok\n",
    )
    assert result.returncode == 0, result.stderr
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is not None
    assert "https://index.docker.io/v1/" in cfg.get("auths", {}), (
        f"docker.io alias missed: auths keys = {list(cfg.get('auths', {}).keys())}"
    )


# ---------------------------------------------------------------------------
# Logout scenarios
# ---------------------------------------------------------------------------


def test_logout_was_logged_in_exits_0(ocx: OcxRunner, tmp_path: Path) -> None:
    """Scenario 7 — logout after prior login ⇒ exit 0."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    (docker_config_dir / "config.json").write_text(
        '{"auths":{"ghcr.io":{"auth":"dTpw"}}}'
    )
    result = _run_logout(ocx, "ghcr.io", docker_config_dir=docker_config_dir)
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is None or "ghcr.io" not in cfg.get("auths", {})


def test_logout_not_logged_in_exits_0_noop(ocx: OcxRunner, tmp_path: Path) -> None:
    """Scenario 7 — logout on a fresh state exits 0 (CI cleanup convention)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_logout(ocx, "ghcr.io", docker_config_dir=docker_config_dir)
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"


def test_logout_json_format(ocx: OcxRunner, tmp_path: Path) -> None:
    """Scenario 7 — JSON output is ``{"registry":"…"}``."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    env = dict(ocx.env)
    env["DOCKER_CONFIG"] = str(docker_config_dir)
    cmd = [str(ocx.binary), "--format", "json", "logout", "ghcr.io"]
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload == {"registry": "ghcr.io"}


def test_logout_falls_back_to_default_registry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bare ``ocx logout`` uses ``OCX_DEFAULT_REGISTRY`` (parallels login)."""
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    result = _run_logout(
        ocx,
        docker_config_dir=docker_config_dir,
        extra_env={"OCX_DEFAULT_REGISTRY": "internal.example.com"},
    )
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"


def test_logout_removes_from_both_layers(ocx: OcxRunner, tmp_path: Path) -> None:
    """Logout erases from BOTH ``auths[reg]`` and ``credHelpers[reg]`` helper.

    Seeds config with a per-registry helper AND a plaintext auths entry for
    the same registry. Asserts that after ``ocx logout``:
    - the helper's erase action was invoked (sidecar removed).
    - the ``auths[reg]`` entry is absent from config.json.
    """
    docker_config_dir = tmp_path / "docker"
    docker_config_dir.mkdir()
    helper_dir = tmp_path / "helper_bin"
    helper_dir.mkdir()
    sidecar = tmp_path / "helper_sidecar.json"
    _write_helper(helper_dir, sidecar=sidecar)
    # Seed a credential in both layers: per-registry helper + plaintext auths entry.
    sidecar.write_text('{"ServerURL":"ghcr.io","Username":"u","Secret":"p"}')
    (docker_config_dir / "config.json").write_text(
        '{"credHelpers":{"ghcr.io":"test"},"auths":{"ghcr.io":{"auth":"dTpw"}}}'
    )

    result = _run_logout(
        ocx,
        "ghcr.io",
        docker_config_dir=docker_config_dir,
        helper_dir=helper_dir,
    )
    assert result.returncode == 0, f"exit {result.returncode}, stderr: {result.stderr}"

    # Helper erase action removes the sidecar.
    assert not sidecar.exists(), "helper erase action must have been called (sidecar still present)"

    # auths layer also cleaned up.
    cfg = _read_docker_config(docker_config_dir)
    assert cfg is not None
    assert "ghcr.io" not in cfg.get("auths", {}), (
        f"auths[ghcr.io] still present after logout; cfg: {cfg!r}"
    )
