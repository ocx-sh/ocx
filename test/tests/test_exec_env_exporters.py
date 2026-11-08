# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the state an exporter writes for a later step (``adr_exec_env_model.md`` § Exporters).

Under a ``[patches]`` tier, ``ocx env --shell`` and ``ocx direnv export`` write into the running
shell: with a readable inherited state they emit **no** state entry (a fresh one would replace the
hook's session records), otherwise a fresh minimal state; ``ocx env --ci`` and the structured report
always write one. It carries the inherited header's narrowing names, no home, an empty policy, no
applied records and empty ``inherited_scopes``, so ``--isolate`` in a later CI step removes nothing
the step set.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

import pytest

from src import exec_state
from src.helpers import make_package, make_package_with_entrypoints
from src.runner import OcxRunner, PackageInfo
from src.shell_eval import run_after_sourcing

pytestmark = pytest.mark.command("toolchain_env", "direnv_export", "env")

STATE_KEY = "__OCX_ENV_STATE"
COMPANION_KEY = "EXPORTER_COMPANION_CA"
COMPANION_VALUE = "exporter-companion-ca-value"
PACKAGE_CONST_KEY = "EXPORTER_PACKAGE_CONST"
PACKAGE_CONST_VALUE = "exporter-package-const-value"
EXIT_SUCCESS = 0


@dataclass(frozen=True)
class PatchedProject:
    """A pulled project whose one tool is matched by a catch-by-repository patch rule."""

    directory: Path
    package: PackageInfo
    base_name: str


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


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
        env=env if env is not None else dict(ocx.env),
        check=False,
    )


def _patched_project(ocx: OcxRunner, tmp_path: Path, registry: str) -> PatchedProject:
    """Publish a launcher-bearing tool and a companion, a `[patches]` tier with a
    rule keyed on the tool's repository, and a locked, pulled project binding it."""
    companion_repo = f"t_{uuid4().hex[:8]}_exporter_companion"
    companion = make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path,
        bins=[],
        env=[
            {
                "key": COMPANION_KEY,
                "type": "constant",
                "value": COMPANION_VALUE,
                "visibility": "public",
            }
        ],
        cascade=True,
        platform="any",
    )
    repo = f"t_{uuid4().hex[:8]}_exporter_tool"
    package = make_package_with_entrypoints(
        ocx,
        repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        file_prefix=repo,
        env=[
            {
                "key": "PATH",
                "type": "path",
                "value": "${installPath}/bin",
                "visibility": "public",
            },
            {
                "key": PACKAGE_CONST_KEY,
                "type": "constant",
                "value": PACKAGE_CONST_VALUE,
                "visibility": "public",
            },
        ],
    )
    base_name = package.fq.rpartition(":")[0]

    tier_path = f"p{uuid4().hex[:12]}"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[patches]\nregistry = "{registry}/{tier_path}"\nrequired = false\n'
    )
    descriptor = tmp_path / f"descriptor_{uuid4().hex[:8]}.json"
    descriptor.write_text(
        json.dumps(
            {
                "version": 1,
                "rules": [{"match": f"{base_name}:*", "packages": [companion.fq]}],
            }
        )
    )
    # `--registry` names this test's own tier path, so the `global` repository under it is not the shared slot.
    published = ocx.run(
        "patch",
        "publish",
        "--descriptor",
        str(descriptor),
        "--global",
        "--registry",
        f"{registry}/{tier_path}",
        format=None,
        check=False,
    )
    assert published.returncode == EXIT_SUCCESS, published.stderr
    ocx.plain("package", "install", package.short)

    project = tmp_path / f"proj_{uuid4().hex[:8]}"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\ntool = "{package.fq}"\n')
    for step in ("lock", "pull"):
        done = _run(ocx, project, step)
        assert done.returncode == EXIT_SUCCESS, f"ocx {step}: {done.stderr}"
    return PatchedProject(project, package, base_name)


@pytest.fixture
def patched(ocx: OcxRunner, tmp_path: Path, registry: str) -> PatchedProject:
    return _patched_project(ocx, tmp_path, registry)


