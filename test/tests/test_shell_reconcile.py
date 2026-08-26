# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""All-shell matrix for the per-prompt environment reconciler (WP-14, tiers 2-3).

Extends the ``test_shell_activation.py`` idiom rather than inventing one: stdlib
+ pytest only, no ``src.runner``, no registry fixture, ``shutil.which`` skips,
and the same shell-zoo container. What it adds is the **reconciler**: what one
prompt does to a live environment when the project under the cursor changes.

Three tiers, each with an honest reach:

* **Tier 2 — evaluate the emitted snippet, all nine arms**
  (``sh, dash, ash, bash, zsh, fish, pwsh, nushell, elvish``). ``ocx self
  activate --reconcile --shell=X`` emits an evaluable stream for **every** arm,
  including the five that host no prompt hook, so a real interpreter can apply
  it and the result read back. This is where apply, retire, idempotency and
  cross-shell inheritance live.
* **Tier 3 — a real pty, a real prompt**, bash and pwsh (plus zsh where it is
  free). Only here does the hook itself fire, so only here can "the hook ran at
  all" be proved.
* **Tier 1 assertions on the emitted string** where a tier-2/3 failure would be
  a *silent wrong value* — the escaping rows.

**Nushell.** Its ``env_change.PWD`` hook is inlined in ``ENV_NU``
(``crates/ocx_lib/src/setup/shims.rs``) and calls only ``ocx --format json
--global env``: it applies the global toolchain and never consumes
``--reconcile``. So the nushell arm takes part in **global-scope tier-2 rows
only**; every project-scope row skips through
:func:`_skip_nushell_without_reconcile`, which **greps the shipped ``env.nu``**
for the string ``reconcile`` and skips on its absence. The skip therefore names
a condition the test observed, not one it assumed.

