"""How the activation hook treats an inherited execution state (ADR exec_env_model § Hook behaviour per state).

Each row starts a prompt from a **crafted** ``__OCX_ENV_STATE`` (``src/exec_state.py``), run
``--offline`` over fixtures built on disk. ``_prompt`` is one hook invocation: ``ocx self activate
--reconcile`` (or the login stream) with the carrier in the environment and the shell's pid in argv,
evaluated in a real bash; it returns the environment that shell would hold, and feeding that to the
next ``_prompt`` threads a session. ``_PID`` is the one shell, another pid a second one (tmux pane,
IDE terminal). Per-hook-shell stand-down rows live in ``test_shell_reconcile_edge_cases.py``.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix
from src import exec_state
from src.runner import OcxRunner
from src.toolchain_fixtures import locked_project

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="the session rows drive POSIX-family shells; the Windows leg is not part of this module.",
    ),
]

_OCX = matrix.ocx_binary()
_BASH = shutil.which("bash")

pytestmark.append(
    pytest.mark.skipif(
        _OCX is None,
        reason="no ocx binary (set OCX_ACTIVATION_BINARY / OCX_COMMAND, or build test/bin/ocx).",
    )
)
pytestmark.append(
    pytest.mark.skipif(
        _BASH is None,
        reason="bash is needed to evaluate the emitted stream (shutil.which returned None).",
    )
)
pytestmark.append(pytest.mark.command("shell_state", "self_group/activate"))

# The one shell every prompt of a test belongs to; the pytest process is alive for the whole run, so
# its start time is readable where the platform allows it.
_PID = os.getpid()
_LINUX = sys.platform.startswith("linux")

_CANDIDATE_REL = (
    Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
)
_GLOBAL = "global"

# The stripped set of the design record: ASCII controls, DEL, U+0080-U+009F and the bidi controls.
_STRIPPED = {
    *range(0x20),
    0x7F,
    *range(0x80, 0xA0),
    0x061C,
    0x200E,
    0x200F,
    *range(0x202A, 0x202F),
    *range(0x2066, 0x206A),
} - {0x0A}

_STAND_DOWN_OTHER_VERSION = (
    "ocx: this shell's environment state was written by another ocx version (format 1; this ocx reads format 2). "
    "ocx stopped updating this shell. Run 'unset __OCX_ENV_STATE' to start over here "
    "(values already applied stay), or open a new shell."
)


# ---------------------------------------------------------------------------
# Arena and world
# ---------------------------------------------------------------------------


@dataclass
class Arena:
    """One test's private ``$HOME``, ``$OCX_HOME``, script dir and project root."""

    home: Path
    ocx_home: Path
    scripts: Path
    projects: Path
    ocx: Path

    def env(self, shell_abs: str = "/bin/sh", **extra: str) -> dict[str, str]:
        return matrix.clean_env(self.home, shell_abs, ocx_home=self.ocx_home, **extra)


@pytest.fixture
def arena(tmp_path: Path) -> Arena:
    """A clean install root with the ocx bootstrap candidate already seeded."""
    root = tmp_path.resolve()
    home = root / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    scripts = root / "scripts"
    scripts.mkdir()
    projects = root / "projects"
    projects.mkdir()
    return Arena(
        home=home, ocx_home=ocx_home, scripts=scripts, projects=projects, ocx=_OCX
    )


def _locked_project(arena: Arena, name: str, env_block: str) -> Path:
    """A project with a real ``ocx.lock`` and therefore a real consent stamp."""
    project = arena.projects / name
    matrix.write_project(project, env_block)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, (
        f"ocx lock must succeed for the fixture; stderr:\n{result.stderr}"
    )
    return project


def _relock(arena: Arena, project: Path, env_block: str) -> None:
    """Rewrite a project's ``[env]`` and lock it again (``ocx lock`` is a consent-stamp writer)."""
    matrix.write_project(project, env_block)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, f"ocx lock must succeed; stderr:\n{result.stderr}"


def _env_block(key: str, value: str, bin_dir: Path) -> str:
    return f'{key} = "{value}"\nPATH = {{ type = "path", value = "{bin_dir}" }}\n'


@dataclass
class World:
    """The global tier, two consented projects ``p`` and ``q``, and a directory in no project."""

    arena: Arena
    p: Path
    q: Path
    outside: Path

    def layer(self, name: str) -> tuple[str, str, Path]:
        """``(key, value, bin dir)`` of the scope ``name`` (``p``, ``q`` or ``global``)."""
        return {
            "p": ("OCXS_P", "p", self.p / "binP"),
            "q": ("OCXS_Q", "q", self.q / "binQ"),
            _GLOBAL: ("OCXS_GLOBAL", "g", self.arena.ocx_home / "binG"),
        }[name]

    def scope(self, name: str) -> str | dict:
        """The ``Scope`` wire form of ``name``."""
        if name == _GLOBAL:
            return _GLOBAL
        return {"project": {"root": str(self.p if name == "p" else self.q)}}


@pytest.fixture
def world(arena: Arena) -> World:
    bin_g = arena.ocx_home / "binG"
    bin_g.mkdir()
    matrix.write_project(arena.ocx_home, _env_block("OCXS_GLOBAL", "g", bin_g))
    p = arena.projects / "p"
    (p / "binP").mkdir(parents=True)
    _locked_project(arena, "p", _env_block("OCXS_P", "p", p / "binP"))
    q = arena.projects / "q"
    (q / "binQ").mkdir(parents=True)
    _locked_project(arena, "q", _env_block("OCXS_Q", "q", q / "binQ"))
    outside = arena.projects / "outside"
    outside.mkdir()
    return World(arena=arena, p=p, q=q, outside=outside)


@pytest.fixture
def other_shell() -> int:
    """The pid of a second live process: another shell instance (a tmux pane, an IDE terminal)."""
    process = subprocess.Popen(["sleep", "600"])
    try:
        yield process.pid
    finally:
        process.kill()
        process.wait()


# ---------------------------------------------------------------------------
# Crafting states
# ---------------------------------------------------------------------------

_INHERITED = "inherited"


def _session_owner(scope: str = _GLOBAL, source: str = "composed") -> dict:
    return {"session": {"scope": scope, "source": source}}