def _hook_shell_state() -> str:
    """A readable state as a hook shell holds it: a claimed session, one applied
    session record, and an inherited scope the exporter must never overwrite."""
    state = exec_state.default_state()
    state["applied"] = [
        exec_state.applied_constant("HOOK_OWNED_KEY", "hook-owned-value")
    ]
    state["inherited_scopes"] = ["global"]
    state["session"]["token"] = {"pid": os.getpid()}
    state["session"]["fp"] = "hook-fingerprint"
    return exec_state.encode(state, narrowing=("offline",))


def _readable_state(narrowing: tuple[str, ...] = ()) -> str:
    return exec_state.encode(exec_state.default_state(), narrowing=narrowing)


def _unreadable_state(narrowing: tuple[str, ...] = ()) -> str:
    return exec_state.craft_other_major(exec_state.MAJOR + 1, narrowing=narrowing)


def _exported_states(shell_output: str) -> list[str]:
    """Every `__OCX_ENV_STATE` value a POSIX export script assigns."""
    values: list[str] = []
    for line in shell_output.splitlines():
        if STATE_KEY not in line:
            continue
        tail = line.split(f"{STATE_KEY}=", 1)[1]
        values.append(shlex.split(tail)[0])
    return values


def _github_files(tmp_path: Path, label: str) -> tuple[Path, Path, dict[str, str]]:
    path_file = tmp_path / f"{label}_github_path"
    env_file = tmp_path / f"{label}_github_env"
    path_file.write_text("")
    env_file.write_text("")
    return (
        path_file,
        env_file,
        {
            "GITHUB_ACTIONS": "true",
            "GITHUB_PATH": str(path_file),
            "GITHUB_ENV": str(env_file),
        },
    )


def _parse_github_env(env_file: Path) -> dict[str, str]:
    """`KEY=VALUE` lines and `KEY<<DELIM ... DELIM` blocks, as the runner reads them."""
    values: dict[str, str] = {}
    lines = env_file.read_text().splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if "<<" in line.split("=", 1)[0]:
            key, _, delimiter = line.partition("<<")
            body: list[str] = []
            i += 1
            while i < len(lines) and lines[i] != delimiter:
                body.append(lines[i])
                i += 1
            values[key] = "\n".join(body)
        elif "=" in line:
            key, _, value = line.partition("=")
            values[key] = value
        i += 1
    return values


def _decode_ready(raw: str | None) -> dict:
    decoded = exec_state.decode(raw)
    assert isinstance(decoded, dict), (
        f"expected a readable state, got {decoded!r} from {raw!r}"
    )
    return decoded


def _assert_fresh_minimal(state: dict, *, narrowing: tuple[str, ...]) -> None:
    """Every field the exporter contract fixes for a state an exporter writes."""
    assert state["major"] == exec_state.MAJOR
    assert tuple(state["narrowing"]) == narrowing, state["narrowing"]
    assert state["home"] is None, "an exported state carries no home"
    assert state["policy"] == {}, state["policy"]
    assert state["applied"] == [], "an exported state records nothing as applied"
    assert state["inherited_scopes"] == [], state["inherited_scopes"]
    assert state["replay"]["env"] == [], state["replay"]["env"]
    assert state["selection"] == "follow_cwd", state["selection"]
    assert state["truncated"] == "none", state["truncated"]
    session = state["session"]
    # The wire omits an absent token, verdict and project, so a missing key is the reset value.
    assert session.get("token") is None, "an exported session is unclaimed"
    assert (
        session.get("fp", "") == ""
        and session.get("ws", "") == ""
        and session.get("messages_fp", "") == ""
    )
    assert (
        session.get("tiers", []) == []
        and session.get("verdict") is None
        and session.get("project") is None
    )


def _assert_identities_inherited(state: dict, base_name: str) -> None:
    identities = state["replay"]["launch_identities"]
    assert identities, "the composition's identities must be in the replay"
    assert all(record["owner"] == "inherited" for record in identities), identities
    assert any(
        base_name in name for record in identities for name in record["names"]
    ), identities