Registry hygiene: this module builds every fixture on disk and runs ocx
``--offline``. It touches none of the shared-registry fixtures, so it is safe to
run concurrently with another worktree's suite.
"""

from __future__ import annotations

import functools
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="the reconciler matrix drives POSIX-family / container shells; the Windows leg is WP-18.",
    ),
]

_OCX = matrix.ocx_binary()

pytestmark.append(
    pytest.mark.skipif(
        _OCX is None,
        reason="no ocx binary (set OCX_ACTIVATION_BINARY / OCX_COMMAND, or build test/bin/ocx).",
    )
)

# The install-layout path the offline bootstrap candidate lives at.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"

# Project-scope rows are parametrized over nushell too, so its exclusion is a
# VISIBLE skip carrying the cause `_skip_nushell_without_reconcile` observed —
# never a silent absence from the matrix.
_PROJECT_SCOPE_SHELLS: tuple[str, ...] = (*matrix.SESSION_SHELLS, "nushell")

# A value with every character that has ever broken a shell escaper, including
# the live command-substitution attempt the `'`-injection fixture exists for.
_HOSTILE_SEGMENT = "/tmp/a';id;'b"
_HOSTILE_CONSTANT = "q'd \"dq\" `tick` $VAR \\slash %WINVAR% end"


# ---------------------------------------------------------------------------
# Arena — one isolated OCX home + project root per test
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
    """A clean install root with the ocx bootstrap candidate already seeded.

    Seeding the candidate is what makes ``ocx --offline self setup`` resolve
    ``already_present`` with no registry, and it puts the install bin directory
    on the reconciler's watch set — the member ``ocx self update`` moves.
    """
    home = tmp_path / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)

    scripts = tmp_path / "scripts"
    scripts.mkdir()
    projects = tmp_path / "projects"
    projects.mkdir()
    return Arena(home=home, ocx_home=ocx_home, scripts=scripts, projects=projects, ocx=_OCX)


def _locked_project(arena: Arena, name: str, env_block: str) -> Path:
    """A project with a real ``ocx.lock`` — and therefore a real consent stamp.

    ``ocx lock`` is one of the six commands on A-29's closed stamp-writer
    allowlist, so this is the acceptance-level spelling of "the user ran an ocx
    command here". It also produces the only lock whose ``declaration_hash``
    satisfies the composer's staleness gate.
    """
    project = arena.projects / name
    matrix.write_project(project, env_block)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, f"ocx lock must succeed for the fixture; stderr:\n{result.stderr}"
    assert (project / "ocx.lock").is_file(), "ocx lock must write ocx.lock"
    return project


def _clone_of(project: Path, destination: Path) -> Path:
    """Copy a locked project to a new directory — a fresh clone, with no stamp.

    The stamp is keyed on the canonical project directory, so the copy carries
    the lock and none of the consent. ``declaration_hash`` covers ``[tools]`` and
    ``[group.*]`` only, so the copied lock is still current for the copy.
    """
    shutil.copytree(project, destination)
    return destination


def _require(shell: str) -> str:
    """Absolute interpreter path for ``shell``, or skip naming what is missing.

    The skip is a failure wherever ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` names
    this arm — on an image that ships the interpreter, "skipped" and "passed"
    carry the same evidence and only the first is honest.
    """
    resolved = matrix.which_arm(shell)
    if resolved is None:
        binary = matrix.ARMS[shell].binary
        assert not matrix.missing_arm_is_fatal(shell), (
            f"{binary} is not installed, so this test asserted nothing — and "
            "__OCX_TESTING_REQUIRE_LIVE_SHELLS names it as an arm that must be live here"
        )
        pytest.skip(f"{binary} is not installed on this host (shutil.which returned None)")
    return resolved


def _self_setup(arena: Arena, shell_name: str = "bash") -> subprocess.CompletedProcess[str]:
    """Run ``ocx --offline self setup``, which writes every ``$OCX_HOME/env.*`` shim."""
    env = arena.env(shell_abs="/bin/sh")
    env["SHELL"] = f"/bin/{shell_name}"
    result = subprocess.run(
        [str(arena.ocx), "--offline", "self", "setup"],
        capture_output=True,
        check=False,
        text=True,
        env=env,
    )
    assert result.returncode == 0, f"self setup must succeed; stderr:\n{result.stderr}"
    return result


def _skip_nushell_without_reconcile(shell: str, arena: Arena) -> None:
    """Skip a project-scope row on nushell — after **observing** why.

    Reads the ``env.nu`` the binary under test actually writes and counts the
    string ``reconcile`` in it. Nushell's whole per-prompt path is inlined in
    that body (it has no string ``eval``, A-24), so a body that never names
    ``--reconcile`` cannot apply a project scope, cannot revert one, and cannot
    advance ``__OCX_ENV_STATE``. The skip message quotes the count it read, so it
    cannot outlive WP-12b landing.
    """
    if shell != "nushell":
        return
    env_nu = arena.ocx_home / "env.nu"
    if not env_nu.is_file():
        _self_setup(arena, "nu")
    assert env_nu.is_file(), f"self setup must write {env_nu}; without it the skip below would be unobserved"
    body = env_nu.read_text(encoding="utf-8")
    occurrences = body.count("reconcile")
    if occurrences == 0:
        pytest.skip(
            "WP-12b unlanded: `ENV_NU` never calls `--reconcile` — the shipped "
            f"{env_nu} contains 0 occurrences of 'reconcile' (observed), so nushell's "
            "env_change.PWD hook applies the global toolchain only and can neither "
            "revert a project scope nor advance __OCX_ENV_STATE"
        )


def _locate_prompt_tool(tool: str) -> str:
    """Resolve a third-party prompt framework, or skip naming what was probed.

    ``starship`` is a binary; ``oh-my-zsh`` and ``powerlevel10k`` are trees. Both
    forms are probed for real — a skip may only name a cause the test observed,
    and "not installed" is a claim about this image that has to be checked.
    """
    if tool == "starship":
        resolved = shutil.which("starship")
        if resolved is None:
            pytest.skip(
                "starship is not installed in this image (shutil.which('starship') is None) "
                "— WP-18 shell-zoo refresh"
            )
        return resolved
    candidates = {
        "oh-my-zsh": [Path.home() / ".oh-my-zsh", Path("/usr/share/oh-my-zsh")],
        "powerlevel10k": [
            Path.home() / "powerlevel10k",
            Path.home() / ".oh-my-zsh" / "custom" / "themes" / "powerlevel10k",
            Path("/usr/share/powerlevel10k"),
        ],
    }[tool]
    for candidate in candidates:
        if candidate.is_dir():
            return str(candidate)
    pytest.skip(
        f"{tool} is not installed in this image (none of {[str(c) for c in candidates]} is a directory) "
        "— WP-18 shell-zoo refresh"
    )
    raise AssertionError("unreachable: pytest.skip raises")


# ---------------------------------------------------------------------------
# Session driver — one shell process, N simulated prompts
# ---------------------------------------------------------------------------


def _session(
    shell: str,
    arena: Arena,
    fragments: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    name: str = "session",
) -> subprocess.CompletedProcess[str]:
    """Run ``fragments`` as one script in ``shell``, with ``__ocx_exe`` bound."""
    shell_abs = _require(shell)
    body = "\n".join([matrix.header(shell, arena.ocx), *fragments])
    return matrix.run_script(
        shell,
        shell_abs,
        body,
        cwd=cwd or arena.projects,
        env=env or arena.env(shell_abs),
        script_dir=arena.scripts,
        name=name,
    )


def _read(result: subprocess.CompletedProcess[str], label: str) -> str:
    """One probe's value, with the whole session in the failure message."""
    found = matrix.probes(result.stdout)
    assert label in found, (
        f"probe '{label}' never printed — the session did not reach it.\n"
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    return found[label]


@functools.cache
def _empty_assignment_terminal_state(shell: str) -> str:
    """How ``shell`` reads back a variable that was just assigned the empty string.

    Emptying a list key is one emitted assignment, but the *observable* result
    is the runtime's, not ocx's: PowerShell 7.4 on .NET 8 deletes an env var
    assigned ``''`` while 7.6 on .NET 10 keeps it set-but-empty, so the shell
    zoo and the developer host disagree about the same emitted plan. Hardcoding
    either spelling makes the assertion red on the other machine; asserting
    "either" would stop discriminating. So ask the interpreter that is actually
    about to run, with a probe that shares no code with the reconciler.

    Returns :data:`shell_matrix.ABSENT` or ``""`` — never a value, so an element
    that was merely shadowed instead of removed still reds.
    """
    shell_abs = _require(shell)
    with tempfile.TemporaryDirectory(prefix="wp14-semantics-") as scratch:
        probe = matrix.run_script(
            shell,
            shell_abs,
            "\n".join(
                [
                    matrix.set_var(shell, "WP14_SEMANTIC_PROBE", "seeded"),
                    matrix.set_var(shell, "WP14_SEMANTIC_PROBE", ""),
                    matrix.probe(shell, "emptied", "WP14_SEMANTIC_PROBE"),
                ]
            ),
            cwd=Path(scratch),
            env=matrix.clean_env(Path(scratch), shell_abs, ocx_home=Path(scratch) / "ocx-home"),
            script_dir=Path(scratch),
            name="empty-assignment",
        )
    observed = _read(probe, "emptied")
    assert observed in ("", matrix.ABSENT), (
        f"{shell}: the empty-assignment probe must read back as unset or empty, never a value; got {observed!r}"
    )
    return observed


# ---------------------------------------------------------------------------
# Tier 2 — lifecycle: enter, leave, switch
# ---------------------------------------------------------------------------

_ENV_BLOCK_A = (
    'WP14_CONST = "alpha"\n'
    'PATH = { type = "path", value = "binA" }\n'
    'CFLAGS = { type = "list", separator = " ", value = "-DPROJECT_A" }\n'
)
_ENV_BLOCK_B = (
    'WP14_CONST = "bravo"\n'
    'PATH = { type = "path", value = "binB" }\n'
)


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_entering_a_consented_project_applies_constant_path_and_list(shell: str, arena: Arena) -> None:
    """S-001 / C-013 / C-018 — one prompt inside a consented project applies all three kinds."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "const", "WP14_CONST"),
            matrix.probe(shell, "path", "PATH"),
            matrix.probe(shell, "list", "CFLAGS"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "const") == "alpha"
    assert _read(result, "list") == "-DPROJECT_A"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments[0] == str(project / "binA"), (
        f"{shell}: the project's path entry must land at the front of PATH; got {segments[:3]}"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_leaving_a_project_reverts_its_constant_path_and_list(shell: str, arena: Arena) -> None:
    """S-002 / C-016 / C-017 — leaving reverts the project section and only it."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    outside = arena.projects / "outside"
    outside.mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "in.const", "WP14_CONST"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "out.const", "WP14_CONST"),
            matrix.probe(shell, "out.path", "PATH"),
            matrix.probe(shell, "out.list", "CFLAGS"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "in.const") == "alpha", "the fixture must actually apply before the revert is meaningful"
    assert _read(result, "out.const") == matrix.ABSENT, (
        f"{shell}: a constant with no prior must be unset on leave, never left applied"
    )
    assert str(project / "binA") not in matrix.path_segments(_read(result, "out.path")), (
        f"{shell}: the project's PATH element must be retired on leave"
    )
    # Shipped behaviour, asserted rather than assumed: `remove_list_element` is
    # `append_unique`'s inverse, so removing ocx's only contribution empties the
    # key rather than restoring a constant (C-016). How "emptied" *reads back*
    # is the host runtime's call, not the emitter's, so the expectation is
    # **observed from the running interpreter** instead of hardcoded per arm —
    # see `_empty_assignment_terminal_state`. Both spellings still red if the
    # element were merely shadowed rather than removed.
    expected_emptied_list = _empty_assignment_terminal_state(shell)
    assert _read(result, "out.list") == expected_emptied_list, (
        f"{shell}: the list contribution must be removed on leave; "
        f"expected {expected_emptied_list!r}, got {_read(result, 'out.list')!r}"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_switching_projects_reverts_a_and_applies_b_in_one_pass(shell: str, arena: Arena) -> None:
    """S-003 / C-018 — a direct A→B switch leaves no element of A behind."""
    _skip_nushell_without_reconcile(shell, arena)
    alpha = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (alpha / "binA").mkdir()
    bravo = _locked_project(arena, "bravo", _ENV_BLOCK_B)
    (bravo / "binB").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, alpha),
            matrix.prompt(shell),
            matrix.probe(shell, "a.const", "WP14_CONST"),
            matrix.cd_to(shell, bravo),
            matrix.prompt(shell),
            matrix.probe(shell, "b.const", "WP14_CONST"),
            matrix.probe(shell, "b.path", "PATH"),
            matrix.probe(shell, "b.list", "CFLAGS"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "a.const") == "alpha"
    assert _read(result, "b.const") == "bravo", f"{shell}: B's constant must win after the switch"
    segments = matrix.path_segments(_read(result, "b.path"))
    assert str(bravo / "binB") in segments, f"{shell}: B's path element must be applied"
    assert segments.count(str(alpha / "binA")) == 0, (
        f"{shell}: no element of A may survive the switch; PATH was {segments}"
    )
    assert "-DPROJECT_A" not in _read(result, "b.list"), (
        f"{shell}: A's list contribution must be retired even though B declares no CFLAGS; "
        f"got {_read(result, 'b.list')!r}"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_project_nested_inside_another_is_a_switch_not_a_push(shell: str, arena: Arena) -> None:
    """S-003 edge / C-018 — the walk returns the NEAREST ocx.toml, so nesting still switches."""
    _skip_nushell_without_reconcile(shell, arena)
    outer = _locked_project(arena, "outer", _ENV_BLOCK_A)
    (outer / "binA").mkdir()
    inner = _locked_project(arena, "outer/inner", _ENV_BLOCK_B)
    (inner / "binB").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, outer),
            matrix.prompt(shell),
            matrix.cd_to(shell, inner),
            matrix.prompt(shell),
            matrix.probe(shell, "const", "WP14_CONST"),
            matrix.probe(shell, "path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "const") == "bravo", f"{shell}: the nearest project must win"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments.count(str(outer / "binA")) == 0, (
        f"{shell}: the outer project's element must be retired, not stacked under the inner one; got {segments}"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_first_prompt_of_a_new_shell_applies_without_a_carrier(shell: str, arena: Arena) -> None:
    """S-042 / C-046 — the carrier is unset by construction on the first prompt."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.probe(shell, "before", matrix.CARRIER),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "after", matrix.CARRIER),
            matrix.probe(shell, "const", "WP14_CONST"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "before") == matrix.ABSENT, (
        f"{shell}: the carrier must be unset before the first prompt, or this test proves nothing"
    )
    assert _read(result, "after").startswith("1."), (
        f"{shell}: the first prompt must write a versioned carrier envelope"
    )
    assert _read(result, "const") == "alpha"


# ---------------------------------------------------------------------------
# Tier 2 — freshness in a live shell
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", matrix.SESSION_SHELLS)
def test_a_lock_written_between_prompts_is_picked_up_in_the_same_shell(shell: str, arena: Arena) -> None:
    """S-004 (owner headline, tier-2 half) — `ocx add --global` lands at the next prompt.

    The mechanism is the watch set, not the command: `$OCX_HOME/ocx.lock` moving
    changes the fingerprint (C-019, C-020). Driven here with the **global**
    toolchain's own `[env]`, so the nushell arm can take part.
    """
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nWP14_GLOBAL = "before"\n', encoding="utf-8")
    result = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        cwd=str(arena.projects),
    )
    assert result.returncode == 0, f"global lock must succeed; stderr:\n{result.stderr}"

    outside = arena.projects / "outside"
    outside.mkdir()
    rewrite = f"{arena.ocx} --offline --global lock"
    session = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "before", "WP14_GLOBAL"),
            _write_file_fragment(shell, global_toml, '[env]\nWP14_GLOBAL = "after"\n'),
            _run_fragment(shell, rewrite),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP14_GLOBAL"),
        ],
    )
    assert session.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{session.stderr}"
    assert _read(session, "before") == "before"
    assert _read(session, "after") == "after", (
        f"{shell}: a global-toolchain change must be picked up at the NEXT PROMPT of the SAME shell"
    )


def _write_file_fragment(shell: str, path: Path, content: str) -> str:
    """Rewrite a file from inside the running shell, without a heredoc."""
    literal_path = matrix.quote(shell, str(path))
    literal_body = matrix.quote(shell, content)
    if shell == "pwsh":
        return f"Set-Content -LiteralPath {literal_path} -Value {literal_body} -NoNewline"
    if shell == "fish":
        return f"printf '%s' {literal_body} > {literal_path}"
    if shell == "elvish":
        return f"print {literal_body} > {literal_path}"
    if shell == "nushell":
        return f"{literal_body} | save --force {literal_path}"
    return f"printf '%s' {literal_body} > {literal_path}"


def _run_fragment(shell: str, command: str) -> str:
    """Run a shell-agnostic command line, discarding its output."""
    parts = command.split()
    literals = " ".join(matrix.quote(shell, part) for part in parts)
    if shell == "pwsh":
        return f"& {literals} | Out-Null"
    if shell == "elvish":
        return f"(external {matrix.quote(shell, parts[0])}) {' '.join(matrix.quote(shell, p) for p in parts[1:])} > /dev/null 2>&1"
    if shell == "nushell":
        return f"^{literals} out+err> /dev/null"
    if shell == "fish":
        return f"{literals} >/dev/null 2>&1"
    return f"{literals} >/dev/null 2>&1"


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_changed_env_value_resolves_at_the_next_prompt(shell: str, arena: Arena) -> None:
    """S-005 / S-009 — an `[env]`-only change (a branch switch that touches no lock) applies."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'WP14_VERSION = "1.0.0"\n')

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "old", "WP14_VERSION"),
            _write_file_fragment(shell, project / "ocx.toml", '[env]\nWP14_VERSION = "2.0.0"\n'),
            matrix.prompt(shell),
            matrix.probe(shell, "new", "WP14_VERSION"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "old") == "1.0.0"
    assert _read(result, "new") == "2.0.0", (
        f"{shell}: `[env]` is in the watch set on its own authority — the lock never moved (C-019)"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_removed_env_entry_is_gone_not_shadowed(shell: str, arena: Arena) -> None:
    """S-007 / C-016 — the retirement rule: the element's count on PATH is ZERO.

    An apply-only reconciler passes "the new one is in front" vacuously; it
    cannot pass this.
    """
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(
        arena,
        "alpha",
        'PATH = { type = "path", value = "doomed" }\nWP14_DOOMED = "yes"\n',
    )
    (project / "doomed").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "before.path", "PATH"),
            _write_file_fragment(shell, project / "ocx.toml", "[env]\n"),
            matrix.prompt(shell),
            matrix.probe(shell, "after.path", "PATH"),
            matrix.probe(shell, "after.const", "WP14_DOOMED"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    doomed = str(project / "doomed")
    assert doomed in matrix.path_segments(_read(result, "before.path")), (
        "the element must be applied first, or its removal proves nothing"
    )
    assert matrix.path_segments(_read(result, "after.path")).count(doomed) == 0, (
        f"{shell}: a retired PATH element must be GONE, not shadowed"
    )
    assert _read(result, "after.const") == matrix.ABSENT


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_digest_change_leaves_zero_occurrences_of_the_old_segment(shell: str, arena: Arena) -> None:
    """S-010 — the named fault-injection row: `…/packages/<old>/bin` count must be 0.

    The two segments are **different strings**, so ``move_to_front`` cannot
    dedupe them: an additive repair leaves the stale one on PATH for the rest of
    the session and any later foreign prepend can put it back in front. The
    assertion is factored into :func:`_assert_stale_digest_retired` so its red
    state is demonstrable on an input this file controls — see
    ``test_the_digest_duplicate_assertion_reds_on_an_additive_path``.
    """
    _skip_nushell_without_reconcile(shell, arena)
    old_digest = "sha256_" + "aa" * 8
    new_digest = "sha256_" + "bb" * 8
    packages = arena.ocx_home / "packages"
    (packages / old_digest / "bin").mkdir(parents=True)
    (packages / new_digest / "bin").mkdir(parents=True)

    project = _locked_project(
        arena,
        "alpha",
        f'PATH = {{ type = "path", value = "{packages / old_digest / "bin"}" }}\n',
    )

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "old.path", "PATH"),
            _write_file_fragment(
                shell,
                project / "ocx.toml",
                f'[env]\nPATH = {{ type = "path", value = "{packages / new_digest / "bin"}" }}\n',
            ),
            matrix.prompt(shell),
            matrix.probe(shell, "new.path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert str(packages / old_digest / "bin") in matrix.path_segments(_read(result, "old.path")), (
        "the old digest's bin dir must be applied first, or the count-zero assertion is vacuous"
    )
    _assert_stale_digest_retired(
        _read(result, "new.path"),
        old_segment=str(packages / old_digest / "bin"),
        new_segment=str(packages / new_digest / "bin"),
    )


def _assert_stale_digest_retired(path_value: str, *, old_segment: str, new_segment: str) -> None:
    """The S-010 assertion, factored out so both of its colours are demonstrable."""
    segments = matrix.path_segments(path_value)
    assert new_segment in segments, f"the new digest's bin dir must be on PATH; got {segments}"
    assert segments.count(old_segment) == 0, (
        f"the old digest's bin dir must appear ZERO times after the repair, not merely behind the new one; "
        f"found {segments.count(old_segment)} in {segments}"
    )


def test_the_digest_duplicate_assertion_reds_on_an_additive_path(arena: Arena) -> None:
    """The named fault injection (plan §7.6), realized at the level this WP owns.

    The Rust-side mutation is "make the list repair additive instead of
    subtractive". Its **observable** is a PATH that still carries the old
    segment. This feeds :func:`_assert_stale_digest_retired` both the subtractive
    output (green) and the additive one (red), on inputs constructed here, so the
    assertion is proved to discriminate rather than merely to have passed.
    """
    old_segment = "/ocx/packages/sha256_aaaa/bin"
    new_segment = "/ocx/packages/sha256_bbbb/bin"
    subtractive = os.pathsep.join([new_segment, "/usr/bin", "/bin"])
    additive = os.pathsep.join([new_segment, old_segment, "/usr/bin", "/bin"])

    # Green: the shipped subtractive repair.
    _assert_stale_digest_retired(subtractive, old_segment=old_segment, new_segment=new_segment)

    # Red: exactly what an additive repair produces. `move_to_front` cannot
    # dedupe two different strings, so the stale segment survives.
    with pytest.raises(AssertionError, match="ZERO times"):
        _assert_stale_digest_retired(additive, old_segment=old_segment, new_segment=new_segment)


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_branch_switch_that_deletes_a_locked_tool_retires_its_scope(shell: str, arena: Arena) -> None:
    """S-008 — a `git checkout` that rewrites `ocx.lock` retires in place, with no ocx command run."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    branch_lock = (project / "ocx.lock").read_text(encoding="utf-8")

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "before.path", "PATH"),
            # The "checkout": both watch-set members change, and no ocx command runs.
            _write_file_fragment(shell, project / "ocx.toml", "[env]\nWP14_CONST = \"branch\"\n"),
            _write_file_fragment(shell, project / "ocx.lock", branch_lock + "\n# branch\n"),
            matrix.prompt(shell),
            matrix.probe(shell, "after.path", "PATH"),
            matrix.probe(shell, "after.const", "WP14_CONST"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert str(project / "binA") in matrix.path_segments(_read(result, "before.path"))
    assert matrix.path_segments(_read(result, "after.path")).count(str(project / "binA")) == 0, (
        f"{shell}: the checked-out branch dropped the entry — it must be retired at the next prompt"
    )
    assert _read(result, "after.const") == "branch"


# ---------------------------------------------------------------------------
# Tier 2 — idempotency, growth, priors
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_five_prompts_in_one_project_leave_path_byte_identical(shell: str, arena: Arena) -> None:
    """S-039 — PATH does not grow across N prompts; the same snippet is idempotent."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    fragments = [matrix.cd_to(shell, project), matrix.prompt(shell), matrix.probe(shell, "p1", "PATH")]
    for index in range(2, 6):
        fragments += [matrix.prompt(shell), matrix.probe(shell, f"p{index}", "PATH")]

    result = _session(shell, arena, fragments)
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    values = [_read(result, f"p{index}") for index in range(1, 6)]
    assert str(project / "binA") in matrix.path_segments(values[0]), "the fixture must apply before growth can be measured"
    assert len(set(values)) == 1, (
        f"{shell}: PATH must be byte-identical across five prompts; distinct values seen:\n"
        + "\n".join(sorted(set(values)))
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_evaluating_one_snippet_twice_leaves_path_byte_identical(shell: str, arena: Arena) -> None:
    """S-039 (double-eval half) — the emitted stream itself is idempotent."""
    _skip_nushell_without_reconcile(shell, arena)
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    emitted = matrix.reconcile(arena.ocx, shell, project, arena.env(shell_abs))
    assert emitted.returncode == 0, f"{shell}: --reconcile must exit 0; stderr:\n{emitted.stderr}"
    assert emitted.stdout.strip(), f"{shell}: the fixture must emit a stream, or the double-eval proves nothing"

    body = "\n".join([matrix.probe(shell, "once", "PATH"), matrix.probe(shell, "once.list", "CFLAGS")])
    first = matrix.eval_snippet(
        shell, shell_abs, emitted.stdout, body, cwd=project, env=arena.env(shell_abs), script_dir=arena.scripts, name="once"
    )
    twice_body = "\n".join(
        [matrix.probe(shell, "twice", "PATH"), matrix.probe(shell, "twice.list", "CFLAGS")]
    )
    doubled = matrix.eval_snippet(
        shell,
        shell_abs,
        emitted.stdout + "\n" + emitted.stdout,
        twice_body,
        cwd=project,
        env=arena.env(shell_abs),
        script_dir=arena.scripts,
        name="twice",
    )
    assert first.returncode == 0 and doubled.returncode == 0, (
        f"{shell}: both evals must exit 0\nfirst:\n{first.stderr}\ndoubled:\n{doubled.stderr}"
    )
    assert _read(first, "once") == _read(doubled, "twice"), (
        f"{shell}: evaluating the snippet twice must leave PATH byte-identical"
    )
    assert _read(first, "once.list") == _read(doubled, "twice.list"), (
        f"{shell}: a list contribution must not be appended twice (append_unique)"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_constant_the_user_set_by_hand_is_restored_on_scope_exit(shell: str, arena: Arena) -> None:
    """S-002 / S-031 / C-015 — the prior is restored, never deleted, on leave."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'JAVA_HOME = "/project/jdk"\n')
    outside = arena.projects / "outside"
    outside.mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "JAVA_HOME", "/my/jdk"),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "inside", "JAVA_HOME"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "outside", "JAVA_HOME"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "inside") == "/project/jdk", f"{shell}: the project's intent wins inside the project"
    assert _read(result, "outside") == "/my/jdk", (
        f"{shell}: leaving must RESTORE the captured prior, not unset a variable the user set by hand"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_constant_overridden_after_apply_is_left_alone_on_exit(shell: str, arena: Arena) -> None:
    """C-015 rule 2 — the `C == L.applied` guard: a hand override survives the leave."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'JAVA_HOME = "/project/jdk"\n')
    outside = arena.projects / "outside"
    outside.mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            # The user overrides AFTER ocx applied: now C != L.applied.
            matrix.set_var(shell, "JAVA_HOME", "/typed/by/hand"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "outside", "JAVA_HOME"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "outside") == "/typed/by/hand", (
        f"{shell}: a value ocx did not put there must be left alone on scope exit (C == L.applied guard)"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_prior_is_recaptured_when_the_projects_intent_changes(shell: str, arena: Arena) -> None:
    """S-031 (b)+(c) / C-018 — prior re-capture, so the leave restores the hand value."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'JAVA_HOME = "/project/jdk"\n')
    outside = arena.projects / "outside"
    outside.mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "JAVA_HOME", "/my/jdk"),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            _write_file_fragment(shell, project / "ocx.toml", '[env]\nJAVA_HOME = "/project/jdk2"\n'),
            matrix.prompt(shell),
            matrix.probe(shell, "inside", "JAVA_HOME"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "outside", "JAVA_HOME"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "inside") == "/project/jdk2", "the project's new intent must win inside the project"
    assert _read(result, "outside") == "/my/jdk", (
        f"{shell}: without prior re-capture the leave would delete a variable the user set by hand"
    )


# ---------------------------------------------------------------------------
# Tier 2 — separators, hostile values, foreign PATH state
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_space_separated_list_applies_and_reverts_around_a_foreign_value(shell: str, arena: Arena) -> None:
    """S-034 / C-014 — `CFLAGS` with `separator = " "` reverts by flank-delimited removal.

    A `Some(" ")` arm that removes nothing, or one that splits on the platform
    separator, passes every other row here and fails this one: the pre-apply
    value must come back byte-identical with the foreign flags preserved.
    """
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'CFLAGS = { type = "list", separator = " ", value = "-DPROJECT_A" }\n')
    outside = arena.projects / "outside"
    outside.mkdir()
    foreign = "-O2 -Wall"

    result = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "CFLAGS", foreign),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "inside", "CFLAGS"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "outside", "CFLAGS"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    inside = _read(result, "inside")
    assert "-DPROJECT_A" in inside.split(" "), f"{shell}: the contribution must be appended; got {inside!r}"
    assert "-O2" in inside and "-Wall" in inside, f"{shell}: foreign flags must survive the apply; got {inside!r}"
    assert _read(result, "outside") == foreign, (
        f"{shell}: the revert must restore the pre-apply value byte-identically with a ' ' separator; "
        f"got {_read(result, 'outside')!r}, expected {foreign!r}"
    )


@pytest.mark.parametrize("shell", matrix.ALL_SHELLS)
def test_a_hostile_path_element_is_escaped_not_executed(shell: str, arena: Arena) -> None:
    """S-026 (tier 1) — assert on the EMITTED string; a wrong escaper is a silent wrong value.

    `escape_value` is the double-quoted-context escaper and leaves `'` untouched,
    so an arm routed through it would execute `id` at every prompt. The emitted
    stream must never contain the raw injection sequence outside a safely quoted
    context, and evaluating it must produce the element verbatim.
    """
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{_HOSTILE_SEGMENT}" }}\n')
    emitted = matrix.reconcile(arena.ocx, shell, project, arena.env())
    assert emitted.returncode == 0, f"{shell}: --reconcile must exit 0; stderr:\n{emitted.stderr}"
    assert emitted.stdout.strip(), f"{shell}: the fixture must emit a stream, or this check is vacuous"
    _assert_quote_context_holds(shell, emitted.stdout)


# The arms whose value escaper targets a DOUBLE-quoted context (`escape_value`,
# fish + nushell): there a `\'` needs no escaping at all, so the raw element is
# expected to appear contiguously. Every other arm quotes with `\'`, so a correct
# escaper necessarily breaks the raw sequence up.
_DOUBLE_QUOTED_ARMS = frozenset({"fish", "nushell"})


def _assert_quote_context_holds(shell: str, stream: str) -> None:
    """S-026 tier 1 — the arm's own escaper, asserted on the emitted string.

    Factored out so both colours are demonstrable on inputs this file controls
    (see ``test_the_quote_context_detector_reds_on_the_wrong_escaper``). Routing
    a single-quoted arm through ``escape_value`` leaves the raw `\'`-injection
    contiguous, which is exactly what makes it execute `id` at every prompt.
    """
    contiguous = _HOSTILE_SEGMENT in stream
    if shell in _DOUBLE_QUOTED_ARMS:
        assert contiguous, (
            f"{shell}: this arm escapes for a DOUBLE-quoted context, where a single quote is literal — "
            f"a broken-up value means the wrong escaper ran:\n{stream}"
        )
        return
    assert not contiguous, (
        f"{shell}: the emitted stream carries the raw quote-break {_HOSTILE_SEGMENT!r} — this arm "
        f"would close its quote and execute `id` at every prompt:\n{stream}"
    )


def test_the_quote_context_detector_reds_on_the_wrong_escaper(arena: Arena) -> None:
    """Both colours of the S-026 emitted-string detector, on synthetic streams."""
    correct_posix = "__ocx_p='/tmp/a'\\''\\'';id;'\\''\\''b'"
    wrong_posix = f"__ocx_p='{_HOSTILE_SEGMENT}'"
    _assert_quote_context_holds("bash", correct_posix)
    with pytest.raises(AssertionError, match="raw quote-break"):
        _assert_quote_context_holds("bash", wrong_posix)

    correct_fish = f'set -gx PATH "{_HOSTILE_SEGMENT}"'
    wrong_fish = "set -gx PATH '/tmp/a'\\''\\'';id;'\\''\\''b'"
    _assert_quote_context_holds("fish", correct_fish)
    with pytest.raises(AssertionError, match="wrong escaper"):
        _assert_quote_context_holds("fish", wrong_fish)


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_hostile_path_element_round_trips_through_a_real_shell(shell: str, arena: Arena) -> None:
    """S-026 (tier 2) — the value the shell ends up with is the value declared, byte for byte."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{_HOSTILE_SEGMENT}" }}\n')

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments[0] == _HOSTILE_SEGMENT, (
        f"{shell}: the hostile element must arrive verbatim; got {segments[0]!r}"
    )
    assert "uid=" not in result.stdout and "uid=" not in result.stderr, (
        f"{shell}: `id` ran — the injection succeeded:\n{result.stdout}\n{result.stderr}"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_constant_with_every_hostile_character_round_trips(shell: str, arena: Arena) -> None:
    """S-026 / C-009 — the ledger holds RAW text, so a value survives one round trip unchanged."""
    _skip_nushell_without_reconcile(shell, arena)
    toml_value = _HOSTILE_CONSTANT.replace("\\", "\\\\").replace('"', '\\"')
    project = _locked_project(arena, "alpha", f'WP14_HOSTILE = "{toml_value}"\n')

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "first", "WP14_HOSTILE"),
            matrix.prompt(shell),
            matrix.probe(shell, "second", "WP14_HOSTILE"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "first") == _HOSTILE_CONSTANT, (
        f"{shell}: got {_read(result, 'first')!r}, expected {_HOSTILE_CONSTANT!r}"
    )
    assert _read(result, "second") == _HOSTILE_CONSTANT, (
        f"{shell}: a second prompt must not double-escape a value carried through the ledger"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_a_foreign_never_tracked_ocx_bin_dir_on_path_is_left_alone(shell: str, arena: Arena) -> None:
    """A hand-written profile line, or a second install, must survive activation untouched.

    The shipped `_clean_env` deliberately excludes this state, so it needs its
    own variant: the reconciler only ever retires what its own ledger recorded.
    """
    _skip_nushell_without_reconcile(shell, arena)
    shell_abs = _require(shell)
    foreign = arena.projects / "foreign-ocx" / "bin"
    foreign.mkdir(parents=True)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join([str(foreign), env["PATH"]])

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "inside", "PATH"),
            matrix.cd_to(shell, arena.projects),
            matrix.prompt(shell),
            matrix.probe(shell, "outside", "PATH"),
        ],
        env=env,
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert str(foreign) in matrix.path_segments(_read(result, "inside")), (
        f"{shell}: a foreign ocx bin dir must survive an apply"
    )
    assert str(foreign) in matrix.path_segments(_read(result, "outside")), (
        f"{shell}: a foreign ocx bin dir must survive a revert — the reconciler retires only what it recorded"
    )


