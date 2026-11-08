# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""End to end: the execution state against older writers, its size cap, secrets and the binary pin.

Specified by the user-experience scenarios of
``.claude/artifacts/plan_exec_env_model.md#user`` and the codec and pin contracts
of ``.claude/artifacts/adr_exec_env_model.md``. Each case runs the real binary in a private home.

Not runnable here: a previous-release *binary* as the middle hop of a live chain. None is reachable
offline and the session registries hold only what the suite pushed, so the middle-hop cases run the
previous-release launcher and trampoline *bodies* from ``data/exec_env/v0.6.5/`` instead.
"""

from __future__ import annotations

import hashlib
import json
import re
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix
from src import exec_state
from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner

pytestmark = pytest.mark.command(
    "exec", "shell_state", "self_group/activate", "launcher/exec"
)

_POSIX_ONLY = pytest.mark.skipif(
    sys.platform == "win32",
    reason="POSIX shells, launcher bodies and the installed-link permission chain; cmd.exe is the Windows leg",
)

CARRIER = matrix.CARRIER
ABSENT = matrix.ABSENT
_SH = "/bin/sh"
_CANDIDATE_REL = (
    Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
)
_PREVIOUS_RELEASE = Path(__file__).resolve().parent / "data" / "exec_env" / "v0.6.5"
_SENTINEL = "fixture-secret-value"
_SIZE_LIMIT = "outgrew its size limit"
_UNREADABLE = (
    "the execution state this process received was written by an incompatible ocx"
)
_STOOD_DOWN = "stopped updating this shell"
_TIMEOUT = 120
EXIT_POLICY_BLOCKED = 81
UNINDEXED = "nonexistent_exec_env_compat_pkg:0"


# ---------------------------------------------------------------------------
# Arena (DAMP -- self-contained)
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Arena:
    """A private ``$HOME``/``$OCX_HOME``, an output directory and a directory outside every project."""

    root: Path
    home: Path
    ocx_home: Path
    out: Path
    outside: Path
    ocx: Path
    bash: str

    def env(self, **extra: str) -> dict[str, str]:
        env = matrix.clean_env(self.home, self.bash, ocx_home=self.ocx_home, **extra)
        env["SHELL"] = self.bash
        env["TERM"] = "dumb"
        env["PS1"] = ""
        return env

    def project(self, name: str, env_block: str) -> Path:
        """An ``[env]``-only project, locked (which also records consent)."""
        directory = self.root / "projects" / name
        matrix.write_project(directory, env_block)
        locked = matrix.run_lock(self.ocx, directory, self.env())
        assert locked.returncode == 0, (
            f"ocx lock must succeed for fixture project {name}:\n{locked.stderr}"
        )
        return directory

    def read(self, name: str) -> str:
        return (self.out / name).read_text(encoding="utf-8").rstrip("\n")


@pytest.fixture
def arena(tmp_path: Path, ocx_binary: Path) -> Arena:
    root = tmp_path.resolve()
    bash = shutil.which("bash")
    assert bash, (
        "bash is required to host the hook and the probe scripts (shutil.which returned None)"
    )
    home = root / "home"
    ocx_home = home / ".ocx"
    out = root / "out"
    outside = root / "outside"
    for directory in (ocx_home, out, outside):
        directory.mkdir(parents=True)
    return Arena(root, home, ocx_home, out, outside, Path(ocx_binary), bash)


def _incompressible(index: int, length: int) -> str:
    """``length`` hex characters zlib cannot fold, distinct per ``index``; the cap is measured compressed."""
    text = ""
    block = hashlib.sha256(f"exec-env-compat-{index}".encode()).hexdigest()
    while len(text) < length:
        text += block
        block = hashlib.sha256(block.encode()).hexdigest()
    return text[:length]


def _constants(prefix: str, count: int, length: int, offset: int) -> str:
    return "\n".join(
        f'{prefix}_{i} = "{_incompressible(offset + i, length)}"' for i in range(count)
    )


def _run(
    command: list[str], *, cwd: Path, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=str(cwd),
        env=env,
        capture_output=True,
        encoding="utf-8",
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=_TIMEOUT,
    )


def _reconcile(ocx: Path) -> str:
    """The line an IDE or a hook runs: one reconcile of the current directory, evaluated in place."""
    return f"eval \"$('{ocx}' --offline self activate --reconcile --shell=bash --shell-pid $$)\""


def _decoded(text: str, what: str) -> dict:
    state = exec_state.decode(text)
    assert isinstance(state, dict), (
        f"{what} must be a readable state; decoded {state!r} from {text[:80]!r}"
    )
    return state


def _project_roots(state: dict) -> list[Path]:
    return [
        Path(scope["project"]["root"])
        for scope in state["inherited_scopes"]
        if isinstance(scope, dict)
    ]


def _lines_with(text: str, needle: str) -> list[str]:
    return [line for line in text.splitlines() if needle in line]


def _render(template: str, destination: Path, **values: str) -> Path:
    """Write a previous-release body from ``data/exec_env/v0.6.5/`` with its placeholders filled."""
    body = (_PREVIOUS_RELEASE / template).read_text(encoding="utf-8")
    for key, value in values.items():
        assert f"@{key}@" in body, f"{template} has no @{key}@ placeholder"
        body = body.replace(f"@{key}@", value)
    assert not re.search(r"@[A-Z_]+@", body), f"{template} kept a placeholder:\n{body}"
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(body, encoding="utf-8")
    destination.chmod(0o755)
    return destination


def _package_root(ocx: OcxRunner, short: str) -> Path:
    return Path(ocx.json("package", "which", short)["paths"][short]["path"])


# ---------------------------------------------------------------------------
# A hook shell holding an older format's state
# ---------------------------------------------------------------------------


def _snap(label: str, *names: str) -> str:
    """One shell line printing ``@@<label>.<NAME>@@<value>`` per variable (``ABSENT`` when unset)."""
    return "; ".join(
        f'printf "%s\\n" "@@{label}.{name}@@${{{name}-{ABSENT}}}"' for name in names
    )


@_POSIX_ONLY
def test_a_hook_shell_holding_an_older_format_state_stands_down_once_and_unset_rearms_it(
    arena: Arena,
) -> None:
    """One notice, the value never rewritten, silence; an exec warns once and runs; ``unset`` re-arms."""
    candidate = arena.ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True)
    shutil.copy2(arena.ocx, candidate)
    candidate.chmod(0o755)
    # The line `ocx self setup` installs; the hook only fires while the installed candidate is executable.
    rc = f'eval "$({arena.ocx} --offline self activate --shell=bash --interactive --no-completion)"\n'
    (arena.home / ".bashrc").write_text(rc, encoding="utf-8")
    project = arena.project("q", 'Q_VAR = "q-fixture-value"')
    legacy = exec_state.craft_other_major(1)
    exec_line = (
        f"printf '%s\\n' \"@@exec.out@@$('{arena.ocx}' --offline --project '{project}' exec -- "
        "printenv Q_VAR 2>&1 | tr '\\n' '|')\""
    )
    lines = [
        _snap("a", CARRIER, "Q_VAR"),
        "true",
        "true",
        _snap("b", CARRIER, "Q_VAR"),
        exec_line,
        _snap("c", CARRIER),
        f"unset {CARRIER}",
        "true",
        _snap("d", CARRIER, "Q_VAR"),
    ]

    output = matrix.pty_session(
        [arena.bash, "-i"],
        lines,
        cwd=project,
        env=arena.env(**{CARRIER: legacy}),
        timeout=_TIMEOUT,
    )
    found = matrix.probes(output)

    ctx = f"\npty transcript:\n{output}"
    assert found["a.Q_VAR"] == ABSENT, (
        f"a stood-down login stream applies nothing, even in a consented project{ctx}"
    )
    for label in ("a", "b", "c"):
        assert found[f"{label}.{CARRIER}"] == legacy, (
            f"({label}) the older value is never rewritten{ctx}"
        )
    assert len(_lines_with(output, _STOOD_DOWN)) == 1, (
        f"exactly one stand-down notice, then silence{ctx}"
    )
    exec_parts = [part for part in found["exec.out"].split("|") if part]
    assert "q-fixture-value" in exec_parts, (
        f"an exec inside the shell still runs and composes{ctx}"
    )
    assert len([part for part in exec_parts if _UNREADABLE in part]) == 1, (
        f"the exec warns exactly once about the unreadable state{ctx}"
    )
    assert re.match(r"^2[.;]", found[f"d.{CARRIER}"]), (
        f"after unset the hook writes a current-format state{ctx}"
    )
    assert found["d.Q_VAR"] == "q-fixture-value", (
        f"after unset the hook applies the project again{ctx}"
    )


# ---------------------------------------------------------------------------
# The size cap
# ---------------------------------------------------------------------------


@_POSIX_ONLY
def test_a_nested_exec_past_the_cap_drops_the_outer_records_first_and_warns_once_with_the_size(
    arena: Arena,
) -> None:
    """Rung 1: the outer frame's records go, scopes and this spawn's replay stay, one warning.

    Project values are both applied records and replay literals, which zlib folds together, so one exec
    cannot reach rung 1 alone: an outer project near the cap and an inner one layered on it can.
    """
    big = arena.project("big", _constants("BIG", 5, 1400, 0))
    med = arena.project("med", _constants("MED", 5, 1000, 100))
    inner = (
        f"printenv {CARRIER} > '{arena.out / 'inner'}'; "
        f"printenv MED_0 > '{arena.out / 'med0'}'; printenv BIG_0 > '{arena.out / 'big0'}'; "
        f"exec '{arena.ocx}' --offline --format json shell state > '{arena.out / 'report.json'}'"
    )
    outer = (
        f"printenv {CARRIER} > '{arena.out / 'outer'}'; "
        f"exec '{arena.ocx}' --offline --project '{med}' exec -- {_SH} -c {shlex.quote(inner)}"
    )

    result = _run(
        [
            str(arena.ocx),
            "--offline",
            "--project",
            str(big),
            "exec",
            "--",
            _SH,
            "-c",
            outer,
        ],
        cwd=arena.outside,
        env=arena.env(),
    )

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, ctx
    outer_text = arena.read("outer")
    outer_state = _decoded(outer_text, "the outer frame's state")
    assert outer_state["truncated"] == "none", f"the outer frame fits{ctx}"
    assert len(outer_text) > exec_state.MAX_ENCODED // 2, (
        f"the fixture must start near the cap: {len(outer_text)}"
    )
    inner_text = arena.read("inner")
    inner_state = _decoded(inner_text, "the inner frame's state")
    assert len(inner_text) <= exec_state.MAX_ENCODED, (
        f"the written value fits the cap: {len(inner_text)}"
    )
    assert inner_state["truncated"] == "inherited_dropped", (
        f"rung 1 is reached and recorded{ctx}"
    )
    assert inner_state["applied"] == [], (
        "an exec's records are inherited-owned in its child, so rung 1 drops all"
    )
    assert _project_roots(inner_state) == [big, med], (
        "rung 1 keeps the composed scopes, oldest first"
    )
    replayed = sorted(record["key"] for record in inner_state["replay"]["env"])
    assert replayed == [f"MED_{i}" for i in range(5)], (
        "rung 1 keeps this spawn's replay"
    )
    assert arena.read("med0") == _incompressible(100, 1000), (
        "the values still reach the child"
    )
    assert arena.read("big0") == _incompressible(0, 1400), (
        "the outer values still reach the child"
    )

    report = json.loads(arena.read("report.json"))
    assert report["env_state"]["truncated"] == "inherited_dropped", json.dumps(
        report["env_state"]
    )[:400]
    assert {"type": "env_state_truncated", "truncated": "inherited_dropped"} in report[
        "notes"
    ], report["notes"]
    warnings = _lines_with(result.stderr, _SIZE_LIMIT)
    assert len(warnings) == 1, f"exactly one truncation warning{ctx}"
    assert (
        warnings[0].startswith("ocx:") and f"({len(inner_text)} bytes)" in warnings[0]
    ), f"the warning names the encoded size it wrote: {warnings[0]!r}"
    assert "the records inherited from outer frames were dropped" in warnings[0], (
        f"the warning names the rung it reached: {warnings[0]!r}"
    )


@_POSIX_ONLY
def test_a_hook_state_past_the_cap_drops_every_record_and_a_later_spawn_keeps_the_mark(
    arena: Arena,
) -> None:
    """Rung 2: the hook's records go (it has no replay to keep), one notice; a spawn stays marked."""
    huge = arena.project("huge", _constants("HUGE", 12, 1000, 200))
    small = arena.project("small", 'SMALL_VAR = "small-fixture-value"')
    spawned = (
        f"printenv {CARRIER} > '{arena.out / 'spawn'}'; printenv SMALL_VAR > '{arena.out / 'small'}'; "
        f"exec '{arena.ocx}' --offline --format json shell state > '{arena.out / 'report.json'}'"
    )
    script = (
        f"{_reconcile(arena.ocx)}; "
        f"printenv {CARRIER} > '{arena.out / 'hook'}'; printenv HUGE_0 > '{arena.out / 'huge0'}'; "
        f"'{arena.ocx}' --offline --project '{small}' exec -- {_SH} -c {shlex.quote(spawned)}"
    )

    result = _run([arena.bash, "-c", script], cwd=huge, env=arena.env())

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, ctx
    assert arena.read("huge0") == _incompressible(200, 1000), (
        f"the hook still applies the values{ctx}"
    )
    hook_text = arena.read("hook")
    hook_state = _decoded(hook_text, "the hook's state")
    assert len(hook_text) <= exec_state.MAX_ENCODED, (
        f"the written value fits the cap: {len(hook_text)}"
    )
    assert hook_state["truncated"] == "all_applied_dropped", (
        f"rung 2 is reached and recorded{ctx}"
    )
    assert hook_state["applied"] == [], "rung 2 drops every applied record"
    notices = _lines_with(result.stdout + result.stderr, _SIZE_LIMIT)
    assert len(notices) == 1, (
        f"one notice from the hook, none from the spawn that only carried it{ctx}"
    )
    assert (
        f"({len(hook_text)} bytes)" in notices[0]
        and "every applied record was dropped" in notices[0]
    ), f"the notice names the encoded size and the rung words: {notices[0]!r}"
    spawn_state = _decoded(arena.read("spawn"), "the spawn's state")
    assert spawn_state["truncated"] == "all_applied_dropped", (
        "a later spawn keeps the received mark"
    )
    assert "SMALL_VAR" in {record["key"] for record in spawn_state["applied"]}, (
        "the spawn still records its own values"
    )
    assert arena.read("small") == "small-fixture-value", ctx
    report = json.loads(arena.read("report.json"))
    assert report["env_state"]["truncated"] == "all_applied_dropped", json.dumps(
        report["env_state"]
    )[:400]


