# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Hardening: shells, terminals and the per-prompt hook under hostile session shapes.

Every case drives a real interactive shell on a real pty (``shell_matrix.pty_session``) whose rc
file carries the line ``ocx self setup`` installs; variables are read back with ``@@label@@value``
probes. No registry: projects are ``[env]``-only and ocx runs ``--offline``. Shells absent from the
host skip, and fail where ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` names them (the shell zoo).
"""

from __future__ import annotations

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
        reason="the cases drive POSIX-family shells on a pty.",
    ),
    pytest.mark.command("exec", "shell"),
    # Where the live-shell escalation names ocx, a missing binary must fail (in
    # `_make_world`), not skip: a skip there reads as a pass.
    pytest.mark.skipif(
        matrix.ocx_binary() is None and not matrix.missing_tool_is_fatal("ocx"),
        reason="no ocx binary (set OCX_COMMAND, or build test/bin/ocx).",
    ),
]

ABSENT = matrix.ABSENT
_OCX = matrix.ocx_binary()
_TIMEOUT = 120
_POSIX_SHELLS = ("bash", "zsh")
_RC_NAME = {"bash": ".bashrc", "zsh": ".zshrc"}
_CURRENT_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current"


@dataclass
class World:
    """A private ``$HOME``/``$OCX_HOME`` with projects ``p``, ``q`` and a bare directory."""

    root: Path
    home: Path
    ocx_home: Path
    p: Path
    q: Path
    outside: Path
    ocx: Path
    shell: str
    shell_abs: str

    def env(self, **extra: str) -> dict[str, str]:
        env = matrix.clean_env(
            self.home, self.shell_abs, ocx_home=self.ocx_home, **extra
        )
        env["SHELL"] = self.shell_abs
        env["TERM"] = "dumb"
        env["PS1"] = ""
        env["PROMPT"] = ""
        return env

    def rc_path(self) -> Path:
        return self.home / _RC_NAME[self.shell]

    def lock(self, project: Path) -> None:
        result = matrix.run_lock(self.ocx, project, self.env())
        assert result.returncode == 0, (
            f"ocx lock must succeed for the fixture:\n{result.stderr}"
        )

    def project(self, name: str, body: str, *, locked: bool = True) -> Path:
        path = self.root / "projects" / name
        matrix.write_project(path, body)
        if locked:
            self.lock(path)
        return path


def _need(shell: str) -> str:
    resolved = matrix.which_arm(shell)
    if resolved is None:
        assert not matrix.missing_arm_is_fatal(shell), (
            f"{shell} is not installed, but __OCX_TESTING_REQUIRE_LIVE_SHELLS names it as live here"
        )
        pytest.skip(
            f"{shell} is not installed on this host (shutil.which returned None)"
        )
    return resolved


def _login_stream(ocx: Path, shell: str) -> str:
    return f'eval "$({ocx} --offline self activate --shell={shell} --interactive --no-completion)"\n'


def _make_world(tmp_path: Path, shell: str) -> World:
    shell_abs = _need(shell)
    assert _OCX is not None, (
        "no ocx binary, but __OCX_TESTING_REQUIRE_LIVE_SHELLS names ocx as live here"
    )
    root = tmp_path.resolve()
    home = root / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    candidate = ocx_home / _CURRENT_REL / "content" / "bin" / "ocx"
    candidate.parent.mkdir(parents=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    outside = root / "projects" / "outside"
    outside.mkdir(parents=True)
    built = World(
        root,
        home,
        ocx_home,
        root / "projects" / "p",
        root / "projects" / "q",
        outside,
        _OCX,
        shell,
        shell_abs,
    )
    built.rc_path().write_text(_login_stream(_OCX, shell), encoding="utf-8")
    if shell == "bash":
        (home / ".bash_profile").write_text('. "$HOME/.bashrc"\n', encoding="utf-8")
    built.project("p", 'P_DEF = "p-default"')
    built.project("q", 'Q_VAR = "q"')
    matrix.write_project(ocx_home, 'GLOBAL_VAR = "g"')
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


@pytest.fixture(params=_POSIX_SHELLS)
def world(request: pytest.FixtureRequest, tmp_path: Path) -> World:
    return _make_world(tmp_path, request.param)


@pytest.fixture
def bash_world(tmp_path: Path) -> World:
    return _make_world(tmp_path, "bash")


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


def _interactive(world: World) -> list[str]:
    return [world.shell_abs, "-i"]


# ---------------------------------------------------------------------------
# cd races and mid-session edits (hook shells)
# ---------------------------------------------------------------------------


def test_cd_chain_before_one_prompt_settles_on_the_last_directory(world: World) -> None:
    """Several ``cd`` between two prompts: only the final directory's project applies."""
    names = ("P_DEF", "Q_VAR", "GLOBAL_VAR")
    lines = [
        f"cd '{world.p}' && cd '{world.q}' && cd '{world.outside}' && cd '{world.p}'",
        _snap("a", *names),
        f"cd '{world.q}'; cd '{world.p}'; cd '{world.q}'",
        _snap("b", *names),
        f"(cd '{world.p}')",
        _snap("c", *names),
        f"cd '{world.outside}'",
        _snap("d", *names),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_DEF"] == "p-default" and found["a.Q_VAR"] == ABSENT, (
        f"a chain ending in P is P{ctx}"
    )
    assert found["b.Q_VAR"] == "q" and found["b.P_DEF"] == ABSENT, (
        f"a chain ending in Q is Q{ctx}"
    )
    assert found["c.Q_VAR"] == "q" and found["c.P_DEF"] == ABSENT, (
        f"a subshell cd moves nothing{ctx}"
    )
    assert found["d.Q_VAR"] == ABSENT and found["d.P_DEF"] == ABSENT, (
        f"leaving reverts the project{ctx}"
    )


def test_removing_the_working_directory_under_the_hook_leaves_the_shell_alive(
    world: World,
) -> None:
    """The cwd deleted while the shell sits in it: the next prompts must not crash or wedge the session."""
    doomed = world.outside / "doomed"
    doomed.mkdir()
    lines = [
        f"cd '{doomed}'",
        f"rmdir '{doomed}'",
        "true",
        _snap("gone", "Q_VAR", "GLOBAL_VAR"),
        f"cd '{world.q}'",
        _snap("back", "Q_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert "gone.Q_VAR" in found, f"the shell must answer after its cwd vanished{ctx}"
    assert found["back.Q_VAR"] == "q", (
        f"the hook still works once the shell has a real cwd again{ctx}"
    )


def test_toml_lock_edits_and_new_projects_mid_session_reach_the_next_prompt(
    world: World,
) -> None:
    """Edit + relock refreshes; a project created under the cwd mid-session activates."""
    fresh = world.root / "projects" / "fresh"
    fresh.mkdir()
    edited = (
        'printf "%s\\n" "[env]" "P_DEF = \\"p-edited\\"" > ocx.toml && '
        + f"'{world.ocx}' --offline lock >/dev/null 2>&1"
    )
    newproj = (
        'printf "%s\\n" "[env]" "FRESH_VAR = \\"fresh\\"" > ocx.toml && '
        + f"'{world.ocx}' --offline lock >/dev/null 2>&1"
    )
    lines = [
        f"cd '{world.p}'",
        _snap("a", "P_DEF"),
        edited,
        "true",
        _snap("b", "P_DEF"),
        f"cd '{fresh}'",
        _snap("c", "FRESH_VAR", "P_DEF"),
        newproj,
        "true",
        _snap("d", "FRESH_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_DEF"] == "p-default", ctx
    assert found["b.P_DEF"] == "p-edited", (
        f"an edited and relocked file reaches the next prompt{ctx}"
    )
    assert found["c.FRESH_VAR"] == ABSENT and found["c.P_DEF"] == ABSENT, (
        f"a bare directory applies nothing{ctx}"
    )
    assert found["d.FRESH_VAR"] == "fresh", (
        f"a project that appears under the cwd activates{ctx}"
    )


@pytest.mark.parametrize("victims", ["ocx.toml ocx.lock", "ocx.toml", "ocx.lock"])
def test_deleting_the_project_files_reverts_its_values_at_the_next_prompt(
    world: World, victims: str
) -> None:
    """Removing the watched project files in place drops the project's values without a ``cd``."""
    lines = [
        f"cd '{world.q}'",
        _snap("a", "Q_VAR"),
        f"rm -f {victims}",
        "true",
        _snap("b", "Q_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.Q_VAR"] == "q", ctx
    assert found["b.Q_VAR"] == ABSENT, (
        f"deleting {victims} must revert the project at the next prompt{ctx}"
    )


def test_a_garbled_ocx_toml_mid_session_keeps_the_shell_responsive_and_recovers(
    world: World,
) -> None:
    """A syntax error in the active ``ocx.toml`` never wedges the shell; fixing it restores the value."""
    good = f"printf '%s\\n' '[env]' 'P_DEF = \"p-fixed\"' > ocx.toml && '{world.ocx}' --offline lock >/dev/null 2>&1"
    lines = [
        f"cd '{world.p}'",
        _snap("a", "P_DEF"),
        "printf '%s\\n' '[env' 'P_DEF = ' > ocx.toml",
        "true",
        _snap("b", "P_DEF"),
        good,
        "true",
        _snap("c", "P_DEF"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_DEF"] == "p-default", ctx
    assert found["b.P_DEF"] == "p-default", (
        f"a garbled file must leave the last applied value in place, got {found.get('b.P_DEF')!r}{ctx}"
    )
    assert found["c.P_DEF"] == "p-fixed", f"repairing the file recovers the value{ctx}"


# ---------------------------------------------------------------------------
# ocx shell inside ocx shell
# ---------------------------------------------------------------------------


def test_ocx_shell_inside_ocx_shell_pins_the_inner_selection_and_restores_the_outer(
    world: World,
) -> None:
    """The inner shell pins project Q; ``exit`` returns to the outer shell's P layer untouched."""
    names = ("P_DEF", "Q_VAR", "GLOBAL_VAR")
    lines = [
        _snap("outer", *names),
        f"'{world.ocx}' --offline --project '{world.q}' shell",
        _snap("inner", *names),
        f"cd '{world.outside}'",
        _snap("innerout", *names),
        "exit",
        _snap("back", *names),
    ]

    found, output = _run(
        [str(world.ocx), "--offline", "--project", str(world.p), "shell"],
        lines,
        cwd=world.p,
        env=world.env(),
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["outer.P_DEF"] == "p-default" and found["outer.Q_VAR"] == ABSENT, ctx
    assert found["inner.Q_VAR"] == "q", (
        f"the inner shell composes its pinned project{ctx}"
    )
    assert found["innerout.Q_VAR"] == "q", f"the inner pin survives a cd{ctx}"
    assert found["back.Q_VAR"] == ABSENT and found["back.P_DEF"] == "p-default", (
        f"leaving the inner shell restores the outer environment{ctx}"
    )


def test_ocx_shell_of_the_same_project_twice_does_not_stack_its_values(
    world: World,
) -> None:
    """Pinned shell P inside pinned shell P: PATH and values equal the outer shell's."""
    names = ("P_DEF", "PATH")
    lines = [
        _snap("outer", *names),
        f"'{world.ocx}' --offline --project '{world.p}' shell",
        _snap("inner", *names),
        "exit",
    ]

    found, output = _run(
        [str(world.ocx), "--offline", "--project", str(world.p), "shell"],
        lines,
        cwd=world.p,
        env=world.env(),
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["inner.P_DEF"] == found["outer.P_DEF"] == "p-default", ctx
    assert found["inner.PATH"] == found["outer.PATH"], (
        f"a repeated pin adds nothing to PATH{ctx}"
    )


def test_an_unconsented_project_pinned_by_ocx_shell_reads_no_values_from_it(
    world: World,
) -> None:
    """A project never locked or consented to: the pin may refuse, but its file never reaches the shell."""
    stranger = world.project("stranger", 'STRANGER_VAR = "stranger"', locked=False)
    lines = [_snap("s", "STRANGER_VAR")]

    found, output = _run(
        [str(world.ocx), "--offline", "--project", str(stranger), "shell"],
        lines,
        cwd=world.outside,
        env=world.env(),
    )

    ctx = f"\npty transcript:\n{output}"
    assert found.get("s.STRANGER_VAR", ABSENT) == ABSENT, (
        f"an unconsented project must compose nothing{ctx}"
    )


# ---------------------------------------------------------------------------
# ocx exec --clean -- <login shell>
# ---------------------------------------------------------------------------


def test_exec_clean_login_shell_drops_host_variables_and_keeps_the_exec_layer(
    bash_world: World,
) -> None:
    """``exec --clean -- bash -l -i``: the host's own variable is gone, the project's layer is present."""
    world = bash_world
    env = world.env(HOST_LEAK="host-leak-value")
    lines = [_snap("s", "HOST_LEAK", "P_DEF", "Q_VAR")]

    found, output = _run(
        [
            str(world.ocx),
            "--offline",
            "--project",
            str(world.p),
            "exec",
            "--clean",
            "--",
            world.shell_abs,
            "-l",
            "-i",
        ],
        lines,
        cwd=world.p,
        env=env,
    )

    ctx = f"\npty transcript:\n{output}"
    assert found["s.HOST_LEAK"] == ABSENT, (
        f"--clean must not carry the host's variables into the login shell{ctx}"
    )
    assert found["s.P_DEF"] == "p-default", (
        f"the exec layer reaches the login shell{ctx}"
    )
    assert found["s.Q_VAR"] == ABSENT, ctx


# ---------------------------------------------------------------------------
# Rollback of ``current`` and token-less hooks (bash)
# ---------------------------------------------------------------------------


def test_rolling_back_current_under_an_open_shell_neither_wedges_nor_spams_it(
    bash_world: World,
) -> None:
    """``current`` points at a build without the binary, then back: the shell stays quiet and recovers."""
    world = bash_world
    current = world.ocx_home / _CURRENT_REL
    release = current.parent / "v1"
    current.rename(release)
    current.symlink_to(release)
    broken = current.parent / "empty"
    broken.mkdir()
    binary = current / "content" / "bin" / "ocx"
    hook = subprocess.run(
        [
            str(binary),
            "--offline",
            "self",
            "activate",
            "--shell=bash",
            "--hook",
            "--no-completion",
        ],
        capture_output=True,
        text=True,
        check=False,
        env=world.env(),
    )
    assert hook.returncode == 0, hook.stderr
    (world.home / "hook.sh").write_text(hook.stdout, encoding="utf-8")
    world.rc_path().write_text('. "$HOME/hook.sh"\n', encoding="utf-8")
    swap_away = f"ln -sfn '{broken}' '{current}'"
    swap_back = f"ln -sfn '{release}' '{current}'"
    lines = [
        f"cd '{world.q}'",
        _snap("a", "Q_VAR"),
        swap_away,
        f"cd '{world.outside}'",
        "echo marker-after-rollback",
        _snap("b", "Q_VAR"),
        swap_back,
        "true",
        _snap("c", "Q_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.Q_VAR"] == "q", (
        f"fixture: the hook must apply Q before the swap{ctx}"
    )
    assert "marker-after-rollback" in output, ctx
    assert "No such file" not in output.split("marker-after-rollback", 1)[1].split(
        "@@b.", 1
    )[0].replace("echo marker-after-rollback", ""), (
        f"a vanished ocx must not print errors at the prompt{ctx}"
    )
    assert found["c.Q_VAR"] == ABSENT, (
        f"once ocx is back the hook reverts Q at the first prompt outside q{ctx}"
    )


def _token_less_hook(world: World) -> None:
    """Rewrite the rc so the hook runs as an older emission would: no ``--shell-pid``."""
    hook = subprocess.run(
        [
            str(world.ocx),
            "--offline",
            "self",
            "activate",
            "--shell=bash",
            "--hook",
            "--no-completion",
        ],
        capture_output=True,
        text=True,
        check=False,
        env=world.env(),
    )
    assert hook.returncode == 0, hook.stderr
    assert " --shell-pid $$" in hook.stdout, (
        "fixture: the current emission passes the shell pid"
    )
    (world.home / "hook.sh").write_text(
        hook.stdout.replace(" --shell-pid $$", ""), encoding="utf-8"
    )
    world.rc_path().write_text('. "$HOME/hook.sh"\n', encoding="utf-8")


def test_a_token_less_hook_applies_and_reverts_like_a_current_one(
    bash_world: World,
) -> None:
    """The older emission (no ``--shell-pid``) still tracks the cwd across several prompts."""
    world = bash_world
    _token_less_hook(world)
    lines = [
        f"cd '{world.q}'",
        _snap("a", "Q_VAR"),
        "true",
        _snap("b", "Q_VAR"),
        f"cd '{world.p}'",
        _snap("c", "Q_VAR", "P_DEF"),
        f"cd '{world.outside}'",
        _snap("d", "Q_VAR", "P_DEF"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["a.Q_VAR"] == found["b.Q_VAR"] == "q", (
        f"a second prompt must not retag away its own records{ctx}"
    )
    assert found["c.Q_VAR"] == ABSENT and found["c.P_DEF"] == "p-default", ctx
    assert found["d.Q_VAR"] == ABSENT and found["d.P_DEF"] == ABSENT, ctx


def test_a_token_less_nested_shell_keeps_what_its_parent_applied_when_it_leaves(
    bash_world: World,
) -> None:
    """Named residual: a nested token-less hook sees the parent's records as its own; the parent must stay coherent."""
    world = bash_world
    _token_less_hook(world)
    lines = [
        f"cd '{world.q}'",
        _snap("parent", "Q_VAR"),
        f"{world.shell_abs} -i",
        _snap("child", "Q_VAR"),
        f"cd '{world.outside}'",
        _snap("childout", "Q_VAR"),
        "exit",
        _snap("parentagain", "Q_VAR"),
        f"cd '{world.outside}'",
        _snap("parentout", "Q_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=world.env())

    ctx = f"\npty transcript:\n{output}"
    assert found["parent.Q_VAR"] == found["child.Q_VAR"] == "q", ctx
    assert found["parentagain.Q_VAR"] == "q", (
        f"leaving the child never changes the parent's own environment{ctx}"
    )
    assert found["parentout.Q_VAR"] == ABSENT, (
        f"the parent still reverts its own project{ctx}"
    )


# ---------------------------------------------------------------------------
# Terminals: tmux, VS Code environment
# ---------------------------------------------------------------------------


def test_vscode_terminal_environment_does_not_disturb_the_hook(world: World) -> None:
    """VS Code marks its terminals with ``TERM_PROGRAM=vscode`` and its own variables; the hook is unaffected."""
    env = world.env(
        TERM_PROGRAM="vscode",
        VSCODE_INJECTION="1",
        VSCODE_GIT_IPC_HANDLE="/nonexistent/vscode-git.sock",
    )
    lines = [
        f"cd '{world.q}'",
        _snap("in", "Q_VAR", "TERM_PROGRAM", "VSCODE_INJECTION"),
        f"cd '{world.outside}'",
        _snap("out", "Q_VAR"),
    ]

    found, output = _run(_interactive(world), lines, cwd=world.outside, env=env)

    ctx = f"\npty transcript:\n{output}"
    assert found["in.Q_VAR"] == "q" and found["out.Q_VAR"] == ABSENT, ctx
    assert (
        found["in.TERM_PROGRAM"] == "vscode" and found["in.VSCODE_INJECTION"] == "1"
    ), f"the hook never rewrites the terminal's own variables{ctx}"


@pytest.mark.skipif(
    shutil.which("tmux") is None and not matrix.missing_tool_is_fatal("tmux"),
    reason="tmux is not installed on this host",
)
def test_two_tmux_shells_on_one_project_refresh_independently(
    bash_world: World,
) -> None:
    """Two panes in project P: an edit made in one reaches both at their next prompt; leaving in one leaves the other."""
    assert shutil.which("tmux") is not None, (
        "tmux is not installed, but __OCX_TESTING_REQUIRE_LIVE_SHELLS names it as live here"
    )
    world = bash_world
    with short_socket("tmux.sock") as socket:
        out = world.root / "pane-out"
        out.mkdir()
        tmux = f"tmux -S '{socket}' -f /dev/null"

        def pane(step: str, target: str, *command: str) -> list[str]:
            """Source a script in one pane, wait for its probe file, print it as probes."""
            probe = out / step
            script = out / f"{step}.sh"
            script.write_text(
                "\n".join([*command, f"{{ {_snap(step, 'P_DEF', 'Q_VAR')}; }} > '{probe}'"])
                + "\n",
                encoding="utf-8",
            )
            return [
                f"{tmux} send-keys -t {target} -l \". '{script}'\" && {tmux} send-keys -t {target} Enter",
                (
                    f"for i in $(seq 150); do [ -s '{probe}' ] && break; sleep 0.2; done; "
                    f"while IFS= read -r l; do printf '%s\\n' \"$l\"; done < '{probe}'"
                ),
            ]

        edit = f"printf '%s\\n' '[env]' 'P_DEF = \"p-both\"' > '{world.p / 'ocx.toml'}' && (cd '{world.p}' && '{world.ocx}' --offline lock >/dev/null 2>&1)"
        ready = f'for i in $(seq 100); do [ -n "$({tmux} capture-pane -p -t {{t}} 2>/dev/null)" ] && break; sleep 0.1; done'
        lines = [
            f"{tmux} new-session -d -s s -n one -x 200 -y 50 -c '{world.p}'",
            f"{tmux} new-window -t s -n two -c '{world.p}'",
            ready.format(t="s:one"),
            ready.format(t="s:two"),
            *pane("one_start", "s:one", "true"),
            *pane("two_start", "s:two", "true"),
            *pane("one_edit", "s:one", edit),
            *pane("one_after", "s:one", "true"),
            *pane("two_prime", "s:two", "true"),
            *pane("two_after", "s:two", "true"),
            *pane("two_leave", "s:two", f"cd '{world.q}'"),
            *pane("two_left", "s:two", "true"),
            *pane("one_still", "s:one", "true"),
            f"{tmux} kill-server",
        ]
        try:
            # A plain outer shell: panes started from a hook shell would inherit its loaded project as an outer scope.
            found, output = _run(
                [world.shell_abs, "--norc", "-i"], lines, cwd=world.outside, env=world.env()
            )
        finally:
            subprocess.run(
                ["tmux", "-S", str(socket), "kill-server"],
                capture_output=True,
                check=False,
                timeout=30,
            )

    ctx = f"\npty transcript:\n{output}"
    assert found["one_start.P_DEF"] == found["two_start.P_DEF"] == "p-default", (
        f"both panes activate P{ctx}"
    )
    assert found["one_after.P_DEF"] == "p-both", (
        f"the editing pane refreshes at its next prompt{ctx}"
    )
    assert found["two_after.P_DEF"] == "p-both", (
        f"the other pane refreshes at its own next prompt{ctx}"
    )
    assert found["two_left.Q_VAR"] == "q" and found["two_left.P_DEF"] == ABSENT, ctx
    assert (
        found["one_still.P_DEF"] == "p-both" and found["one_still.Q_VAR"] == ABSENT
    ), f"leaving in one pane never changes the other{ctx}"


# ---------------------------------------------------------------------------
# Hook shells absent from this host: carried by the shell zoo
# ---------------------------------------------------------------------------

_ZOO_SHELLS = ("fish", "pwsh")


def _zoo_recipe(shell: str, ocx: Path) -> tuple[list[str], str, str, str]:
    """``(argv, install line, cd verb, probe kind)`` for a hook shell the zoo carries."""
    if shell == "fish":
        return (
            ["--no-config", "-i"],
            f'"{ocx}" --offline self activate --shell=fish --hook --no-completion | source',
            "cd",
            "fish",
        )
    return (
        ["-NoProfile", "-NoLogo"],
        f'Invoke-Expression (& "{ocx}" --offline self activate --shell=pwsh --hook --no-completion | Out-String)',
        "Set-Location",
        "pwsh",
    )


def _zoo_probe(kind: str, label: str, name: str) -> str:
    if kind == "pwsh":
        return (
            f'$v = if (Test-Path env:{name}) {{ (Get-Item env:{name}).Value }} else {{ "{ABSENT}" }}; '
            f'Write-Output ("@" + "@{label}@" + "@" + $v)'
        )
    return (
        f'if set -q {name}; printf \'%s\\n\' "@""@{label}@""@${name}"; '
        f'else; printf \'%s\\n\' "@""@{label}@""@{ABSENT}"; end'
    )


@pytest.mark.parametrize("shell", _ZOO_SHELLS)
def test_cd_chain_and_leave_in_fish_and_pwsh_hooks(shell: str, tmp_path: Path) -> None:
    """Zoo row: a ``cd`` chain settles on the last directory and leaving reverts, in the fish and PowerShell hooks."""
    shell_abs = _need(shell)
    assert _OCX is not None, (
        "no ocx binary, but __OCX_TESTING_REQUIRE_LIVE_SHELLS names ocx as live here"
    )
    root = tmp_path.resolve()
    home = root / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    # The emitted hook runs the *installed* ocx and gates on it existing; without one it never reconciles.
    matrix.seed_binary(_OCX, ocx_home / _CURRENT_REL / "content" / "bin" / "ocx")
    env = matrix.clean_env(home, shell_abs, ocx_home=ocx_home)
    env.update({"TERM": "dumb", "PS1": "", "SHELL": shell_abs})
    projects = root / "projects"
    q = projects / "q"
    outside = projects / "outside"
    outside.mkdir(parents=True)
    matrix.write_project(q, 'Q_VAR = "q"')
    assert matrix.run_lock(_OCX, q, env).returncode == 0
    argv, install, cd, kind = _zoo_recipe(shell, _OCX)
    lines = [
        install,
        "$null = $true" if kind == "pwsh" else "true",
        f"{cd} '{q}'",
        _zoo_probe(kind, "inq", "Q_VAR"),
        f"{cd} '{outside}'; {cd} '{q}'; {cd} '{outside}'",
        _zoo_probe(kind, "out", "Q_VAR"),
    ]

    output = matrix.pty_session(
        [shell_abs, *argv],
        lines,
        cwd=outside,
        env=env,
        timeout=_TIMEOUT,
        controlling_tty=shell == "fish",
    )
    found = matrix.probes(output)

    ctx = f"\npty transcript:\n{output}"
    assert found.get("inq") == "q", f"{shell}: cd into Q applies it{ctx}"
    assert found.get("out") == ABSENT, (
        f"{shell}: a chain ending outside reverts it{ctx}"
    )