# ---------------------------------------------------------------------------
# Tier 2 — the nushell arm: what it CAN do, and the evidence for what it cannot
# ---------------------------------------------------------------------------


def test_the_nushell_arm_applies_the_global_scope_from_the_emitted_snippet(arena: Arena) -> None:
    """S-040 (the half that works today) — nushell has no string `eval`, so its
    stream goes through a file; the global toolchain still lands.

    This is the one scope nushell participates in: `ENV_NU`'s `env_change.PWD`
    hook calls `ocx --format json --global env`, and `--reconcile --shell=nushell`
    emits an evaluable stream for it. Everything project-scoped is skipped with
    the cause :func:`_skip_nushell_without_reconcile` observes.
    """
    shell_abs = _require("nushell")
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nWP14_GLOBAL = "from-global"\n', encoding="utf-8")
    locked = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        cwd=str(arena.projects),
    )
    assert locked.returncode == 0, f"global lock must succeed; stderr:\n{locked.stderr}"

    emitted = matrix.reconcile(arena.ocx, "nushell", arena.projects, arena.env(shell_abs))
    assert emitted.returncode == 0, f"--reconcile must exit 0 for nushell; stderr:\n{emitted.stderr}"
    assert emitted.stdout.strip(), "the nushell arm must emit a stream, or this proves nothing"

    result = matrix.eval_snippet(
        "nushell",
        shell_abs,
        emitted.stdout,
        matrix.probe("nushell", "global", "WP14_GLOBAL"),
        cwd=arena.projects,
        env=arena.env(shell_abs),
        script_dir=arena.scripts,
        name="nu_global",
    )
    assert result.returncode == 0, f"nushell must evaluate the emitted stream\nstderr:\n{result.stderr}"
    assert _read(result, "global") == "from-global", (
        "the global scope must apply on the nushell arm through its file-sourced stream"
    )