def _constant(
    key: str, value: str, owner: str | dict, prior: str | dict = "unset"
) -> dict:
    return {
        "key": key,
        "value": {"literal": value},
        "kind": "constant",
        "owner": owner,
        "prior": prior,
    }


def _path(
    element: Path | str, owner: str | dict, *, pre_existing: bool = False
) -> dict:
    record = {
        "key": "PATH",
        "value": {"literal": str(element)},
        "kind": "path",
        "owner": owner,
    }
    if pre_existing:
        record["pre_existing"] = True
    return record


def _state(
    *,
    applied: list[dict] | None = None,
    scopes: list | None = None,
    selection: str | dict = "follow_cwd",
    token: dict | None = None,
    truncated: str = "none",
    hint: dict | None = None,
    **extra: object,
) -> dict:
    state = exec_state.default_state()
    state["applied"] = applied or []
    state["inherited_scopes"] = scopes or []
    state["selection"] = selection
    state["session"]["token"] = token
    state["session"]["project"] = hint
    state["truncated"] = truncated
    state.update(extra)
    return state


def _pinned(tier: Path | str, *, groups: tuple[str, ...] = ("default",)) -> dict:
    """``Selection::Pinned`` over a project dir or the global tier."""
    return {
        "pinned": {
            "tier": _GLOBAL if tier == _GLOBAL else {"project": {"dir": str(tier)}},
            "groups": list(groups),
            "with_names": [],
            "env_keys": [],
            "invocation_digest": "0" * 64,
        }
    }


def _exec_layer(world: World, names: list[str]) -> tuple[dict[str, str], dict]:
    """The environment and the state of a shell that ``ocx exec`` started with ``names`` composed.

    ``names`` run oldest first; every record is ``Inherited``, as a spawn leaves them.
    """
    applied: list[dict] = []
    scopes: list = []
    values: dict[str, str] = {}
    bins: list[str] = []
    for name in names:
        key, value, bin_dir = world.layer(name)
        values[key] = value
        bins.insert(0, str(bin_dir))
        applied += [_constant(key, value, _INHERITED), _path(bin_dir, _INHERITED)]
        scopes.append(world.scope(name))
    env = world.arena.env(PATH=os.pathsep.join([*bins, matrix.BASE_PATH]), **values)
    env[matrix.CARRIER] = exec_state.encode(_state(applied=applied, scopes=scopes))
    return env, _state(applied=applied, scopes=scopes)


def _carrier_with(env: dict[str, str], state: dict, **header: object) -> dict[str, str]:
    return {**env, matrix.CARRIER: exec_state.encode(state, **header)}


def _reencode(decoded: dict) -> str:
    """Encode a decoded state again, header included."""
    payload = {
        key: value
        for key, value in decoded.items()
        if key not in ("narrowing", "home", "major")
    }
    return exec_state.encode(
        payload, narrowing=tuple(decoded["narrowing"]), home=decoded["home"]
    )


# ---------------------------------------------------------------------------
# One hook invocation
# ---------------------------------------------------------------------------


@dataclass
class Prompt:
    """What one invocation printed and the environment a bash that evaluated it holds."""

    process: subprocess.CompletedProcess[str]
    env: dict[str, str]
    eval_stderr: str = ""

    @property
    def stdout(self) -> str:
        return self.process.stdout

    @property
    def stderr(self) -> str:
        """What the user would see: ocx's own stderr plus what the evaluated stream printed (notices are emitted code)."""
        return self.process.stderr + self.eval_stderr

    @property
    def state(self) -> dict:
        decoded = exec_state.decode(self.env.get(matrix.CARRIER))
        assert isinstance(decoded, dict), (
            f"the carrier must hold a readable state afterwards; got {decoded!r}"
        )
        return decoded


def _evaluate(
    env: dict[str, str], stream: str, cwd: Path
) -> tuple[dict[str, str], str]:
    done = subprocess.run(
        [_BASH, "--norc", "-c", 'eval "$1"; env -0', "bash", stream],
        cwd=str(cwd),
        env=env,
        capture_output=True,
        check=False,
        timeout=60,
    )
    assert done.returncode == 0, (
        f"the emitted stream must evaluate cleanly:\n{stream}\n{done.stderr.decode(errors='replace')}"
    )
    after: dict[str, str] = {}
    for entry in done.stdout.split(b"\0"):
        name, equals, value = entry.partition(b"=")
        if equals:
            after[name.decode()] = value.decode()
    for volatile in ("_", "SHLVL", "PWD", "OLDPWD"):
        after.pop(volatile, None)
    return after, done.stderr.decode(errors="replace")


def _prompt(
    arena: Arena,
    env: dict[str, str],
    cwd: Path,
    *,
    pid: int | None = _PID,
    login: bool = False,
    shell: str = "bash",
) -> Prompt:
    """Run the hook (or the login stream) once from ``cwd`` and evaluate its output."""
    argv = [str(arena.ocx), "--offline", "self", "activate"]
    if not login:
        argv.append("--reconcile")
    argv.append(f"--shell={shell}")
    if pid is not None:
        argv += ["--shell-pid", str(pid)]
    process = subprocess.run(
        argv,
        cwd=str(cwd),
        env=env,
        capture_output=True,
        check=False,
        text=True,
        timeout=120,
    )
    assert process.returncode == 0, (
        f"the hook path exits 0 in every state:\nstderr:\n{process.stderr}"
    )
    after, eval_stderr = _evaluate(env, process.stdout, cwd)
    return Prompt(process=process, env=after, eval_stderr=eval_stderr)


def _claim(world: World, cwd: Path | None = None, *, pid: int = _PID) -> Prompt:
    """A first prompt with no inherited state: the shell claims a session."""
    return _prompt(world.arena, world.arena.env(), cwd or world.outside, pid=pid)


def _owner_is_session(record: dict) -> bool:
    return isinstance(record["owner"], dict) and "session" in record["owner"]


def _records(state: dict, key: str) -> list[dict]:
    return [record for record in state["applied"] if record["key"] == key]


def _path_elements(env: dict[str, str]) -> list[str]:
    return matrix.path_segments(env.get("PATH", ""))


def _scopes_as_set(state: dict) -> list[str]:
    return sorted(
        json.dumps(scope, sort_keys=True) for scope in state["inherited_scopes"]
    )