def _runner_path(ocx: OcxRunner, tmp_path: Path) -> str:
    """A runner PATH that resolves a bare `ocx` to the binary under test, as the setup action leaves it.

    A generated launcher falls back to `ocx` on `PATH` when no pin is exported, so the
    environment a later step runs in has to carry one.
    """
    bin_dir = tmp_path / "runner_bin"
    bin_dir.mkdir(exist_ok=True)
    link = bin_dir / ocx.binary.name
    if not link.exists():
        try:
            link.symlink_to(ocx.binary)
        except OSError:
            # Windows grants symlinks to privileged accounts only; a copy answers `ocx` as well.
            shutil.copy2(ocx.binary, link)
    return os.pathsep.join([str(bin_dir), ocx.env.get("PATH", "")])


def _launcher_env(
    ocx: OcxRunner, path_file: Path, env_file: Path, tmp_path: Path
) -> dict[str, str]:
    """The environment a later GitHub step sees: the sinks' contents on a runner PATH."""
    env = dict(ocx.env)
    env.update(_parse_github_env(env_file))
    added = [ln for ln in path_file.read_text().splitlines() if ln.strip()]
    env["PATH"] = os.pathsep.join([*added, _runner_path(ocx, tmp_path)])
    return env


def _dump_command() -> list[str]:
    """A child printing `NAME=value` lines of the environment it received, in this platform's spelling."""
    return [
        sys.executable,
        "-c",
        "import os; [print(f'{k}={v}') for k, v in os.environ.items()]",
    ]


def _run_showenv(env: dict[str, str], cwd: Path) -> subprocess.CompletedProcess[str]:
    # CreateProcess resolves a bare name on the *parent's* PATH, so resolve it on the child's.
    launcher = shutil.which("showenv", path=env.get("PATH")) or "showenv"
    return subprocess.run(
        [launcher], cwd=cwd, capture_output=True, text=True, env=env, check=False
    )


# ---------------------------------------------------------------------------
# --shell and direnv export: no state entry when the inherited state is readable
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("command", [("env", "--shell=bash"), ("direnv", "export")])
def test_shell_export_with_readable_state_emits_no_state_entry(
    ocx: OcxRunner, patched: PatchedProject, command: tuple[str, ...]
) -> None:
    """A fresh state would replace the hook's session records, so an
    exporter into the running shell emits none while the inherited one is readable."""
    env = {**ocx.env, STATE_KEY: _hook_shell_state()}
    result = _run(ocx, patched.directory, *command, env=env)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert STATE_KEY not in result.stdout, (
        f"`ocx {' '.join(command)}` must not write {STATE_KEY} over a readable state:\n{result.stdout}"
    )
    assert COMPANION_KEY in result.stdout, (
        "the export itself must still carry the overlay"
    )


# ---------------------------------------------------------------------------
# --shell and direnv export: a fresh minimal state when none is readable
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("command", [("env", "--shell=bash"), ("direnv", "export")])
@pytest.mark.parametrize("inherited", [None, "unreadable"])
def test_shell_export_without_readable_state_emits_a_fresh_minimal_state(
    ocx: OcxRunner,
    patched: PatchedProject,
    command: tuple[str, ...],
    inherited: str | None,
) -> None:
    """Absent or unreadable inherited state -> a fresh minimal state, with the
    inherited header's narrowing names kept (an unreadable payload still honours its header)."""
    env = dict(ocx.env)
    narrowing: tuple[str, ...] = ()
    if inherited == "unreadable":
        narrowing = ("offline",)
        env[STATE_KEY] = _unreadable_state(narrowing)
    result = _run(ocx, patched.directory, *command, env=env)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    states = _exported_states(result.stdout)
    assert len(states) == 1, (
        f"exactly one {STATE_KEY} entry expected, got {states}\n{result.stdout}"
    )
    state = _decode_ready(states[0])
    _assert_fresh_minimal(state, narrowing=narrowing)
    _assert_identities_inherited(state, patched.base_name)