@pytest.mark.xfail(
    strict=True,
    reason=(
        "WP-12b unlanded: the shipped `env.nu` never names `--reconcile`. Its env_change.PWD hook "
        "calls only `ocx --format json --global env`, so nushell applies the global toolchain and "
        "can neither revert a project scope nor advance __OCX_ENV_STATE. This is the evidence every "
        "nushell project-scope skip in this module cites — when it lands, this row goes green and "
        "the strict marker fails the suite until the skips are removed."
    ),
)
def test_the_shipped_env_nu_registers_a_reconcile_call(arena: Arena) -> None:
    """C-048 / S-040 — the nushell hook must invoke the reconciler, like every other arm.

    Deliberately paired with :func:`_skip_nushell_without_reconcile`: the helper
    skips on the same observation this row asserts, so the two can never drift
    apart into "skipped for a reason nobody re-checks".
    """
    _self_setup(arena, "nu")
    env_nu = arena.ocx_home / "env.nu"
    assert env_nu.is_file(), f"self setup must write {env_nu}"
    body = env_nu.read_text(encoding="utf-8")
    assert "--global env" in body, "the fixture must be the real shipped body, not an empty file"
    assert "reconcile" in body, (
        f"{env_nu} contains 0 occurrences of 'reconcile' — nushell's inlined hook cannot reconcile"
    )


# ---------------------------------------------------------------------------
# Tier 2 — inheritance and containment
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", ("bash", "zsh", "fish", "pwsh"))
def test_a_subshell_rewrites_the_carrier_in_its_own_environment_only(shell: str, arena: Arena) -> None:
    """S-023 — the subshell inherits the carrier atomically and cannot corrupt the parent's."""
    alpha = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (alpha / "binA").mkdir()
    bravo = _locked_project(arena, "bravo", _ENV_BLOCK_B)
    (bravo / "binB").mkdir()
    shell_abs = _require(shell)

    child = arena.scripts / f"child{matrix.ARMS[shell].extension}"
    child.write_text(
        "\n".join(
            [
                matrix.header(shell, arena.ocx),
                matrix.probe(shell, "child.inherited", "WP14_CONST"),
                matrix.cd_to(shell, bravo),
                matrix.prompt(shell),
                matrix.probe(shell, "child.after", "WP14_CONST"),
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    spawn = _run_child_fragment(shell, shell, shell_abs, child)

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, alpha),
            matrix.prompt(shell),
            spawn,
            matrix.probe(shell, "parent.after", "WP14_CONST"),
            matrix.probe(shell, "parent.path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "child.inherited") == "alpha", (
        f"{shell}: the subshell must inherit the parent's applied environment"
    )
    assert _read(result, "child.after") == "bravo", f"{shell}: the subshell must be able to switch projects"
    assert _read(result, "parent.after") == "alpha", (
        f"{shell}: the subshell's carrier rewrite must not reach the parent"
    )
    assert str(alpha / "binA") in matrix.path_segments(_read(result, "parent.path"))
    assert str(bravo / "binB") not in matrix.path_segments(_read(result, "parent.path")), (
        f"{shell}: the subshell's apply must be contained"
    )


def _run_child_fragment(parent: str, child: str, child_abs: str, script: Path) -> str:
    """Spawn ``child`` running ``script``, written in ``parent``'s own syntax.

    The two shells are different languages: the invocation has to be quoted for
    the shell that reads the line, and carry the flags the shell being launched
    understands.
    """
    words = [child_abs, *matrix.ARMS[child].flags, str(script)]
    literals = " ".join(matrix.quote(parent, word) for word in words)
    if parent == "pwsh":
        return f"& {literals}"
    if parent == "elvish":
        head = matrix.quote(parent, words[0])
        tail = " ".join(matrix.quote(parent, word) for word in words[1:])
        return f"(external {head}) {tail}"
    return literals


@pytest.mark.parametrize(
    ("parent", "child"),
    [("zsh", "bash"), ("bash", "fish"), ("bash", "pwsh"), ("zsh", "pwsh")],
)
def test_a_ledger_written_by_one_shell_is_read_by_another(parent: str, child: str, arena: Arena) -> None:
    """S-024 / C-009 — Invariant L-2 end to end: the child re-escapes from raw values.

    A pre-escaped value leaking into the ledger would be double-escaped by the
    inheriting shell and correctly escaped by none — silent and per-value. The
    hostile constant is the fixture that makes that visible.
    """
    parent_abs = _require(parent)
    child_abs = _require(child)
    toml_value = _HOSTILE_CONSTANT.replace("\\", "\\\\").replace('"', '\\"')
    project = _locked_project(arena, "alpha", f'WP14_HOSTILE = "{toml_value}"\nWP14_CONST = "alpha"\n')

    child_script = arena.scripts / f"cross_child{matrix.ARMS[child].extension}"
    child_script.write_text(
        "\n".join(
            [
                matrix.header(child, arena.ocx),
                matrix.probe(child, "child.inherited", "WP14_HOSTILE"),
                matrix.prompt(child),
                matrix.probe(child, "child.reapplied", "WP14_HOSTILE"),
                matrix.probe(child, "child.const", "WP14_CONST"),
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    result = _session(
        parent,
        arena,
        [
            matrix.cd_to(parent, project),
            matrix.prompt(parent),
            _run_child_fragment(parent, child, child_abs, child_script),
        ],
        name=f"cross_{parent}_{child}",
        env=matrix.clean_env(arena.home, parent_abs, ocx_home=arena.ocx_home)
        | {"PATH": os.pathsep.join([str(Path(child_abs).parent), matrix.BASE_PATH, str(Path(parent_abs).parent)])},
    )
    assert result.returncode == 0, (
        f"{parent} -> {child}: the session must exit 0\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert _read(result, "child.inherited") == _HOSTILE_CONSTANT, (
        f"{parent} -> {child}: the applied value must cross the process boundary verbatim"
    )
    assert _read(result, "child.reapplied") == _HOSTILE_CONSTANT, (
        f"{parent} -> {child}: the child must re-escape from the ledger's RAW value, not re-escape shell text"
    )
    assert _read(result, "child.const") == "alpha"


# ---------------------------------------------------------------------------
# Tier 2 — carrier degradation and repair
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("label", "carrier"),
    [
        ("no-separator", "1"),
        ("empty-payload", "1."),
        ("no-tag", ".abc"),
        ("garbage-payload", "1.not-base64url-@@@"),
        ("unknown-tag", "x.abc"),
        ("future-tag", "2.eyJ2IjoxfQ"),
    ],
)
def test_a_degraded_carrier_is_treated_as_absent_and_never_breaks_the_prompt(
    label: str, carrier: str, arena: Arena
) -> None:
    """S-028 / C-006 — every malformed envelope degrades: exit 0, a usable stream, no panic."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    result = matrix.reconcile(arena.ocx, "bash", project, arena.env(), carrier=carrier)
    assert result.returncode == 0, (
        f"{label}: the hook path exits 0 in every state (C-051); got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert f"export {matrix.CARRIER}=" in result.stdout, (
        f"{label}: a degraded carrier must be REPLACED with a fresh one, not left corrupt:\n{result.stdout}"
    )
    assert "panicked" not in result.stderr, f"{label}: the reconciler must never panic\n{result.stderr}"


def test_a_truncated_but_well_formed_carrier_is_treated_as_absent(arena: Arena) -> None:
    """S-028 — truncation of a genuinely valid payload, not a hand-typed string."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    first = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", first.stdout)
    assert match, f"the fixture must emit a carrier to truncate:\n{first.stdout}"
    valid = match.group(1)
    truncated = valid[: len(valid) // 2]
    assert truncated != valid and len(truncated) > 2, "the truncation must actually remove bytes"

    result = matrix.reconcile(arena.ocx, "bash", project, arena.env(), carrier=truncated)
    assert result.returncode == 0, f"a truncated carrier must not break the prompt; stderr:\n{result.stderr}"
    assert f"export {matrix.CARRIER}=" in result.stdout
    state = matrix.shell_state(arena.ocx, project, arena.env() | {matrix.CARRIER: truncated})
    assert state["carrier_present"] is True
    assert state["inert_reason"] == {"reason": "ledger_unreadable", "first_prompt": False}, (
        "a corrupt carrier and an absent one are DIFFERENT reasons (C-006); got "
        f"{state['inert_reason']}"
    )


@pytest.mark.parametrize("shell", ("bash", "zsh", "fish", "pwsh"))
def test_unsetting_the_carrier_repairs_lists_and_leaves_constants_in_place(shell: str, arena: Arena) -> None:
    """S-021 / C-012 / C-006 — the repair gesture, inside the project where S-021 places it.

    With the ledger gone, `D` is rebuilt from truth and PATH is repaired
    **subtractively against the owned prefix** — every `$OCX_HOME`-owned segment
    `D` no longer wants is removed even though nothing records it. Constants are
    **left in place**, never guess-unset, because `priors` went with the ledger.
    A segment outside the owned prefix is left alone: the repair is scoped, and a
    repair that reached outside it would eat a user's own PATH.
    """
    owned_old = arena.ocx_home / "packages" / "old" / "bin"
    owned_new = arena.ocx_home / "packages" / "new" / "bin"
    owned_old.mkdir(parents=True)
    owned_new.mkdir(parents=True)
    foreign = arena.projects / "foreign" / "bin"
    foreign.mkdir(parents=True)
    project = _locked_project(
        arena,
        "alpha",
        f'WP14_CONST = "alpha"\nPATH = {{ type = "path", value = "{owned_old}" }}\n',
    )
    shell_abs = _require(shell)
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join([str(foreign), env["PATH"]])

    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "applied.path", "PATH"),
            matrix.unset_var(shell, matrix.CARRIER),
            matrix.probe(shell, "unset", matrix.CARRIER),
            _write_file_fragment(
                shell,
                project / "ocx.toml",
                f'[env]\nWP14_CONST = "alpha"\nPATH = {{ type = "path", value = "{owned_new}" }}\n',
            ),
            matrix.prompt(shell),
            matrix.probe(shell, "after.path", "PATH"),
            matrix.probe(shell, "after.const", "WP14_CONST"),
            matrix.probe(shell, "after.carrier", matrix.CARRIER),
        ],
        env=env,
    )
    assert result.returncode == 0, f"{shell}: the session must exit 0\nstderr:\n{result.stderr}"
    assert str(owned_old) in matrix.path_segments(_read(result, "applied.path")), (
        "the owned element must be applied first, or the repair proves nothing"
    )
    assert _read(result, "unset") == matrix.ABSENT, "the gesture must actually unset the carrier"
    after = matrix.path_segments(_read(result, "after.path"))
    assert after.count(str(owned_old)) == 0, (
        f"{shell}: an OCX_HOME-owned segment D no longer wants must be repaired away subtractively "
        f"with no ledger to name it; PATH was {after}"
    )
    assert str(owned_new) in after, f"{shell}: the newly declared segment must be applied; PATH was {after}"
    assert str(foreign) in after, (
        f"{shell}: the repair is scoped to owned prefixes — a foreign segment must be left alone"
    )
    assert _read(result, "after.const") == "alpha", (
        f"{shell}: a constant must be LEFT IN PLACE after the repair — priors are gone, so it is never guess-unset"
    )
    assert _read(result, "after.carrier").startswith("1."), f"{shell}: the next prompt must rebuild a carrier"


def test_shell_state_distinguishes_an_absent_carrier_from_a_corrupt_one(arena: Arena) -> None:
    """S-022 reason 6 / C-006 — `first_prompt` is the distinction, and it is observable."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)

    absent = matrix.shell_state(arena.ocx, project, arena.env())
    assert absent["carrier_present"] is False
    assert absent["inert_reason"] == {"reason": "ledger_unreadable", "first_prompt": True}

    corrupt = matrix.shell_state(arena.ocx, project, arena.env() | {matrix.CARRIER: "1.@@@not-a-payload"})
    assert corrupt["carrier_present"] is True
    assert corrupt["inert_reason"] == {"reason": "ledger_unreadable", "first_prompt": False}


def test_an_over_cap_ledger_still_carries_a_decodable_marker(arena: Arena) -> None:
    """S-027 / C-004 / A-01 — over cap keeps `v`, `fp`, `verdict`, `over_cap`; both payloads drop.

    Driven by a project whose `[env]` alone exceeds the 16 KiB carrier cap, so
    the ledger has to abandon the project scope rather than omit the variable —
    omission would lose `fp` with it and every later prompt would recompose,
    re-overflow and re-report.
    """
    padding = "x" * 900
    block = "".join(f'WP14_BIG_{index:02d} = "{padding}"\n' for index in range(24))
    project = _locked_project(arena, "big", block)

    emitted = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert emitted.returncode == 0, f"an over-cap project must not break the prompt; stderr:\n{emitted.stderr}"
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", emitted.stdout)
    assert match, (
        "the carrier must still be SET, to a marker-only ledger — omitting it loses `fp` too:\n"
        f"{emitted.stdout}"
    )
    carrier = match.group(1)
    assert len(carrier) <= 16384, f"the marker must fit the cap; got {len(carrier)} bytes"

    state = matrix.shell_state(arena.ocx, project, arena.env() | {matrix.CARRIER: carrier})
    assert state["inert_reason"]["reason"] == "ledger_over_cap", (
        f"the over-cap state must be read FROM THE MARKER, never inferred from an absent carrier; got "
        f"{state['inert_reason']}"
    )
    ledger = state["ledger"]
    assert ledger is not None and ledger["fp"], "the marker must retain the fingerprint"
    # One rule, not a ladder (C-004): BOTH payloads drop, so both scopes the
    # ledger carried are named — never a partial payload.
    assert "project" in ledger["over_cap"], f"the abandoned project scope must be named; got {ledger['over_cap']}"
    assert not ledger["scopes"].get("project"), "the abandoned scope's payload must be dropped whole"
    assert "too large to record" in emitted.stdout, (
        f"one summary line must name the abandoned scope:\n{emitted.stdout}"
    )


# ---------------------------------------------------------------------------
# Tier 2 — the probe guard
# ---------------------------------------------------------------------------


def test_a_binary_removed_mid_session_makes_the_hook_a_silent_no_op(arena: Arena) -> None:
    """S-029 — nothing on stdout, nothing on stderr, exit 0, and the prompt still renders."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    copied = arena.scripts / "ocx-copy"
    shutil.copy2(arena.ocx, copied)
    copied.chmod(0o755)

    body = "\n".join(
        [
            f"__ocx_exe={matrix.quote('bash', str(copied))}",
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "applied", "WP14_CONST"),
            f"rm -f {matrix.quote('bash', str(copied))}",
            # The emitted hook body's own guard shape: no binary, no exec, no noise.
            'if [ -x "$__ocx_exe" ]; then ' + matrix.prompt("bash").replace("\n", " ") + "; fi",
            matrix.probe("bash", "after", "WP14_CONST"),
            "echo READY",
        ]
    )
    result = matrix.run_script(
        "bash", shell_abs, body, cwd=project, env=arena.env(shell_abs), script_dir=arena.scripts, name="probe_guard"
    )
    assert result.returncode == 0, f"a removed binary must not break the shell; stderr:\n{result.stderr}"
    assert _read(result, "applied") == "alpha"
    assert _read(result, "after") == "alpha", "the environment must be left exactly as it was"
    assert "READY" in result.stdout, "the prompt must keep rendering after the binary disappears"
    assert "No such file" not in result.stderr, f"the guard must be silent; stderr:\n{result.stderr}"