def _assert_stripped(text: str) -> None:
    offending = sorted({hex(ord(char)) for char in text if ord(char) in _STRIPPED})
    assert not offending, (
        f"a state-derived string must not carry control or bidi characters {offending}:\n{text!r}"
    )


# ---------------------------------------------------------------------------
# Desired composition under FollowCwd
# ---------------------------------------------------------------------------


def test_inside_an_inherited_project_the_hook_composes_nothing_global_included(
    world: World,
) -> None:
    """A shell inside the project an outer ``ocx exec`` composed keeps that layer and adds none."""
    env, _ = _exec_layer(world, ["p"])
    result = _prompt(world.arena, env, world.p)
    assert "OCXS_GLOBAL" not in result.env, (
        "the global tier must not be composed inside an inherited project"
    )
    assert result.env["OCXS_P"] == "p"
    assert str(world.arena.ocx_home / "binG") not in _path_elements(result.env)
    assert str(world.p / "binP") in _path_elements(result.env)
    assert "unset OCXS_P" not in result.stdout, (
        "the hook never removes what an outer frame composed"
    )


def test_in_an_uninherited_project_the_hook_layers_global_and_q_with_q_in_front(
    world: World,
) -> None:
    """Inside Q, which no outer frame composed: global plus Q, Q in front; P's layer stays."""
    env, outer = _exec_layer(world, ["p"])
    result = _prompt(world.arena, env, world.q)
    assert result.env["OCXS_GLOBAL"] == "g" and result.env["OCXS_Q"] == "q"
    assert result.env["OCXS_P"] == "p"
    elements = _path_elements(result.env)
    assert elements.index(str(world.q / "binQ")) < elements.index(
        str(world.arena.ocx_home / "binG")
    ), f"the project must sit in front of the global tier: {elements}"
    after = result.state
    assert _scopes_as_set(after) == _scopes_as_set(outer), (
        "a layering prompt must not touch inherited_scopes"
    )
    assert all(
        _owner_is_session(r)
        for r in after["applied"]
        if r["key"] in ("OCXS_Q", "OCXS_GLOBAL")
    )


@pytest.mark.parametrize(
    "global_inherited", [False, True], ids=["global-free", "global-inherited"]
)
def test_outside_every_project_the_hook_composes_global_unless_it_is_inherited(
    world: World, global_inherited: bool
) -> None:
    """Outside every project: the global tier, unless an outer frame composed it."""
    env, _ = _exec_layer(world, [_GLOBAL] if global_inherited else ["p"])
    result = _prompt(world.arena, env, world.outside)
    if global_inherited:
        assert result.env["OCXS_GLOBAL"] == "g"
        assert "OCXS_GLOBAL" not in result.stdout, (
            "an inherited global tier is never composed again"
        )
    else:
        assert result.env["OCXS_GLOBAL"] == "g", (
            "an uninherited global tier is composed outside every project"
        )
        assert "OCXS_GLOBAL" in result.stdout


def test_an_inherited_global_does_not_hide_project_q(world: World) -> None:
    """Only the missing scopes are added: global inherited, so Q alone."""
    env, _ = _exec_layer(world, [_GLOBAL])
    result = _prompt(world.arena, env, world.q)
    assert result.env["OCXS_Q"] == "q"
    assert "OCXS_GLOBAL" not in result.stdout


def test_an_inherited_projects_files_are_not_watched(world: World) -> None:
    """Editing an inherited project's ``ocx.toml`` changes nothing: exec is a snapshot."""
    env, _ = _exec_layer(world, ["p"])
    first = _prompt(world.arena, env, world.p)
    _relock(world.arena, world.p, _env_block("OCXS_P", "edited", world.p / "binP"))
    second = _prompt(world.arena, first.env, world.p)
    assert second.env["OCXS_P"] == "p", "the inherited layer is a snapshot of the exec"
    assert "OCXS_P" not in second.stdout
    assert "OCXS_GLOBAL" not in second.env


def test_a_received_no_project_ends_the_walk_at_the_hook(world: World) -> None:
    """``OCX_NO_PROJECT=1 ocx exec --clean -- bash`` wrote ``no_project`` into the state: the hook's
    walk may not compose the consented project the prompt stands in."""
    state = _state(policy={"no_project": True})
    env = _carrier_with(world.arena.env(), state)
    result = _prompt(world.arena, env, world.p)
    assert "OCXS_P" not in result.env, (
        f"a received no_project must end the project walk; stdout:\n{result.stdout}"
    )
    assert str(world.p / "binP") not in _path_elements(result.env)
    # A positive a stood-down hook would not produce: with the project walk ended the global tier
    # is what composes in its place.
    assert result.env.get("OCXS_GLOBAL") == "g", (
        f"the hook must honour the received policy and compose the global tier; stdout:\n{result.stdout}"
    )


def test_the_hook_walks_the_owner_scenario_s007_a_through_e(world: World) -> None:
    """``ocx --project P -g x exec -- bash``, then prompts (a) through (e)."""
    env, _ = _exec_layer(world, ["p"])
    # (a) inside P nothing changes, global included
    in_p = _prompt(world.arena, env, world.p)
    assert "OCXS_GLOBAL" not in in_p.env
    # (b) cd Q layers global + Q
    in_q = _prompt(world.arena, in_p.env, world.q)
    assert in_q.env["OCXS_GLOBAL"] == "g" and in_q.env["OCXS_Q"] == "q"
    # (c) back in P they go, and P's values are byte-identical
    back = _prompt(world.arena, in_q.env, world.p)
    assert "OCXS_Q" not in back.env and "OCXS_GLOBAL" not in back.env
    assert back.env["OCXS_P"] == "p"
    assert str(world.p / "binP") in _path_elements(back.env)
    # (d) editing P's ocx.toml changes nothing
    _relock(world.arena, world.p, _env_block("OCXS_P", "edited", world.p / "binP"))
    edited = _prompt(world.arena, back.env, world.p)
    assert edited.env["OCXS_P"] == "p" and "OCXS_GLOBAL" not in edited.env
    # (e) outside, the global tier is composed because nobody inherited it
    outside = _prompt(world.arena, edited.env, world.outside)
    assert outside.env["OCXS_GLOBAL"] == "g" and outside.env["OCXS_P"] == "p"


