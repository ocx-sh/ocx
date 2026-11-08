# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""End to end: the shells an ``ocx exec`` / ``ocx shell`` spawns, and how their prompt hook treats them.

An explicit ``ocx exec`` composes what it was asked for whatever is already loaded; the scopes an
outer frame loaded gate only the activation hook and the login stream (``adr_exec_env_model.md``).
Every case drives a real interactive bash on a real pty (``shell_matrix.pty_session``) that reads a
``~/.bashrc`` with the line ``ocx self setup`` would install; variables are read back with
``@@label@@value`` probes. No registry: every project is ``[env]``/group-only, ocx runs ``--offline``.
Groups use PATH-free constants so an assertion reads the group's own contribution.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix
from unix_socket import short_socket

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="the cases drive POSIX-family shells on a pty; the Windows leg is not part of this module.",
    ),
    pytest.mark.command("exec", "shell"),
]

_OCX = matrix.ocx_binary()
_BASH = shutil.which("bash")

pytestmark.append(
    pytest.mark.skipif(
        _OCX is None, reason="no ocx binary (set OCX_COMMAND, or build test/bin/ocx)."
    )
)
pytestmark.append(
    pytest.mark.skipif(
        _BASH is None,
        reason="bash is needed as the interactive shell (shutil.which returned None).",
    )
)

ABSENT = matrix.ABSENT
_CANDIDATE_REL = (
    Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
)
_TIMEOUT = 90


# ---------------------------------------------------------------------------
# Arena and world (DAMP -- self-contained)
# ---------------------------------------------------------------------------


@dataclass
class World:
    """A private ``$HOME``/``$OCX_HOME``, the global tier, projects ``p`` and ``q``, and a bare directory."""

    root: Path
    home: Path
    ocx_home: Path
    p: Path
    q: Path
    outside: Path
    dirs: Path
    ocx: Path
    bash: str

    def env(self, **extra: str) -> dict[str, str]:
        env = matrix.clean_env(self.home, self.bash, ocx_home=self.ocx_home, **extra)
        env["SHELL"] = self.bash
        env["TERM"] = "dumb"
        env["PS1"] = ""
        return env


def _lock(world_ocx: Path, project: Path, env: dict[str, str]) -> None:
    result = matrix.run_lock(world_ocx, project, env)
    assert result.returncode == 0, (
        f"ocx lock must succeed for the fixture; stderr:\n{result.stderr}"
    )


def _project_body(dirs: Path) -> str:
    return f"""\
[tools]

[env]
P_DEF = "p-default"

[group.x.env]
P_X = "p-x"

[group.g1.env]
G1_VAR = "g1-value"
G1_LIST = {{ type = "list", value = "g1-item", separator = "," }}
G1_DIRS = {{ type = "path", value = "{dirs / "g1"}" }}
SHARED_DIRS = {{ type = "path", value = "{dirs / "shared1"}" }}

[group.g2.env]
G2_VAR = "g2-value"
SHARED_DIRS = {{ type = "path", value = "{dirs / "shared2"}" }}
"""


@pytest.fixture
def world(tmp_path: Path) -> World:
    root = tmp_path.resolve()
    home = root / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    assert _OCX is not None and _BASH is not None
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    # The rc line `ocx self setup` installs: the login stream plus the per-prompt hook. A login
    # shell (tmux starts one) reads `.bash_profile`, which sources `.bashrc` as users' files do.
    rc = f'eval "$({_OCX} --offline self activate --shell=bash --interactive --no-completion)"\n'
    (home / ".bashrc").write_text(rc, encoding="utf-8")
    (home / ".bash_profile").write_text('. "$HOME/.bashrc"\n', encoding="utf-8")

    dirs = root / "dirs"
    for name in ("g1", "shared1", "shared2"):
        (dirs / name).mkdir(parents=True)
    matrix.write_project(ocx_home, 'GLOBAL_VAR = "g"')
    projects = root / "projects"
    p = projects / "p"
    p.mkdir(parents=True)
    (p / "ocx.toml").write_text(_project_body(dirs), encoding="utf-8")
    q = projects / "q"
    matrix.write_project(q, 'Q_VAR = "q"')
    outside = projects / "outside"
    outside.mkdir()
    built = World(root, home, ocx_home, p, q, outside, dirs, _OCX, _BASH)
    for project in (p, q):
        _lock(_OCX, project, built.env())
    glock = subprocess.run(
        [str(_OCX), "--offline", "--global", "lock"],
        capture_output=True,
        text=True,
        check=False,
        env=built.env(),
    )
    assert glock.returncode == 0, (
        f"ocx --global lock must succeed for the fixture: {glock.stderr}"
    )
    return built