# ---------------------------------------------------------------------------
# --ci and the report: always a fresh minimal state
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("label", "inherited", "narrowing"),
    [
        ("absent", None, ()),
        ("readable_hook_state", "hook", ("offline",)),
        ("readable_offline_parent", "offline", ("offline",)),
        ("unreadable_offline_parent", "unreadable", ("offline",)),
    ],
)
def test_ci_export_always_writes_a_fresh_minimal_state(
    ocx: OcxRunner,
    patched: PatchedProject,
    tmp_path: Path,
    label: str,
    inherited: str | None,
    narrowing: tuple[str, ...],
) -> None:
    """`--ci` writes a fresh state even over a readable one, carrying `n=offline`
    from an `--offline` parent and never the parent's applied records or session."""
    _path_file, env_file, ci_env = _github_files(tmp_path, label)
    env = {**ocx.env, **ci_env}
    inherited_values = {
        "hook": _hook_shell_state(),
        "offline": _readable_state(("offline",)),
        "unreadable": _unreadable_state(("offline",)),
    }
    if inherited is not None:
        env[STATE_KEY] = inherited_values[inherited]
    result = _run(ocx, patched.directory, "env", "--ci=github", env=env)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    exported = _parse_github_env(env_file)
    assert STATE_KEY in exported, (
        f"--ci must write {STATE_KEY}; got keys {sorted(exported)}"
    )
    state = _decode_ready(exported[STATE_KEY])
    _assert_fresh_minimal(state, narrowing=narrowing)
    _assert_identities_inherited(state, patched.base_name)
    assert "hook-owned-value" not in env_file.read_text(), (
        "the parent's records must not be copied"
    )


def test_report_carries_a_fresh_minimal_state(
    ocx: OcxRunner, patched: PatchedProject
) -> None:
    """The structured report is an exporter; it carries the fresh state over a readable one."""
    env = {**ocx.env, STATE_KEY: _hook_shell_state()}
    result = _run(ocx, patched.directory, "--format", "json", "env", env=env)
    assert result.returncode == EXIT_SUCCESS, result.stderr
    items = json.loads(result.stdout)["items"]
    entry = next((e for e in items if e["key"] == STATE_KEY), None)
    assert entry is not None, (
        f"the report must list {STATE_KEY}; got {[e['key'] for e in items]}"
    )
    state = _decode_ready(entry["value"])
    _assert_fresh_minimal(state, narrowing=("offline",))
    _assert_identities_inherited(state, patched.base_name)