# ---------------------------------------------------------------------------
# The session token
# ---------------------------------------------------------------------------


def test_a_first_prompt_claims_the_session_for_this_shell(world: World) -> None:
    """An absent state starts a follow-cwd session and claims the token with this shell's pid."""
    result = _prompt(world.arena, world.arena.env(), world.p)
    state = result.state
    assert state["session"]["token"]["pid"] == _PID
    if _LINUX:
        assert isinstance(state["session"]["token"].get("started"), int), (
            "Linux reads field 22 of /proc/<pid>/stat"
        )
    assert state["selection"] == "follow_cwd" and state["inherited_scopes"] == []
    assert _records(state, "OCXS_P") and all(
        _owner_is_session(r) for r in _records(state, "OCXS_P")
    )
    assert result.env["OCXS_P"] == "p" and result.env["OCXS_GLOBAL"] == "g"


def test_this_shells_own_token_keeps_its_session_across_prompts(world: World) -> None:
    """Re-sourcing the rc file or ``exec $SHELL`` keeps the pid, so the session continues and reverts."""
    first = _prompt(world.arena, world.arena.env(), world.p)
    again = _prompt(world.arena, first.env, world.p)
    assert again.state["inherited_scopes"] == [], (
        "the same shell must not retag its own records"
    )
    assert all(_owner_is_session(r) for r in _records(again.state, "OCXS_P"))
    left = _prompt(world.arena, again.env, world.outside)
    assert "OCXS_P" not in left.env, (
        "a session the shell owns reverts when it leaves the project"
    )
    assert left.env["OCXS_GLOBAL"] == "g"


def test_a_foreign_token_is_retagged_and_a_fresh_session_layers_on_top(
    world: World, other_shell: int
) -> None:
    """A tmux pane or IDE shell inheriting another shell's state owns none of its records."""
    theirs = _prompt(world.arena, world.arena.env(), world.p, pid=other_shell)
    assert theirs.state["session"]["token"]["pid"] == other_shell

    ours = _prompt(world.arena, theirs.env, world.q, pid=_PID)
    state = ours.state
    assert state["session"]["token"]["pid"] == _PID, (
        "the retag ends with this shell's claim"
    )
    assert state["selection"] == "follow_cwd"
    for key in ("OCXS_P", "OCXS_GLOBAL"):
        assert _records(state, key) and not any(
            _owner_is_session(r) for r in _records(state, key)
        ), f"{key}: the other shell's session records must be retagged Inherited"
    assert _scopes_as_set(state) == sorted(
        json.dumps(s, sort_keys=True)
        for s in [_GLOBAL, {"project": {"root": str(world.p)}}]
    )
    assert ours.env["OCXS_Q"] == "q" and ours.env["OCXS_P"] == "p"
    assert all(_owner_is_session(r) for r in _records(state, "OCXS_Q"))

    left = _prompt(world.arena, ours.env, world.outside, pid=_PID)
    assert "OCXS_Q" not in left.env, "this shell's own layer reverts"
    assert left.env["OCXS_P"] == "p" and left.env["OCXS_GLOBAL"] == "g", (
        "what was inherited is never reverted"
    )


@pytest.mark.skipif(
    not _LINUX,
    reason="a start time is readable only on Linux (/proc/<pid>/stat field 22)",
)
def test_a_token_without_a_start_time_is_foreign_where_this_shells_start_time_is_readable(
    world: World,
) -> None:
    """Pid reuse is told apart by the start time; a token without one is foreign here."""
    first = _prompt(world.arena, world.arena.env(), world.p)
    decoded = first.state
    assert "started" in decoded["session"]["token"], (
        "the fixture needs a claimed token with a start time"
    )
    del decoded["session"]["token"]["started"]
    env = {**first.env, matrix.CARRIER: _reencode(decoded)}
    result = _prompt(world.arena, env, world.q)
    assert not any(_owner_is_session(r) for r in _records(result.state, "OCXS_P")), (
        "the start-less token must be foreign"
    )
    assert isinstance(result.state["session"]["token"].get("started"), int)


def test_an_unclaimed_follow_cwd_state_with_session_records_is_retagged_before_the_claim(
    world: World,
) -> None:
    """Only ``ocx shell`` writes Session records, always Pinned; under FollowCwd with no claim they are foreign."""
    key, value, bin_dir = world.layer("p")
    state = _state(
        applied=[
            _constant(key, value, _session_owner("project")),
            _path(bin_dir, _session_owner("project")),
        ],
        hint={"key": "0123456789abcdef", "dir": str(world.p)},
    )
    env = _carrier_with(
        world.arena.env(
            PATH=os.pathsep.join([str(bin_dir), matrix.BASE_PATH]), OCXS_P=value
        ),
        state,
    )
    result = _prompt(world.arena, env, world.outside)
    after = result.state
    assert not any(_owner_is_session(r) for r in _records(after, "OCXS_P")), (
        "unclaimed Session records are retagged"
    )
    assert {"project": {"root": str(world.p)}} in after["inherited_scopes"]
    assert after["session"]["token"]["pid"] == _PID
    assert result.env["OCXS_P"] == "p" and "unset OCXS_P" not in result.stdout, (
        "a retagged record is never reverted"
    )


def test_token_less_mode_never_claims_the_session(world: World) -> None:
    """A hook without ``--shell-pid`` (an older emission, an older nu shim) never claims."""
    result = _prompt(world.arena, world.arena.env(), world.p, pid=None)
    assert result.state["session"].get("token") is None, (
        "no pid, so there is nothing to claim with"
    )
    assert result.env["OCXS_P"] == "p", "token-less mode still composes"


def test_token_less_mode_reconciles_session_records_as_its_own(
    world: World, other_shell: int
) -> None:
    """Token-less never classifies a token foreign: today's semantics for Session records."""
    theirs = _prompt(world.arena, world.arena.env(), world.p, pid=other_shell)
    result = _prompt(world.arena, theirs.env, world.outside, pid=None)
    assert "OCXS_P" not in result.env, (
        "token-less mode reverts Session records it finds"
    )
    assert all(_owner_is_session(r) for r in result.state["applied"]), (
        "and retags nothing"
    )