def test_a_binary_that_rejects_reconcile_emits_nothing_on_either_stream(arena: Arena) -> None:
    """S-030 — a rollback to a pre-hook ocx must be invisible, not one usage error per prompt.

    The stand-in is a script that behaves like a binary with no `--reconcile`
    flag: clap's unknown-argument error on stderr and exit 64. The emitted hook
    body discards that stderr and ignores the status, so the shell sees nothing.
    """
    shell_abs = _require("bash")
    old_ocx = arena.scripts / "old-ocx"
    old_ocx.write_text(
        "#!/bin/sh\n"
        "echo \"error: unexpected argument '--reconcile' found\" >&2\n"
        "exit 64\n",
        encoding="utf-8",
    )
    old_ocx.chmod(0o755)

    body = "\n".join(
        [
            f"__ocx_exe={matrix.quote('bash', str(old_ocx))}",
            # Exactly the shape the emitted body uses: stderr discarded, status ignored.
            'eval "$("$__ocx_exe" --offline self activate --reconcile --shell=bash 2>/dev/null || true)"',
            "echo READY",
        ]
    )
    result = matrix.run_script(
        "bash", shell_abs, body, cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="rollback"
    )
    assert result.returncode == 0, f"a rollback must not break the prompt; rc={result.returncode}"
    assert result.stdout.strip() == "READY", f"nothing may reach stdout:\n{result.stdout!r}"
    assert result.stderr.strip() == "", f"nothing may reach stderr:\n{result.stderr!r}"


# ---------------------------------------------------------------------------
# Tier 2 — `set -u` safety
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", ("sh", "dash", "ash", "bash", "zsh"))
def test_the_first_prompt_is_safe_under_set_u(shell: str, arena: Arena) -> None:
    """S-025 / C-046 — every ledger read uses default expansion; the carrier is unset by construction."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    result = _session(
        shell,
        arena,
        [
            matrix.strict_mode(shell),
            matrix.probe(shell, "carrier", matrix.CARRIER),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.probe(shell, "const", "WP14_CONST"),
            matrix.prompt(shell),
            matrix.probe(shell, "const2", "WP14_CONST"),
            "echo READY",
        ],
    )
    assert result.returncode == 0, (
        f"{shell}: `set -u` must not turn the first prompt into an unbound-variable error\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert _read(result, "carrier") == matrix.ABSENT, "the carrier must be unset for this to be the first-prompt case"
    assert "unbound variable" not in result.stderr and "parameter not set" not in result.stderr, (
        f"{shell}: stderr carries an unbound-variable diagnostic:\n{result.stderr}"
    )
    assert _read(result, "const") == "alpha"
    assert "READY" in result.stdout


# ---------------------------------------------------------------------------
# Tier 2 — consent
# ---------------------------------------------------------------------------


def test_a_fresh_clone_is_inert(arena: Arena) -> None:
    """S-011 — a clone carrying ocx.toml + ocx.lock and no stamp changes nothing."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (source / "binA").mkdir()
    clone = _clone_of(source, arena.projects / "clone")

    state = matrix.shell_state(arena.ocx, clone, arena.env())
    assert state["project_stamped"] is False, "the clone must carry no stamp, or the row proves nothing"
    assert state["inert_reason"]["reason"] == "no_stamp_no_grant", (
        f"a fresh clone must be inert; got {state['inert_reason']}"
    )

    emitted = matrix.reconcile(arena.ocx, "bash", clone, arena.env())
    assert emitted.returncode == 0
    assert "export WP14_CONST=" not in emitted.stdout, (
        f"an inert project must apply ZERO env change:\n{emitted.stdout}"
    )
    assert "is not activated" in emitted.stdout, (
        f"exactly one hint line must be emitted by the FIRST --reconcile run:\n{emitted.stdout}"
    )
    assert not matrix.stamp_dir(arena.ocx_home, state["project_key"]).exists(), (
        "the activation path must never write a stamp (A-26/A-29)"
    )


def test_a_project_with_no_lock_at_all_is_inert(arena: Arena) -> None:
    """S-011 vacuity edge / C-025 — an EMPTY source set never satisfies clause 2.

    The clone that carries `[env] PATH = { type = "path", value = "bin" }` and no
    lock is precisely the project this decision exists to stop.
    """
    project = arena.projects / "nolock"
    matrix.write_project(project, 'PATH = { type = "path", value = "bin" }\n')
    (project / "bin").mkdir()

    state = matrix.shell_state(arena.ocx, project, arena.env())
    assert state["inert_reason"] == {"reason": "lock_unavailable"}
    emitted = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert str(project / "bin") not in emitted.stdout, (
        f"a lockless clone must not put its own bin dir PATH-front:\n{emitted.stdout}"
    )


def test_an_unreadable_lock_is_inert_exactly_like_an_absent_one(arena: Arena) -> None:
    """S-011 edge — absent, unreadable and unparseable share one outcome."""
    if os.geteuid() == 0:
        pytest.skip("running as root: chmod 000 does not make a file unreadable, so the cause cannot be observed")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    lock = project / "ocx.lock"
    lock.chmod(0o000)
    try:
        with pytest.raises(PermissionError):
            lock.read_text(encoding="utf-8")
        state = matrix.shell_state(arena.ocx, project, arena.env())
    finally:
        lock.chmod(0o644)
    assert state["inert_reason"] == {"reason": "lock_unavailable"}


def test_an_unparseable_lock_is_inert_exactly_like_an_absent_one(arena: Arena) -> None:
    """S-011 edge — the third member of the one-outcome trio."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "ocx.lock").write_text("this is not TOML {[", encoding="utf-8")
    state = matrix.shell_state(arena.ocx, project, arena.env())
    assert state["inert_reason"] == {"reason": "lock_unavailable"}


def test_a_paths_granted_project_activates_and_writes_no_stamp(arena: Arena) -> None:
    """S-012 / C-027 / A-26 — both arms: `Activate` AND `state/projects/<key>/` absent."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (source / "binA").mkdir()
    clone = _clone_of(source, arena.projects / "granted")
    (clone / "binA").mkdir(exist_ok=True)

    key = matrix.shell_state(arena.ocx, clone, arena.env())["project_key"]
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), "the clone must start unstamped"

    granted = arena.env(OCX_CONSENT_PATHS=str(clone))
    state = matrix.shell_state(arena.ocx, clone, granted)
    assert state["inert_reason"]["reason"] != "no_stamp_no_grant", (
        f"a paths grant must activate; got {state['inert_reason']}"
    )
    emitted = matrix.reconcile(arena.ocx, "bash", clone, granted)
    assert "export WP14_CONST='alpha'" in emitted.stdout, f"the grant must actually apply:\n{emitted.stdout}"
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), (
        "A-26: a grant writes NO stamp — `state/projects/<key>/` must stay absent before, during and after"
    )

    # Revoking is immediately effective precisely because no stamp was derived.
    revoked = matrix.shell_state(arena.ocx, clone, arena.env())
    assert revoked["inert_reason"]["reason"] == "no_stamp_no_grant", (
        f"revoking a paths grant must be effective at the very next prompt; got {revoked['inert_reason']}"
    )