# ---------------------------------------------------------------------------
# A secret-named project constant is recorded as composed
# ---------------------------------------------------------------------------


@_POSIX_ONLY
@pytest.mark.parametrize("writer", ["exec", "hook"])
def test_a_secret_named_project_constant_reaches_the_child_and_is_recorded_verbatim(
    arena: Arena, writer: str
) -> None:
    """``shell state`` and the decoded payload hold ``GITHUB_TOKEN`` with its composed value."""
    project = arena.project(
        "secret", f'GITHUB_TOKEN = "{_SENTINEL}"\nPLAIN_VAR = "plain-fixture-value"'
    )
    probe = (
        f"printenv {CARRIER} > '{arena.out / 'state'}'; printenv GITHUB_TOKEN > '{arena.out / 'token'}'; "
        f"exec '{arena.ocx}' --offline --format json shell state > '{arena.out / 'report.json'}'"
    )
    if writer == "exec":
        command = [
            str(arena.ocx),
            "--offline",
            "--project",
            str(project),
            "exec",
            "--",
            _SH,
            "-c",
            probe,
        ]
        cwd = arena.outside
    else:
        command = [arena.bash, "-c", f"{_reconcile(arena.ocx)}; {probe}"]
        cwd = project

    result = _run(command, cwd=cwd, env=arena.env())

    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert arena.read("token") == _SENTINEL, (
        "the secret value itself still reaches the child"
    )
    report_text = arena.read("report.json")
    records = {
        record["key"]: record
        for record in json.loads(report_text)["env_state"]["applied"]
    }
    assert records["GITHUB_TOKEN"].get("value") == _SENTINEL, records["GITHUB_TOKEN"]
    assert "withheld" not in records["GITHUB_TOKEN"], records["GITHUB_TOKEN"]
    assert records["PLAIN_VAR"].get("value") == "plain-fixture-value", (
        "a plain constant is recorded with its value"
    )
    payload = json.dumps(_decoded(arena.read("state"), "the child's state"))
    assert "plain-fixture-value" in payload, (
        "the decoded payload is what this reads, or the next line proves nothing"
    )
    assert _SENTINEL in payload, "the decoded payload records the value as composed"
    for stream, text in (
        ("stdout", result.stdout),
        ("stderr", result.stderr),
    ):
        assert _SENTINEL not in text, f"the value was printed on {stream}"