def test_a_received_home_is_ignored_and_the_header_is_never_changed(
    world: World, tmp_path: Path
) -> None:
    """A hook shell carries ``OCX_HOME`` from setup's files; ``h`` is ignored and passed on."""
    decoy = world.arena.projects / "decoy-home"
    decoy.mkdir()
    matrix.write_project(decoy, 'OCXS_DECOY = "decoy"')
    env = _carrier_with(
        world.arena.env(), _state(), narrowing=("offline", "frozen"), home=str(decoy)
    )
    result = _prompt(world.arena, env, world.outside)
    assert result.env["OCXS_GLOBAL"] == "g", (
        "the global tier comes from this shell's OCX_HOME"
    )
    assert "OCXS_DECOY" not in result.env, "the received home must not steer the hook"
    after = result.state
    assert tuple(after["narrowing"]) == ("offline", "frozen") and after["home"] == str(
        decoy
    ), "the hook never changes the header"


# ---------------------------------------------------------------------------
# Stand-down (the shell-agnostic half; per hook shell rows live in the edge-case module)
# ---------------------------------------------------------------------------


def test_a_legacy_state_stands_the_hook_down_with_one_notice_and_no_write(
    world: World,
) -> None:
    """A ``1.`` value: one notice, the value never rewritten, nothing applied or reverted."""
    legacy = exec_state.craft_other_major(1)
    env = {**world.arena.env(), matrix.CARRIER: legacy}
    result = _prompt(world.arena, env, world.p, pid=None)
    assert result.env[matrix.CARRIER] == legacy, (
        "an unreadable state is never written over"
    )
    assert not [key for key in result.env if key.startswith("OCXS_")], (
        "a stood-down hook applies nothing"
    )
    assert _STAND_DOWN_OTHER_VERSION in result.stderr
    assert result.stderr.count("ocx stopped updating this shell") == 1, (
        "the notice is printed once"
    )
    assert matrix.CARRIER in result.stdout and legacy not in result.stdout, (
        "the emitted code copies the value by reference and never embeds it"
    )


@pytest.mark.parametrize(
    ("label", "value", "message"),
    [
        (
            "newer-major",
            exec_state.craft_other_major(3),
            "written by a newer ocx (format 3)",
        ),
        (
            "garbled-payload",
            exec_state.craft_garbled_payload("inflate"),
            "could not be read",
        ),
        ("over-cap", exec_state.craft_over_cap(), "could not be read"),
        ("no-header", "not-a-state", "another ocx version"),
    ],
    ids=["newer-major", "garbled-payload", "over-cap", "no-header"],
)
def test_every_unreadable_state_stands_down_without_writing(
    world: World, label: str, value: str, message: str
) -> None:
    """Unreadable for any reason and any major: stand down, name the cause, write nothing."""
    result = _prompt(
        world.arena,
        {**world.arena.env(), matrix.CARRIER: value},
        world.outside,
        pid=None,
    )
    assert result.env[matrix.CARRIER] == value, (
        f"{label}: the value must stay as it was"
    )
    assert "OCXS_GLOBAL" not in result.env, (
        f"{label}: nothing is applied under an unreadable state"
    )
    assert message in result.stderr, (
        f"{label}: the notice must name the cause:\n{result.stderr}"
    )
    assert "unset __OCX_ENV_STATE" in result.stderr, (
        f"{label}: the notice names the fix"
    )


def test_a_stood_down_hook_does_not_revert_what_the_unreadable_state_may_have_applied(
    world: World,
) -> None:
    """Nothing is reverted: the values the old hook applied stay."""
    env = world.arena.env(OCXS_P="p")
    env[matrix.CARRIER] = exec_state.craft_other_major(1)
    result = _prompt(world.arena, env, world.outside, pid=None)
    assert result.env["OCXS_P"] == "p" and "unset OCXS_P" not in result.stdout, (
        "nothing is reverted (the notice itself names the unset that re-arms the hook)"
    )


# ---------------------------------------------------------------------------
# The pinned session
# ---------------------------------------------------------------------------


def _pinned_shell(
    world: World, *, dir: Path | None = None, extra_records: list[dict] | None = None
) -> dict[str, str]:
    """An ``ocx shell`` in ``p``: Pinned, unclaimed, its composed layer applied as Session records."""
    project = dir or world.p
    key, value, bin_dir = ("OCXS_P", "p", project / "binP")
    applied = [
        _constant(key, value, _session_owner("project")),
        _path(bin_dir, _session_owner("project")),
        *(extra_records or []),
    ]
    env = world.arena.env(
        PATH=os.pathsep.join([str(bin_dir), matrix.BASE_PATH]), **{key: value}
    )
    state = _state(
        applied=applied,
        selection=_pinned(project),
        hint={"key": "0123456789abcdef", "dir": str(project)},
    )
    return {**env, matrix.CARRIER: exec_state.encode(state)}


def test_a_pinned_session_refreshes_its_layer_when_its_ocx_toml_changes(
    world: World,
) -> None:
    """D is the consented groups of the pinned tier; a changed file recomposes them."""
    first = _prompt(world.arena, _pinned_shell(world), world.outside)
    assert first.state["session"]["token"]["pid"] == _PID, (
        "an unclaimed pinned state is claimed"
    )
    assert "OCXS_GLOBAL" not in first.env, (
        "a pinned shell composes neither the cwd's scope nor the global tier"
    )

    _relock(world.arena, world.p, _env_block("OCXS_P", "refreshed", world.p / "binP"))
    refreshed = _prompt(world.arena, first.env, world.outside)
    assert refreshed.env["OCXS_P"] == "refreshed", (
        "a changed ocx.toml refreshes the pinned layer from anywhere"
    )
    assert _records(refreshed.state, "OCXS_P")[0]["value"] == {"literal": "refreshed"}


def test_a_pinned_session_does_not_recompose_while_its_files_are_unchanged(
    world: World,
) -> None:
    """Refresh is gated on the fingerprint of that ``ocx.toml`` and ``ocx.lock``."""
    first = _prompt(world.arena, _pinned_shell(world), world.outside)
    second = _prompt(world.arena, first.env, world.outside)
    assert "OCXS_P" not in second.stdout, "an unchanged project is not applied again"
    assert second.state["session"]["fp"] == first.state["session"]["fp"] != ""


