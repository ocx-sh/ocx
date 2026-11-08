"""Hardening scenarios for nested ``ocx exec`` chains: depth, mixed modes, shells and launchers.

Each case drives the real binary through a chain of explicit execs; the project groups define
PATH-free constants (``G<n>_VAR``) so a value is visible in the innermost ``env`` dump.
"""

from __future__ import annotations

import shutil
import signal
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from src import exec_state, previous_release
from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner, PackageInfo

pytestmark = [
    pytest.mark.command("exec", "shell"),
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX tools and scripts"),
]

_ENV_STATE = "__OCX_ENV_STATE"


def _project(ocx: OcxRunner, root: Path, body: str) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    (root / "ocx.toml").write_text(body, encoding="utf-8")
    result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=root,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert result.returncode == 0, f"fixture lock failed: {result.stderr}"
    return root


def _run(
    ocx: OcxRunner, cwd: Path, argv: list[str], env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env if env is not None else ocx.env,
        check=False,
        stdin=subprocess.DEVNULL,
        timeout=120,
    )


def _dump(stdout: str) -> dict[str, str]:
    return dict(line.split("=", 1) for line in stdout.splitlines() if "=" in line)


def _env_tool() -> str:
    tool = shutil.which("env")
    assert tool, "the host has no `env`"
    return tool


def _hop(ocx: OcxRunner, project: Path, *exec_args: str) -> list[str]:
    return [str(ocx.binary), "--project", str(project), "exec", *exec_args, "--"]


@pytest.fixture
def groups(ocx: OcxRunner, tmp_path: Path) -> Path:
    """One project with twenty constant-only groups plus a root constant."""
    body = '[tools]\n\n[env]\nROOT_VAR = "root-value"\n'
    for index in range(20):
        body += f'\n[group.g{index}.env]\nG{index}_VAR = "g{index}-value"\n'
    return _project(ocx, tmp_path / "a", body)


@pytest.fixture
def other(ocx: OcxRunner, tmp_path: Path) -> Path:
    return _project(ocx, tmp_path / "b", '[tools]\n\n[env]\nB_VAR = "b-value"\n')


def test_three_deep_chain_across_two_projects_layers_every_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other),
        *_hop(ocx, groups, "-g", "g2"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert (seen.get("G1_VAR"), seen.get("B_VAR"), seen.get("G2_VAR")) == (
        "g1-value",
        "b-value",
        "g2-value",
    )


def test_isolate_in_the_middle_of_a_three_deep_chain_drops_only_the_outer_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other, "--isolate"),
        *_hop(ocx, groups, "-g", "g2"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert "G1_VAR" not in seen, "the middle --isolate must revert the outer group"
    assert seen.get("B_VAR") == "b-value"
    assert seen.get("G2_VAR") == "g2-value"


def test_clean_in_the_middle_leaves_the_inner_exec_only_its_own_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other, "--clean"),
        *_hop(ocx, groups, "-g", "g2"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert "G1_VAR" not in seen, "--clean starts from nothing"
    assert seen.get("B_VAR") == "b-value"
    assert seen.get("G2_VAR") == "g2-value"


def test_clean_innermost_after_isolate_holds_only_its_own_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other, "--isolate"),
        *_hop(ocx, groups, "-g", "g3", "--clean"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G3_VAR") == "g3-value"
    assert not {"G1_VAR", "B_VAR"} & seen.keys()


def test_twenty_nested_scopes_all_reach_the_innermost_child_within_the_state_cap(
    ocx: OcxRunner, groups: Path, tmp_path: Path
) -> None:
    argv: list[str] = []
    for index in range(20):
        argv += _hop(ocx, groups, "-g", f"g{index}")
    argv.append(_env_tool())

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    missing = [i for i in range(20) if seen.get(f"G{i}_VAR") != f"g{i}-value"]
    assert not missing, f"scopes lost past the sixteenth: {missing}"
    state = seen.get(_ENV_STATE, "")
    assert state, "the innermost child must carry an execution state"
    assert len(state) <= exec_state.MAX_ENCODED
    assert exec_state.decode(state) is not None


def test_same_group_twenty_times_keeps_one_value_and_a_bounded_state(
    ocx: OcxRunner, groups: Path, tmp_path: Path
) -> None:
    argv: list[str] = []
    for _ in range(20):
        argv += _hop(ocx, groups, "-g", "g0")
    argv.append(_env_tool())

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G0_VAR") == "g0-value"
    assert len(seen.get(_ENV_STATE, "")) <= exec_state.MAX_ENCODED


def _shell_script(tmp_path: Path, body: str) -> Path:
    script = tmp_path / "fake-shell.sh"
    script.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script


def test_exec_inside_an_ocx_shell_layers_on_the_pinned_shell_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    inner = " ".join([*_hop(ocx, other), _env_tool()])
    script = _shell_script(tmp_path, inner)
    env = {**ocx.env, "SHELL": str(script)}

    result = _run(
        ocx,
        tmp_path,
        [str(ocx.binary), "--project", str(groups), "shell", "-g", "g1"],
        env,
    )

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value", (
        "the shell's scope reaches the exec inside it"
    )
    assert seen.get("B_VAR") == "b-value"


def test_ocx_shell_inside_an_exec_keeps_the_exec_scope_and_adds_its_own(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    script = _shell_script(tmp_path, _env_tool())
    env = {**ocx.env, "SHELL": str(script)}
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        str(ocx.binary),
        "--project",
        str(other),
        "shell",
    ]

    result = _run(ocx, tmp_path, argv, env)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value"
    assert seen.get("B_VAR") == "b-value"