# ---------------------------------------------------------------------------
# A launcher after an update
# ---------------------------------------------------------------------------


@_POSIX_ONLY
def test_a_launcher_in_an_existing_environment_follows_current_to_the_updated_build(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The pin names the installed path through ``current``, so an update mid-session is followed.

    The update is the ``current`` flip ``ocx self update`` ends with; the new build is a wrapper that logs
    its argv and runs the binary under test.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    home = ocx.ocx_home
    cli = home / "symlinks" / "ocx.sh" / "ocx" / "cli"
    old, new = home / "ocx-build-old", home / "ocx-build-new"
    log = tmp_path / "new-build.log"
    for build in (old, new):
        (build / "content" / "bin").mkdir(parents=True)
    shutil.copy2(ocx.binary, old / "content" / "bin" / "ocx")
    wrapper = (
        f"#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\nexec '{ocx.binary}' \"$@\"\n"
    )
    (new / "content" / "bin" / "ocx").write_text(wrapper, encoding="utf-8")
    cli.mkdir(parents=True, exist_ok=True)
    # The pin follows `current` only through a chain no other principal can write, whatever the umask.
    chain = [
        cli,
        *cli.parents,
        *(
            path
            for build in (old, new)
            for path in (build, build / "content", build / "content" / "bin")
        ),
    ]
    for directory in chain:
        if directory == home or home in directory.parents:
            directory.chmod(0o755)
    for build in (old, new):
        (build / "content" / "bin" / "ocx").chmod(0o755)
    current = cli / "current"
    current.symlink_to(old)
    installed = current / "content" / "bin" / "ocx"
    flip = f"printf 'pin=%s\\n' \"$OCX_BINARY_PIN\"; ln -sfn '{new}' '{current}'; hello"

    result = _run(
        [str(installed), "package", "exec", pkg.short, "--", _SH, "-c", flip],
        cwd=tmp_path,
        env=dict(ocx.env),
    )

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, ctx
    assert f"pin={installed}" in result.stdout.splitlines(), (
        f"the pin is the installed path through current{ctx}"
    )
    assert pkg.marker in result.stdout, f"the entrypoint ran{ctx}"
    logged = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
    assert any(
        line.startswith("launcher exec ") and line.endswith(" -- hello")
        for line in logged
    ), (
        f"the launcher must run the build `current` names now; the new build logged {logged!r}{ctx}"
    )


# ---------------------------------------------------------------------------
# Another major's state, previous-release bodies
# ---------------------------------------------------------------------------

_PROBE_SCRIPT = f"""\
#!/bin/sh
printf 'state=%s\\n' "${{{CARRIER}-}}"
"$OCX_BINARY_PIN" package install {UNINDEXED} >/dev/null 2>&1
printf 'install_rc=%s\\n' "$?"
"""


def _probe_package(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> Path:
    """Publish and install a package whose ``probe`` entrypoint reports its state and tries an install."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"probe": {"command": "sh", "args": ["${installPath}/probe.sh"]}},
        extra_files={"probe.sh": _PROBE_SCRIPT},
    )
    ocx.plain("package", "install", "--select", pkg.short)
    return _package_root(ocx, pkg.short)


def _probe_fields(stdout: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in stdout.splitlines():
        name, sep, value = line.partition("=")
        if sep and name in ("state", "install_rc"):
            fields[name] = value
    return fields


def _state_warnings(stderr: str) -> list[str]:
    return [
        line
        for line in stderr.splitlines()
        if line.startswith("ocx:") and "state" in line.lower()
    ]


@_POSIX_ONLY
@pytest.mark.parametrize("body", ["installed", "v0.6.5"])
def test_a_launcher_under_another_majors_state_keeps_offline_finds_the_home_and_warns_once(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, body: str
) -> None:
    """A major-3 state with ``n=offline`` and ``h=`` reaching a launcher, current or previous-release body."""
    pkg_root = _probe_package(ocx, unique_repo, tmp_path)
    if body == "installed":
        launcher = pkg_root / "entrypoints" / "probe"
    else:
        launcher = _render(
            "launcher.sh.in", tmp_path / "v0.6.5" / "probe", PKG_ROOT=str(pkg_root)
        )
    fake_home = tmp_path / "unrelated-home"
    fake_home.mkdir()
    env = {key: value for key, value in ocx.env.items() if key != "OCX_HOME"}
    env.update(
        {
            "HOME": str(fake_home),
            CARRIER: exec_state.craft_other_major(
                3, narrowing=("offline",), home=str(ocx.ocx_home)
            ),
            "OCX_BINARY_PIN": str(ocx.binary),
            "OCX_OFFLINE": "0",
        }
    )

    result = _run([str(launcher)], cwd=tmp_path, env=env)

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, (
        f"the launcher finds the store through the header's home{ctx}"
    )
    assert len(_state_warnings(result.stderr)) == 1, (
        f"exactly one warning about the unreadable state{ctx}"
    )
    fields = _probe_fields(result.stdout)
    child = _decoded(fields.get("state", ""), "the entrypoint's state")
    assert child["major"] == exec_state.MAJOR, ctx
    assert "offline" in child["narrowing"], (
        f"the header's offline narrowing survives the hop{ctx}"
    )
    assert Path(child["home"]) == Path(ocx.ocx_home), (
        f"the header's home is handed on{ctx}"
    )
    assert fields.get("install_rc") == str(EXIT_POLICY_BLOCKED), (
        f"an ocx inside the entrypoint stays offline despite OCX_OFFLINE=0{ctx}"
    )


@_POSIX_ONLY
def test_a_previous_release_launcher_in_the_middle_of_a_chain_hands_the_outer_state_on(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx --project P exec`` → v0.6.5 launcher body → new ``launcher exec`` decodes P's state."""
    pkg_root = _probe_package(ocx, unique_repo, tmp_path)
    launcher = _render(
        "launcher.sh.in", tmp_path / "v0.6.5" / "probe", PKG_ROOT=str(pkg_root)
    )
    project = tmp_path.resolve() / "projects" / "p"
    matrix.write_project(project, 'P_VAR = "p-fixture-value"')
    locked = matrix.run_lock(ocx.binary, project, dict(ocx.env))
    assert locked.returncode == 0, (
        f"ocx lock must succeed for the fixture project:\n{locked.stderr}"
    )

    result = _run(
        [
            str(ocx.binary),
            "--offline",
            "--project",
            str(project),
            "exec",
            "--",
            str(launcher),
        ],
        cwd=tmp_path,
        env=dict(ocx.env),
    )

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, ctx
    assert _state_warnings(result.stderr) == [], (
        f"a current-format state passes through with no warning{ctx}"
    )
    fields = _probe_fields(result.stdout)
    child = _decoded(fields.get("state", ""), "the entrypoint's state")
    assert project in _project_roots(child), (
        f"the outer frame's scope survives the previous-release hop{ctx}"
    )
    assert "offline" in child["narrowing"], ctx
    assert fields.get("install_rc") == str(EXIT_POLICY_BLOCKED), (
        f"the outer --offline reaches the innermost ocx{ctx}"
    )


@_POSIX_ONLY
def test_a_previous_release_trampoline_in_the_middle_of_a_chain_layers_its_project_on_the_outer_state(
    arena: Arena,
) -> None:
    """``ocx --project Q exec`` → sh → v0.6.5 trampoline for P → new ``ocx --project P exec``.

    A shell runs the trampoline, as a user's would: ``ocx exec`` refuses a trampoline as its own command.
    """
    p = arena.project("p", 'P_VAR = "p-fixture-value"')
    q = arena.project("q", 'Q_VAR = "q-fixture-value"')
    # Named for its dispatch target and kept off PATH, so `${0##*/}` resolves the real printenv.
    trampoline = _render(
        "trampoline.sh.in",
        arena.root / "v0.6.5" / "printenv",
        OCX=str(arena.ocx),
        PROJECT=str(p),
    )
    # One name per call: BSD/macOS printenv honours only its first operand.
    script = f"for name in P_VAR Q_VAR {CARRIER}; do '{trampoline}' \"$name\" || exit; done"

    result = _run(
        [
            str(arena.ocx),
            "--offline",
            "--project",
            str(q),
            "exec",
            "--",
            _SH,
            "-c",
            script,
        ],
        cwd=arena.outside,
        env=arena.env(),
    )

    ctx = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert result.returncode == 0, ctx
    lines = result.stdout.splitlines()
    assert len(lines) == 3, ctx
    assert lines[:2] == ["p-fixture-value", "q-fixture-value"], (
        f"P layers on top of the received Q{ctx}"
    )
    assert _state_warnings(result.stderr) == [], ctx
    assert _project_roots(_decoded(lines[2], "the innermost state")) == [q, p], (
        f"Q stays inherited under P{ctx}"
    )


# ---------------------------------------------------------------------------
# cmd.exe
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    sys.platform != "win32",
    reason="cmd.exe's 8191-character command line exists only on Windows; no POSIX shell has that limit",
)
def test_a_state_at_the_cap_fits_one_cmd_set_line(tmp_path: Path) -> None:
    """``SET "__OCX_ENV_STATE=<8169 chars>"`` is exactly cmd.exe's 8191-character line and survives it."""
    value = exec_state.craft_over_cap(extra=0)
    assert len(value) == exec_state.MAX_ENCODED
    line = f'SET "{CARRIER}={value}"'
    assert len(line) == 8191, len(line)
    script = tmp_path / "set_state.cmd"
    reader = f'"{sys.executable}" -c "import os; print(len(os.environ[\'{CARRIER}\']))"'
    script.write_text(f"@echo off\r\n{line}\r\n{reader}\r\n", encoding="ascii")

    result = subprocess.run(
        ["cmd.exe", "/d", "/c", str(script)],
        capture_output=True,
        encoding="utf-8",
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=_TIMEOUT,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == str(exec_state.MAX_ENCODED), result.stdout