def test_an_invocation_value_wins_over_a_project_edit_of_the_same_key(
    world: World,
) -> None:
    """Invocation records are remove-only, never overwritten by a refresh."""
    project_block = 'OCXS_P = "p"\nOCXS_COMPOSED = "one"\nOCXS_INV = "from-project"\n'
    _relock(world.arena, world.p, project_block)
    records = [
        _constant("OCXS_COMPOSED", "one", _session_owner("project")),
        _constant(
            "OCXS_INV", "from-invocation", _session_owner("project", "invocation")
        ),
    ]
    env = _pinned_shell(world, extra_records=records)
    env.update(OCXS_COMPOSED="one", OCXS_INV="from-invocation")
    first = _prompt(world.arena, env, world.outside)

    _relock(
        world.arena,
        world.p,
        'OCXS_P = "p"\nOCXS_COMPOSED = "two"\nOCXS_INV = "edited-in-project"\n',
    )
    refreshed = _prompt(world.arena, first.env, world.outside)
    assert refreshed.env["OCXS_COMPOSED"] == "two", (
        "the composed key follows the project edit"
    )
    assert refreshed.env["OCXS_INV"] == "from-invocation", (
        "--env wins over a project edit of the same key"
    )
    kept = _records(refreshed.state, "OCXS_INV")
    assert len(kept) == 1 and kept[0]["owner"] == _session_owner(
        "project", "invocation"
    )
    assert kept[0]["value"] == {"literal": "from-invocation"}, (
        "the record's literal is never re-applied or replaced"
    )


def test_a_cd_into_another_project_prints_one_notice_and_applies_nothing(
    world: World,
) -> None:
    """A pinned shell ignores the cwd: one notice naming the pinned selection, once."""
    first = _prompt(world.arena, _pinned_shell(world), world.q)
    assert "OCXS_Q" not in first.env and "OCXS_GLOBAL" not in first.env
    notice = [line for line in first.stderr.splitlines() if line.strip()]
    assert len(notice) == 1, f"exactly one notice line is expected:\n{first.stderr}"
    assert notice[0].startswith("ocx: ") and notice[0].isascii()
    second = _prompt(world.arena, first.env, world.q)
    assert second.stderr.strip() == "", (
        f"the notice is not repeated on the next prompt:\n{second.stderr}"
    )


def test_an_unconsented_pinned_project_keeps_its_spawn_time_layer(world: World) -> None:
    """No consent, no recomposition: the layer stays and nothing is removed or rewritten."""
    clone = world.arena.projects / "clone"
    shutil.copytree(world.p, clone)
    stamp = matrix.stamp_dir(
        world.arena.ocx_home,
        matrix.project_key(world.arena.ocx, clone, world.arena.env()),
    )
    assert not stamp.exists(), (
        "the clone must carry no consent stamp, or this row proves nothing"
    )
    env = _pinned_shell(world, dir=clone)
    first = _prompt(world.arena, env, world.outside)
    matrix.write_project(
        clone, _env_block("OCXS_P", "unconsented-edit", clone / "binP")
    )
    second = _prompt(world.arena, first.env, world.outside)
    assert second.env["OCXS_P"] == "p" and "unset" not in second.stdout
    assert _records(second.state, "OCXS_P")[0]["value"] == {"literal": "p"}


def test_consent_is_checked_before_the_pinned_projects_files_are_opened(
    world: World,
) -> None:
    """An unconsented pinned dir is never opened: a FIFO where ``ocx.toml`` would be does not hang the hook."""
    hostile = world.arena.projects / "hostile"
    hostile.mkdir()
    os.mkfifo(hostile / "ocx.toml")
    os.mkfifo(hostile / "ocx.lock")
    result = _prompt(world.arena, _pinned_shell(world, dir=hostile), world.outside)
    assert result.state["session"]["token"]["pid"] == _PID
    assert result.env["OCXS_P"] == "p", "the spawn-time layer stays"


def test_a_pinned_global_tier_refreshes_from_the_global_toml(world: World) -> None:
    """``PinnedTier::Global`` is always ``$OCX_HOME/ocx.toml``; nothing about it comes from the state."""
    bin_g = world.arena.ocx_home / "binG"
    applied = [
        _constant("OCXS_GLOBAL", "g", _session_owner(_GLOBAL)),
        _path(bin_g, _session_owner(_GLOBAL)),
    ]
    state = _state(applied=applied, selection=_pinned(_GLOBAL))
    env = {**world.arena.env(OCXS_GLOBAL="g"), matrix.CARRIER: exec_state.encode(state)}
    first = _prompt(world.arena, env, world.q)
    assert "OCXS_Q" not in first.env
    matrix.write_project(
        world.arena.ocx_home, _env_block("OCXS_GLOBAL", "refreshed", bin_g)
    )
    refreshed = _prompt(world.arena, first.env, world.q)
    assert refreshed.env["OCXS_GLOBAL"] == "refreshed"


def test_notices_never_print_control_or_bidi_characters_from_the_state(
    world: World,
) -> None:
    """Every state-derived string passes ``display_safe`` before it reaches stderr."""
    hostile_dir = "/tmp/ocx-hostile\x1b[31m\u202e\u061c\u009b\x07evil"
    state = _state(
        selection=_pinned(hostile_dir),
        hint={"key": "0123456789abcdef", "dir": hostile_dir},
    )
    state["inherited_scopes"] = [{"project": {"root": hostile_dir}}]
    env = {**world.arena.env(), matrix.CARRIER: exec_state.encode(state)}
    result = _prompt(world.arena, env, world.q)
    assert result.stderr.strip(), (
        "the fixture must produce a notice, or the check below passes vacuously"
    )
    _assert_stripped(result.stderr)
    _assert_stripped(result.stdout)


# ---------------------------------------------------------------------------
# The login stream
# ---------------------------------------------------------------------------


def test_the_login_stream_of_an_unclaimed_pinned_shell_claims_and_applies_nothing(
    world: World,
) -> None:
    """The rc login stream of the entered shell claims the token and composes nothing."""
    result = _prompt(world.arena, _pinned_shell(world), world.p, login=True)
    assert result.state["session"]["token"]["pid"] == _PID
    assert "OCXS_GLOBAL" not in result.env and "OCXS_Q" not in result.env
    assert result.env["OCXS_P"] == "p", "the layer ocx shell composed stays"
    assert result.state["selection"] != "follow_cwd", "the selection stays pinned"