# ---------------------------------------------------------------------------
# Driving a pty
# ---------------------------------------------------------------------------


def _snap(label: str, *names: str) -> str:
    """One shell line printing ``@@<label>.<NAME>@@<value>`` per variable (``ABSENT`` when unset)."""
    return "; ".join(
        f'printf "%s\\n" "@@{label}.{name}@@${{{name}-{ABSENT}}}"' for name in names
    )


def _run(
    command: list[str], lines: list[str], *, cwd: Path, env: dict[str, str]
) -> tuple[dict[str, str], str]:
    output = matrix.pty_session(command, lines, cwd=cwd, env=env, timeout=_TIMEOUT)
    print(f"pty transcript:\n{output}")
    return matrix.probes(output), output


def _elements(value: str | None) -> list[str]:
    return [element for element in (value or "").split(os.pathsep) if element]


def _exec_bash(world: World, *prefix: str) -> list[str]:
    """``ocx --offline <prefix> -- bash -i`` -- the user's shell under an explicit exec."""
    return [str(world.ocx), "--offline", *prefix, "--", world.bash, "-i"]


# ---------------------------------------------------------------------------
# ocx --project P -g x exec -- bash
# ---------------------------------------------------------------------------


def test_project_exec_shell_keeps_its_layer_and_the_hook_adds_nothing_inside_the_project(
    world: World,
) -> None:
    """Inside P nothing changes (global included); ``cd Q`` layers global and Q; back in P they go."""
    env = world.env()
    names = ("P_X", "GLOBAL_VAR", "Q_VAR", "PATH")
    lines = [
        _snap("a", *names),
        f"cd '{world.q}'",
        _snap("b", *names),
        f"cd '{world.p}'",
        _snap("c", *names),
        # (d) the project is inherited, so its files are not watched: an edit changes nothing.
        f"printf '%s\\n' '[env]' 'P_EDIT = \"edited\"' '[group.x.env]' 'P_X = \"p-x-edited\"' > '{world.p / 'ocx.toml'}'",
        "true",
        _snap("d", "P_X", "P_EDIT", "PATH"),
        f"cd '{world.outside}'",
        _snap("e", *names),
    ]

    found, output = _run(
        _exec_bash(world, "--project", str(world.p), "exec", "-g", "x"),
        lines,
        cwd=world.p,
        env=env,
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_X"] == "p-x", f"the exec layer must reach the shell{ctx}"
    assert found["a.GLOBAL_VAR"] == ABSENT, (
        f"(a) the login stream and hook add no global inside P{ctx}"
    )
    assert found["a.Q_VAR"] == ABSENT, ctx
    assert found["a.PATH"] == env["PATH"], (
        f"(a) nothing may move PATH inside the inherited project{ctx}"
    )
    assert found["b.GLOBAL_VAR"] == "g" and found["b.Q_VAR"] == "q", (
        f"(b) cd Q layers global and Q{ctx}"
    )
    assert found["b.P_X"] == "p-x", f"(b) the exec layer stays under the hook's{ctx}"
    assert found["c.GLOBAL_VAR"] == ABSENT and found["c.Q_VAR"] == ABSENT, (
        f"(c) back in P they go{ctx}"
    )
    assert found["c.P_X"] == found["a.P_X"] and found["c.PATH"] == found["a.PATH"], (
        f"(c) P's values come back byte-identical{ctx}"
    )
    assert found["d.P_EDIT"] == ABSENT and found["d.P_X"] == "p-x", (
        f"(d) editing P's ocx.toml changes nothing{ctx}"
    )
    assert found["d.PATH"] == found["a.PATH"], ctx
    assert found["e.GLOBAL_VAR"] == "g", (
        f"(e) outside, the global tier applies because nothing inherited it{ctx}"
    )
    assert found["e.Q_VAR"] == ABSENT and found["e.P_X"] == "p-x", ctx


def test_global_exec_shell_inherits_global_and_the_hook_layers_only_the_project(
    world: World,
) -> None:
    """``ocx --global exec -- bash`` inherits global; the hook adds Q in Q and drops it outside."""
    env = world.env()
    names = ("GLOBAL_VAR", "Q_VAR")
    lines = [
        _snap("out", *names),
        f"cd '{world.q}'",
        _snap("inq", *names),
        f"cd '{world.outside}'",
        _snap("back", *names),
    ]

    found, output = _run(
        _exec_bash(world, "--global", "exec"), lines, cwd=world.outside, env=env
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["out.GLOBAL_VAR"] == "g" and found["out.Q_VAR"] == ABSENT, (
        f"outside: global only{ctx}"
    )
    assert found["inq.GLOBAL_VAR"] == "g" and found["inq.Q_VAR"] == "q", (
        f"in an uninherited project: Q in front{ctx}"
    )
    assert found["back.GLOBAL_VAR"] == "g" and found["back.Q_VAR"] == ABSENT, (
        f"leaving Q drops Q and keeps the inherited global tier{ctx}"
    )


# ---------------------------------------------------------------------------
# The owner-rule pair: an explicit exec composes what it was asked for, twice
# ---------------------------------------------------------------------------


def test_same_project_and_group_twice_yields_each_value_once_and_the_hook_adds_nothing(
    world: World,
) -> None:
    """``exec -g g1 -- exec -g g1 -- bash -i`` -- g1 once, nothing from the hook in the project."""
    env = world.env()
    g1_dir = str(world.dirs / "g1")
    lines = [
        _snap(
            "s", "G1_VAR", "G1_LIST", "G1_DIRS", "G2_VAR", "P_DEF", "GLOBAL_VAR", "PATH"
        )
    ]
    inner = ["--project", str(world.p), "exec", "-g", "g1"]
    command = [
        str(world.ocx), "--offline", *inner, "--",
        str(world.ocx), "--offline", *inner, "--",
        world.bash, "-i",
    ]  # fmt: skip

    found, output = _run(command, lines, cwd=world.p, env=env)

    ctx = f"\npty transcript:\n{output}"
    assert found["s.G1_VAR"] == "g1-value", ctx
    assert found["s.G1_LIST"] == "g1-item", f"a list value appears once, not twice{ctx}"
    assert _elements(found["s.G1_DIRS"]) == [g1_dir], f"a path value appears once{ctx}"
    assert found["s.G2_VAR"] == ABSENT, ctx
    assert found["s.GLOBAL_VAR"] == ABSENT, (
        f"the hook inside P composes nothing, global included{ctx}"
    )
    assert found["s.PATH"] == env["PATH"], ctx


def test_same_project_with_another_group_layers_on_top_and_keeps_the_first(
    world: World,
) -> None:
    """``exec -g g1 -- exec -g g2 -- bash -i`` -- both groups, g2 in front of g1."""
    env = world.env()
    lines = [_snap("s", "G1_VAR", "G2_VAR", "SHARED_DIRS", "GLOBAL_VAR")]
    command = [
        str(world.ocx), "--offline", "--project", str(world.p), "exec", "-g", "g1", "--",
        str(world.ocx), "--offline", "--project", str(world.p), "exec", "-g", "g2", "--",
        world.bash, "-i",
    ]  # fmt: skip

    found, output = _run(command, lines, cwd=world.p, env=env)

    ctx = f"\npty transcript:\n{output}"
    assert found["s.G1_VAR"] == "g1-value" and found["s.G2_VAR"] == "g2-value", (
        f"both groups are present{ctx}"
    )
    assert _elements(found["s.SHARED_DIRS"]) == [
        str(world.dirs / "shared2"),
        str(world.dirs / "shared1"),
    ], f"the later group is in front{ctx}"
    assert found["s.GLOBAL_VAR"] == ABSENT, ctx


# ---------------------------------------------------------------------------
# ocx shell, pinned
# ---------------------------------------------------------------------------

_NOTICE = re.compile(r"^ocx: this shell is pinned to .*is not applied", re.MULTILINE)


def test_pinned_shell_ignores_cd_refreshes_its_layer_and_keeps_invocation_values(
    world: World,
) -> None:
    """End to end: one notice on ``cd Q``, nothing applied, an ``ocx.toml`` edit refreshes, ``--env`` survives."""
    env = world.env()
    new_body = _project_body(world.dirs).replace(
        'P_DEF = "p-default"', 'P_DEF = "p-refreshed-value"'
    )
    names = ("P_DEF", "INV_VAR", "GLOBAL_VAR", "Q_VAR")
    lines = [
        _snap("a", *names),
        f"cd '{world.q}'",
        _snap("b", *names),
        "true",
        f"cd '{world.outside}'",
        _snap("c", *names),
        f"printf '%s' '{new_body}' > '{world.p / 'ocx.toml'}' && (cd '{world.p}' && '{world.ocx}' --offline lock >/dev/null 2>&1)",
        "true",
        _snap("d", *names),
    ]

    found, output = _run(
        [
            str(world.ocx),
            "--offline",
            "--project",
            str(world.p),
            "shell",
            "--env",
            "INV_VAR=inv-value",
        ],
        lines,
        cwd=world.p,
        env=env,
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_DEF"] == "p-default" and found["a.INV_VAR"] == "inv-value", (
        f"the shell opens pinned{ctx}"
    )
    assert found["a.GLOBAL_VAR"] == ABSENT, (
        f"the rc login stream applies nothing to a pinned shell{ctx}"
    )
    assert found["b.Q_VAR"] == ABSENT and found["b.GLOBAL_VAR"] == ABSENT, (
        f"cd into Q applies nothing{ctx}"
    )
    assert found["c.Q_VAR"] == ABSENT and found["c.GLOBAL_VAR"] == ABSENT, ctx
    assert len(_NOTICE.findall(output)) == 1, (
        f"exactly one pinned notice across several prompts{ctx}"
    )
    assert found["d.P_DEF"] == "p-refreshed-value", (
        f"an edit of the pinned ocx.toml refreshes the layer{ctx}"
    )
    assert found["d.INV_VAR"] == "inv-value", f"--env values survive the refresh{ctx}"


def test_pinned_shell_of_an_unconsented_project_keeps_its_spawn_time_layer(
    world: World,
) -> None:
    """Error case: consent revoked, the file edited -- the layer stays and nothing is removed."""
    env = world.env()
    edited = _project_body(world.dirs).replace(
        'P_DEF = "p-default"', 'P_DEF = "p-unconsented-edit"'
    )
    lines = [
        _snap("a", "P_DEF"),
        f"'{world.ocx}' --offline --project '{world.p}' shell revoke >/dev/null 2>&1",
        f"printf '%s' '{edited}' > '{world.p / 'ocx.toml'}'",
        "true",
        _snap("b", "P_DEF"),
    ]

    found, output = _run(
        [str(world.ocx), "--offline", "--project", str(world.p), "shell"],
        lines,
        cwd=world.p,
        env=env,
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_DEF"] == "p-default", ctx
    assert found["b.P_DEF"] == "p-default", (
        f"no consent, no recomposition: the spawn-time layer stays{ctx}"
    )


# ---------------------------------------------------------------------------
# A long-lived descendant activates fresh
# ---------------------------------------------------------------------------


def test_a_descendant_shell_activates_fresh_and_reverts_nothing_its_parent_applied(
    world: World,
) -> None:
    """A shell started from a hook shell is another session: it owns none of the parent's records."""
    env = world.env()
    lines = [
        f"cd '{world.q}'",
        _snap("parent", "Q_VAR"),
        f"{world.bash} -i",
        _snap("child", "Q_VAR"),
        f"cd '{world.outside}'",
        _snap("childout", "Q_VAR"),
        "exit",
        _snap("parentagain", "Q_VAR"),
        f"cd '{world.outside}'",
        _snap("parentout", "Q_VAR"),
    ]

    found, output = _run([world.bash, "-i"], lines, cwd=world.outside, env=env)

    ctx = f"\npty transcript:\n{output}"
    assert found["parent.Q_VAR"] == "q", ctx
    assert found["child.Q_VAR"] == "q", f"the descendant inherits the value{ctx}"
    assert found["childout.Q_VAR"] == "q", (
        f"the descendant owns none of the parent's records, so it reverts none{ctx}"
    )
    assert found["parentagain.Q_VAR"] == "q", ctx
    assert found["parentout.Q_VAR"] == ABSENT, (
        f"the parent still reverts what it applied{ctx}"
    )


# ---------------------------------------------------------------------------
# tmux, rc re-source, exec $SHELL, an IDE-captured state
# ---------------------------------------------------------------------------


def test_rc_resource_and_exec_shell_keep_the_pinned_session(world: World) -> None:
    """``source ~/.bashrc`` and ``exec $SHELL`` inside ``ocx shell`` keep the layer and the session."""
    env = world.env()
    names = ("P_DEF", "INV_VAR", "GLOBAL_VAR", "Q_VAR")
    lines = [
        _snap("a", *names, "PATH"),
        "source ~/.bashrc",
        _snap("b", *names, "PATH"),
        "exec $SHELL",
        _snap("c", *names, "PATH"),
        f"cd '{world.q}'",
        _snap("d", *names),
    ]

    found, output = _run(
        [
            str(world.ocx),
            "--offline",
            "--project",
            str(world.p),
            "shell",
            "--env",
            "INV_VAR=inv-value",
        ],
        lines,
        cwd=world.p,
        env=env,
    )

    ctx = f"\npty transcript:\n{output}"
    for stage in ("b", "c"):
        for name in (*names, "PATH"):
            assert found[f"{stage}.{name}"] == found[f"a.{name}"], (
                f"{stage}: {name} must survive the re-source{ctx}"
            )
    assert found["d.Q_VAR"] == ABSENT and found["d.GLOBAL_VAR"] == ABSENT, (
        f"still pinned after exec $SHELL{ctx}"
    )
    assert found["d.P_DEF"] == "p-default", ctx


@pytest.mark.skipif(
    shutil.which("tmux") is None,
    reason="tmux is not installed on this host (shutil.which returned None)",
)
def test_a_tmux_pane_inside_ocx_shell_activates_for_its_cwd_and_owns_none_of_the_pinned_records(
    world: World,
) -> None:
    """A new pane (a new shell, another pid) layers its cwd's project and reverts nothing pinned."""
    env = world.env()
    with short_socket("tmux.sock") as socket:
        out = world.root / "pane-out"
        out.mkdir()
        tmux = f"tmux -S '{socket}' -f /dev/null"
        names = ("P_DEF", "Q_VAR", "GLOBAL_VAR")

        def pane(step: str, *command: str) -> list[str]:
            """Source a script in the pane, wait for its probe file, and print it as probes."""
            target = out / step
            script = out / f"{step}.sh"
            script.write_text(
                "\n".join([*command, f"{{ {_snap(step, *names)}; }} > '{target}'"]) + "\n",
                encoding="utf-8",
            )
            return [
                f"{tmux} send-keys -t s -l \". '{script}'\" && {tmux} send-keys -t s Enter",
                (
                    f"for i in $(seq 150); do [ -s '{target}' ] && break; sleep 0.2; done; "
                    f"while IFS= read -r l; do printf '%s\\n' \"$l\"; done < '{target}'"
                ),
            ]

        lines = [
            f"{tmux} new-session -d -s s -x 200 -y 50 -c '{world.q}'",
            # Ready when the pane's shell has drawn a prompt; a deadline replaces a fixed guess.
            (
                f"for i in $(seq 100); do [ -n \"$({tmux} capture-pane -p -t s 2>/dev/null)\" ] && break; "
                "sleep 0.1; done; "
                f"[ -n \"$({tmux} capture-pane -p -t s 2>/dev/null)\" ] "
                "|| echo 'tmux pane never drew a prompt within 10s'"
            ),
            *pane("inq", "true"),
            *pane(
                "cd", f"cd '{world.outside}'"
            ),  # the hook runs at the prompt after this line
            *pane("outq", "true"),
            f"{tmux} kill-server",
        ]
        try:
            found, output = _run(
                [str(world.ocx), "--offline", "--project", str(world.p), "shell"],
                lines,
                cwd=world.p,
                env=env,
            )
        finally:
            subprocess.run(
                ["tmux", "-S", str(socket), "kill-server"],
                capture_output=True,
                check=False,
                env=env,
                timeout=30,
            )

    ctx = f"\npty transcript:\n{output}"
    # The transcript echoes the command, which quotes the message; only the echo's own output is a whole line.
    assert not re.search(r"^tmux pane never drew a prompt within 10s\r?$", output, re.MULTILINE), (
        f"the tmux pane was not ready{ctx}"
    )
    assert found["inq.Q_VAR"] == "q", f"the pane activates for its own cwd{ctx}"
    assert found["inq.P_DEF"] == "p-default", (
        f"the pinned layer is inherited by the pane{ctx}"
    )
    assert found["outq.Q_VAR"] == ABSENT, f"the pane reverts what it applied{ctx}"
    assert found["outq.P_DEF"] == "p-default", (
        f"the pane owns none of the pinned records, so it reverts none{ctx}"
    )


def test_an_ide_captured_state_is_layered_on_and_nothing_inherited_is_reverted(
    world: World,
) -> None:
    """A terminal started with another project's captured state layers its own project on top."""
    env = world.env()
    other = world.root / "projects" / "r"
    matrix.write_project(other, 'R_VAR = "r"')
    _lock(world.ocx, other, env)
    capture = subprocess.run(
        [
            world.bash,
            "-c",
            f"eval \"$('{world.ocx}' --offline self activate --reconcile --shell=bash --shell-pid $$)\"; env -0",
        ],
        cwd=other,
        env=env,
        capture_output=True,
        check=False,
        timeout=60,
    )
    assert capture.returncode == 0, capture.stderr
    captured = dict(
        item.decode().split("=", 1)
        for item in capture.stdout.split(b"\0")
        if b"=" in item
    )
    assert captured.get("R_VAR") == "r", (
        f"fixture: the capture must carry the other project's layer: {captured.keys()}"
    )
    ide_env = {
        **env,
        **{
            key: value
            for key, value in captured.items()
            if key in ("R_VAR", "PATH", matrix.CARRIER)
        },
    }
    names = ("R_VAR", "Q_VAR", "GLOBAL_VAR")
    lines = [
        _snap("inq", *names),
        f"cd '{world.outside}'",
        _snap("out", *names),
    ]

    found, output = _run([world.bash, "-i"], lines, cwd=world.q, env=ide_env)

    ctx = f"\npty transcript:\n{output}"
    assert found["inq.Q_VAR"] == "q", f"the IDE shell layers its own project{ctx}"
    assert found["inq.R_VAR"] == "r", f"the captured values stay{ctx}"
    assert found["out.Q_VAR"] == ABSENT, f"leaving Q drops Q{ctx}"
    assert found["out.R_VAR"] == "r", (
        f"nothing inherited from the captured state is reverted{ctx}"
    )
