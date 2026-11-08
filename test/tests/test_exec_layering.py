"""Acceptance tests for how a nested `ocx exec` layers its composition.

Specification of ``.claude/artifacts/adr_exec_env_model.md`` (§Exec modes and flag grammar, §The one
projection function, §Validation). An explicit ``ocx exec`` composes what it was asked for, whatever
is loaded, the same project and group twice included; outer scopes gate only the hook and the login
stream. A plain ``exec -- exec`` chain layers by accident of the environment, so those rows pin what
the projection must keep: a guard in ``exec_env::compose`` skipping composition when
``inherited_scopes`` holds the requested scope reds them. Groups use PATH-free ``G1_VAR``/``G2_VAR``.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.runner import OcxRunner

pytestmark = pytest.mark.command("exec")

EXIT_USAGE = 64

_POSIX_ONLY = "the hook shell and the `--clean` child are driven through POSIX tools"

# ---------------------------------------------------------------------------
# Helpers (DAMP -- self-contained)
# ---------------------------------------------------------------------------


def _project(ocx: OcxRunner, root: Path, body: str) -> Path:
    """Write ``root/ocx.toml`` and lock it, which also records the consent the hook needs."""
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
    assert result.returncode == 0, (
        f"ocx lock must succeed for the fixture: {result.stderr}"
    )
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
        text=True,
        env=env if env is not None else ocx.env,
        check=False,
        stdin=subprocess.DEVNULL,
    )


def _dump(stdout: str) -> dict[str, str]:
    """``env`` output as a dict; the values used here are single-line by construction."""
    return dict(line.split("=", 1) for line in stdout.splitlines() if "=" in line)


def _elements(value: str | None) -> list[str]:
    return [element for element in (value or "").split(os.pathsep) if element]


def _chain(ocx: OcxRunner, cwd: Path, outer: list[str], inner: list[str], *tail: str):
    """``ocx <outer> exec -- ocx <inner> exec -- <tail>`` -- the inner ocx is the one under test."""
    return _run(ocx, cwd, *outer, "--", str(ocx.binary), *inner, "--", *tail)


def _toml_path(path: Path) -> str:
    """``path`` as a TOML string; JSON quoting doubles the backslashes of a Windows path."""
    return json.dumps(str(path))


def _dump_command() -> list[str]:
    """A child printing ``NAME=value`` lines: MSYS ``env`` rewrites ``PATH`` to POSIX form on Windows."""
    return [
        sys.executable,
        "-c",
        "import os; [print(f'{k}={v}') for k, v in os.environ.items()]",
    ]


def _env_tool() -> str:
    """An absolute ``env``: a ``--clean`` child has no ``PATH`` to find it on."""
    tool = shutil.which("env")
    assert tool, "the host has no `env`"
    return tool


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def group_project(ocx: OcxRunner, tmp_path: Path) -> Path:
    """One project with two groups: PATH-free constants, a path-typed and a list-typed variable."""
    dirs = tmp_path / "dirs"
    for name in ("g1", "g2", "shared1", "shared2"):
        (dirs / name).mkdir(parents=True)
    return _project(
        ocx,
        tmp_path / "a",
        f"""\
[tools]

[group.g1.env]
G1_VAR = "g1-value"
G1_DIRS = {{ type = "path", value = {_toml_path(dirs / "g1")} }}
G1_LIST = {{ type = "list", value = "g1-item", separator = "," }}
SHARED_DIRS = {{ type = "path", value = {_toml_path(dirs / "shared1")} }}

[group.g2.env]
G2_VAR = "g2-value"
SHARED_DIRS = {{ type = "path", value = {_toml_path(dirs / "shared2")} }}
""",
    )


# ---------------------------------------------------------------------------
# Two projects
# ---------------------------------------------------------------------------


def test_nested_exec_of_another_project_puts_it_in_front_and_keeps_the_outer_reachable(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``b``'s directory first on ``PATH``, ``a``'s still reachable behind it."""
    a = _project(
        ocx,
        tmp_path / "a",
        '[tools]\n\n[env]\nPATH = { type = "path", value = "bin" }\n',
    )
    b = _project(
        ocx,
        tmp_path / "b",
        '[tools]\n\n[env]\nPATH = { type = "path", value = "bin" }\n',
    )
    (a / "bin").mkdir()
    (b / "bin").mkdir()

    result = _chain(
        ocx,
        tmp_path,
        ["--project", str(a), "exec"],
        ["--project", str(b), "exec"],
        *_dump_command(),
    )

    assert result.returncode == 0, result.stderr
    path = [
        os.path.realpath(element)
        for element in _elements(_dump(result.stdout).get("PATH"))
    ]
    inner, outer = os.path.realpath(b / "bin"), os.path.realpath(a / "bin")
    assert inner in path and outer in path, f"both projects must be on PATH: {path}"
    assert path.index(inner) < path.index(outer), f"b must be in front of a: {path}"