def test_the_login_stream_composes_nothing_inside_an_inherited_project(
    world: World,
) -> None:
    """``ocx exec -- bash -l`` inside P composes nothing; the same shell with no state does."""
    control = _prompt(world.arena, world.arena.env(), world.p, login=True)
    assert control.env.get("OCXS_GLOBAL") == "g", (
        "the control: with nothing inherited the login stream composes"
    )
    env, _ = _exec_layer(world, ["p"])
    result = _prompt(world.arena, env, world.p, login=True)
    assert "OCXS_GLOBAL" not in result.env
    assert result.env["OCXS_P"] == "p"


def test_the_login_stream_composes_only_the_scopes_inherited_scopes_lacks(
    world: World,
) -> None:
    """Global inherited, cwd in Q: the login stream adds no global; the first prompt adds Q.

    The login stream composes the global tier alone; a project is composed by the hook, which checks
    consent first. So the scope the login stream lacks is the global one, and an inherited global tier
    leaves it nothing to do.
    """
    env, _ = _exec_layer(world, [_GLOBAL])
    login = _prompt(world.arena, env, world.q, login=True)
    assert "--global env" not in login.stdout and "OCXS_GLOBAL" not in login.stdout, (
        "an inherited global tier is not composed again"
    )
    first = _prompt(world.arena, login.env, world.q)
    assert first.env["OCXS_Q"] == "q"
    assert "OCXS_GLOBAL" not in first.stdout


# ---------------------------------------------------------------------------
# Record discipline
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("label", "truncated", "scopes", "repairs"),
    [
        ("all-dropped-no-scopes", "all_applied_dropped", [], True),
        ("all-dropped-with-scopes", "all_applied_dropped", [_GLOBAL], False),
        ("inherited-dropped", "inherited_dropped", [], False),
        ("payload-dropped", "payload_dropped", [], False),
    ],
)
def test_owned_prefix_repair_runs_only_under_all_applied_dropped_with_no_inherited_scopes(
    world: World, label: str, truncated: str, scopes: list, repairs: bool
) -> None:
    """Repair strips an unrecorded ``$OCX_HOME`` segment only where no outer frame can own it; never after rung 3."""
    owned = world.arena.ocx_home / "packages" / "old" / "bin"
    owned.mkdir(parents=True)
    env = world.arena.env(PATH=os.pathsep.join([str(owned), matrix.BASE_PATH]))
    carrier = (
        exec_state.craft_header_only()
        if truncated == "payload_dropped"
        else exec_state.encode(_state(truncated=truncated, scopes=scopes))
    )
    result = _prompt(world.arena, {**env, matrix.CARRIER: carrier}, world.outside)
    present = str(owned) in _path_elements(result.env)
    assert present is not repairs, (
        f"{label}: the unrecorded owned segment must {'go' if repairs else 'stay'}: {_path_elements(result.env)}"
    )


def test_the_hook_never_removes_an_element_an_inherited_record_names(
    world: World,
) -> None:
    """A session record and an inherited record naming one element: leaving the scope keeps the element."""
    shared = world.arena.projects / "shared-bin"
    shared.mkdir()
    token = _claim(world).state["session"]["token"]
    applied = [
        _path(shared, _INHERITED),
        _path(shared, _session_owner("project")),
        _constant("OCXS_Q", "q", _session_owner("project")),
    ]
    state = _state(
        applied=applied,
        token=token,
        hint={"key": "0123456789abcdef", "dir": str(world.q)},
    )
    env = {
        **world.arena.env(
            PATH=os.pathsep.join([str(shared), matrix.BASE_PATH]), OCXS_Q="q"
        ),
        matrix.CARRIER: exec_state.encode(state),
    }
    result = _prompt(world.arena, env, world.outside)
    assert "OCXS_Q" not in result.env, "the session constant reverts"
    assert str(shared) in _path_elements(result.env), (
        "the element an inherited record names stays"
    )
    assert any(r["owner"] == _INHERITED for r in _records(result.state, "PATH")), (
        "the inherited record is kept"
    )


def test_the_hook_writes_only_session_data(world: World) -> None:
    """Header, policy, replay and inherited records pass through a layering prompt unchanged."""
    env, outer = _exec_layer(world, ["p"])
    outer["policy"] = {"remote": False, "default_registry": "registry.example.com"}
    outer["replay"] = {
        "env": [
            {"key": "OCXS_REPLAY", "kind": "constant", "value": {"literal": "replayed"}}
        ],
        "launch_identities": [],
    }
    env = _carrier_with(env, outer, narrowing=("offline",))
    result = _prompt(world.arena, env, world.q)
    after = result.state
    assert after["policy"] == outer["policy"] and after["replay"] == outer["replay"]
    assert tuple(after["narrowing"]) == ("offline",)
    before_inherited = [r for r in outer["applied"] if r["owner"] == _INHERITED]
    after_inherited = [r for r in after["applied"] if r["owner"] == _INHERITED]
    assert after_inherited == before_inherited
    assert after["inherited_scopes"] == outer["inherited_scopes"]


# ---------------------------------------------------------------------------
# --shell-pid per hook shell
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("shell", "expression"),
    [
        ("bash", "$$"),
        ("zsh", "$$"),
        ("fish", "$fish_pid"),
        ("pwsh", "$PID"),
        ("elvish", "$pid"),
    ],
)
def test_each_hook_passes_its_own_shells_pid(
    arena: Arena, shell: str, expression: str
) -> None:
    """The emitted hook passes ``--shell-pid`` from the shell's own pid variable; no shell needs to be installed."""
    emitted = subprocess.run(
        [
            str(arena.ocx),
            "--offline",
            "self",
            "activate",
            f"--shell={shell}",
            "--hook",
            "--no-completion",
        ],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        timeout=60,
    )
    assert emitted.returncode == 0, emitted.stderr
    at = emitted.stdout.find("--shell-pid")
    assert at != -1, f"{shell}: the hook must pass --shell-pid:\n{emitted.stdout}"
    assert expression in emitted.stdout[at : at + 48], (
        f"{shell}: expected {expression!r} after --shell-pid:\n{emitted.stdout[at : at + 80]}"
    )