def test_no_patches_tier_exports_no_state_entry(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """C9: the entry exists only when a `[patches]` tier is in effect."""
    package = make_package(
        ocx, f"t_{uuid4().hex[:8]}_exporter_plain", "1.0.0", tmp_path, cascade=False
    )
    project = tmp_path / "plain_project"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\ntool = "{package.fq}"\n')
    for step in ("lock", "pull"):
        assert _run(ocx, project, step).returncode == EXIT_SUCCESS
    _path_file, env_file, ci_env = _github_files(tmp_path, "plain")
    result = _run(ocx, project, "env", "--ci=github", env={**ocx.env, **ci_env})
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert STATE_KEY not in _parse_github_env(env_file)
    shell = _run(ocx, project, "env", "--shell=bash")
    assert STATE_KEY not in shell.stdout


# ---------------------------------------------------------------------------
# A launcher run from the exported environment matches patch rules by name
# ---------------------------------------------------------------------------


def test_launcher_from_ci_export_matches_patch_rules_by_name(
    ocx: OcxRunner, patched: PatchedProject, tmp_path: Path
) -> None:
    """A later step runs the tool's launcher from the exported `PATH` and
    `$GITHUB_ENV`; the rule keyed on the tool's repository applies, so the companion
    variable reaches the launchered process."""
    path_file, env_file, ci_env = _github_files(tmp_path, "launch")
    result = _run(
        ocx, patched.directory, "env", "--ci=github", env={**ocx.env, **ci_env}
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    step = _run_showenv(_launcher_env(ocx, path_file, env_file, tmp_path), tmp_path)
    assert step.returncode == EXIT_SUCCESS, step.stderr
    assert f"{COMPANION_KEY}={COMPANION_VALUE}" in step.stdout, step.stdout


def test_launcher_from_shell_export_matches_patch_rules_by_name(
    ocx: OcxRunner, patched: PatchedProject, tmp_path: Path
) -> None:
    """`eval "$(ocx env --shell)"` in a shell with no state delivers the identities."""
    exported = _run(ocx, patched.directory, "env", "--shell=bash")
    assert exported.returncode == EXIT_SUCCESS, exported.stderr
    result = run_after_sourcing(
        exported.stdout,
        "showenv",
        tmp_path,
        {**ocx.env, "PATH": _runner_path(ocx, tmp_path)},
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert f"{COMPANION_KEY}={COMPANION_VALUE}" in result.stdout, result.stdout


# ---------------------------------------------------------------------------
# GitHub composite action: a later step with a step-level `env: PATH:` override
# ---------------------------------------------------------------------------


def test_isolate_in_a_later_ci_step_removes_nothing_the_step_set(
    ocx: OcxRunner, patched: PatchedProject, tmp_path: Path
) -> None:
    """A composite action's own step overrides `PATH` and a variable the
    export set; `ocx exec --isolate` there reverts only what ocx applied, and the exported
    state holds no applied records, so the step's `PATH` entry and its own variables survive.
    A key the package composes is set by the composition itself, so the package value wins."""
    path_file, env_file, ci_env = _github_files(tmp_path, "composite")
    first = _run(
        ocx, patched.directory, "env", "--ci=github", env={**ocx.env, **ci_env}
    )
    assert first.returncode == EXIT_SUCCESS, first.stderr

    step_bin = tmp_path / "step_bin"
    step_bin.mkdir()
    step_env = _launcher_env(ocx, path_file, env_file, tmp_path)
    # `env: PATH:` on the step replaces the runner-built PATH wholesale.
    step_env["PATH"] = os.pathsep.join([str(step_bin), _runner_path(ocx, tmp_path)])
    step_env[PACKAGE_CONST_KEY] = "step-level-value"
    step_env["STEP_ONLY_KEY"] = "step-only-value"

    result = _run(
        ocx,
        patched.directory,
        "exec",
        "--isolate",
        "--",
        # Not `env`: on Windows that is MSYS `env`, which prints `PATH` as `/c/...:/c/...`.
        *_dump_command(),
        env=step_env,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    child = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
    assert str(step_bin) in child["PATH"].split(os.pathsep), (
        f"--isolate must keep the step's PATH entry; got PATH={child['PATH']}"
    )
    assert child["STEP_ONLY_KEY"] == "step-only-value"
    assert child[PACKAGE_CONST_KEY] == PACKAGE_CONST_VALUE, (
        "the exec composition applies the package's own constant over the step's value"
    )


def test_composite_action_second_export_does_not_claim_the_first_steps_values(
    ocx: OcxRunner, patched: PatchedProject, tmp_path: Path
) -> None:
    """C9: the state holds no applied records, so a second export in a composite action's
    own step finds nothing of the first step's to claim or revert; it writes its own fresh state."""
    path_file, env_file, ci_env = _github_files(tmp_path, "twice")
    assert (
        _run(
            ocx, patched.directory, "env", "--ci=github", env={**ocx.env, **ci_env}
        ).returncode
        == EXIT_SUCCESS
    )
    second_path = tmp_path / "second_github_path"
    second_env = tmp_path / "second_github_env"
    second_path.write_text("")
    second_env.write_text("")
    env = _launcher_env(ocx, path_file, env_file, tmp_path)
    env.update(
        {
            "GITHUB_PATH": str(second_path),
            "GITHUB_ENV": str(second_env),
            "GITHUB_ACTIONS": "true",
        }
    )
    env["PATH"] = os.pathsep.join(["/step/override/bin", _runner_path(ocx, tmp_path)])
    assert (
        _run(ocx, patched.directory, "env", "--ci=github", env=env).returncode
        == EXIT_SUCCESS
    )

    state = _decode_ready(_parse_github_env(second_env)[STATE_KEY])
    _assert_fresh_minimal(state, narrowing=())
    assert "/step/override/bin" not in second_path.read_text()
