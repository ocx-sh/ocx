"""Hardening scenarios for limits, platforms and paths of the execution-state model.

Each test is one probe: overflow rungs across spawns, unusual project roots, separator-bearing
values, empty ``PATH`` segments, a pin naming a deleted build and a ``PATH`` without ocx.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src import exec_state
from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner, registry_dir

pytestmark = [
    pytest.mark.command("exec"),
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX tools host the probes"),
]

CARRIER = "__OCX_ENV_STATE"
TIMEOUT = 60
# POSIX sh reports a failed exec of a missing program with 127.
EXIT_COMMAND_NOT_FOUND = 127


def _sh() -> str:
    tool = shutil.which("sh")
    assert tool, "the host has no `sh`"
    return tool


def _project(ocx: OcxRunner, root: Path, body: str) -> Path:
    """Write ``root/ocx.toml`` and lock it, which also records the consent."""
    root.mkdir(parents=True, exist_ok=True)
    (root / "ocx.toml").write_text(body, encoding="utf-8")
    result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=root,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
        timeout=TIMEOUT,
    )
    assert result.returncode == 0, f"lock must succeed for the fixture: {result.stderr}"
    return root


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        encoding="utf-8",
        env=env if env is not None else ocx.env,
        check=False,
        stdin=subprocess.DEVNULL,
        timeout=TIMEOUT,
    )


def _incompressible(index: int, length: int) -> str:
    """``length`` hex characters zlib cannot fold, distinct per ``index``."""
    text = ""
    block = hashlib.sha256(f"hardening-limits-{index}".encode()).hexdigest()
    while len(text) < length:
        text += block
        block = hashlib.sha256(block.encode()).hexdigest()
    return text[:length]


def _constants(prefix: str, count: int, length: int, offset: int) -> str:
    return "\n".join(
        f'{prefix}_{i} = "{_incompressible(offset + i, length)}"' for i in range(count)
    )


def _state_of(text: str) -> dict:
    state = exec_state.decode(text)
    assert isinstance(state, dict), f"state must be readable, got {state!r}"
    return state


def _project_roots(state: dict) -> list[str]:
    return [
        scope["project"]["root"]
        for scope in state["inherited_scopes"]
        if isinstance(scope, dict) and scope.get("project")
    ]


# ---------------------------------------------------------------------------
# Overflow rungs across spawns
# ---------------------------------------------------------------------------


def test_rung_three_state_reaches_the_child_as_header_only_and_values_still_apply(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A replay too large for any payload degrades to the header-only marker; the env still applies."""
    big = _project(
        ocx,
        tmp_path / "big",
        "[tools]\n\n[env]\n" + _constants("HUGE", 20, 1000, 0) + "\n",
    )
    result = _run(
        ocx, big, "--offline", "exec", "--",
        _sh(), "-c", f'printf "%s\\n%s" "${CARRIER}" "$HUGE_0"',
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    carrier, _, value = result.stdout.partition("\n")
    assert value == _incompressible(0, 1000), (
        "the values reach the child whatever the rung"
    )
    assert len(carrier) <= exec_state.MAX_ENCODED
    assert carrier.endswith("."), (
        f"rung 3 is the empty payload marker: {carrier[:60]!r}"
    )
    assert _state_of(carrier)["truncated"] == "payload_dropped"


def test_a_nested_exec_below_a_rung_three_parent_keeps_the_mark_and_its_own_values(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The marker stays sticky: a small inner project still applies, the mark is not cleared."""
    big = _project(
        ocx,
        tmp_path / "big",
        "[tools]\n\n[env]\n" + _constants("HUGE", 20, 1000, 0) + "\n",
    )
    small = _project(
        ocx, tmp_path / "small", '[tools]\n\n[env]\nSMALL_VAR = "small-fixture-value"\n'
    )
    probe = f'printf "%s\\n%s" "${CARRIER}" "$SMALL_VAR"'
    result = _run(
        ocx, big, "--offline", "exec", "--",
        str(ocx.binary), "--offline", "--project", str(small), "exec", "--",
        _sh(), "-c", probe,
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    carrier, _, value = result.stdout.partition("\n")
    assert value == "small-fixture-value"
    assert len(carrier) <= exec_state.MAX_ENCODED
    assert _state_of(carrier)["truncated"] == "payload_dropped", (
        "a header-only parent leaves no record to rebuild, so the mark persists"
    )


def test_narrowing_survives_a_rung_three_state_into_a_third_spawn(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--offline`` lives in the plaintext header, so it outlives a dropped payload."""
    big = _project(
        ocx,
        tmp_path / "big",
        "[tools]\n\n[env]\n" + _constants("HUGE", 20, 1000, 0) + "\n",
    )
    result = _run(
        ocx, big, "--offline", "exec", "--",
        str(ocx.binary), "exec", "--",
        _sh(), "-c", f'printf "%s" "${CARRIER}"',
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    state = _state_of(result.stdout)
    assert "offline" in state["narrowing"], (
        f"header lost the narrowing: {result.stdout[:60]!r}"
    )


def test_rung_two_across_three_spawns_never_exceeds_the_cap(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Three layered near-cap projects: each frame's value fits and the chain exits 0."""
    roots = [
        _project(ocx, tmp_path / name, "[tools]\n\n[env]\n" + _constants(name.upper(), 5, 1400, 100 * i) + "\n")
        for i, name in enumerate(("aa", "bb", "cc"))
    ]  # fmt: skip
    probe = f'printf "%s" "${CARRIER}"'
    result = _run(
        ocx, roots[0], "--offline", "exec", "--",
        str(ocx.binary), "--offline", "--project", str(roots[1]), "exec", "--",
        str(ocx.binary), "--offline", "--project", str(roots[2]), "exec", "--",
        _sh(), "-c", probe,
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    assert len(result.stdout) <= exec_state.MAX_ENCODED
    assert _state_of(result.stdout)["truncated"] != "none", (
        "three near-cap frames must have truncated"
    )


# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------


def test_a_symlinked_project_root_applies_and_records_one_root(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--project`` through a symlink composes the project once and the state names a single root."""
    real = _project(
        ocx, tmp_path / "real", '[tools]\n\n[env]\nLINK_VAR = "link-fixture-value"\n'
    )
    link = tmp_path / "link"
    link.symlink_to(real, target_is_directory=True)
    probe = f'printf "%s\\n%s" "$LINK_VAR" "${CARRIER}"'
    result = _run(
        ocx,
        tmp_path,
        "--offline",
        "--project",
        str(link),
        "exec",
        "--",
        _sh(),
        "-c",
        probe,
    )
    assert result.returncode == 0, result.stderr
    value, _, carrier = result.stdout.partition("\n")
    assert value == "link-fixture-value"
    roots = _project_roots(_state_of(carrier))
    assert len(roots) == 1, roots
    assert Path(roots[0]).resolve() == real.resolve()


def test_exec_in_a_symlinked_cwd_finds_the_project_and_nested_exec_of_the_real_path_is_not_duplicated(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The same project reached through a link and by its real path is one scope for a nested spawn."""
    real = _project(
        ocx, tmp_path / "real", '[tools]\n\n[env]\nLINK_VAR = "link-fixture-value"\n'
    )
    link = tmp_path / "link"
    link.symlink_to(real, target_is_directory=True)
    result = _run(
        ocx, link, "--offline", "exec", "--",
        str(ocx.binary), "--offline", "--project", str(real), "exec", "--",
        _sh(), "-c", f'printf "%s" "${CARRIER}"',
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    roots = _project_roots(_state_of(result.stdout))
    assert len({Path(r).resolve() for r in roots}) == 1, (
        f"one project, one resolved root: {roots}"
    )


def test_a_unicode_root_with_spaces_round_trips_through_state_and_values(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Non-ASCII and spaces in the root and in values reach the child and the decoded state intact."""
    root = _project(
        ocx,
        tmp_path / "pröjekt wörld ✓",
        '[tools]\n\n[env]\nUNI_VAR = "wert mit  zwei spaces ä✓"\n',
    )
    probe = f'printf "%s\\n%s" "$UNI_VAR" "${CARRIER}"'
    result = _run(ocx, root, "--offline", "exec", "--", _sh(), "-c", probe)
    assert result.returncode == 0, result.stderr
    value, _, carrier = result.stdout.partition("\n")
    assert value == "wert mit  zwei spaces ä✓"
    assert _project_roots(_state_of(carrier)) == [str(root)]


def test_a_unicode_root_survives_a_nested_exec(ocx: OcxRunner, tmp_path: Path) -> None:
    """The inner frame still names the unicode outer root after being re-encoded."""
    outer = _project(
        ocx, tmp_path / "äußer ✓", '[tools]\n\n[env]\nOUT_VAR = "out-fixture-value"\n'
    )
    inner = _project(
        ocx, tmp_path / "inner dir", '[tools]\n\n[env]\nIN_VAR = "in-fixture-value"\n'
    )
    probe = f'printf "%s\\n%s\\n%s" "$OUT_VAR" "$IN_VAR" "${CARRIER}"'
    result = _run(
        ocx, outer, "--offline", "exec", "--",
        str(ocx.binary), "--offline", "--project", str(inner), "exec", "--",
        _sh(), "-c", probe,
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    out_value, in_value, carrier = result.stdout.split("\n", 2)
    assert (out_value, in_value) == ("out-fixture-value", "in-fixture-value")
    assert _project_roots(_state_of(carrier)) == [str(outer), str(inner)]


# ---------------------------------------------------------------------------
# Separators and empty PATH segments
# ---------------------------------------------------------------------------


def test_colon_and_semicolon_values_reach_the_child_verbatim(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A constant holding ``:`` and ``;`` is not split, and a nested frame replays it unchanged."""
    root = _project(ocx, tmp_path / "p", '[tools]\n\n[env]\nSEP_VAR = "a:b;c::d;"\n')
    probe = 'printf "%s" "$SEP_VAR"'
    result = _run(
        ocx, root, "--offline", "exec", "--",
        str(ocx.binary), "--offline", "exec", "--",
        _sh(), "-c", probe,
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    assert result.stdout == "a:b;c::d;"


def test_a_list_variable_with_separator_in_an_item_is_kept_as_written(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A ``;``-separated list value arrives joined by exactly the declared separator."""
    root = _project(
        ocx,
        tmp_path / "p",
        '[tools]\n\n[env]\nSEMI_LIST = { type = "list", value = "one:two", separator = ";" }\n',
    )
    result = _run(
        ocx, root, "--offline", "exec", "--", _sh(), "-c", 'printf "%s" "$SEMI_LIST"'
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "one:two"


def test_empty_ambient_path_segments_are_dropped_so_the_working_directory_never_joins_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Leading, middle and trailing empty segments (which POSIX reads as the cwd) do not survive composition."""
    root = _project(
        ocx,
        tmp_path / "p",
        '[tools]\n\n[env]\nPATH = { type = "path", value = "bin" }\n',
    )
    (root / "bin").mkdir()
    sh_dir = str(Path(_sh()).parent)
    env = {**ocx.env, "PATH": f"::{sh_dir}::/nonexistent-fixture-dir:"}
    result = _run(
        ocx,
        root,
        "--offline",
        "exec",
        "--",
        _sh(),
        "-c",
        'printf "%s" "$PATH"',
        env=env,
    )
    assert result.returncode == 0, result.stderr
    segments = result.stdout.split(os.pathsep)
    assert segments[0] == str(root / "bin"), result.stdout
    assert "" not in segments, (
        f"an empty segment would put the cwd on PATH: {result.stdout!r}"
    )
    assert sh_dir in segments and "/nonexistent-fixture-dir" in segments, result.stdout


def test_an_empty_ambient_path_still_composes_the_project_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``PATH=""`` in the environment yields the project directory without a stray trailing separator."""
    root = _project(
        ocx,
        tmp_path / "p",
        '[tools]\n\n[env]\nPATH = { type = "path", value = "bin" }\n',
    )
    (root / "bin").mkdir()
    env = {**ocx.env, "PATH": ""}
    result = _run(
        ocx,
        root,
        "--offline",
        "exec",
        "--",
        "/bin/sh",
        "-c",
        'printf "%s" "$PATH"',
        env=env,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.split(os.pathsep)[0] == str(root / "bin"), result.stdout
    assert not result.stdout.endswith(os.pathsep), (
        f"stray trailing separator: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# The pin and ocx absent from PATH
# ---------------------------------------------------------------------------


def _install_launcher(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> Path:
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    launcher = (
        Path(str(ocx.ocx_home)) / "symlinks" / registry_dir(ocx.registry) / pkg.repo
        / "current" / "entrypoints" / "hello"
    )  # fmt: skip
    assert launcher.exists(), launcher
    return launcher


def _launcher_env(ocx: OcxRunner, **extra: str) -> dict[str, str]:
    """A minimal environment: no ocx directory on ``PATH``."""
    env = {
        "PATH": "/usr/bin:/bin",
        "OCX_HOME": str(ocx.ocx_home),
        "OCX_DEFAULT_REGISTRY": ocx.registry,
        "OCX_INSECURE_REGISTRIES": ocx.registry,
        "HOME": ocx.env["HOME"],
        "XDG_CONFIG_HOME": ocx.env["XDG_CONFIG_HOME"],
    }
    return {**env, **extra}


def test_a_launcher_with_the_pin_runs_although_no_ocx_is_on_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The pin carries the launcher to ocx when ``PATH`` cannot."""
    launcher = _install_launcher(ocx, unique_repo, tmp_path)
    assert shutil.which("ocx", path="/usr/bin:/bin") is None, (
        "the host has an ocx on the minimal PATH"
    )
    result = subprocess.run(
        [_sh(), str(launcher)], env=_launcher_env(ocx, OCX_BINARY_PIN=str(ocx.binary)),
        capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=TIMEOUT, check=False,
    )  # fmt: skip
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip(), "the entrypoint must have produced its marker"


def test_a_launcher_with_neither_pin_nor_ocx_on_path_fails_fast_and_names_ocx(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No pin and no ocx on ``PATH``: a prompt non-zero exit that names the missing program."""
    launcher = _install_launcher(ocx, unique_repo, tmp_path)
    result = subprocess.run(
        [_sh(), str(launcher)], env=_launcher_env(ocx),
        capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=TIMEOUT, check=False,
    )  # fmt: skip
    assert result.returncode == EXIT_COMMAND_NOT_FOUND, result.stderr
    assert "exec: ocx: not found" in result.stderr, (
        f"the failure must name the missing ocx: {result.stderr!r}"
    )


def test_a_launcher_whose_pin_names_a_deleted_build_fails_fast_and_names_the_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A pin to a binary that no longer exists does not hang and does not fall back silently."""
    launcher = _install_launcher(ocx, unique_repo, tmp_path)
    gone = tmp_path / "deleted-build" / "ocx"
    result = subprocess.run(
        [_sh(), str(launcher)], env=_launcher_env(ocx, OCX_BINARY_PIN=str(gone)),
        capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=TIMEOUT, check=False,
    )  # fmt: skip
    # How `sh` reports an exec of a missing absolute path is the shell's own:
    # dash exits 127 "not found", macOS /bin/sh (bash in POSIX mode) exits 126
    # "cannot execute". The launcher must fail exactly as the host `sh` does.
    reference = subprocess.run(
        [_sh(), "-c", 'exec "$1"', "sh", str(gone)],
        capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=TIMEOUT, check=False,
    )  # fmt: skip
    assert reference.returncode in (EXIT_COMMAND_NOT_FOUND, 126), reference.stderr
    assert result.returncode == reference.returncode, result.stderr
    assert str(gone) in result.stderr, f"the failure must name the pin: {result.stderr!r}"


def test_exec_replaces_an_ambient_pin_naming_a_deleted_build_with_the_running_binary(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The child sees the running binary, never the stale ambient pin."""
    root = _project(ocx, tmp_path / "p", "[tools]\n")
    env = {**ocx.env, "OCX_BINARY_PIN": str(tmp_path / "deleted-build" / "ocx")}
    result = _run(
        ocx,
        root,
        "--offline",
        "exec",
        "--",
        _sh(),
        "-c",
        'printf "%s" "$OCX_BINARY_PIN"',
        env=env,
    )
    assert result.returncode == 0, result.stderr
    # The kernel reports the running image by its resolved path, so a symlinked
    # spelling of the binary (a runfiles link) pins to the link's target.
    assert result.stdout == str(ocx.binary.resolve())


def test_exec_runs_with_a_path_that_has_no_ocx_and_pins_an_absolute_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Invoked by absolute path under a PATH without ocx: the pin is absolute, an inner pinned call works."""
    root = _project(
        ocx, tmp_path / "p", '[tools]\n\n[env]\nPIN_VAR = "pin-fixture-value"\n'
    )
    env = {**ocx.env, "PATH": "/usr/bin:/bin"}
    probe = '"$OCX_BINARY_PIN" --offline exec -- /bin/sh -c \'printf "%s" "$PIN_VAR"\''
    result = _run(ocx, root, "--offline", "exec", "--", _sh(), "-c", probe, env=env)
    assert result.returncode == 0, result.stderr
    assert result.stdout == "pin-fixture-value"


def test_clean_exec_with_an_empty_path_still_carries_a_usable_pin(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--clean`` leaves no ``PATH`` at all; the pin alone reaches the next ocx."""
    root = _project(
        ocx, tmp_path / "p", '[tools]\n\n[env]\nPIN_VAR = "pin-fixture-value"\n'
    )
    probe = '"$OCX_BINARY_PIN" --offline exec -- /bin/sh -c \'printf "%s" "$PIN_VAR"\''
    result = _run(
        ocx, root, "--offline", "exec", "--clean", "--", "/bin/sh", "-c", probe
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "pin-fixture-value"