def test_a_paths_grant_does_not_match_a_sibling_with_a_longer_name(arena: Arena) -> None:
    """S-043 — exact directory, no prefix, no glob: `/p` must not grant `/p-evil`."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    victim = _clone_of(source, arena.projects / "project")
    evil = _clone_of(source, arena.projects / "project-evil")

    granted = arena.env(OCX_CONSENT_PATHS=str(victim))
    assert matrix.shell_state(arena.ocx, victim, granted)["inert_reason"]["reason"] != "no_stamp_no_grant"
    assert matrix.shell_state(arena.ocx, evil, granted)["inert_reason"]["reason"] == "no_stamp_no_grant", (
        "a `paths` entry must not match a sibling whose name merely starts with it"
    )


def test_a_namespaces_grant_goes_inert_when_a_source_leaves_it(arena: Arena) -> None:
    """S-012 edge / C-025 clause 2 — the quantifier re-runs every prompt, with no stamp."""
    project = arena.projects / "ns"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nhello = "ghcr.io/acme/hello:1"\n')
    matrix.write_lock(project, matrix.lock_tool("hello", "ghcr.io/acme/hello"))

    granted = arena.env(OCX_CONSENT_NAMESPACES="ghcr.io/acme")
    inside = matrix.shell_state(arena.ocx, project, granted)
    assert inside["inert_reason"]["reason"] != "no_stamp_no_grant", (
        f"the grant must cover the whole source set; got {inside['inert_reason']}"
    )
    key = inside["project_key"]
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), "a namespaces grant writes no stamp either"

    # A second source arrives that the grant does not cover.
    matrix.write_lock(
        project,
        matrix.lock_tool("hello", "ghcr.io/acme/hello") + "\n" + matrix.lock_tool("evil", "ghcr.io/evil/tool"),
    )
    after = matrix.shell_state(arena.ocx, project, granted)
    assert after["inert_reason"]["reason"] == "no_stamp_no_grant", (
        f"one source leaving the grant must make the project inert at the next prompt; got {after['inert_reason']}"
    )
    assert "ghcr.io/evil" in after["inert_reason"]["derived_sources"]


def test_a_same_cardinality_source_swap_re_prompts_a_stamped_project(arena: Arena) -> None:
    """S-013 — `ghcr.io/acme → ghcr.io/evil` is drift even though the set size is unchanged."""
    project = arena.projects / "stamped"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nhello = "ghcr.io/acme/hello:1"\n')
    matrix.write_lock(project, matrix.lock_tool("hello", "ghcr.io/acme/hello"))

    key = matrix.shell_state(arena.ocx, project, arena.env())["project_key"]
    stamp = matrix.stamp_dir(arena.ocx_home, key)
    stamp.mkdir(parents=True)
    (stamp / "consent.json").write_text(
        json.dumps(
            {
                "v": 1,
                "project_dir": str(project.resolve()),
                "sources": ["ghcr.io/acme"],
                "stamped_at": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )
    before = matrix.shell_state(arena.ocx, project, arena.env())
    assert before["project_stamped"] is True, "the hand-written stamp must be accepted, or the drift row is vacuous"
    assert before["inert_reason"]["reason"] != "source_set_drift"

    matrix.write_lock(project, matrix.lock_tool("hello", "ghcr.io/evil/tool"))
    after = matrix.shell_state(arena.ocx, project, arena.env())
    assert after["inert_reason"] == {"reason": "source_set_drift", "new_sources": ["ghcr.io/evil"]}, (
        f"the reason must NAME the source that is new; got {after['inert_reason']}"
    )


def test_growth_inside_an_already_stamped_source_does_not_re_prompt(arena: Arena) -> None:
    """S-013 — ordinary growth INSIDE a stamped source is a subset, so it is not drift."""
    project = arena.projects / "grow"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nhello = "ghcr.io/acme/hello:1"\n')
    matrix.write_lock(project, matrix.lock_tool("hello", "ghcr.io/acme/hello"))

    key = matrix.shell_state(arena.ocx, project, arena.env())["project_key"]
    stamp = matrix.stamp_dir(arena.ocx_home, key)
    stamp.mkdir(parents=True)
    (stamp / "consent.json").write_text(
        json.dumps(
            {
                "v": 1,
                "project_dir": str(project.resolve()),
                "sources": ["ghcr.io/acme"],
                "stamped_at": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )
    matrix.write_lock(
        project,
        matrix.lock_tool("hello", "ghcr.io/acme/hello") + "\n" + matrix.lock_tool("other", "ghcr.io/acme/other"),
    )
    state = matrix.shell_state(arena.ocx, project, arena.env())
    assert state["inert_reason"]["reason"] != "source_set_drift", (
        f"a new repository inside a stamped source is a subset, not drift; got {state['inert_reason']}"
    )


def test_a_port_bearing_registry_is_a_distinct_consent_source(arena: Arena) -> None:
    """S-013 edge / C-026 — `localhost:5000` and `localhost` must never be the same source."""
    project = arena.projects / "ported"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nhello = "localhost:5000/acme/hello:1"\n')
    matrix.write_lock(project, matrix.lock_tool("hello", "localhost:5000/acme/hello"))

    derived = matrix.shell_state(arena.ocx, project, arena.env())["inert_reason"]["derived_sources"]
    assert derived == ["localhost:5000/acme"], f"the port must be preserved in the source; got {derived}"

    portless = matrix.shell_state(arena.ocx, project, arena.env(OCX_CONSENT_NAMESPACES="localhost/acme"))
    assert portless["inert_reason"]["reason"] == "no_stamp_no_grant", (
        "a portless grant must not cover a ported registry"
    )
    ported = matrix.shell_state(arena.ocx, project, arena.env(OCX_CONSENT_NAMESPACES="localhost:5000/acme"))
    assert ported["inert_reason"]["reason"] != "no_stamp_no_grant"


@pytest.mark.parametrize(
    "value",
    ["ghcr.io/acme/*,", "ghcr.io/acme,,other.io/team", ",", ""],
)
def test_empty_tokens_in_the_namespace_env_channel_grant_nothing(value: str, arena: Arena) -> None:
    """S-037 — an empty token must never become a pattern that matches an untrusted source.

    Asserted against `ghcr.io/evil/tool`, which no listed pattern covers. A parser
    that keeps empty tokens makes this source start matching — the named fault
    injection for this row.
    """
    project = arena.projects / "evil"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nevil = "ghcr.io/evil/tool:1"\n')
    matrix.write_lock(project, matrix.lock_tool("evil", "ghcr.io/evil/tool"))

    state = matrix.shell_state(arena.ocx, project, arena.env(OCX_CONSENT_NAMESPACES=value))
    assert state["inert_reason"]["reason"] == "no_stamp_no_grant", (
        f"OCX_CONSENT_NAMESPACES={value!r} must grant nothing to ghcr.io/evil; got {state['inert_reason']}"
    )
    tested = state["inert_reason"]["namespaces_tested"]
    assert "" not in tested, f"an empty token reached the pattern set: {tested}"
    assert all(pattern for pattern in tested), f"an empty pattern reached the matcher: {tested}"


def test_an_empty_token_in_the_path_env_channel_grants_nothing(arena: Arena) -> None:
    """S-037 (paths half) — an empty token must not become a PathBuf that matches any project."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    clone = _clone_of(project, arena.projects / "clone")

    state = matrix.shell_state(arena.ocx, clone, arena.env(OCX_CONSENT_PATHS=os.pathsep))
    assert state["inert_reason"]["reason"] == "no_stamp_no_grant", (
        f"an OS-separator-only OCX_CONSENT_PATHS must grant nothing; got {state['inert_reason']}"
    )
    assert all(str(path) for path in state["inert_reason"]["paths_tested"]), (
        f"an empty path reached the comparison: {state['inert_reason']['paths_tested']}"
    )


def test_a_grant_arriving_mid_session_activates_at_the_next_prompt(arena: Arena) -> None:
    """S-011 edge / C-042 — the negative verdict is cached, and the cache is expirable."""
    shell_abs = _require("bash")
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (source / "binA").mkdir()
    clone = _clone_of(source, arena.projects / "clone")
    (clone / "binA").mkdir(exist_ok=True)

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", clone),
            matrix.prompt("bash"),
            matrix.probe("bash", "inert", "WP14_CONST"),
            matrix.set_var("bash", "OCX_CONSENT_PATHS", str(clone)),
            matrix.prompt("bash"),
            matrix.probe("bash", "granted", "WP14_CONST"),
        ],
        env=arena.env(shell_abs),
    )
    assert result.returncode == 0, f"the session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "inert") == matrix.ABSENT, "the clone must start inert"
    assert _read(result, "granted") == "alpha", (
        "exporting OCX_CONSENT_PATHS must activate at the NEXT PROMPT, not at the next shell start — "
        "the raw values are folded into the fingerprint (C-019)"
    )


@pytest.mark.parametrize("command", (["env"], ["inspect"], ["shell", "state"], ["self", "activate", "--reconcile", "--shell=bash"]))
def test_a_read_only_command_never_writes_a_consent_stamp(command: list[str], arena: Arena) -> None:
    """A-29 / S-012 — the stamp-writer allowlist is closed, and these four are not on it."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    clone = _clone_of(source, arena.projects / "clone")
    key = matrix.shell_state(arena.ocx, clone, arena.env())["project_key"]
    stamp = matrix.stamp_dir(arena.ocx_home, key)
    assert not stamp.exists(), "the clone must start unstamped"

    granted = arena.env(OCX_CONSENT_PATHS=str(clone))
    subprocess.run(
        [str(arena.ocx), "--offline", *command],
        cwd=str(clone),
        capture_output=True,
        check=False,
        text=True,
        env=granted,
    )
    assert not stamp.exists(), (
        f"`ocx {' '.join(command)}` must not create {stamp} — it is a named non-member of the six-writer allowlist"
    )


def test_ocx_lock_does_write_a_consent_stamp(arena: Arena) -> None:
    """A-29 — the positive half: without it, the negative tests above pass vacuously."""
    project = arena.projects / "stamps"
    matrix.write_project(project, _ENV_BLOCK_A)
    key_env = arena.env()
    matrix.run_lock(arena.ocx, project, key_env)
    key = matrix.shell_state(arena.ocx, project, key_env)["project_key"]
    assert matrix.stamp_dir(arena.ocx_home, key).joinpath("consent.json").is_file(), (
        "`ocx lock` is on the six-writer allowlist and MUST stamp — otherwise every negative stamp "
        "assertion in this module is unfalsifiable"
    )


# ---------------------------------------------------------------------------
# Tier 2 — `[shell]` config, enablement rungs, managed strip
# ---------------------------------------------------------------------------


def _write_config(arena: Arena, body: str) -> Path:
    config = arena.ocx_home / "config.toml"
    config.write_text(body, encoding="utf-8")
    return config


def test_shell_hook_false_in_a_config_tier_disables_the_hook(arena: Arena) -> None:
    """S-016 rung 4 — `[shell] hook = false`, with the deciding tier named."""
    _write_config(arena, "[shell]\nhook = false\n")
    state = matrix.shell_state(arena.ocx, arena.projects, arena.env())
    assert state["inert_reason"]["reason"] == "hook_disabled"
    assert state["inert_reason"]["rung"] == "[shell] hook", f"got {state['inert_reason']}"
    assert state["inert_reason"]["tier"], "the deciding tier must be named, never hard-coded"


def test_ocx_no_hook_disables_the_hook_and_names_its_own_rung(arena: Arena) -> None:
    """S-015 rung 3 — `OCX_NO_HOOK` beats the config tier and says so."""
    _write_config(arena, "[shell]\nhook = true\n")
    state = matrix.shell_state(arena.ocx, arena.projects, arena.env(OCX_NO_HOOK="1"))
    assert state["inert_reason"] == {"reason": "hook_disabled", "rung": "OCX_NO_HOOK", "tier": None}


def test_a_non_boolean_ocx_no_hook_warns_and_falls_back(arena: Arena) -> None:
    """S-015 edge — `OCX_NO_HOOK=maybe` is not truthy and not an error (BooleanString)."""
    state = matrix.shell_state(arena.ocx, arena.projects, arena.env(OCX_NO_HOOK="maybe"))
    assert state["hook"]["rung"] != "OCX_NO_HOOK" or state["hook"]["enabled"] is not False, (
        f"an unrecognised value must fall back to the default, not disable the hook; got {state['hook']}"
    )


def test_no_hook_suppresses_the_hook_in_the_startup_stream(arena: Arena) -> None:
    """S-014 rung 1 — `--no-hook` emits no hook, and PATH/completions are unaffected."""
    with_hook = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=bash", "--hook", "--no-completion"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
    )
    without = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=bash", "--no-hook", "--no-completion"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
    )
    assert with_hook.returncode == 0 and without.returncode == 0
    assert "--reconcile" in with_hook.stdout, (
        f"`--hook` must emit a body that calls --reconcile:\n{with_hook.stdout}"
    )
    assert "--reconcile" not in without.stdout, (
        f"`--no-hook` must emit no hook at all:\n{without.stdout}"
    )
    bin_dir = str(arena.ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin")
    assert bin_dir in without.stdout, "the PATH prepend must be unaffected by the hook rung"


def test_a_project_shell_section_is_refused_at_parse(arena: Arena) -> None:
    """S-035 (a) / C-033 — `[shell]` can never come from an `ocx.toml`."""
    project = arena.projects / "shellish"
    project.mkdir()
    (project / "ocx.toml").write_text("[shell]\nhook = true\n", encoding="utf-8")
    matrix.write_lock(project)
    result = subprocess.run(
        [str(arena.ocx), "--offline", "status"],
        cwd=str(project),
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
    )
    assert result.returncode == 78, (
        f"a `[shell]` section in ocx.toml must be a hard parse error (exit 78); got {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "config.toml" in (result.stdout + result.stderr), (
        "the refusal must name config.toml so the user knows where the section belongs"
    )


def _acme_project(arena: Arena, name: str) -> Path:
    """A project whose only source is ``ocx.sh/acme`` — the org both grants below name.

    Unstamped on purpose: ``namespaces`` is the rung that consents without a
    stamp, so an inert verdict here is the grant's verdict and nothing else's.
    """
    project = arena.projects / name
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nt = "ocx.sh/acme/tool:1"\n')
    matrix.write_lock(project, matrix.lock_tool("t", "ocx.sh/acme/tool"))
    return project


def _assert_grant_voided_and_file_survived(arena: Arena, project: Path) -> None:
    """The two halves a refused ``[shell.consent]`` owes, neither sufficient alone.

    ``matrix.shell_state`` raises unless the command exits 0, so reaching the
    assertions is itself the "the tier still loads" half — before ocx-sh/ocx#344
    both payloads below exited 78 and every ``ocx`` invocation on the host died
    on a file that is otherwise fine.

    * **The grant is gone.** ``no_stamp_no_grant`` with an empty
      ``namespaces_tested``: the refused table granted nothing and was not even
      consulted. Fail closed.
    * **The file still applies.** ``[shell] hook`` is the sibling key in the very
      same table, and it still decides the hook and names the tier that set it.
      Without this, a loader that ignored the whole file would satisfy the first
      half perfectly. The fleet sections the strip really protects
      (``[registries]``, ``[mirrors]``, ``[[trust.policy]]``) have no offline
      read-only surface here; they are pinned one layer down, on
      ``ConfigLoader::parse_config_stripping_refused_consent``.
    """
    state = matrix.shell_state(arena.ocx, project, arena.env())
    inert = state["inert_reason"]
    assert inert["reason"] == "no_stamp_no_grant", (
        f"a refused consent table must grant NOTHING; got {inert}"
    )
    assert inert["namespaces_tested"] == [], (
        f"the refused table must not survive as a spec that gets tested; got {inert}"
    )
    hook = state["hook"]
    assert hook["rung"] == "[shell] hook" and hook["enabled"] is True, (
        f"only the `consent` key is dropped — `[shell] hook` in the same file must still decide; got {hook}"
    )
    assert hook["tier"], f"the surviving tier must be named, never hard-coded; got {hook}"


def test_a_grammatically_invalid_namespaces_pattern_voids_the_grant_not_the_file(arena: Arena) -> None:
    """S-043 / ocx-sh/ocx#344 — a three-component pattern names a repository, not a source.

    ``ConsentScopeSpec`` refuses it. That refusal used to fail the whole config
    tier, which on a fleet-read ``config.toml`` dropped ``[registries]``,
    ``[mirrors]`` and ``[[trust.policy]]`` with it — a commit whose subject is a
    *narrowing* silently widening the effective posture on every host. The
    loader now re-parses with ``[shell.consent]`` removed and keeps the rest.

    The control arm is what makes the inert verdict mean something: spelled at
    source granularity the same pattern grants this same project, so "gone" is
    distinguishable from "never possible".
    """
    project = _acme_project(arena, "invalid-pattern")
    _write_config(arena, '[shell]\nhook = true\n[shell.consent]\nnamespaces = "ocx.sh/acme/team"\n')
    _assert_grant_voided_and_file_survived(arena, project)

    _write_config(arena, '[shell]\nhook = true\n[shell.consent]\nnamespaces = "ocx.sh/acme"\n')
    assert matrix.shell_state(arena.ocx, project, arena.env())["inert_reason"]["reason"] != "no_stamp_no_grant", (
        "control: at source granularity the same pattern must grant — otherwise the arm above proves nothing"
    )


def test_an_unknown_key_inside_the_consent_table_voids_the_grant_it_appears_in(arena: Arena) -> None:
    """C-029 carve-out — an unknown NARROWING key refuses the table rather than widening it.

    ``future_narrowing`` sits beside an ``include`` that on its own grants this
    project, so reading the table without the key ocx does not understand would
    *widen* trust — the one direction fleet forward-compat must not take, and
    why ``ShellConsent``/``ConsentScopeSpec`` carry ``deny_unknown_fields``. The
    refusal costs the grant; since ocx-sh/ocx#344 it no longer costs the file.

    The ``include`` is the discriminator: a loader that merely *ignored* the
    unknown key would grant here, so the inert verdict cannot be satisfied by a
    spec that never matched anything. The control arm removes the key alone and
    shows that same include granting.
    """
    project = _acme_project(arena, "unknown-key")
    _write_config(
        arena,
        '[shell]\nhook = true\n[shell.consent]\n'
        'namespaces = { include = ["ocx.sh/acme"], future_narrowing = ["x"] }\n',
    )
    _assert_grant_voided_and_file_survived(arena, project)

    _write_config(arena, '[shell]\nhook = true\n[shell.consent]\nnamespaces = { include = ["ocx.sh/acme"] }\n')
    assert matrix.shell_state(arena.ocx, project, arena.env())["inert_reason"]["reason"] != "no_stamp_no_grant", (
        "control: the include alone must grant — otherwise the refusal arm above proves nothing"
    )


def test_a_carve_out_beats_coverage_at_source_granularity(arena: Arena) -> None:
    """S-043 — an ``exclude`` subtracts an org another tier (or the same one) included.

    The include is spelled org by org: there is no whole-registry pattern to
    carve out of (ocx-sh/ocx#344 — that spelling voided the grant's only bound,
    publisher identity, on any host where anyone can register).
    """
    _write_config(
        arena,
        '[shell.consent]\nnamespaces = { include = ["ocx.sh/acme", "ocx.sh/acme-compromised"], '
        'exclude = ["ocx.sh/acme-compromised"] }\n',
    )
    good = arena.projects / "good"
    matrix.write_project(good, _ENV_BLOCK_A, tools_block='[tools]\nt = "ocx.sh/acme/tool:1"\n')
    matrix.write_lock(good, matrix.lock_tool("t", "ocx.sh/acme/tool"))
    bad = arena.projects / "bad"
    matrix.write_project(bad, _ENV_BLOCK_A, tools_block='[tools]\nt = "ocx.sh/acme-compromised/tool:1"\n')
    matrix.write_lock(bad, matrix.lock_tool("t", "ocx.sh/acme-compromised/tool"))

    assert matrix.shell_state(arena.ocx, good, arena.env())["inert_reason"]["reason"] != "no_stamp_no_grant"
    assert matrix.shell_state(arena.ocx, bad, arena.env())["inert_reason"]["reason"] == "no_stamp_no_grant", (
        "an exclude must subtract an org the same spec included"
    )


# ---------------------------------------------------------------------------
# Tier 2 — coexistence with direnv / mise
# ---------------------------------------------------------------------------


def test_direnv_live_for_this_directory_yields_the_project_scope(arena: Arena) -> None:
    """S-017 / C-049 — `DIRENV_DIR` naming this project: global only, one info line."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    # The format `shell::coexistence::detect` compares against: a bare path.
    env = arena.env(DIRENV_DIR=str(project.resolve()))

    emitted = matrix.reconcile(arena.ocx, "bash", project, env)
    assert emitted.returncode == 0
    assert "export WP14_CONST=" not in emitted.stdout, (
        f"the project scope must be yielded, not applied:\n{emitted.stdout}"
    )
    assert "direnv manages this directory" in emitted.stdout, (
        f"one info line must name direnv and the live signal:\n{emitted.stdout}"
    )
    state = matrix.shell_state(arena.ocx, project, env)
    assert state["inert_reason"]["reason"] == "yielded_to"
    assert state["yielded_to"], "the yield must be reported with its evidence"