def test_exec_refuses_isolate_together_with_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Error row: ``--isolate --clean`` is a flag conflict, exit 64."""
    project = _project(ocx, tmp_path / "a", "[tools]\n")

    result = _run(ocx, project, "exec", "--isolate", "--clean", "--", *_dump_command())

    assert result.returncode == EXIT_USAGE, result.stderr
    assert "cannot be used with" in result.stderr, (
        f"the refusal must be the flag conflict, not an unknown flag: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# The owner run rule
# ---------------------------------------------------------------------------


def test_the_same_project_and_group_twice_gives_the_inner_command_each_value_once(
    ocx: OcxRunner, group_project: Path, tmp_path: Path
) -> None:
    """An explicit exec composes what it was asked for, whatever an outer exec loaded."""
    root = ["--project", str(group_project)]

    result = _chain(
        ocx,
        tmp_path,
        [*root, "exec", "-g", "g1"],
        [*root, "exec", "-g", "g1"],
        *_dump_command(),
    )
    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value"
    dirs = _elements(seen.get("G1_DIRS"))
    assert len(dirs) == 1, f"the path element must appear once: {dirs}"
    assert seen.get("G1_LIST") == "g1-item", "the list element must appear once"
    assert (
        _elements(seen.get("SHARED_DIRS")).count(str(tmp_path / "dirs" / "shared1"))
        == 1
    )


def test_another_group_of_the_same_project_layers_on_top_with_its_value_in_front(
    ocx: OcxRunner, group_project: Path, tmp_path: Path
) -> None:
    """The inner command sees g1's and g2's values, g2's shared path in front."""
    result = _run(
        ocx,
        tmp_path,
        "--project",
        str(group_project),
        "exec",
        "-g",
        "g1",
        "--",
        str(ocx.binary),
        "--project",
        str(group_project),
        "exec",
        "-g",
        "g2",
        "--",
        *_dump_command(),
    )

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("G1_VAR") == "g1-value"
    assert seen.get("G2_VAR") == "g2-value"
    shared = _elements(seen.get("SHARED_DIRS"))
    assert shared == [
        str(tmp_path / "dirs" / "shared2"),
        str(tmp_path / "dirs" / "shared1"),
    ], f"g2 must be in front of g1: {shared}"