def test_the_nushell_shim_passes_its_own_pid(arena: Arena) -> None:
    """Nu's hook is the static ``env.nu`` that ``ocx self setup`` writes.

    It reconciles from a ``pre_prompt`` hook and once at startup, and both calls hand the binary
    ``$nu.pid``, so a nushell session claims a token like every other hooked shell.
    """
    env = arena.env()
    env["SHELL"] = "/usr/bin/nu"
    done = subprocess.run(
        [str(arena.ocx), "--offline", "self", "setup"],
        capture_output=True,
        check=False,
        text=True,
        env=env,
        timeout=120,
    )
    assert done.returncode == 0, done.stderr
    shim = arena.ocx_home / "env.nu"
    assert shim.is_file(), f"self setup must write {shim}"
    body = shim.read_text(encoding="utf-8")
    call = "self activate --reconcile --shell=nushell --shell-pid $nu.pid"
    assert body.count(call) == 2, f"env.nu must reconcile at startup and from the prompt hook, each with its pid:\n{body}"
    assert "--global env" not in body, f"env.nu must apply the reconcile plan, not the global-only document:\n{body}"


# ---------------------------------------------------------------------------
# Tier 3 — a real bash on a pty
# ---------------------------------------------------------------------------


def _bash_pty(arena: Arena, lines: list[str]) -> str:
    shell_abs = matrix.which_arm("bash")
    assert shell_abs is not None
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""
    return matrix.pty_session(
        [shell_abs, "--norc", "-i"], lines, cwd=arena.projects, env=env
    )


def _hook_line(arena: Arena) -> str:
    return f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"'


_MAJOR_PROBE = 'printf "%s\\n" "@@major@@${__OCX_ENV_STATE%%[.;]*}"'


def test_sourcing_the_rc_file_again_and_exec_of_the_shell_keep_the_session(
    world: World,
) -> None:
    """The same process keeps its token: leaving the project still reverts what the session applied."""
    hook = _hook_line(world.arena)
    output = _bash_pty(
        world.arena,
        [
            hook,
            f"cd '{world.p}'",
            "true",
            hook,
            "true",
            "exec bash --norc -i",
            hook,
            "true",
            f"cd '{world.outside}'",
            "true",
            matrix.probe("bash", "const", "OCXS_P"),
            _MAJOR_PROBE,
        ],
    )
    found = matrix.probes(output)
    assert found.get("major") == "2", (
        f"the session must run on a format 2 state\npty transcript:\n{output}"
    )
    assert found.get("const") == matrix.ABSENT, (
        f"a re-sourced rc file and an exec'd shell are the same shell: the project layer must revert\npty transcript:\n{output}"
    )


def test_a_nested_shell_owns_none_of_the_outer_shells_records(world: World) -> None:
    """A tmux pane or a plain ``bash`` inside the session is another process: it reverts nothing inherited."""
    hook = _hook_line(world.arena)
    output = _bash_pty(
        world.arena,
        [
            hook,
            f"cd '{world.p}'",
            "true",
            "bash --norc -i",
            hook,
            "true",
            f"cd '{world.outside}'",
            "true",
            matrix.probe("bash", "const", "OCXS_P"),
            _MAJOR_PROBE,
            "exit",
        ],
    )
    found = matrix.probes(output)
    assert found.get("major") == "2", (
        f"the nested shell must run on a format 2 state\npty transcript:\n{output}"
    )
    assert found.get("const") == "p", (
        f"the nested shell must keep the outer project's values: they are inherited, not its own\npty transcript:\n{output}"
    )


# ---------------------------------------------------------------------------
# The project's trampoline directory leaves with the project
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("leave", ["bin to none", "the project"])
def test_leaving_bin_mode_or_the_project_withdraws_the_trampoline_directory(
    ocx: OcxRunner, tmp_path: Path, leave: str
) -> None:
    """``<project home>/bin`` leaves ``PATH`` by the record that put it there.

    The state is intact (nothing truncated), so no owned-prefix repair is in play: a planner that
    retires only the elements it recorded as applied strands the trampoline directory the moment the
    project stops asking for it, because session directories were never recorded. This is the one row
    of the module that needs a rendered toolchain, so it publishes a package to the registry fixture.
    """
    bash = _BASH
    assert bash is not None
    project = locked_project(ocx, tmp_path, label="bm").directory
    ocx_home = Path(ocx.env["OCX_HOME"])
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    home = tmp_path / "shell-home"
    home.mkdir()
    scripts = tmp_path / "shell-scripts"
    scripts.mkdir()
    outside = tmp_path / "shell-outside"
    outside.mkdir()

    leaving = (
        "export OCX_TOOLCHAIN_ACTIVATE=none"
        if leave == "bin to none"
        else matrix.cd_to("bash", outside)
    )
    body = "\n".join(
        [
            matrix.header("bash", _OCX),
            matrix.cd_to("bash", outside),
            matrix.prompt("bash"),
            matrix.probe("bash", "base", "PATH"),
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "in", "PATH"),
            leaving,
            matrix.prompt("bash"),
            matrix.probe("bash", "out", "PATH"),
        ]
    )
    env = matrix.clean_env(home, bash, ocx_home=ocx_home, OCX_TOOLCHAIN_ACTIVATE="bin")
    result = matrix.run_script(
        "bash", bash, body, cwd=outside, env=env, script_dir=scripts, name="bin-mode"
    )

    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    found = matrix.probes(result.stdout)
    assert {"base", "in", "out"} <= found.keys(), (
        f"the session never reached its probes\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    base = matrix.path_segments(found["base"])
    entered = matrix.path_segments(found["in"])
    trampolines = [segment for segment in entered if segment not in base]
    assert len(trampolines) == 1 and trampolines[0].endswith("/bin"), (
        f"entering a `bin` project must add exactly its trampoline directory; got {trampolines}"
    )
    left = matrix.path_segments(found["out"])
    assert trampolines[0] not in left, (
        f"leaving ({leave}) must withdraw {trampolines[0]}; PATH is {left}"
    )
    assert left == base, f"after leaving ({leave}) PATH is what it was outside: {left} != {base}"