def test_direnv_yields_for_the_dash_prefixed_dir_real_direnv_exports(arena: Arena) -> None:
    """S-017 against the value real direnv actually sets, not the one the code assumes.

    `direnv version 2.35.0` exports ``DIRENV_DIR=-/abs/path``. Every other test
    in this section feeds the bare path, which is the only spelling
    ``coexistence::detect`` recognises — so without this row the yield looks
    covered while never firing in the field.
    """
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(DIRENV_DIR="-" + str(project.resolve()))

    emitted = matrix.reconcile(arena.ocx, "bash", project, env)
    assert emitted.returncode == 0
    assert "direnv manages this directory" in emitted.stdout, (
        f"a real direnv session must be observed as live:\n{emitted.stdout}"
    )


def test_direnv_naming_a_different_directory_does_not_yield(arena: Arena) -> None:
    """S-020 — direnv is active for some ancestor, not for this project."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    elsewhere = arena.projects / "elsewhere"
    elsewhere.mkdir()

    emitted = matrix.reconcile(arena.ocx, "bash", project, arena.env(DIRENV_DIR="-" + str(elsewhere.resolve())))
    assert "export WP14_CONST='alpha'" in emitted.stdout, (
        f"a DIRENV_DIR naming a DIFFERENT directory must be treated as absent:\n{emitted.stdout}"
    )


def test_an_envrc_on_disk_without_a_live_direnv_does_not_yield(arena: Arena) -> None:
    """S-019 — a config file is evidence of someone else's workflow, not of a live hook."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    (project / ".envrc").write_text("use nix\n", encoding="utf-8")
    (project / "mise.toml").write_text("[tools]\n", encoding="utf-8")
    (project / ".tool-versions").write_text("nodejs 20\n", encoding="utf-8")

    emitted = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert "export WP14_CONST='alpha'" in emitted.stdout, (
        f"yielding to a file on disk would leave the project managed by nobody:\n{emitted.stdout}"
    )


def test_both_direnv_and_mise_live_produce_one_line_each(arena: Arena) -> None:
    """S-018 / A-37 — the two checks are independent `if`s; an `elif` suppresses the second line."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(DIRENV_DIR=str(project.resolve()), MISE_SHELL="bash")

    emitted = matrix.reconcile(arena.ocx, "bash", project, env)
    assert "direnv manages this directory" in emitted.stdout, f"missing direnv line:\n{emitted.stdout}"
    assert "mise manages this directory" in emitted.stdout, f"missing mise line:\n{emitted.stdout}"


def test_mise_alone_yields_the_project_scope(arena: Arena) -> None:
    """S-018 — `MISE_SHELL` on its own is a live session signal."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    emitted = matrix.reconcile(arena.ocx, "bash", project, arena.env(MISE_SHELL="bash"))
    assert "export WP14_CONST=" not in emitted.stdout
    assert "mise manages this directory" in emitted.stdout


# ---------------------------------------------------------------------------
# Tier 2 — `ocx clean` against a real state/projects tree
# ---------------------------------------------------------------------------


def test_clean_retains_an_env_only_projects_stamp_and_collects_a_dead_one(arena: Arena) -> None:
    """S-033 — (a) `[env]`-only project retained; (b) a stamp whose `project_dir` is gone collected."""
    alive = arena.projects / "alive"
    matrix.write_project(alive, _ENV_BLOCK_A)
    matrix.run_lock(arena.ocx, alive, arena.env())
    alive_key = matrix.shell_state(arena.ocx, alive, arena.env())["project_key"]
    (alive / "ocx.lock").unlink()  # the `[env]`-only shape: a stamp, and no ledger entry
    assert matrix.stamp_dir(arena.ocx_home, alive_key).is_dir()

    dead = arena.projects / "dead"
    matrix.write_project(dead, _ENV_BLOCK_A)
    matrix.run_lock(arena.ocx, dead, arena.env())
    dead_key = matrix.shell_state(arena.ocx, dead, arena.env())["project_key"]
    dead_stamp = matrix.stamp_dir(arena.ocx_home, dead_key)
    assert dead_stamp.is_dir()
    shutil.rmtree(dead)

    dry = subprocess.run(
        [str(arena.ocx), "--offline", "clean", "--dry-run", "--force"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        cwd=str(arena.projects),
    )
    assert dry.returncode == 0, f"clean --dry-run must succeed; stderr:\n{dry.stderr}"
    assert dead_stamp.is_dir(), "`--dry-run` must delete NOTHING"
    assert matrix.stamp_dir(arena.ocx_home, alive_key).is_dir()

    swept = subprocess.run(
        [str(arena.ocx), "--offline", "clean", "--force"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        cwd=str(arena.projects),
    )
    assert swept.returncode == 0, f"clean must succeed; stderr:\n{swept.stderr}"
    assert matrix.stamp_dir(arena.ocx_home, alive_key).is_dir(), (
        "an `[env]`-only project's stamp has no ledger entry and must still be RETAINED"
    )
    assert not dead_stamp.exists(), "a stamp whose recorded project_dir is gone must be COLLECTED"


# ---------------------------------------------------------------------------
# Tier 2 — `ocx shell state` is a diagnostic, never a stream
# ---------------------------------------------------------------------------


#: Strips SGR sequences from a captured stream. Under ``--color always`` the
#: theme puts its own escape in front of every heading, so a ``startswith``
#: over the raw bytes would answer about ``\x1b[`` instead of about the text
#: and pass on an injected ``export`` line forever.
_ANSI = re.compile(r"\x1b\[[0-9;]*m")


@pytest.mark.parametrize("shell", ("sh", "bash", "zsh", "fish"))
@pytest.mark.parametrize(
    ("root", "sub"),
    (
        ((), ()),
        ((), ("--verbose",)),
        (("--color", "always"), ()),
        (("--color", "always"), ("--verbose",)),
    ),
    ids=("default", "verbose", "colour", "colour-verbose"),
)
def test_shell_state_output_is_never_eval_able(
    shell: str, root: tuple[str, ...], sub: tuple[str, ...], arena: Arena
) -> None:
    """C-050 — no line of `ocx shell state` may be valid export/set syntax, at either detail tier, coloured or not.

    ``--color always`` is a distinct arm rather than a stylistic variant: the
    property is about the bytes a user can put on a clipboard, and colour
    changes those bytes. The escapes are stripped before the check, and the
    colour arms are paired with a positive control below so "stripped" cannot
    quietly mean "emptied".
    """
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    # `--color` is a root flag and precedes the subcommand; `--verbose` is the
    # subcommand's own and follows it.
    argv = [str(arena.ocx), "--offline", *root, "shell", "state", *sub]
    plain = subprocess.run(
        argv,
        cwd=str(project),
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(shell_abs),
    )
    assert plain.returncode == 0, f"argv={argv}\nstderr:\n{plain.stderr}"
    assert plain.stdout.strip(), f"argv={argv}: the diagnostic printed nothing at all"
    if root:
        assert _ANSI.search(plain.stdout), (
            f"argv={argv}: `--color always` emitted no escape sequence, so the stripped-then-checked "
            "arms below are indistinguishable from the uncoloured ones"
        )
    for line in plain.stdout.splitlines():
        stripped = _ANSI.sub("", line).strip()
        assert not stripped.startswith(("export ", "set -gx ", "set -x ", "$env:", "$env.", "set E:")), (
            f"`ocx shell state` emitted an eval-able line: {line!r}"
        )
    assert "WP14_CONST=" not in _ANSI.sub("", plain.stdout).replace(" ", "")


def test_shell_state_verbose_is_a_rendering_tier_not_a_payload(arena: Arena) -> None:
    """`--verbose` trims the human default only; the structured report is complete either way.

    Both halves are asserted: the JSON documents are byte-equal with and
    without the flag, **and** the human default genuinely omits what the
    verbose tier carries — without the second half a rendering that trimmed
    nothing would satisfy the first.
    """
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    env = arena.env()

    def run(*extra: str) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [str(arena.ocx), "--offline", *extra],
            cwd=str(project),
            capture_output=True,
            check=False,
            text=True,
            env=env,
        )
        assert result.returncode == 0, f"{extra}: exited {result.returncode}\nstderr:\n{result.stderr}"
        return result

    bare_json = json.loads(run("--format", "json", "shell", "state").stdout)
    verbose_json = json.loads(run("--format", "json", "shell", "state", "--verbose").stdout)
    assert bare_json == verbose_json, "`--verbose` must not change the structured payload"
    for key in ("ledger", "watch_set", "carrier_bytes", "priors", "hook", "project_key"):
        assert key in bare_json, f"the structured report must carry {key!r}: {sorted(bare_json)}"

    default_text = run("shell", "state").stdout
    verbose_text = run("shell", "state", "--verbose").stdout
    for section in ("watch set:", "carrier:", "ledger:"):
        assert section in verbose_text, f"`--verbose` must carry {section!r}:\n{verbose_text}"
        assert section not in default_text, f"the default rendering must not carry {section!r}:\n{default_text}"


def test_shell_state_runs_no_background_update_check(arena: Arena) -> None:
    """C-050 — `Shell::State` is on `should_check_for_update`'s skip list.

    The negative assertion is paired with a **positive control** on a command
    that is *not* on the skip list. Without it, a renamed log line would make the
    negative pass forever while the diagnostic quietly started doing background
    work again — a green indistinguishable from the check never running.
    """
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    env = arena.env()
    env["OCX_DEFAULT_REGISTRY"] = "127.0.0.1:1"  # any real contact would fail loudly

    diagnostic = subprocess.run(
        [str(arena.ocx), "-l", "debug", "shell", "state"],
        cwd=str(project),
        capture_output=True,
        check=False,
        text=True,
        env=env,
        timeout=60,
    )
    control = subprocess.run(
        [str(arena.ocx), "-l", "debug", "status"],
        cwd=str(project),
        capture_output=True,
        check=False,
        text=True,
        env=env,
        timeout=60,
    )
    assert diagnostic.returncode == 0, f"shell state must exit 0 with no network; stderr:\n{diagnostic.stderr}"
    assert "Update check" in control.stderr, (
        "positive control: a command OUTSIDE the skip list must reach the update-check path, or the "
        f"negative assertion below is unfalsifiable.\nstderr:\n{control.stderr}"
    )
    assert "Update check" not in diagnostic.stderr, (
        f"the diagnostic path must do no background update work:\n{diagnostic.stderr}"
    )


# ---------------------------------------------------------------------------
# Tier 2 — C-045: no emitted snippet calls bare `ocx`
# ---------------------------------------------------------------------------


# C-045 is violated by one shipped arm, pinned as a strict xfail below rather
# than dropped, so fixing it turns this row red until the marker goes.
#
# The elvish entry is gone because the arm is fixed, not because the pin was
# tidied away: its global-env line now probes `?(test -x '<path>')` and calls the
# resolved absolute binary. It had to be — elvish gained an `ocx` wrapper, and
# `has-external ocx` is a name lookup that finds a function.
_BARE_OCX_XFAIL = {
    "nushell": (
        "SHIPPED C-045 VIOLATION in `ENV_NU`/the nushell activation stream — it emits "
        "`if (which ocx | length) > 0 { try { let _ocx_json = (ocx --format json --global env ...` , "
        "two bare `ocx` calls, and nushell CAN shadow an external with `def ocx`."
    ),
}


@pytest.mark.parametrize("shell", matrix.ALL_SHELLS)
def test_no_emitted_snippet_calls_bare_ocx(shell: str, arena: Arena, request: pytest.FixtureRequest) -> None:
    """C-045 — the wrapper is named `ocx`, so a bare call would run the wrapper inside `$( )`.

    The detector strips every occurrence of the resolved absolute path first, so
    the path's own trailing `ocx` cannot match itself.
    """
    if shell in _BARE_OCX_XFAIL:
        request.node.add_marker(pytest.mark.xfail(strict=True, reason=_BARE_OCX_XFAIL[shell]))
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", f"--shell={shell}", "--hook", "--no-completion"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
    )
    assert startup.returncode == 0, f"{shell}: self activate must exit 0; stderr:\n{startup.stderr}"
    reconciled = matrix.reconcile(arena.ocx, shell, project, arena.env())
    for label, stream in (("startup", startup.stdout), ("reconcile", reconciled.stdout)):
        assert stream.strip(), f"{shell}: the {label} stream must be non-empty, or this check is vacuous"
        _assert_no_bare_ocx_call(stream, str(arena.ocx), f"{shell}/{label}")