@pytest.fixture
def relay_project(
    ocx: OcxRunner, groups: Path, unique_repo: str, tmp_path: Path
) -> Path:
    """A project whose ``relay`` launcher calls ``ocx exec -g g2`` on the same groups project."""
    script = (
        "#!/bin/sh\n"
        f"exec '{ocx.binary}' --project '{groups}' exec -g g2 -- '{_env_tool()}'\n"
    )
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["relay"],
        bins=["relay"],
        extra_files={"bin/relay": script},
    )
    ocx.plain("package", "install", "--select", pkg.short)
    return _project(ocx, tmp_path / "relay", f'[tools]\nrelay = "{pkg.fq}"\n')


def test_launcher_started_tool_calling_ocx_exec_adds_its_scope_to_the_callers(
    ocx: OcxRunner, groups: Path, relay_project: Path, tmp_path: Path
) -> None:
    argv = [*_hop(ocx, groups, "-g", "g1"), *_hop(ocx, relay_project), "relay"]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value"
    assert seen.get("G2_VAR") == "g2-value"


def test_launcher_started_tool_calling_ocx_exec_under_clean_still_composes(
    ocx: OcxRunner, relay_project: Path, tmp_path: Path
) -> None:
    result = _run(ocx, tmp_path, [*_hop(ocx, relay_project, "--clean"), "relay"])

    assert result.returncode == 0, result.stderr
    assert _dump(result.stdout).get("G2_VAR") == "g2-value"


def test_a_previous_release_ocx_in_the_middle_of_a_chain_passes_the_outer_scope_through(
    ocx: OcxRunner, groups: Path, published_package: PackageInfo, tmp_path: Path
) -> None:
    """new exec -> previous-release ocx -> new exec: the new innermost still composes."""
    old = previous_release.resolve()
    ocx.plain("package", "install", published_package.short)
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        str(old),
        "--offline",
        "package",
        "exec",
        published_package.fq,
        "--",
        *_hop(ocx, groups, "-g", "g2")[:-1],
        "--",
        _env_tool(),
    ]
    result = _run(ocx, tmp_path, argv)
    assert result.returncode == 0, result.stderr
    assert _dump(result.stdout).get("G2_VAR") == "g2-value"


def test_clean_exec_of_a_launcher_inside_an_ocx_shell_still_composes(
    ocx: OcxRunner, other: Path, relay_project: Path, tmp_path: Path
) -> None:
    inner = " ".join([*_hop(ocx, relay_project, "--clean"), "relay"])
    script = _shell_script(tmp_path, inner)
    env = {**ocx.env, "SHELL": str(script)}

    result = _run(
        ocx, tmp_path, [str(ocx.binary), "--project", str(other), "shell"], env
    )

    assert result.returncode == 0, result.stderr
    assert _dump(result.stdout).get("G2_VAR") == "g2-value"


@pytest.fixture
def contested(ocx: OcxRunner, tmp_path: Path) -> Path:
    """Two groups declaring the same constant with different values."""
    return _project(
        ocx,
        tmp_path / "c",
        '[tools]\n\n[group.one.env]\nSAME_VAR = "one"\n\n[group.two.env]\nSAME_VAR = "two"\n',
    )


def test_the_same_key_in_two_layers_takes_the_innermost_value(
    ocx: OcxRunner, contested: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, contested, "-g", "one"),
        *_hop(ocx, contested, "-g", "two"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    assert _dump(result.stdout).get("SAME_VAR") == "two"


def test_isolate_in_the_middle_resets_a_key_the_outer_layer_set(
    ocx: OcxRunner, contested: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, contested, "-g", "one"),
        *_hop(ocx, other, "--isolate"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert "SAME_VAR" not in seen
    assert seen.get("B_VAR") == "b-value"


def test_the_innermost_exit_code_survives_a_mixed_mode_chain(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other, "--isolate"),
        *_hop(ocx, groups, "-g", "g2", "--clean"),
        "/bin/sh",
        "-c",
        "exit 42",
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 42, result.stderr


def test_a_signal_that_kills_the_innermost_child_ends_the_whole_chain_with_the_same_signal(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    argv = [
        *_hop(ocx, groups, "-g", "g1"),
        *_hop(ocx, other),
        "/bin/sh",
        "-c",
        "kill -TERM $$",
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == -signal.SIGTERM, (result.returncode, result.stderr)


def test_cwd_moving_to_another_project_between_layers_adds_that_projects_scope(
    ocx: OcxRunner, groups: Path, other: Path, tmp_path: Path
) -> None:
    move = f"cd '{other}' && exec '{ocx.binary}' exec -- '{_env_tool()}'"
    argv = [
        str(ocx.binary),
        "exec",
        "-g",
        "g1",
        "--",
        "/bin/sh",
        "-c",
        move,
    ]

    result = _run(ocx, groups, argv)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value", "the outer scope stays"
    assert seen.get("B_VAR") == "b-value", "the inner scope follows the new cwd"


def test_a_dev_build_under_a_previous_release_ocx_still_composes(
    ocx: OcxRunner, groups: Path, published_package: PackageInfo, tmp_path: Path
) -> None:
    """previous-release ocx outermost, the binary under test innermost."""
    old = previous_release.resolve()
    ocx.plain("package", "install", published_package.short)
    argv = [
        str(old),
        "--offline",
        "package",
        "exec",
        published_package.fq,
        "--",
        *_hop(ocx, groups, "-g", "g2"),
        _env_tool(),
    ]

    result = _run(ocx, tmp_path, argv)

    assert result.returncode == 0, result.stderr
    assert _dump(result.stdout).get("G2_VAR") == "g2-value"