# ---------------------------------------------------------------------------
# The chain run from a hook shell
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_chain_from_a_hook_shell_keeps_the_hook_layer_and_adds_both_groups(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Red today -- the default exec reverts what the hook put on the shell.

    The shell below is the per-prompt hook's first reconcile applied to a bash inside project
    ``a``; the chain then runs the way a user types it. The hook's ``PATH`` entry comes from the
    default ``[tools]`` binding, which neither ``-g g1`` nor ``-g g2`` composes, so the only way
    it reaches the innermost command is the hook's layer staying where it was.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(
        ocx,
        tmp_path / "a",
        f"""\
[tools]
hooked = "{pkg.fq}"

[group.g1.env]
G1_VAR = "g1-value"

[group.g2.env]
G2_VAR = "g2-value"
""",
    )
    pulled = _run(ocx, project, "pull")
    assert pulled.returncode == 0, (
        f"fixture: ocx pull must install the default tool: {pulled.stderr}"
    )
    env = {**ocx.env, "OCX_BIN": str(ocx.binary), "ENV_TOOL": _env_tool()}
    script = (
        'eval "$("$OCX_BIN" --offline self activate --reconcile --shell=bash)"; '
        'printf "HOOK_PATH_ENTRY=%s\\n" "${PATH%%:*}"; '
        '"$OCX_BIN" exec -g g1 -- "$OCX_BIN" exec -g g2 -- "$ENV_TOOL"'
    )

    result = subprocess.run(
        ["bash", "-c", script],
        cwd=project,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    hook_entry = seen.get("HOOK_PATH_ENTRY", "")
    assert hook_entry and hook_entry not in _elements(ocx.env["PATH"]), (
        f"fixture: the hook must put its own directory first on PATH: {hook_entry!r}\n{result.stderr}"
    )
    assert hook_entry in _elements(seen.get("PATH")), (
        "the hook's layer must stay under a default exec"
    )
    assert seen.get("G1_VAR") == "g1-value"
    assert seen.get("G2_VAR") == "g2-value"


# ---------------------------------------------------------------------------
# --isolate
# ---------------------------------------------------------------------------


def test_isolate_at_depth_two_drops_the_outer_group_and_keeps_the_users_own_path_entry(
    ocx: OcxRunner, group_project: Path, tmp_path: Path
) -> None:
    """Pristine environment plus this composition; the user's own ``PATH`` entry survives."""
    own = tmp_path / "user-own-bin"
    own.mkdir()
    env = {**ocx.env, "PATH": os.pathsep.join([str(own), ocx.env["PATH"]])}

    result = subprocess.run(
        [
            str(ocx.binary),
            "--project",
            str(group_project),
            "exec",
            "-g",
            "g1",
            "--",
            str(ocx.binary),
            "--project",
            str(group_project),
            "exec",
            "--isolate",
            "-g",
            "g2",
            "--",
            *_dump_command(),
        ],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert "G1_VAR" not in seen, "--isolate must revert what the outer exec applied"
    assert "G1_DIRS" not in seen and "G1_LIST" not in seen
    assert seen.get("G2_VAR") == "g2-value"
    assert str(own) in _elements(seen.get("PATH")), (
        "the user's own PATH entry must survive --isolate"
    )


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_isolate_at_depth_one_under_a_hook_shell_drops_the_hook_layer_of_another_project(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """(hook shell): the hook's layer is gone, the other project's composition is present."""
    a = _project(ocx, tmp_path / "a", '[tools]\n\n[env]\nHOOK_ONLY = "hook-value"\n')
    b = _project(ocx, tmp_path / "b", '[tools]\n\n[env]\nB_VAR = "b-value"\n')
    own = tmp_path / "user-own-bin"
    own.mkdir()
    env = {
        **ocx.env,
        "PATH": os.pathsep.join([str(own), ocx.env["PATH"]]),
        "OCX_BIN": str(ocx.binary),
        "PROJECT_B": str(b),
        "ENV_TOOL": _env_tool(),
    }
    script = (
        'eval "$("$OCX_BIN" --offline self activate --reconcile --shell=bash)"; '
        'test "$HOOK_ONLY" = hook-value || exit 97; '
        '"$OCX_BIN" --project "$PROJECT_B" exec --isolate -- "$ENV_TOOL"'
    )

    result = subprocess.run(
        ["bash", "-c", script],
        cwd=a,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )

    assert result.returncode != 97, (
        f"fixture: the hook must apply the project layer: {result.stderr}"
    )
    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert "HOOK_ONLY" not in seen, "--isolate must revert the hook's layer"
    assert seen.get("B_VAR") == "b-value"
    assert str(own) in _elements(seen.get("PATH")), (
        "the user's own PATH entry must survive --isolate"
    )


# ---------------------------------------------------------------------------
# --clean
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_child_holds_only_composed_variables_and_the_pin(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Nothing public of ocx's own, nothing ambient -- the home travels in the state header."""
    project = _project(
        ocx, tmp_path / "a", '[tools]\n\n[env]\nPROJ_ONLY = "proj-value"\n'
    )
    env = {**ocx.env, "AMBIENT_ONLY": "ambient"}

    result = _run(ocx, project, "exec", "--clean", "--", _env_tool(), env=env)

    assert result.returncode == 0, result.stderr
    seen = _dump(result.stdout)
    assert seen.get("PROJ_ONLY") == "proj-value"
    assert "AMBIENT_ONLY" not in seen and "HOME" not in seen
    public_ocx = sorted(
        key for key in seen if key.startswith("OCX_") and key != "OCX_BINARY_PIN"
    )
    assert public_ocx == [], (
        f"a clean child carries no public OCX_* variable: {public_ocx}"
    )
    assert seen.get("OCX_BINARY_PIN"), (
        "a launcher inside --clean finds ocx through the pin"
    )


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_launcher_inside_a_clean_exec_finds_ocx_and_the_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A generated launcher run in ``ocx exec --clean`` resolves ocx via the pin, the home via the header."""
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    project = _project(ocx, tmp_path / "a", f'[tools]\nhello = "{pkg.fq}"\n')

    result = _run(ocx, project, "exec", "--clean", "--", "hello")

    assert result.returncode == 0, (
        f"the launcher must run under --clean: {result.stderr}"
    )
    assert pkg.marker in result.stdout