_BARE_OCX = re.compile(r"(?:^|[;&|(`$\s])ocx(?=[\s;&|)']|$)", re.MULTILINE)
# The wrapper's own DEFINITION names `ocx` and is not a call — every arm that has
# a wrapper must contain exactly this line, so a detector that flagged it would
# be red on correct output in every arm.
_OCX_DEFINITION = re.compile(r"(?:function|def|fn)\s+ocx\b|^\s*ocx\s*\(\s*\)", re.MULTILINE)
# Any absolute (or otherwise slash-bearing) invocation of the binary. Scrubbed
# first so a resolved path's own trailing `ocx` cannot match itself — a detector
# that matches its own subject measures nothing.
_PATHED_OCX = re.compile(r"[^\s'\"]*/ocx(?:\.exe)?\b")


def _assert_no_bare_ocx_call(stream: str, absolute: str, label: str) -> None:
    """The C-045 detector, factored out so its red state is demonstrable."""
    scrubbed = _PATHED_OCX.sub("<OCX>", stream.replace(absolute, "<OCX>"))
    scrubbed = _OCX_DEFINITION.sub("<OCX-WRAPPER-DEF>", scrubbed)
    match = _BARE_OCX.search(scrubbed)
    assert match is None, (
        f"{label}: the emitted stream calls bare `ocx` at offset {match.start() if match else -1} — "
        f"the wrapper would be invoked inside a command substitution:\n{scrubbed}"
    )


def test_the_bare_ocx_detector_reds_on_a_bare_call(arena: Arena) -> None:
    """Both colours of the C-045 detector, on inputs this file controls."""
    absolute = str(arena.ocx)
    # Green: every call goes through the resolved absolute path.
    _assert_no_bare_ocx_call(f'eval "$("{absolute}" self activate --reconcile)"', absolute, "green")
    # Red: the exact regression C-045 names.
    with pytest.raises(AssertionError, match="calls bare `ocx`"):
        _assert_no_bare_ocx_call('eval "$(ocx self activate --reconcile)"', absolute, "red")


# ---------------------------------------------------------------------------
# Tier 3 — a real pty, a real prompt
# ---------------------------------------------------------------------------


def test_a_real_bash_prompt_hook_applies_on_cd(arena: Arena) -> None:
    """S-001 tier 3 — the hook itself fires. Nothing one tier down can prove this."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""

    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            f"cd '{project}'",
            'printf "%s\\n" "@@const@@${WP14_CONST-__OCX_ABSENT__}"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    assert found.get("const") == "alpha", (
        "the bash prompt hook must reconcile on `cd` with no explicit reconcile call.\n"
        f"pty transcript:\n{output}"
    )


def test_a_real_bash_prompt_hook_preserves_the_previous_exit_status(arena: Arena) -> None:
    """C-043 — `$?` survives the hook, or every shell prompt lies about the last command."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""

    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            f"cd '{project}'",
            "(exit 42)",
            'printf "%s\\n" "@@status@@$?"',
        ],
        cwd=arena.projects,
        env=env,
    )
    assert matrix.probes(output).get("status") == "42", (
        f"the hook must not clobber $?\npty transcript:\n{output}"
    )


def test_path_does_not_grow_across_prompts_in_a_real_bash_session(arena: Arena) -> None:
    """S-039 tier 3 — the prompt loop itself, not a scripted eval."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""

    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            f"cd '{project}'",
            'printf "%s\\n" "@@p1@@$PATH"',
            "true",
            "true",
            "true",
            'printf "%s\\n" "@@p4@@$PATH"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    assert found.get("p1"), f"the first probe must run\npty transcript:\n{output}"
    assert str(project / "binA") in matrix.path_segments(found["p1"])
    assert found.get("p4") == found["p1"], (
        f"PATH grew across prompts: {found.get('p4')!r} != {found['p1']!r}\npty transcript:\n{output}"
    )


def test_a_global_lock_change_reaches_the_next_prompt_of_a_real_bash_session(arena: Arena) -> None:
    """S-004 tier 3 — the owner's headline criterion, at a real prompt in the SAME shell."""
    shell_abs = _require("bash")
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nWP14_GLOBAL = "before"\n', encoding="utf-8")
    locked = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env(),
        cwd=str(arena.projects),
    )
    assert locked.returncode == 0, f"global lock must succeed; stderr:\n{locked.stderr}"
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""

    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            'printf "%s\\n" "@@before@@${WP14_GLOBAL-__OCX_ABSENT__}"',
            f"printf '[env]\\nWP14_GLOBAL = \"after\"\\n' > '{global_toml}'",
            f'"{arena.ocx}" --offline --global lock >/dev/null 2>&1',
            "true",
            'printf "%s\\n" "@@after@@${WP14_GLOBAL-__OCX_ABSENT__}"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    assert found.get("before") == "before", f"the fixture must apply first\npty transcript:\n{output}"
    assert found.get("after") == "after", (
        "a global-toolchain change must be visible at the NEXT PROMPT of the SAME shell, with no "
        f"re-source and no new terminal\npty transcript:\n{output}"
    )


def test_a_real_pwsh_prompt_hook_applies_on_cd(arena: Arena) -> None:
    """S-001 tier 3, PowerShell arm — the wrapped `prompt` function fires."""
    shell_abs = _require("pwsh")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"

    output = matrix.pty_session(
        [shell_abs, "-NoProfile", "-NoLogo"],
        [
            f'Invoke-Expression (& "{arena.ocx}" --offline self activate --shell=pwsh --hook --no-completion | Out-String)',
            f"Set-Location -LiteralPath '{project}'",
            'if (Test-Path env:WP14_CONST) { Write-Output ("@@const@@" + $env:WP14_CONST) } else { Write-Output "@@const@@__OCX_ABSENT__" }',
        ],
        cwd=arena.projects,
        env=env,
    )
    assert matrix.probes(output).get("const") == "alpha", (
        f"the pwsh prompt hook must reconcile on Set-Location\npty transcript:\n{output}"
    )


@pytest.mark.parametrize("tool", ("starship", "oh-my-zsh", "powerlevel10k"))
def test_prompt_hook_coexists_with_a_third_party_prompt_framework(tool: str, arena: Arena) -> None:
    """S-001 / C-043 — append-only registration must survive a foreign prompt owner.

    The framework installs its own prompt machinery **first**; ocx's
    registration then appends to `PROMPT_COMMAND` / `precmd_functions` rather
    than replacing it. Both properties are asserted: the reconcile still fires,
    and `$?` still reaches the user's own prompt.

    Neither shell-zoo image ships any of the three, so this skips there with the
    directories it probed named — a **WP-18 shell-zoo-refresh** item, not a
    silently dropped row.
    """
    location = _locate_prompt_tool(tool)
    shell = "bash" if tool == "starship" else "zsh"
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()

    env = arena.env(shell_abs)
    env["TERM"] = "xterm-256color"
    if tool == "starship":
        preamble = ['eval "$(starship init bash)"']
        launch = [shell_abs, "--norc", "-i"]
    elif tool == "oh-my-zsh":
        env["ZSH"] = location
        env["ZSH_DISABLE_COMPFIX"] = "true"
        preamble = ["ZSH_THEME=''", 'source "$ZSH/oh-my-zsh.sh"']
        launch = [shell_abs, "--no-rcs", "-i"]
    else:
        preamble = [f"source '{location}/powerlevel10k.zsh-theme'"]
        launch = [shell_abs, "--no-rcs", "-i"]

    output = matrix.pty_session(
        launch,
        [
            *preamble,
            f'eval "$("{arena.ocx}" --offline self activate --shell={shell} --hook --no-completion)"',
            f"cd '{project}'",
            "(exit 42)",
            'printf "%s\\n" "@@status@@$?"',
            'printf "%s\\n" "@@const@@${WP14_CONST-__OCX_ABSENT__}"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    assert found.get("const") == "alpha", (
        f"ocx's hook must still reconcile with {tool} owning the prompt\npty transcript:\n{output}"
    )
    assert found.get("status") == "42", (
        f"$? must survive both hooks with {tool} installed\npty transcript:\n{output}"
    )


def test_a_windows_only_row_is_out_of_scope_for_this_matrix() -> None:
    """The zoo is Linux; the Windows leg (batch/WinPS) is WP-18's."""
    if os.name == "nt":
        pytest.fail("this module is skipped on Windows by pytestmark; reaching here means the guard broke")
    pytest.skip(
        "Windows-only rows (cmd/batch PATH idempotency, Windows PowerShell 5.1) need a Windows runner; "
        f"os.name is {os.name!r} — WP-18 owns the cross-platform legs"
    )
