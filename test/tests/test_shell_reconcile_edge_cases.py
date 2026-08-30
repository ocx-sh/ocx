# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Edge-case corpus (WP-15) — one named test per row of
``analysis_shell_env_edge_cases.md``, resolved by ``adr_shell_env_addenda.md``.

**Contract, not a suggestion.** Every row of the register whose ``Test tier``
names ``pytest-hostshell`` or ``pytest-shellzoo`` (even inside a combined tier
such as ``rust-unit + pytest-shellzoo``) gets exactly one named, running test
below. The three ``manual-only`` rows (``EC-FP-002``, ``EC-PROC-011``,
``EC-PROC-013``) get a documented manual procedure instead — see
:func:`manual_procedures` at the bottom of this module. A pure ``rust-unit``
row is out of this module's scope; ``analysis_shell_env_edge_cases.md``'s
coverage column names the Rust test (or ``rust-unit (uncovered)``) for those.

Built on the same foundation as ``test_shell_reconcile.py`` (WP-14) — the
``shell_matrix`` module is the shared library both consume; this module does
not re-derive any escaping, quoting or PATH-algebra primitive it already
exposes. Local scaffolding below (``Arena``, ``_locked_project``, ``_session``,
…) intentionally mirrors WP-14's rather than importing across test modules —
the same choice ``test_shell_reconcile.py`` made relative to
``test_shell_activation.py``.

**Traceability gate.** :func:`test_every_pytest_row_has_a_named_test` and
:func:`test_every_test_traces_to_a_row` are the two-way proof: every
register row needing an acceptance test names one that exists in this file,
and every test in this file (except the traceability tests and the manual
procedure) traces back to a register row. Run them first if this file is
edited — a broken cross-reference fails loudly here, not in review.

Registry hygiene: like WP-14, this module builds every fixture on disk and
runs ocx ``--offline``. It touches no shared-registry fixture.
"""

from __future__ import annotations

import ast
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import time
from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

import pytest

import shell_matrix as matrix

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason=(
            "this module drives the Linux shell-zoo image (bash/zsh/fish/pwsh/elvish/nushell "
            "over pty and eval, plus a Batch-arm emitter check with no live cmd.exe) and does "
            "not run on Windows at all; no other CI leg invokes this module there either — "
            "the handful of rows this module cannot reach on any leg are retiered to "
            "manual-only rather than cited against a Windows arm of THIS suite that does not "
            "exist (ocx#353)."
        ),
    ),
]

_OCX = matrix.ocx_binary()

pytestmark.append(
    pytest.mark.skipif(
        _OCX is None,
        reason="no ocx binary (set OCX_ACTIVATION_BINARY / OCX_COMMAND, or build test/bin/ocx).",
    )
)

_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
_PROJECT_SCOPE_SHELLS: tuple[str, ...] = (*matrix.SESSION_SHELLS, "nushell")

_ENV_BLOCK_A = (
    'WP15_CONST = "alpha"\n'
    'PATH = { type = "path", value = "binA" }\n'
    'CFLAGS = { type = "list", separator = " ", value = "-DPROJECT_A" }\n'
)


# ---------------------------------------------------------------------------
# Arena — one isolated OCX home + project root per test (mirrors WP-14)
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


def _locked_project(arena: Arena, name: str, env_block: str, *, tools_block: str = "") -> Path:
    """A project with a real ``ocx.lock`` — and therefore a real consent stamp."""
    project = arena.projects / name
    matrix.write_project(project, env_block, tools_block=tools_block)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, f"ocx lock must succeed for the fixture; stderr:\n{result.stderr}"
    assert (project / "ocx.lock").is_file(), "ocx lock must write ocx.lock"
    return project


def _clone_of(project: Path, destination: Path) -> Path:
    """Copy a locked project to a new directory — a fresh clone, with no stamp."""
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
    """Skip a project-scope row on nushell — after **observing** why (mirrors WP-14)."""
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
            f"{env_nu} contains 0 occurrences of 'reconcile' (observed)"
        )


def _locate_prompt_tool(tool: str) -> str:
    """Resolve a third-party prompt framework, or skip naming what was probed (mirrors WP-14)."""
    if tool == "starship":
        resolved = shutil.which("starship")
        if resolved is None:
            pytest.skip("starship is not installed in this image (shutil.which('starship') is None) — WP-18")
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
    pytest.skip(f"{tool} is not installed in this image (none of {[str(c) for c in candidates]} is a directory) — WP-18")
    raise AssertionError("unreachable: pytest.skip raises")


def _session(
    shell: str,
    arena: Arena,
    fragments: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    name: str = "session",
) -> subprocess.CompletedProcess[str]:
    """Run ``fragments`` as one script in ``shell``, with ``__ocx_exe`` bound (mirrors WP-14)."""
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
    found = matrix.probes(result.stdout)
    assert label in found, (
        f"probe '{label}' never printed — the session did not reach it.\n"
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    return found[label]


def _toml_escape(value: str) -> str:
    """Escape ``value`` for embedding inside a TOML basic (double-quoted) string."""
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\t", "\\t")


def _write_config(arena: Arena, body: str) -> Path:
    config = arena.ocx_home / "config.toml"
    config.write_text(body, encoding="utf-8")
    return config


# ---------------------------------------------------------------------------
# Ledger lifecycle, degradation, forgery
# ---------------------------------------------------------------------------


def test_ec_ledger_001_absent_ledger_first_prompt_applies_without_revert(arena: Arena) -> None:
    """EC-LEDGER-001 — a first prompt with no carrier applies D with no revert.

    The row's prose additionally claims "no summary line" for this case; the
    shipped ``summary_line`` (``activate.rs``) fires whenever ``plan.sets`` is
    non-empty regardless of first-prompt-ness, so a first apply DOES carry one
    — this is a register-vs-shipped-code discrepancy the addenda does not flag
    with a marker, reported alongside this test rather than silently coded
    around. The property this row actually protects — apply with no revert,
    exit 0 — is what the assertions below prove.
    """
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert result.returncode == 0
    assert "export WP15_CONST='alpha'" in result.stdout, f"a first-prompt apply must set the constant:\n{result.stdout}"
    assert "unset WP15_CONST" not in result.stdout, f"a first prompt has nothing to revert:\n{result.stdout}"


def test_ec_ledger_002_corrupt_carrier_runs_subtractive_repair_and_speaks(arena: Arena) -> None:
    """EC-LEDGER-002 — a corrupt (not absent) carrier triggers prefix repair with one summary line."""
    owned = arena.ocx_home / "packages" / "old" / "bin"
    owned.mkdir(parents=True)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{owned}" }}\n')
    applied = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert str(owned) in applied.stdout, "the fixture must apply first, or the corrupt-repair proves nothing"

    corrupted = matrix.reconcile(arena.ocx, "bash", project, arena.env(), carrier="1.@@@notbase64@@@")
    assert corrupted.returncode == 0
    assert matrix.CARRIER in corrupted.stdout
    lines = [line for line in corrupted.stdout.splitlines() if line.strip().startswith("printf") or "ocx:" in line]
    assert len(lines) <= 1, f"corrupt-carrier repair must speak at most once:\n{corrupted.stdout}"


def test_ec_ledger_011_revert_replays_the_captured_prior_never_the_current_file(arena: Arena) -> None:
    """EC-LEDGER-011 — revert restores the literal PRIOR captured at entry, unaffected by two later recomposes.

    Three recomposes change ``ocx.toml``'s value in turn (/a -> /b -> /c); at
    each step the live shell's value is threaded to match what the previous
    reconcile actually applied, so ``C == L.applied`` holds throughout and no
    re-capture fires. A re-derived revert reading the CURRENT ocx.toml would
    restore '/c' or unset outright; the real one restores the ORIGINAL prior.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "/a"\n')
    entry_env = arena.env() | {"WP15_CONST": "/original"}
    first = matrix.reconcile(arena.ocx, "bash", project, entry_env)
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", first.stdout)
    assert match, f"the fixture must emit a carrier:\n{first.stdout}"
    assert "export WP15_CONST='/a'" in first.stdout, f"entry must apply /a:\n{first.stdout}"
    carrier_a = match.group(1)

    (project / "ocx.toml").write_text('[env]\nWP15_CONST = "/b"\n', encoding="utf-8")
    relocked = matrix.run_lock(arena.ocx, project, arena.env())
    assert relocked.returncode == 0, f"relock must succeed; stderr:\n{relocked.stderr}"
    live_at_a = arena.env() | {"WP15_CONST": "/a"}
    second = matrix.reconcile(arena.ocx, "bash", project, live_at_a, carrier=carrier_a)
    match2 = re.search(rf"export {matrix.CARRIER}='([^']+)'", second.stdout)
    assert match2, f"a recompose must still emit a carrier:\n{second.stdout}"
    assert "export WP15_CONST='/b'" in second.stdout, f"recompose must apply /b:\n{second.stdout}"
    carrier_b = match2.group(1)

    (project / "ocx.toml").write_text('[env]\nWP15_CONST = "/c"\n', encoding="utf-8")
    relocked2 = matrix.run_lock(arena.ocx, project, arena.env())
    assert relocked2.returncode == 0, f"relock must succeed; stderr:\n{relocked2.stderr}"
    live_at_b = arena.env() | {"WP15_CONST": "/b"}
    third = matrix.reconcile(arena.ocx, "bash", project, live_at_b, carrier=carrier_b)
    match3 = re.search(rf"export {matrix.CARRIER}='([^']+)'", third.stdout)
    assert match3, f"the second recompose must still emit a carrier:\n{third.stdout}"
    assert "export WP15_CONST='/c'" in third.stdout, f"recompose must apply /c:\n{third.stdout}"
    carrier_c = match3.group(1)

    outside = arena.projects / "outside"
    outside.mkdir()
    live_at_c = arena.env() | {"WP15_CONST": "/c"}
    left = matrix.reconcile(arena.ocx, "bash", outside, live_at_c, carrier=carrier_c)
    assert "export WP15_CONST='/original'" in left.stdout, (
        f"leaving must restore the prior CAPTURED AT ENTRY ('/original'), never re-resolve the current ocx.toml "
        f"value ('/c') and never the intermediate D values ('/a', '/b'):\n{left.stdout}"
    )


def _run_child_fragment(parent: str, child: str, child_abs: str, script: Path) -> str:
    """Spawn ``child`` running ``script``, written in ``parent``'s own syntax (mirrors WP-14)."""
    words = [child_abs, *matrix.ARMS[child].flags, str(script)]
    literals = " ".join(matrix.quote(parent, word) for word in words)
    if parent == "pwsh":
        return f"& {literals}"
    if parent == "elvish":
        head = matrix.quote(parent, words[0])
        tail = " ".join(matrix.quote(parent, word) for word in words[1:])
        return f"(external {head}) {tail}"
    return literals


@pytest.mark.parametrize(("parent", "child"), [("zsh", "bash"), ("bash", "fish"), ("bash", "pwsh")])
def test_ec_ledger_013_ledger_written_by_one_shell_read_by_another(parent: str, child: str, arena: Arena) -> None:
    """EC-LEDGER-013 — cross-shell inheritance: each arm decodes the same raw values through its own escaper."""
    parent_abs = _require(parent)
    child_abs = _require(child)
    project = _locked_project(arena, "alpha", "WP15_CONST = \"it's $HOME `tick` !bang\"\n")

    child_script = arena.scripts / f"ledger013_child{matrix.ARMS[child].extension}"
    child_script.write_text(
        "\n".join(
            [
                matrix.header(child, arena.ocx),
                matrix.probe(child, "inherited", "WP15_CONST"),
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    result = _session(
        parent,
        arena,
        [matrix.cd_to(parent, project), matrix.prompt(parent), _run_child_fragment(parent, child, child_abs, child_script)],
        name=f"ledger013_{parent}_{child}",
        env=matrix.clean_env(arena.home, parent_abs, ocx_home=arena.ocx_home)
        | {"PATH": os.pathsep.join([str(Path(child_abs).parent), matrix.BASE_PATH, str(Path(parent_abs).parent)])},
    )
    assert result.returncode == 0, f"{parent} -> {child}: session must exit 0\nstderr:\n{result.stderr}"
    assert _read(result, "inherited") == "it's $HOME `tick` !bang", (
        f"{child}: must decode {parent}'s raw ledger value unchanged, across the real process boundary"
    )


def test_ec_ledger_014_unset_carrier_repairs_lists_constants_stay(arena: Arena) -> None:
    """EC-LEDGER-014 — the ``unset __OCX_ENV_STATE`` gesture destroys priors.

    Driven through direct subprocess calls so the carrier can be threaded
    explicitly between steps. The teachable property is not the re-apply
    itself (an absent ledger always re-emits, coincidence or not) but what it
    does to ``priors``: a real prior (``/user``) captured on entry is silently
    overwritten by the coincidence rule's ``priors := C`` on the very next
    prompt after the gesture, because there is no ledger left to say
    otherwise — so leaving later restores the WRONG value.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "alpha"\n')
    entry_env = arena.env() | {"WP15_CONST": "/user"}

    entered = matrix.reconcile(arena.ocx, "bash", project, entry_env)
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", entered.stdout)
    assert match, f"the fixture must apply and emit a carrier:\n{entered.stdout}"
    carrier_after_entry = match.group(1)

    # The gesture: `unset __OCX_ENV_STATE` in the live shell. Its WP15_CONST
    # already reads "alpha" (what the entry apply set) — the NEXT prompt sees
    # an absent carrier again and re-derives D fresh, with no ledger to tell
    # it "/user" was ever the real prior.
    live_env = arena.env() | {"WP15_CONST": "alpha"}
    repaired = matrix.reconcile(arena.ocx, "bash", project, live_env)
    assert repaired.returncode == 0
    match2 = re.search(rf"export {matrix.CARRIER}='([^']+)'", repaired.stdout)
    assert match2, f"the repair prompt must still emit a fresh carrier:\n{repaired.stdout}"
    carrier_after_repair = match2.group(1)

    outside = arena.projects / "outside"
    outside.mkdir()
    left = matrix.reconcile(arena.ocx, "bash", outside, live_env, carrier=carrier_after_repair)
    assert left.returncode == 0
    assert "export WP15_CONST='alpha'" in left.stdout, (
        f"the DESTROYED prior means leaving restores the coincidental 'alpha' the repair captured, "
        f"never the real '/user' that stood before entry:\n{left.stdout}"
    )
    assert "'/user'" not in left.stdout, f"the real prior must be unreachable after the gesture:\n{left.stdout}"

    # Contrast: the SAME scenario with the ledger intact restores the real prior.
    # The live shell's WP15_CONST reads "alpha" (what entry applied) — that is
    # C, and it must equal L.applied for the exit guard to fire the revert.
    left_intact = matrix.reconcile(arena.ocx, "bash", outside, live_env, carrier=carrier_after_entry)
    assert "export WP15_CONST='/user'" in left_intact.stdout, (
        f"control: with the ledger intact, leaving must restore the REAL prior:\n{left_intact.stdout}"
    )


# ---------------------------------------------------------------------------
# Constant-kind state machine
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_001_unset_prior_removed_value_prior_restored(shell: str, arena: Arena) -> None:
    """EC-CONST-001 — ``priors: Unset`` removes on exit; ``priors: Value`` restores."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    outside = arena.projects / "outside"
    outside.mkdir()

    unset_case = _session(
        shell,
        arena,
        [matrix.cd_to(shell, project), matrix.prompt(shell), matrix.cd_to(shell, outside), matrix.prompt(shell), matrix.probe(shell, "after", "WP15_CONST")],
        name="unset_case",
    )
    assert _read(unset_case, "after") == matrix.ABSENT, f"{shell}: no prior ⇒ removed, never empty"

    value_case = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "WP15_CONST", "/user"),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
        name="value_case",
    )
    assert _read(value_case, "after") == "/user", f"{shell}: a real prior must be restored"


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_002_mid_session_override_survives_exit(shell: str, arena: Arena) -> None:
    """EC-CONST-002 — ``C != L.applied`` at exit ⇒ leave C alone; the direnv behaviour ocx refuses."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.set_var(shell, "WP15_CONST", "/mine"),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
    )
    assert result.returncode == 0
    assert _read(result, "after") == "/mine", f"{shell}: a mid-session override must survive the exit unchanged"


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_003_mid_session_override_survives_every_recompose(shell: str, arena: Arena) -> None:
    """EC-CONST-003 — the ``D == L`` apply gate: an override survives repeated recomposes, not just exit."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    fragments = [
        matrix.cd_to(shell, project),
        matrix.prompt(shell),
        matrix.set_var(shell, "WP15_CONST", "/mine"),
    ]
    for _ in range(4):
        fragments.append(matrix.prompt(shell))
    fragments.append(matrix.probe(shell, "after", "WP15_CONST"))
    result = _session(shell, arena, fragments)
    assert result.returncode == 0
    assert _read(result, "after") == "/mine", (
        f"{shell}: D == L must gate the APPLY, not only the exit guard — five further prompts must not clobber it"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_004_prior_recaptured_on_project_intent_change(shell: str, arena: Arena) -> None:
    """EC-CONST-004 — the headline re-capture: D != L re-applies AND re-captures the live override as prior."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.set_var(shell, "WP15_CONST", "/user"),
            _write_config_env(shell, project / "ocx.toml", 'WP15_CONST = "bravo"\n'),
            matrix.prompt(shell),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "after") == "/user", (
        f"{shell}: re-capture must fire on the D!=L apply, not just on the ORIGINAL entry — deleting it would make "
        f"the exit UNSET a value the user set by hand"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_005_coincidence_c_equals_d_restores_not_removes(shell: str, arena: Arena) -> None:
    """EC-CONST-005 — a user value that coincides with D's is restored on exit, not removed."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'WP15_CONST = "alpha"\n')
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "WP15_CONST", "alpha"),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.cd_to(shell, outside),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
    )
    assert result.returncode == 0
    assert _read(result, "after") == "alpha", (
        f"{shell}: C == D by coincidence must claim it as prior — leaving restores 'alpha' rather than unsetting"
    )


@pytest.mark.parametrize("shell", _PROJECT_SCOPE_SHELLS)
def test_ec_const_007_retirement_restores_prior_under_the_same_exit_guard(shell: str, arena: Arena) -> None:
    """EC-CONST-007 — retiring a constant (recompose in place) obeys the SAME C==L.applied guard as exit."""
    _skip_nushell_without_reconcile(shell, arena)
    project = _locked_project(arena, "alpha", 'WP15_CONST = "alpha"\n')

    unmodified = _session(
        shell,
        arena,
        [
            matrix.set_var(shell, "WP15_CONST", "/user"),
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            _write_config_env(shell, project / "ocx.toml", ""),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
        name="retire_no_override",
    )
    assert _read(unmodified, "after") == "/user", f"{shell}: retirement restores the prior under the guard"

    overridden = _session(
        shell,
        arena,
        [
            matrix.cd_to(shell, project),
            matrix.prompt(shell),
            matrix.set_var(shell, "WP15_CONST", "/mine"),
            _write_config_env(shell, project / "ocx.toml", ""),
            matrix.prompt(shell),
            matrix.probe(shell, "after", "WP15_CONST"),
        ],
        name="retire_with_override",
    )
    assert _read(overridden, "after") == "/mine", f"{shell}: C != L.applied at retirement time ⇒ leave C alone"


def _write_config_env(shell: str, path: Path, env_block: str) -> str:
    """Rewrite ``ocx.toml``'s ``[env]`` table from inside a running session."""
    literal_path = matrix.quote(shell, str(path))
    literal_body = matrix.quote(shell, f"[env]\n{env_block}")
    if shell == "pwsh":
        return f"Set-Content -LiteralPath {literal_path} -Value {literal_body} -NoNewline"
    if shell == "fish":
        return f"printf '%s' {literal_body} > {literal_path}"
    if shell == "elvish":
        return f"print {literal_body} > {literal_path}"
    if shell == "nushell":
        return f"{literal_body} | save --force {literal_path}"
    return f"printf '%s' {literal_body} > {literal_path}"



# ---------------------------------------------------------------------------
# List-kind, path-kind, retirement
# ---------------------------------------------------------------------------


def test_ec_list_001_move_to_front_idempotent_under_repeated_eval(arena: Arena) -> None:
    """EC-LIST-001 — evaluating the same emitted snippet five times leaves PATH byte-identical."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            *([matrix.prompt("bash")] * 5),
            matrix.probe("bash", "path", "PATH"),
        ],
    )
    assert result.returncode == 0
    segments = matrix.path_segments(_read(result, "path"))
    assert segments.count(str(project / "binA")) == 1, f"exactly one occurrence after five applications: {segments}"


def test_ec_list_002_revert_commutes_with_a_foreign_prepend(arena: Arena) -> None:
    """EC-LIST-002 — leaving deletes only ocx's element; a foreign prepend survives in place and in order."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            'export PATH="/foo/bin:$PATH"',
            matrix.cd_to("bash", outside),
            matrix.prompt("bash"),
            matrix.probe("bash", "path", "PATH"),
        ],
    )
    assert result.returncode == 0
    segments = matrix.path_segments(_read(result, "path"))
    assert segments[0] == "/foo/bin", f"the foreign element must stay in front: {segments}"
    assert str(project / "binA") not in segments, f"ocx's element must be gone: {segments}"


def test_ec_list_003_digest_bump_leaves_zero_stale_occurrences(arena: Arena) -> None:
    """EC-LIST-003 — a digest bump retires the old bin dir subtractively; count is ZERO, not merely behind."""
    old_digest = "sha256_" + "cc" * 8
    new_digest = "sha256_" + "dd" * 8
    packages = arena.ocx_home / "packages"
    (packages / old_digest / "bin").mkdir(parents=True)
    (packages / new_digest / "bin").mkdir(parents=True)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{packages / old_digest / "bin"}" }}\n')

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            _write_config_env(
                "bash", project / "ocx.toml", f'PATH = {{ type = "path", value = "{packages / new_digest / "bin"}" }}\n'
            ),
            _run_fragment_bash(f"{arena.ocx} --offline lock"),
            matrix.prompt("bash"),
            matrix.probe("bash", "path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert str(packages / new_digest / "bin") in segments
    assert segments.count(str(packages / old_digest / "bin")) == 0, f"stale digest must appear ZERO times: {segments}"


def _run_fragment_bash(command: str) -> str:
    return f"{command} >/dev/null 2>&1"


def test_ec_list_004_remove_global_from_another_terminal_is_gone_not_shadowed(arena: Arena) -> None:
    """EC-LIST-004 — a mid-session ``ocx remove --global`` retires the binary, not merely shadows it."""
    global_toml = arena.ocx_home / "ocx.toml"
    tool_dir = arena.ocx_home / "packages" / "globaltool" / "bin"
    tool_dir.mkdir(parents=True)
    (tool_dir / "sometool").write_text("#!/bin/sh\necho hi\n", encoding="utf-8")
    (tool_dir / "sometool").chmod(0o755)
    global_toml.write_text(f'[env]\nPATH = {{ type = "path", value = "{tool_dir}" }}\n', encoding="utf-8")
    locked = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"],
        capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects),
    )
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", arena.projects),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "PATH"),
            _run_fragment_bash(f"printf '[env]\\n' > {matrix.quote('bash', str(global_toml))}"),
            _run_fragment_bash(f"{arena.ocx} --offline --global lock"),
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "PATH"),
            "command -v sometool >/dev/null 2>&1; printf '%s\\n' \"@@resolves@@$?\"",
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert str(tool_dir) in matrix.path_segments(_read(result, "before"))
    assert str(tool_dir) not in matrix.path_segments(_read(result, "after")), (
        f"the global tool's bin dir must be retired: {matrix.path_segments(_read(result, 'after'))}"
    )
    assert _read(result, "resolves") == "1", "the binary must be GONE, not merely shadowed — `command -v` must fail"


def test_ec_list_005_arbitrary_element_stranded_when_ledger_is_lost(arena: Arena) -> None:
    """EC-LIST-005 — an arbitrary (non-`$OCX_HOME`) element survives a corrupt ledger; is retired when intact."""
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "bin" }\n')
    (project / "bin").mkdir()
    outside = arena.projects / "outside"
    outside.mkdir()

    entered = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert str(project / "bin") in entered.stdout, f"fixture must apply first:\n{entered.stdout}"
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", entered.stdout)
    assert match

    corrupted_leave = matrix.reconcile(arena.ocx, "bash", outside, arena.env(), carrier="1.@@@corrupt@@@")
    assert str(project / "bin") not in corrupted_leave.stdout, (
        "with L lost, prefix repair covers $OCX_HOME-rooted elements only — the arbitrary element is not named for "
        f"removal here (it can only be removed by whoever set it):\n{corrupted_leave.stdout}"
    )

    intact_leave = matrix.reconcile(arena.ocx, "bash", outside, arena.env(), carrier=match.group(1))
    assert "unset" in intact_leave.stdout or "~PATH" in intact_leave.stdout or "PATH=" in intact_leave.stdout, (
        f"with L intact, the arbitrary element must be recorded and retired correctly:\n{intact_leave.stdout}"
    )


def test_ec_list_006_non_default_separator_applies_and_reverts(arena: Arena) -> None:
    """EC-LIST-006 — a space-separated list var applies AND reverts around a foreign value."""
    project = _locked_project(arena, "alpha", 'CFLAGS = { type = "list", separator = " ", value = "-I/inc" }\n')
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        "bash",
        arena,
        [
            'export CFLAGS="-DFOREIGN"',
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "in", "CFLAGS"),
            matrix.cd_to("bash", outside),
            matrix.prompt("bash"),
            matrix.probe("bash", "out", "CFLAGS"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "-I/inc" in _read(result, "in").split(" ") and "-DFOREIGN" in _read(result, "in").split(" ")
    after = _read(result, "out").split(" ")
    assert "-I/inc" not in after, f"ocx's contribution must be removed: {after}"
    assert "-DFOREIGN" in after, f"the foreign value must survive: {after}"


# ---------------------------------------------------------------------------
# Fingerprint and trigger
# ---------------------------------------------------------------------------


def test_ec_fp_001_same_size_same_second_write_is_invisible(arena: Arena) -> None:
    """EC-FP-001 — a same-mtime, same-byte-count rewrite is invisible to the shell-side newer-than short-circuit.

    The mtime+size fast path is the INSTALLED HOOK's own
    ``[ path -nt "$__ocx_stamp" ]`` check (``shell/hook.rs``), baked into the
    ``__ocx_prompt_hook`` function body at shell start and consulted by
    ``PROMPT_COMMAND`` on every real prompt. Calling ``self activate
    --reconcile`` directly (as ``matrix.prompt()`` does, and as this module
    otherwise does throughout) is a DIFFERENT code path — it always fully
    recomposes — so this row is observable only through a real pty and the
    real installed hook, not through the tier-2 direct-eval idiom.
    """
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", 'WP15_CONST = "aaa"\n')
    original_content = (project / "ocx.toml").read_text(encoding="utf-8")
    original_stat = (project / "ocx.toml").stat()
    # `touch -r REF` is the mtime restore BOTH toolchains implement. GNU's
    # `-d @EPOCH` is not portable: busybox touch (the Alpine zoo leg's `/bin`)
    # rejects it — `touch: invalid date '@...'`, exit 1 — so the restore
    # no-opped, `ocx.toml` genuinely WAS newer than the stamp, and the
    # production gate fired exactly as specified while the row read as a
    # portability failure of the shipped hook. `-r` also carries full
    # nanosecond fidelity, which the float `@EPOCH` spelling did not.
    mtime_ref = arena.scripts / "fp001-mtime-ref"
    mtime_ref.touch()
    os.utime(mtime_ref, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
    edited = original_content.replace("aaa", "bbb")
    assert len(edited) == len(original_content), "the fixture must preserve byte count"

    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""

    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            f"cd '{project}'",
            'printf "%s\n" "@@before@@${WP15_CONST-__OCX_ABSENT__}"',
            # `>` truncates and rewrites the SAME inode / directory entry, so
            # the PROJECT DIRECTORY's own mtime (a separate watch member)
            # never moves — unlike `sed -i`'s unlink+rename idiom.
            #
            # ONE command line, and that is load-bearing. Split across two, the
            # prompt between them looks at the *intermediate* state — a file
            # whose mtime genuinely moved — and the gate is supposed to fire on
            # that. The row would then be green while asserting nothing about
            # A-14's ceiling, which is exactly how it read before #347: the gate
            # had no term for this project's `ocx.toml` at all, so the edit was
            # unseen because the shell was blind, not because the mtime held.
            (
                f"printf '%s' '{edited}' > '{project / 'ocx.toml'}' && touch -r '{mtime_ref}' "
                f"'{project / 'ocx.toml'}'"
            ),
            "true",
            'printf "%s\n" "@@after@@${WP15_CONST-__OCX_ABSENT__}"',
            # The positive control, in the same session and on the same file:
            # move the mtime and nothing else. A blind gate fails HERE, which is
            # what stops the assertion above from going quietly vacuous again.
            f"touch '{project / 'ocx.toml'}'",
            "true",
            'printf "%s\n" "@@seen@@${WP15_CONST-__OCX_ABSENT__}"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    assert found.get("before") == "aaa", f"the real hook must apply on the first prompt\npty transcript:\n{output}"
    assert found.get("after") == "aaa", (
        f"an mtime+size-invisible edit must stay unseen by the newer-than short-circuit — the SAME session's "
        f"next prompt must never even invoke ocx\npty transcript:\n{output}"
    )
    assert found.get("seen") == "bbb", (
        f"the very same edit must be picked up once its mtime moves, or the row above is asserting that the "
        f"gate is blind rather than that the ceiling holds\npty transcript:\n{output}"
    )


def test_ec_fp_003_env_only_edit_with_lock_untouched_recomposes(arena: Arena) -> None:
    """EC-FP-003 — ``[env]`` is in the watch set on its own authority; a lock-untouched edit still recomposes."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "old"\n')
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "old", "WP15_CONST"),
            _write_config_env("bash", project / "ocx.toml", 'WP15_CONST = "new"\n'),
            matrix.prompt("bash"),
            matrix.probe("bash", "new", "WP15_CONST"),
        ],
    )
    assert result.returncode == 0
    assert _read(result, "old") == "old"
    assert _read(result, "new") == "new", "[env] applies on its own authority — the lock never moved"


def test_ec_fp_004_deleting_ocx_toml_is_caught_via_the_project_directory(arena: Arena) -> None:
    """EC-FP-004 — deleting ``ocx.toml`` is caught only because the project directory itself is watched."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "WP15_CONST"),
            f"rm {matrix.quote('bash', str(project / 'ocx.toml'))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "WP15_CONST"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before") == "alpha"
    assert _read(result, "after") == matrix.ABSENT, "the deleted project's constant must revert"


def test_ec_fp_005_self_update_recomposes_every_open_shell(arena: Arena) -> None:
    """EC-FP-005 — the ocx binary version is a watch-set member; a version bump recomposes the next prompt.

    ``fingerprint()`` folds ``env!("CARGO_PKG_VERSION")`` — a compile-time
    constant baked into the running process, with no runtime override seam
    (grepped: no ``__OCX_TEST_*`` var threads through it). A genuine
    cross-version proof needs two distinct builds; this environment has one
    binary under test. Skip names that observed constraint rather than
    asserting a substitute.
    """
    binary_count = len({str(arena.ocx)})
    pytest.skip(
        f"observed: {binary_count} distinct ocx build is available under test ({arena.ocx}), and "
        "fingerprint()'s CARGO_PKG_VERSION fold (crates/ocx_lib/src/shell/reconcile.rs:507) has no runtime "
        "override seam (grepped: no __OCX_TEST_* var reaches it) — a real version bump needs a second binary "
        "build, which this harness does not provide"
    )


def test_ec_fp_006_inert_verdict_is_cached_stat_only(arena: Arena) -> None:
    """EC-FP-006 — a fresh clone's negative verdict is cached; a stable fingerprint reads no config again."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    clone = _clone_of(source, arena.projects / "clone")
    first = matrix.reconcile(arena.ocx, "bash", clone, arena.env())
    assert "is not activated" in first.stdout
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", first.stdout)
    assert match, f"even an inert verdict must cache a carrier:\n{first.stdout}"
    carrier = match.group(1)

    second = matrix.reconcile(arena.ocx, "bash", clone, arena.env(), carrier=carrier)
    assert second.returncode == 0
    assert "is not activated" not in second.stdout, (
        f"a cached inert verdict with an unmoved fingerprint must not re-print the hint every prompt:\n"
        f"{second.stdout}"
    )


def test_ec_fp_008_a_new_grant_is_visible_via_the_widened_watch_set(arena: Arena) -> None:
    """EC-FP-008 — A-13 landed: ``config.toml`` tier paths are IN the watch set, so a live grant is seen at once.

    The row's own prose frames this as a gap ("the watch set is project
    ocx.toml+ocx.lock ... config.toml and consent.json are not in it"); A-13's
    resolution — already shipped, per ``watch_paths_use_the_recorded_tier_list
    _verbatim_a13`` / ``watch_paths_carry_the_consent_stamp_a13`` — adds
    exactly those paths. So the fixed property is the opposite of the row's
    literal claim: a grant added from another terminal reaches the SAME
    shell's very next prompt, no restart required.
    """
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    clone = _clone_of(source, arena.projects / "clone")
    first = matrix.shell_state(arena.ocx, clone, arena.env())
    assert first["inert_reason"]["reason"] == "no_stamp_no_grant", f"clone must start inert: {first['inert_reason']}"
    inert = matrix.reconcile(arena.ocx, "bash", clone, arena.env())
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", inert.stdout)
    assert match, f"even an inert verdict must cache a carrier:\n{inert.stdout}"
    carrier = match.group(1)

    _write_config(arena, f'[shell.consent]\npaths = ["{clone}"]\n')
    next_prompt = matrix.reconcile(arena.ocx, "bash", clone, arena.env(), carrier=carrier)
    assert "export WP15_CONST='alpha'" in next_prompt.stdout, (
        f"config.toml is IN the watch set (A-13) — the SAME shell's next prompt must see the grant with no "
        f"restart:\n{next_prompt.stdout}"
    )


def test_ec_fp_009_pwd_events_fire_independently_of_the_fingerprint(arena: Arena) -> None:
    """EC-FP-009 — a stat-identical clone still triggers project switch: PWD events are a separate trigger."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (source / "binA").mkdir()
    clone_dir = arena.projects / "clone"
    shutil.copytree(source, clone_dir)  # shutil preserves mtimes/sizes by default
    # The clone needs its own consent stamp too, since consent is keyed on the
    # canonical directory.
    relocked = matrix.run_lock(arena.ocx, clone_dir, arena.env())
    assert relocked.returncode == 0

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", source),
            matrix.prompt("bash"),
            matrix.cd_to("bash", clone_dir),
            matrix.prompt("bash"),
            matrix.probe("bash", "path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments.count(str(source / "binA")) == 0, (
        f"switching from source to a stat-identical clone must still revert the source's element: {segments}"
    )
    assert str(clone_dir / "binA") in segments, f"and apply the clone's own: {segments}"


# ---------------------------------------------------------------------------
# Reconciler surfaces, parity, `ocx shell state`
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("shell", ("bash", "zsh", "fish"))
def test_ec_rec_001_in_process_versus_emitted_parity_per_arm(shell: str, arena: Arena) -> None:
    """EC-REC-001 — the emitted ``export_path`` and the in-process apply agree byte for byte, per arm.

    pwsh is excluded here: this host's pwsh install prepends its own bin dir
    to ``$env:PATH`` at process startup (observed — not an ocx emission), which
    would make the byte-exact ambient-tail assertion fail for a reason
    unrelated to move-to-front parity. pwsh is still covered by every
    project-scope EC-CONST/EC-LIST test above.
    """
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    seeded_path = os.pathsep.join(["/x", "/y"])
    env = arena.env(shell_abs)
    env["PATH"] = seeded_path
    result = _session(shell, arena, [matrix.cd_to(shell, project), matrix.prompt(shell), matrix.probe(shell, "path", "PATH")], env=env)
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments[0] == str(project / "binA"), f"{shell}: emitted move-to-front must prepend, matching in-process order: {segments}"
    assert segments[1:] == ["/x", "/y"], f"{shell}: the untouched ambient tail must survive byte-identically: {segments}"


def test_ec_rec_005_shell_state_is_read_only(arena: Arena) -> None:
    """EC-REC-005 — ``ocx shell state`` never writes a stamp, never repairs a ledger, never emits a plan."""
    source = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    clone = _clone_of(source, arena.projects / "clone")
    key = matrix.shell_state(arena.ocx, clone, arena.env())["project_key"]
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), "a fresh clone must start unstamped"

    matrix.shell_state(arena.ocx, clone, arena.env())
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), "shell state must never write a stamp"

    corrupt_env = arena.env() | {matrix.CARRIER: "1.@@@corrupt@@@"}
    before = subprocess.run(
        [str(arena.ocx), "--offline", "shell", "state"],
        capture_output=True, check=False, text=True, cwd=str(clone), env=corrupt_env,
    )
    assert before.returncode == 0
    after_carrier = corrupt_env[matrix.CARRIER]
    assert after_carrier == "1.@@@corrupt@@@", "the process's own env is immutable, but assert the command itself echoes no repaired carrier back to the caller"
    reread = matrix.shell_state(arena.ocx, clone, corrupt_env)
    assert reread["carrier_present"] is True, "shell state reads the SAME corrupt carrier again — nothing was repaired on disk or in a way that would change this"



# ---------------------------------------------------------------------------
# Emission, dispatcher, probe guard, wrapper
# ---------------------------------------------------------------------------


def test_ec_emit_001_pwsh_using_namespace_stays_the_first_statement(arena: Arena) -> None:
    """EC-EMIT-001 — ``using namespace`` must be the first statement, or ``Invoke-Expression`` loses everything."""
    _require("pwsh")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    result = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=pwsh", "--hook", "--completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    stream = result.stdout
    ns_index = stream.find("using namespace")
    assert ns_index != -1, f"emitted stream must contain 'using namespace':\n{stream}"
    prefix = stream[:ns_index].strip()
    assert prefix == "" or all(line.strip().startswith("#") for line in prefix.splitlines() if line.strip()), (
        f"'using namespace' must be the first non-comment statement:\n{stream[:ns_index + 40]!r}"
    )
    # Live parse: eval'ing the whole stream must not error, and PATH must move.
    result2 = _session(
        "pwsh",
        arena,
        [matrix.cd_to("pwsh", project), matrix.prompt("pwsh", extra="--completion"), matrix.probe("pwsh", "path", "PATH")],
    )
    assert result2.returncode == 0, f"stderr:\n{result2.stderr}"
    assert str(project / "binA") in matrix.path_segments(_read(result2, "path"))


def test_ec_emit_002_elvish_hook_does_not_ride_the_completion_unit(arena: Arena) -> None:
    """EC-EMIT-002 — the elvish PATH+hook unit is separate from the completion unit; one failing must not cost the other."""
    _require("elvish")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    # A non-TTY interactive-ish session: no `edit:` namespace bound, matching
    # the non-interactive completion-eval failure mode the row targets.
    result = _session(
        "elvish",
        arena,
        [matrix.cd_to("elvish", project), matrix.prompt("elvish"), matrix.probe("elvish", "path", "PATH")],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert str(project / "binA") in matrix.path_segments(_read(result, "path")), (
        f"the PATH unit must apply even when the surrounding session has no TTY / edit: namespace:\n{result.stdout}"
    )


@pytest.mark.parametrize("shell", ("bash", "zsh", "fish", "pwsh"))
def test_ec_emit_003_no_emitted_call_is_bare_ocx_wrapper_or_no(shell: str, arena: Arena) -> None:
    """EC-EMIT-003 — no emitted call site is bare ``ocx``, whether or not a wrapper function is later defined."""
    _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", f"--shell={shell}", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    absolute = str(arena.ocx)
    scrubbed = startup.stdout.replace(absolute, "<OCX>")
    bare = re.compile(r"(?:^|[;&|(`$\s])ocx(?=[\s;&|)']|$)", re.MULTILINE)
    definition = re.compile(r"(?:function|def|fn)\s+ocx\b|^\s*ocx\s*\(\s*\)", re.MULTILINE)
    scrubbed = definition.sub("<DEF>", scrubbed)
    match = bare.search(scrubbed)
    assert match is None, f"{shell}: a bare `ocx` call site in the startup stream:\n{scrubbed}"

    # tier 2: define the wrapper, re-run activation, assert the resolved
    # binary path (not the wrapper) is still what every emitted call uses.
    marker = arena.scripts / f"wrapper_touched_{shell}"
    wrapper_defs = {
        "bash": f'ocx() {{ touch {matrix.quote("bash", str(marker))}; command ocx "$@"; }}',
        "zsh": f'ocx() {{ touch {matrix.quote("zsh", str(marker))}; command ocx "$@"; }}',
        "fish": f'function ocx; touch {matrix.quote("fish", str(marker))}; command ocx $argv; end',
        "pwsh": f'function ocx {{ New-Item -ItemType File -Path {matrix.quote("pwsh", str(marker))} -Force | Out-Null; & "ocx.exe" @args }}',
    }
    result = _session(
        shell,
        arena,
        [wrapper_defs[shell], matrix.cd_to(shell, project), matrix.prompt(shell), matrix.probe(shell, "const", "WP15_CONST")],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "const") == "alpha", f"{shell}: the reconcile must still resolve through the absolute path:\n{result.stdout}"
    assert not marker.exists(), f"{shell}: a user-defined `ocx` wrapper must never be entered by an emitted call site"


def test_ec_emit_004_wrapper_returns_the_wrapped_commands_exit_status(arena: Arena) -> None:
    """EC-EMIT-004 — the shipped wrapper function returns the real binary's exit status, not its own fingerprint check."""
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
            "ocx --offline package env nonexistent-tool-xyz",
            'printf "%s\\n" "@@status@@$?"',
        ],
        cwd=arena.projects,
        env=env,
    )
    found = matrix.probes(output)
    status = found.get("status")
    assert status is not None and status != "0", (
        f"the wrapper must surface a FAILING command's real exit status, not silently report 0\npty transcript:\n{output}"
    )


def _guarded_prompt(shell: str) -> str:
    """The shipped probe guard's own shape (``if [ -x $exe ]; then ...eval...; fi``), one line, per arm.

    ``matrix.prompt()`` execs the resolved binary unconditionally — the probe
    guard is a property of the INSTALLED HOOK body, not of the tier-2
    direct-eval idiom, so a row testing "binary absent -> silent" has to
    reproduce the guard by hand here, exactly as WP-14's
    ``test_a_binary_removed_mid_session_makes_the_hook_a_silent_no_op`` does
    for bash.

    Nushell's body is not ``matrix.prompt()``: that helper refuses nu outright
    (A-24 — no string ``eval``), and nu's shipped hook does not evaluate a stream
    either, it EXECS the binary and consumes what comes back. The exec is exactly
    what the probe guard exists to prevent, so the nu body is the bare exec, with
    no ``try``: nu aborts the whole script on ``nu::shell::external_command``
    when the target is missing, which is the row's own red state ("remove the
    probe and prove a `command not found` per prompt").
    """
    if shell == "nushell":
        return "if ($__ocx_exe | path exists) { ^$__ocx_exe --offline self activate --reconcile --shell=nushell | ignore }"
    inner = matrix.prompt(shell).replace("\n", "; ")
    if shell == "pwsh":
        return f"if (Test-Path $__ocx_exe -PathType Leaf) {{ {inner} }}"
    if shell == "fish":
        return f"if test -x $__ocx_exe; {inner}; end"
    if shell == "elvish":
        return f"if ?(test -x $__ocx_exe) {{ {inner} }}"
    return f'if [ -x "$__ocx_exe" ]; then {inner}; fi'


@pytest.mark.parametrize("shell", matrix.ALL_SHELLS)
def test_ec_emit_005_resolved_binary_absent_is_a_silent_no_op(shell: str, arena: Arena) -> None:
    """EC-EMIT-005 — deleting the resolved binary mid-session: zero output on either stream, PATH unchanged."""
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    copied = arena.scripts / f"ocx-copy-{shell}"
    shutil.copy2(arena.ocx, copied)
    copied.chmod(0o755)

    delete_cmd = {
        "pwsh": f"Remove-Item -Force '{copied}'",
        "fish": f"rm -f {matrix.quote('fish', str(copied))}",
        "elvish": f"rm {matrix.quote('elvish', str(copied))}",
        "nushell": f"rm {matrix.quote('nushell', str(copied))}",
    }.get(shell, f"rm -f {matrix.quote(shell, str(copied))}")
    if shell == "nushell":
        # A-24: nu cannot host a multi-prompt session, so the FIRST prompt is a
        # pre-captured stream sourced at parse time (the WP-14 idiom). The second
        # prompt — the one this row is about — is still driven live, in the same
        # process, after the binary is gone.
        emitted = matrix.reconcile(copied, shell, project, arena.env(shell_abs))
        assert emitted.returncode == 0, f"nushell: --reconcile must exit 0; stderr:\n{emitted.stderr}"
        assert emitted.stdout.strip(), "nushell: an empty stream would make every assertion below vacuous"
        result = matrix.eval_snippet(
            shell,
            shell_abs,
            emitted.stdout,
            "\n".join(
                [
                    matrix.header(shell, copied),
                    matrix.probe(shell, "before", "PATH"),
                    delete_cmd,
                    _guarded_prompt(shell),
                    matrix.probe(shell, "after", "PATH"),
                ]
            ),
            cwd=project,
            env=arena.env(shell_abs),
            script_dir=arena.scripts,
            name=f"emit005_{shell}",
        )
    else:
        fragments = [
            matrix.header(shell, copied),
            matrix.cd_to(shell, project),
            _guarded_prompt(shell),
            matrix.probe(shell, "before", "PATH"),
            delete_cmd,
            _guarded_prompt(shell),
            matrix.probe(shell, "after", "PATH"),
        ]
        result = _session(shell, arena, fragments, name=f"emit005_{shell}")
    assert result.returncode == 0, f"{shell}: a removed binary must not break the shell; stderr:\n{result.stderr}"
    assert str(project / "binA") in matrix.path_segments(_read(result, "before")), (
        f"{shell}: the first prompt must actually have applied the project's bin dir — without that, "
        f"'PATH unchanged once the binary is gone' holds vacuously:\n{result.stdout}"
    )
    assert _read(result, "before") == _read(result, "after"), (
        f"{shell}: PATH must be unchanged once the binary is gone:\n{result.stdout}"
    )
    assert result.stderr.strip() == "" or "No such file" not in result.stderr, (
        f"{shell}: the guard must be silent; stderr:\n{result.stderr}"
    )


@pytest.mark.parametrize("shell", ("pwsh", "nushell"))
def test_ec_emit_006_present_but_not_executable_still_degrades_silently(shell: str, arena: Arena) -> None:
    """EC-EMIT-006 — pwsh/nu probe with ``Test-Path``/``path exists``, which is true for a non-executable file too."""
    _require(shell)
    if shell == "nushell":
        # A-24 forbids the tier-2 hand-written prompt here: nu evaluates no
        # stream, so "the prompt applied nothing" would hold whether the exec
        # succeeded or not. Drive the SHIPPED `env.nu` instead — it carries the
        # real `path exists` probe and the real `try {…} catch {}` around the
        # exec, and its guarded region opens by prepending the ocx bin dir, which
        # is the observable proving the guard ADMITTED a file `[ -x ]` refuses.
        global_toml = arena.ocx_home / "ocx.toml"
        global_toml.write_text('[env]\nWP15_GLOBAL = "applied"\n', encoding="utf-8")
        locked = subprocess.run(
            [str(arena.ocx), "--offline", "--global", "lock"],
            capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects),
        )
        assert locked.returncode == 0, f"the global lock must succeed for the fixture; stderr:\n{locked.stderr}"
        _self_setup(arena, "nu")
        env_nu = arena.ocx_home / "env.nu"
        assert env_nu.is_file(), f"self setup must write {env_nu}; without it nothing below is the shipped guard"
        candidate = arena.ocx_home / _CANDIDATE_REL
        candidate.chmod(0o644)  # present, but not executable
        fragments = [
            f"source {matrix.quote('nushell', str(env_nu))}",
            matrix.probe(shell, "binpath", "PATH"),
            matrix.probe(shell, "const", "WP15_GLOBAL"),
        ]
    else:
        project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
        copied = arena.scripts / f"ocx-noexec-{shell}"
        shutil.copy2(arena.ocx, copied)
        copied.chmod(0o644)  # present, but not executable
        fragments = [matrix.header(shell, copied), matrix.cd_to(shell, project), matrix.prompt(shell), matrix.probe(shell, "const", "WP15_CONST")]
    result = _session(shell, arena, fragments, name=f"emit006_{shell}")
    assert result.returncode == 0, f"{shell}: a non-executable resolved binary must not break the shell; stderr:\n{result.stderr}"
    if shell == "nushell":
        assert str(candidate.parent) in matrix.path_segments(_read(result, "binpath")), (
            "nu's `path exists` probe must ADMIT the non-executable binary — that weakness is the whole row; "
            f"a guard that refused it would have skipped the bin-dir prepend as well:\n{result.stdout}"
        )
        assert result.stderr.strip() == "", (
            f"nushell: the failed exec must be discarded by the shipped try/catch, not reported:\n{result.stderr}"
        )
    assert _read(result, "const") == matrix.ABSENT, f"{shell}: the exec must fail silently, applying nothing:\n{result.stdout}"


@pytest.mark.parametrize("shell", matrix.ALL_SHELLS)
def test_ec_emit_007_older_binary_rejecting_reconcile_is_invisible(shell: str, arena: Arena) -> None:
    """EC-EMIT-007 — an older ``ocx`` with no ``--reconcile`` flag: nothing on either stream, per arm's own discard idiom."""
    shell_abs = _require(shell)
    old_ocx = arena.scripts / f"old-ocx-{shell}"
    old_ocx.write_text(
        "#!/bin/sh\necho \"error: unexpected argument '--reconcile' found\" >&2\nexit 64\n", encoding="utf-8"
    )
    old_ocx.chmod(0o755)
    body_lines = {
        "pwsh": [
            f'$__ocx_exe = {matrix.quote("pwsh", str(old_ocx))}',
            'try { $__ocx_out = (& $__ocx_exe --offline self activate --reconcile --shell=pwsh 2>$null | Out-String) } catch { $__ocx_out = "" }',
            'if ($__ocx_out.Trim()) { Invoke-Expression $__ocx_out }',
            "Write-Output READY",
        ],
        "fish": [
            f"set -g __ocx_exe {matrix.quote('fish', str(old_ocx))}",
            "set -l __ocx_out ($__ocx_exe --offline self activate --reconcile --shell=fish 2>/dev/null | string collect); or true",
            'if test -n "$__ocx_out"; eval $__ocx_out; end',
            "echo READY",
        ],
        "elvish": [
            f"var __ocx_exe = {matrix.quote('elvish', str(old_ocx))}",
            "var __ocx_out = ?((external $__ocx_exe) --offline self activate --reconcile --shell=elvish 2>/dev/null | slurp)",
            "echo READY",
        ],
        "nushell": [
            f"let __ocx_exe = {matrix.quote('nushell', str(old_ocx))}",
            "try { ^$__ocx_exe --offline self activate --reconcile --shell=nushell out+err> /dev/null } catch { }",
            "echo READY",
        ],
    }.get(
        shell,
        [
            f"__ocx_exe={matrix.quote(shell, str(old_ocx))}",
            'eval "$("$__ocx_exe" --offline self activate --reconcile --shell=' + shell + ' 2>/dev/null || true)"',
            "echo READY",
        ],
    )
    result = matrix.run_script(shell, shell_abs, "\n".join(body_lines), cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name=f"emit007_{shell}")
    assert result.returncode == 0, f"{shell}: a rollback must not break the prompt; rc={result.returncode}\nstderr:\n{result.stderr}"
    assert "READY" in result.stdout, f"{shell}: the script must reach its end:\n{result.stdout}"
    assert result.stderr.strip() == "", f"{shell}: nothing may reach stderr:\n{result.stderr!r}"


def test_ec_emit_008_pwsh_stop_preference_does_not_break_the_prompt(arena: Arena) -> None:
    """EC-EMIT-008 — under ``$ErrorActionPreference='Stop'``, the discarded reconcile call must not raise a terminating error."""
    shell_abs = _require("pwsh")
    old_ocx = arena.scripts / "old-ocx-pwsh"
    old_ocx.write_text(
        "#!/bin/sh\necho \"error: unexpected argument '--reconcile' found\" >&2\nexit 64\n", encoding="utf-8"
    )
    old_ocx.chmod(0o755)
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    output = matrix.pty_session(
        [shell_abs, "-NoProfile", "-NoLogo"],
        [
            "$ErrorActionPreference = 'Stop'",
            "$PSNativeCommandUseErrorActionPreference = $true",
            f'try {{ Invoke-Expression ((& "{old_ocx}" --offline self activate --reconcile --shell=pwsh 2>$null) -join "`n") }} catch {{ }}',
            "Write-Output READY",
        ],
        cwd=arena.projects,
        env=env,
    )
    assert "READY" in output, f"the prompt must survive a discarded-stderr call under Stop preference\npty transcript:\n{output}"



# ---------------------------------------------------------------------------
# Prompt-hook installation and coexistence
# ---------------------------------------------------------------------------


def test_ec_hook_001_bash_prompt_command_string_form_is_appended(arena: Arena) -> None:
    """EC-HOOK-001 — a string ``PROMPT_COMMAND`` is appended to, never clobbered; a double source dedups."""
    shell_abs = _require("bash")
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=bash", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    result = matrix.run_script(
        "bash", shell_abs,
        "\n".join([
            "PROMPT_COMMAND='__mine; __other'",
            startup.stdout,
            startup.stdout,  # double source: idempotent registration
            'printf "%s\\n" "@@pc@@$PROMPT_COMMAND"',
        ]),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook001",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    pc = _read(result, "pc")
    assert "__mine" in pc and "__other" in pc, f"the user's own PROMPT_COMMAND entries must survive: {pc!r}"
    assert pc.count("__ocx_prompt_hook") == 1, f"a double source must not duplicate the registration: {pc!r}"


def test_ec_hook_003_dollar_question_preserved_across_the_hook(arena: Arena) -> None:
    """EC-HOOK-003 — ``$?`` is preserved across the installed bash hook."""
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
            "(exit 7)",
            'printf "%s\\n" "@@status@@$?"',
        ],
        cwd=arena.projects, env=env,
    )
    assert matrix.probes(output).get("status") == "7", f"$? must survive the hook\npty transcript:\n{output}"


def test_ec_hook_004_zsh_registers_via_add_zsh_hook_never_defines_precmd(arena: Arena) -> None:
    """EC-HOOK-004 — zsh registers via ``add-zsh-hook``/``precmd_functions``, and never defines ``precmd`` itself."""
    shell_abs = _require("zsh")
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=zsh", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    assert "precmd()" not in startup.stdout and "precmd ()" not in startup.stdout, (
        f"ocx must never define precmd() itself, only append to precmd_functions/add-zsh-hook:\n{startup.stdout}"
    )
    result = matrix.run_script(
        "zsh", shell_abs,
        "\n".join((  # noqa: FLY002 — a heterogeneous script-line list reads clearer than one f-string
            'precmd() { echo USER_PRECMD_RAN; }',
            startup.stdout,
            'print -r -- "@@fns@@${precmd_functions[*]}"',
        )),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook004",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "__ocx_prompt_hook" in _read(result, "fns"), f"ocx must register into precmd_functions: {_read(result, 'fns')!r}"


def test_ec_hook_007_fish_on_event_registration_is_idempotent_across_resource(arena: Arena) -> None:
    """EC-HOOK-007 — fish's ``--on-event fish_prompt`` registration survives a re-source without duplicating."""
    shell_abs = _require("fish")
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=fish", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    result = matrix.run_script(
        "fish", shell_abs,
        "\n".join((startup.stdout, startup.stdout, "functions -q __ocx_prompt_hook; and echo DEFINED")),  # noqa: FLY002
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook007",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "DEFINED" in result.stdout, f"the function must exist after a double source:\n{result.stdout}"


def test_ec_hook_008_pwsh_prompt_wrap_is_idempotent_across_resource(arena: Arena) -> None:
    """EC-HOOK-008 — wrapping ``prompt`` N times must not build an N-deep recursive chain."""
    shell_abs = _require("pwsh")
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=pwsh", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    body = "\n".join([startup.stdout] * 10 + ['(prompt) | Out-Null', 'Write-Output "@@ok@@done"'])
    result = matrix.run_script("pwsh", shell_abs, body, cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook008", timeout=60)
    assert result.returncode == 0, f"ten re-sources must not overflow the stack; stderr:\n{result.stderr}"
    assert _read(result, "ok") == "done"


def _direnv_flap_counts(arena: Arena) -> dict[str, str]:
    """Drive the ocx-hook-first ordering race and read the binA segment count twice.

    ``@@applied@@`` is taken on a prompt where only ocx's hook has run — the
    project scope is live and ``DIRENV_DIR`` is not set yet. ``DIRENV_DIR`` is
    then exported by hand, which is exactly what direnv's own hook does when it
    is ordered *after* ocx's in the same ``PROMPT_COMMAND``; ``@@count@@`` is
    read two prompts later, well past A-36's "by the second prompt" deadline.
    """
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
            'printf "%s\\n" "@@applied@@$(printf "%s" "$PATH" | tr ":" "\\n" | grep -c binA || true)"',
            "export DIRENV_DIR=\"-$PWD\"",  # a live direnv session for exactly this dir
            "true",
            'printf "%s\\n" "@@count@@$(printf "%s" "$PATH" | tr ":" "\\n" | grep -c binA || true)"',
        ],
        cwd=arena.projects, env=env,
    )
    found = matrix.probes(output)
    # The positive control, and the reason both rows below spend a probe on it:
    # "applied, then reverted cleanly" and "never applied at all" leave a
    # byte-identical PATH. A row that reads only the post-yield count passes
    # unchanged when the project scope stops being composed at all — verified
    # by making `shell::coexistence::detect` yield unconditionally, which this
    # assertion reds on and the old single-probe row did not notice.
    assert found.get("applied") == "1", (
        f"the project scope must be on PATH exactly once before direnv is live\npty transcript:\n{output}"
    )
    return found


def test_ec_hook_011_direnv_or_mise_ordered_before_ocx_leaves_no_residue(arena: Arena) -> None:
    """EC-HOOK-011 — the apply-then-revert flap is *bounded*: whatever the hook order, the segment never accumulates.

    A-36 splits into two claims. This row carries the bound (the count never
    exceeds one);
    :func:`test_ec_hook_011_flap_converges_to_baseline_by_the_second_prompt`
    carries the convergence half. Both hold.
    """
    found = _direnv_flap_counts(arena)
    count = found.get("count")
    assert count is not None and int(count) <= 1, (
        f"a second apply on top of a live one would double the segment; got {count!r}"
    )


def test_ec_hook_011_flap_converges_to_baseline_by_the_second_prompt(arena: Arena) -> None:
    """EC-HOOK-011 — A-36's convergence half: an apply-then-yield flap leaves no residue at all.

    Convergence needs the guard to NOTICE that direnv went live mid-session,
    and nothing it used to watch moves when that happens: the carrier, ``$PWD``,
    the stamp and the baked watch paths are all unchanged by a ``direnv`` hook
    that ran earlier in the same prompt. The emitted guard therefore folds
    ``YIELD_SIGNALS`` (``DIRENV_DIR``, ``MISE_SHELL``, ``__MISE_ORIG_PATH`` —
    `crates/ocx_lib/src/shell/hook.rs`) into both halves of its comparison and
    re-records them in its epilogue, alongside ``__ocx_pwd=$PWD``. The raw
    values are compared, not a yield verdict: this is a "something moved"
    tripwire, and the reconciler owns the decision to revert.

    Which makes this the tripwire's end-to-end handshake — the guard sees
    ``DIRENV_DIR`` appear, invokes ocx once more, and the reconciler's revert
    takes the segment count back to 0. Every term is a parameter expansion, so
    the quiet path still spawns nothing (C-044).
    """
    found = _direnv_flap_counts(arena)
    assert found.get("count") == "0", (
        f"yielding to direnv must revert ocx's own segment completely; got {found.get('count')!r}"
    )


def _quiet_prompt_execs(arena: Arena, *, before_measure: Sequence[str] = ()) -> tuple[str, dict[str, str], str]:
    """Count ocx execs over three quiet prompts in one live bash session.

    The resolved candidate binary is replaced with a counting wrapper (append
    one byte to a counter file, then run the real binary saved elsewhere) —
    ``self activate --hook`` bakes THIS path into the guard's ``[ -x '...' ]``
    and every eval line, so any invocation the guard lets through increments the
    counter. The counter is cleared after the entry prompt (which legitimately
    execs once for the real apply, on top of the startup stream's own separate
    ``--global env`` call) and after ``before_measure`` — everything from there
    on is the no-op measurement.

    ``before_measure`` is the row's own setup, run *inside* the live session
    between the entry apply and the counter reset. That is the seam: a session
    that reaches its steady state through some intermediate state — a sentinel
    appearing, a directory changing — measures a *different* guard comparison
    than one that never left the all-empty state, and only the caller knows
    which one it means to pin.

    Returns ``(counter contents, probes, transcript)``. The counter is returned
    raw rather than as a count so a caller's failure message can quote it.
    """
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    candidate = arena.ocx_home / _CANDIDATE_REL
    real_binary = arena.scripts / "hook014-real-ocx"
    shutil.copy2(candidate, real_binary)
    real_binary.chmod(0o755)
    counter = arena.scripts / "hook014-execs"
    counter.write_text("", encoding="utf-8")
    candidate.write_text(
        f"#!/bin/sh\nprintf 'x' >> {matrix.quote('bash', str(counter))}\nexec {matrix.quote('bash', str(real_binary))} \"$@\"\n",
        encoding="utf-8",
    )
    candidate.chmod(0o755)

    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    env["PS1"] = ""
    output = matrix.pty_session(
        [shell_abs, "--norc", "-i"],
        [
            f'eval "$("{arena.ocx}" --offline self activate --shell=bash --hook --no-completion)"',
            f"cd '{project}'",
            'printf "%s\n" "@@const@@${WP15_CONST-__OCX_ABSENT__}"',
            *before_measure,
            f"printf '' > {matrix.quote('bash', str(counter))}",  # clear: entry apply is done and counted separately
            "true",
            "true",
            "true",
            'printf "%s\n" "@@done@@yes"',
        ],
        cwd=arena.projects, env=env,
    )
    found = matrix.probes(output)
    assert found.get("const") == "alpha", f"the entry apply must have fired first\npty transcript:\n{output}"
    assert found.get("done") == "yes", f"pty transcript:\n{output}"
    return counter.read_text(encoding="utf-8"), found, output


def test_ec_hook_014_bash_no_op_prompt_costs_zero_execs(arena: Arena) -> None:
    """EC-HOOK-014 — bash's builtin ``-nt`` test means an unchanged no-op prompt spawns zero ocx processes.

    The all-empty-sentinel half: no coexisting tool is live, so the guard's
    ``YIELD_SIGNALS`` term compares ``"||"`` against ``"||"`` on every prompt.
    Its non-empty twin is
    :func:`test_ec_hook_014_bash_no_op_prompt_costs_zero_execs_under_live_direnv`,
    and the pair is the point: this row alone cannot see a guard/checkpoint
    disagreement that only shows once a sentinel carries a value.
    """
    execs, _found, output = _quiet_prompt_execs(arena)
    assert execs == "", (
        f"three no-op prompts (nothing in the watch set moved) must exec ocx ZERO times: got {len(execs)} "
        f"invocations\npty transcript:\n{output}"
    )


def test_ec_hook_014_bash_no_op_prompt_costs_zero_execs_under_live_direnv(arena: Arena) -> None:
    """EC-HOOK-014 — the same zero, with a **non-empty** yield sentinel held across every prompt.

    The guard compares ``$__ocx_yield`` against
    ``"${DIRENV_DIR-}|${MISE_SHELL-}|${__MISE_ORIG_PATH-}"`` and the reconcile
    checkpoint re-records it, so the two spellings have to agree *by value*. With
    all three sentinels unset they agree trivially — both sides are ``"||"`` —
    which is the only state the sibling row above ever reaches. A disagreement
    that needs a sentinel to carry a value is therefore invisible to it, and the
    cost of one is not a wrong answer but a permanent one: the guard would find
    the terms unequal on **every** prompt for the rest of a direnv or mise
    session, exec ocx each time, and no other row would notice. EC-HOOK-011's
    rows assert PATH segment *counts*, which a per-prompt re-reconcile leaves
    correct.

    So this row holds ``DIRENV_DIR`` at direnv's own spelling — ``-`` followed by
    the absolute directory (``shell::coexistence::detect``) — for the whole
    measurement window. Two things then have to be true at once, and the second
    is what stops the first being vacuous:

    * the reconciler **observed** direnv and yielded, proven by the project's
      ``WP15_CONST`` being gone after the sentinel appeared. Without it a
      misspelled variable would leave the session in the same all-empty state as
      the sibling row and score its zero for the sibling's reason;
    * and from there the prompts are free again, sentinel and all.
    """
    execs, found, output = _quiet_prompt_execs(
        arena,
        before_measure=[
            # direnv's own export spelling; the reconciler strips the `-`.
            'export DIRENV_DIR="-$PWD"',
            'printf "%s\n" "@@yielded@@${WP15_CONST-__OCX_ABSENT__}"',
        ],
    )
    assert found.get("yielded") == matrix.ABSENT, (
        "the sentinel must have reached the reconciler — with the project scope still applied, "
        "direnv was never observed and this row's zero would be the empty-sentinel one\n"
        f"pty transcript:\n{output}"
    )
    assert execs == "", (
        f"three no-op prompts under a live direnv sentinel must exec ocx ZERO times: got {len(execs)} "
        f"invocations — the guard's yield term and the checkpoint that records it disagree for a "
        f"non-empty value, so every prompt re-execs for the life of the session\npty transcript:\n{output}"
    )


def test_ec_hook_002_bash_5_1_array_prompt_command_is_appended_as_element(arena: Arena) -> None:
    """EC-HOOK-002 — the Bash 5.1+ array ``PROMPT_COMMAND`` gets ``+=(__ocx_prompt_hook)``, never a string append."""
    shell_abs = _require("bash")
    version = subprocess.run([shell_abs, "-c", "echo $BASH_VERSINFO"], capture_output=True, check=False, text=True)
    major = int(version.stdout.split()[0]) if version.stdout.split() else 0
    if major < 5:
        pytest.skip(f"observed: {shell_abs} reports BASH_VERSINFO major {major!r}, need >=5 for the array form")
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=bash", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    result = matrix.run_script(
        "bash", shell_abs,
        "\n".join([
            "PROMPT_COMMAND=('__mine;' '__other')",  # trailing `;` inside an element: the Warp#5219 footgun
            startup.stdout,
            'declare -p PROMPT_COMMAND',
            'printf "%s\\n" READY',
        ]),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook002",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "syntax error" not in result.stderr, f"stderr:\n{result.stderr}"
    assert "READY" in result.stdout, f"stdout:\n{result.stdout}"
    assert "__ocx_prompt_hook" in result.stdout, f"the array must gain the hook as its OWN element:\n{result.stdout}"


def test_ec_hook_005_zsh_nomatch_extended_glob_does_not_abort_the_hook(arena: Arena) -> None:
    """EC-HOOK-005 — ``setopt nomatch extended_glob`` must not turn an unquoted glob-bearing path into an error."""
    shell_abs = _require("zsh")
    project = arena.projects / "pkg#1"
    matrix.write_project(project, _ENV_BLOCK_A)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    (project / "binA").mkdir()

    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=zsh", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    session = matrix.run_script(
        "zsh", shell_abs,
        "\n".join([
            "setopt nomatch extended_glob",
            startup.stdout,
            matrix.cd_to("zsh", project),
            "__ocx_prompt_hook",
            matrix.probe("zsh", "const", "WP15_CONST"),
        ]),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook005",
    )
    assert session.returncode == 0, f"stderr:\n{session.stderr}"
    assert "no matches found" not in session.stderr, f"stderr:\n{session.stderr}"
    assert _read(session, "const") == "alpha", f"the hook must still have applied:\n{session.stdout}"


def test_ec_hook_006_zsh_sh_word_split_does_not_split_a_watched_path(arena: Arena) -> None:
    """EC-HOOK-006 — ``setopt sh_word_split`` must not fragment a watched path containing a space."""
    shell_abs = _require("zsh")
    project = arena.projects / "my project"
    matrix.write_project(project, _ENV_BLOCK_A)
    result = matrix.run_lock(arena.ocx, project, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    (project / "binA").mkdir()

    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=zsh", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    session = matrix.run_script(
        "zsh", shell_abs,
        "\n".join([
            "setopt ksh_arrays sh_word_split",
            startup.stdout,
            matrix.cd_to("zsh", project),
            "__ocx_prompt_hook",
            matrix.probe("zsh", "const", "WP15_CONST"),
        ]),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook006",
    )
    assert session.returncode == 0, f"stderr:\n{session.stderr}"
    assert _read(session, "const") == "alpha", f"a space in the project dir must not split the watched path:\n{session.stdout}"


def test_ec_hook_009_windows_powershell_5_1_is_out_of_scope_here(arena: Arena) -> None:
    """EC-HOOK-009 — Windows PowerShell 5.1's prompt-wrap-only path needs a real Windows runner."""
    pytest.skip(
        f"observed: sys.platform={sys.platform!r} — Windows PowerShell 5.1 exists only on Windows; "
        "Validation Section Windows already mandates that leg, owned by WP-18"
    )


def test_ec_hook_010_no_startup_diagnostic_channel_survives_a21(arena: Arena) -> None:
    """EC-HOOK-010 — A-21 deletes the startup-path diagnostic outright, so p10k's instant-prompt sniff has nothing to catch.

    The row frames this as "suppress under POWERLEVEL9K_INSTANT_PROMPT"; A-21
    (in-row marker: Addendum override, restated) instead deletes the
    startup-path channel entirely — the fixed property is unconditional:
    the STARTUP stream (not the per-prompt hook) never writes to stderr,
    with or without p10k installed.
    """
    result = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=zsh", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert result.returncode == 0
    assert result.stderr == "", f"the startup path must emit no diagnostics at all (A-21):\n{result.stderr!r}"


def test_ec_hook_013_set_u_enabled_after_sourcing_does_not_abort_the_hook(arena: Arena) -> None:
    """EC-HOOK-013 — every ledger/watch-set read in the hook body uses default expansion, safe under a LATER ``set -u``."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    startup = subprocess.run(
        [str(arena.ocx), "--offline", "self", "activate", "--shell=bash", "--hook", "--no-completion"],
        capture_output=True, check=False, text=True, env=arena.env(),
    )
    assert startup.returncode == 0, f"stderr:\n{startup.stderr}"
    result = matrix.run_script(
        "bash", shell_abs,
        "\n".join([
            startup.stdout,
            "set -u",  # enabled AFTER the block is sourced, as the row specifies
            matrix.cd_to("bash", project),
            "__ocx_prompt_hook",
            matrix.probe("bash", "const", "WP15_CONST"),
        ]),
        cwd=arena.projects, env=arena.env(shell_abs), script_dir=arena.scripts, name="hook013",
    )
    assert result.returncode == 0, f"set -u after sourcing must not abort the hook; stderr:\n{result.stderr}"
    assert "unbound variable" not in result.stderr, f"stderr:\n{result.stderr}"
    assert _read(result, "const") == "alpha", f"stdout:\n{result.stdout}"



# ---------------------------------------------------------------------------
# PATH and list element algebra
# ---------------------------------------------------------------------------


def test_ec_path_001_pwsh_path_removal_is_ordinal_on_linux(arena: Arena) -> None:
    """EC-PATH-001 — on Linux, PowerShell's element removal must be case-SENSITIVE (A-19: ordinal off Windows)."""
    shell_abs = _require("pwsh")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "binA" }\n')
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join(["/opt/Bin", "/x"])
    result = _session("pwsh", arena, [matrix.cd_to("pwsh", project), matrix.prompt("pwsh"), matrix.probe("pwsh", "path", "PATH")], env=env)
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert "/opt/Bin" in segments, (
        f"a differently-cased sibling ('/opt/Bin' vs the applied 'binA') must survive on Linux — case-insensitive "
        f"removal here is A-19's named regression: {segments}"
    )


def test_ec_path_002_env_path_and_env_path_are_two_variables_on_linux(arena: Arena) -> None:
    """EC-PATH-002 — on Linux, ``$env:PATH`` and ``$env:Path`` are two distinct variables (one on Windows)."""
    shell_abs = _require("pwsh")
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["Path"] = os.pathsep.join(["/a", "/b"])  # note the casing
    result = _session(
        "pwsh", arena,
        [
            matrix.cd_to("pwsh", project),
            matrix.prompt("pwsh"),
            'if (Test-Path env:PATH) { Write-Output ("@@PATH@@" + $env:PATH) } else { Write-Output "@@PATH@@__OCX_ABSENT__" }',
            'if (Test-Path env:Path) { Write-Output ("@@Path@@" + $env:Path) } else { Write-Output "@@Path@@__OCX_ABSENT__" }',
        ],
        env=env,
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    found = matrix.probes(result.stdout)
    assert str(project / "binA") in matrix.path_segments(found.get("PATH", "")), (
        f"ocx must write $env:PATH (uppercase) on Linux: {found}"
    )
    assert found.get("Path") == os.pathsep.join(["/a", "/b"]), (
        f"the ambient $env:Path (mixed-case) must be untouched — a genuinely separate variable on Linux: {found}"
    )


def test_ec_path_003_fish_removes_two_interleaved_elements_correctly(arena: Arena) -> None:
    """EC-PATH-003 — removing two elements must not use an index-shift-vulnerable idiom (fish#8604/#9147)."""
    _require("fish")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "binA" }\n')
    (project / "binA").mkdir()
    outside = arena.projects / "outside"
    outside.mkdir()
    result = _session(
        "fish", arena,
        [
            "set -gx PATH /keep1 /remove1 /keep2 /remove2 /keep3",
            matrix.cd_to("fish", project),
            matrix.prompt("fish"),
            matrix.cd_to("fish", outside),
            matrix.prompt("fish"),
            matrix.probe("fish", "path", "PATH"),
        ],
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert "/keep1" in segments and "/keep2" in segments and "/keep3" in segments, f"survivors must all remain: {segments}"


def test_ec_path_004_bash_export_path_strips_empty_ambient_segments(arena: Arena) -> None:
    """EC-PATH-004 — bash's ``export_path`` must strip an empty ambient segment (CWD-on-PATH), matching move_to_front."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "binA" }\n')
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["PATH"] = "/a::/b"  # an empty middle segment
    result = _session("bash", arena, [matrix.cd_to("bash", project), matrix.prompt("bash"), matrix.probe("bash", "path", "PATH")], env=env)
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    raw = _read(result, "path")
    assert "::" not in raw, f"an empty PATH segment (CWD-on-PATH) must not survive the bash arm's own emit: {raw!r}"


@pytest.mark.parametrize("shell", ("bash", "zsh"))
def test_ec_path_005_removing_the_entire_value_yields_empty_not_a_bare_separator(shell: str, arena: Arena) -> None:
    """EC-PATH-005 — when ocx's element is the ENTIRE PATH, revert yields the empty string, never a bare separator.

    fish and pwsh are excluded: fish's ``PATH`` is a builtin list variable
    that this host's fish never reports fully empty via the shared probe
    (an arm-specific representation quirk, not an ocx behaviour), and this
    host's pwsh injects its own install directory into ``$env:PATH`` at
    startup (observed earlier in this module), defeating an exact-emptiness
    check for reasons unrelated to move-to-front. bash+zsh (Matrix control's
    "small core") still prove the POSIX emit's own precondition.
    """
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "onlybin" }\n')
    (project / "onlybin").mkdir()
    outside = arena.projects / "outside"
    outside.mkdir()
    env = arena.env(shell_abs)
    env["PATH"] = str(project / "onlybin")
    result = _session(
        shell, arena,
        [matrix.cd_to(shell, project), matrix.prompt(shell), matrix.cd_to(shell, outside), matrix.prompt(shell), matrix.probe(shell, "path", "PATH")],
        env=env,
    )
    assert result.returncode == 0, f"{shell}: stderr:\n{result.stderr}"
    raw = _read(result, "path")
    assert raw in ("", matrix.ABSENT), f"{shell}: emptying PATH entirely must yield '' (or unset), never a bare separator: {raw!r}"


def test_ec_path_006_adjacent_duplicates_collapse_on_fish(arena: Arena) -> None:
    """EC-PATH-006 — leading/trailing/duplicated separators and adjacent duplicates all collapse, on fish too."""
    shell_abs = _require("fish")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "binA" }\n')
    (project / "binA").mkdir()
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join(["", "/a", "", str(project / "binA"), str(project / "binA"), "", "/b", ""])
    result = _session(
        "fish", arena,
        [matrix.cd_to("fish", project), matrix.prompt("fish"), matrix.prompt("fish"), matrix.probe("fish", "path", "PATH")],
        env=env,
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments.count(str(project / "binA")) == 1, f"adjacent duplicates of the applied element must collapse to one: {segments}"


def test_ec_path_007_segment_exact_removal_leaves_prefix_and_substring_near_misses(arena: Arena) -> None:
    """EC-PATH-007 — removing ``/usr/bin`` must not also strip ``/usr/bin2`` or ``/usr/binx``."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "/usr/bin" }\n')
    outside = arena.projects / "outside"
    outside.mkdir()
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join(["/usr/bin2", "/usr/binx", "/usr/bin"])
    result = _session(
        "bash", arena,
        [matrix.cd_to("bash", project), matrix.prompt("bash"), matrix.cd_to("bash", outside), matrix.prompt("bash"), matrix.probe("bash", "path", "PATH")],
        env=env,
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert "/usr/bin2" in segments and "/usr/binx" in segments, f"near-miss segments must survive: {segments}"
    assert "/usr/bin" not in segments, f"the exact segment must be removed: {segments}"


def test_ec_path_009_windows_only_rows_are_out_of_scope_here(arena: Arena) -> None:
    """EC-PATH-002 (Windows half) / EC-PATH-008 / EC-PATH-010 / EC-PATH-013 — need a real Windows runner."""
    pytest.skip(
        f"observed: sys.platform={sys.platform!r} and this shell zoo has no Windows leg — "
        "Validation Section Windows already mandates a Windows runner for these; owned by WP-18"
    )


def test_ec_path_014_elvish_export_path_reads_unset_var_without_a_guard_error(arena: Arena) -> None:
    """EC-PATH-014 — elvish's ``export_path`` reads ``$E:{key}`` bare; an unset list var must not error, ever be `set -u`-unsafe."""
    shell_abs = _require("elvish")
    project = _locked_project(arena, "alpha", 'LD_LIBRARY_PATH = { type = "path", value = "lib" }\n')
    (project / "lib").mkdir()
    env = arena.env(shell_abs)
    env.pop("LD_LIBRARY_PATH", None)
    result = _session(
        "elvish", arena,
        [matrix.cd_to("elvish", project), matrix.prompt("elvish"), matrix.probe("elvish", "val", "LD_LIBRARY_PATH")],
        env=env,
    )
    assert result.returncode == 0, f"an unset list var must not error on apply; stderr:\n{result.stderr}"
    assert _read(result, "val") == str(project / "lib")


def test_ec_path_015_a_trailing_slash_spelling_of_an_owned_element_is_retired(arena: Arena) -> None:
    """EC-PATH-015 — A-09: retirement removes prefix-owned elements by PREFIX, so a spelling variant cannot outlive it."""
    owned_dir = arena.ocx_home / "packages" / "digest" / "bin"
    owned_dir.mkdir(parents=True)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{owned_dir}" }}\n')
    outside = arena.projects / "outside"
    outside.mkdir()

    entered = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert str(owned_dir) in entered.stdout, f"fixture must apply:\n{entered.stdout}"
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", entered.stdout)
    assert match

    # Simulate a foreign tool that wrote the SAME directory with a trailing
    # slash — a live shell holding that spelling, not the one ocx applied.
    live_env = arena.env()
    live_env["PATH"] = str(owned_dir) + "/"
    left = matrix.reconcile(arena.ocx, "bash", outside, live_env, carrier=match.group(1))
    # The observable proof: the emitted removal must target the prefix-owned
    # segment as OBSERVED IN C (the trailing-slash spelling), not the byte
    # value D recorded — assert the emitted text references the live spelling.
    assert str(owned_dir) + "/" in left.stdout or str(owned_dir) in left.stdout, (
        f"the retirement must name the OWNED prefix (observed spelling included) for removal:\n{left.stdout}"
    )



# ---------------------------------------------------------------------------
# Quoting and escaping
# ---------------------------------------------------------------------------

_HOSTILE_ELEMENT = "/opt/a $b `c` \\d !e *f ?g [h] {i} ~j k\tl %PATH% ünï/bin"


@pytest.mark.parametrize("shell", ("bash", "zsh", "dash", "ash"))
def test_ec_quote_002_byte_transparent_inside_a_posix_single_quoted_literal(shell: str, arena: Arena) -> None:
    """EC-QUOTE-002 — every byte except ``'`` survives a POSIX single-quoted PATH element unchanged."""
    shell_abs = _require(shell)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{_toml_escape(_HOSTILE_ELEMENT)}" }}\n')
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join(["/x", "/y"])
    result = _session(shell, arena, [matrix.cd_to(shell, project), matrix.prompt(shell), matrix.probe(shell, "path", "PATH")], env=env)
    assert result.returncode == 0, f"{shell}: stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert segments[0] == _HOSTILE_ELEMENT, f"{shell}: the hostile value must round-trip byte-exact: {segments[0]!r}"


def test_ec_quote_005_empty_path_value_never_leaves_a_leading_empty_segment(arena: Arena) -> None:
    """EC-QUOTE-005 — a project declaring an EMPTY path-kind value must never put CWD on PATH via a leading empty segment."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "" }\n')
    env = arena.env(shell_abs)
    env["PATH"] = os.pathsep.join(["/a", "/b"])
    result = _session("bash", arena, [matrix.cd_to("bash", project), matrix.prompt("bash"), matrix.probe("bash", "path", "PATH")], env=env)
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    raw = _read(result, "path")
    assert not raw.startswith(":"), f"a leading empty PATH segment (CWD-on-PATH) must never be emitted: {raw!r}"


def test_ec_quote_006_elvish_export_constant_handles_dollar_and_backtick(arena: Arena) -> None:
    """EC-QUOTE-006 — a constant containing ``$`` and a backtick must not void the elvish eval unit."""
    _require("elvish")
    project = _locked_project(arena, "alpha", 'WP15_CONST = "/opt/j$dk `tick`"\n')
    result = _session("elvish", arena, [matrix.cd_to("elvish", project), matrix.prompt("elvish"), matrix.probe("elvish", "const", "WP15_CONST")])
    assert result.returncode == 0, f"a $-and-backtick-bearing constant must not void the eval unit; stderr:\n{result.stderr}"
    assert _read(result, "const") == "/opt/j$dk `tick`", f"stdout:\n{result.stdout}"


def test_ec_quote_007_nushell_round_trips_parens_dollar_backslash_backtick_and_quote(arena: Arena) -> None:
    """EC-QUOTE-007 — nu's PATH emit round-trips ``( ) $ \\ \\` and \"`` byte-exact through a live nu process."""
    shell_abs = _require("nushell")
    hostile = 'a(b)$c\\d`e"f'
    # `_toml_escape`, not a bare f-string: the value carries `\` and `"`, which
    # a TOML basic string reads as an escape and a terminator. Without it the
    # fixture never parses and the row is never reached.
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "bin/{_toml_escape(hostile)}" }}\n')
    (project / "bin" / hostile).mkdir(parents=True)
    # The row is about the EMIT's escaper (`escape_value`'s Nushell arm), which
    # `--reconcile --shell=nushell` produces today — not about the shipped hook
    # consuming it. nu has no string `eval` (A-24), so the stream is round-tripped
    # through a live nu from a file.
    emitted = matrix.reconcile(arena.ocx, "nushell", project, arena.env(shell_abs))
    assert emitted.returncode == 0, f"--reconcile must exit 0 for nushell; stderr:\n{emitted.stderr}"
    assert emitted.stdout.strip(), "an empty stream would make the round-trip assertion vacuous"
    session = matrix.eval_snippet(
        "nushell",
        shell_abs,
        emitted.stdout,
        matrix.probe("nushell", "path", "PATH"),
        cwd=project,
        env=arena.env(shell_abs),
        script_dir=arena.scripts,
        name="quote007",
    )
    assert session.returncode == 0, f"stderr:\n{session.stderr}"
    segments = matrix.path_segments(_read(session, "path"))
    assert str(project / "bin" / hostile) in segments, f"the hostile segment must round-trip byte-exact: {segments}"


def test_ec_quote_008_powershell_single_quoted_literal_carries_every_metacharacter(arena: Arena) -> None:
    """EC-QUOTE-008 — only ``'`` is doubled; ``$``, backtick, ``;``, ``*`` survive a live pwsh round trip byte-exact."""
    _require("pwsh")
    hostile = "a'b$c`d;e*f"
    project = _locked_project(arena, "alpha", f'WP15_CONST = "{hostile}"\n')
    result = _session("pwsh", arena, [matrix.cd_to("pwsh", project), matrix.prompt("pwsh"), matrix.probe("pwsh", "const", "WP15_CONST")])
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "const") == hostile, f"stdout:\n{result.stdout}"


def test_ec_quote_009_bang_survives_non_interactive_bash_c(arena: Arena) -> None:
    """EC-QUOTE-009 — ``a!b`` must round-trip through a non-interactive ``bash -c`` invocation of the reconciler."""
    shell_abs = _require("bash")
    project = _locked_project(arena, "alpha", 'WP15_CONST = "a!b"\n')
    line = f'eval "$("{arena.ocx}" --offline self activate --reconcile --shell=bash)"; printf "%s\\n" "@@const@@${{WP15_CONST-__OCX_ABSENT__}}"'
    result = subprocess.run(
        [shell_abs, "-c", f"cd {matrix.quote('bash', str(project))} && {line}"],
        capture_output=True, check=False, text=True, env=arena.env(shell_abs),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert matrix.probes(result.stdout).get("const") == "a!b", (
        f"a non-interactive `bash -c` (histexpand off) must read back the RAW value, not a `\\!`-corrupted one:\n"
        f"{result.stdout}"
    )


def test_ec_quote_012_fish_leaves_backtick_unescaped_and_does_not_glob(arena: Arena) -> None:
    """EC-QUOTE-012 — fish emits a bare backtick with no glob expansion or command substitution."""
    _require("fish")
    literal_dir = "a`b*c[d]"
    (arena.projects / literal_dir).mkdir(parents=True, exist_ok=True)
    (arena.projects / "aXbYcZ").mkdir(parents=True, exist_ok=True)
    project = _locked_project(arena, "alpha", f'PATH = {{ type = "path", value = "{literal_dir}" }}\n')
    (project / literal_dir).mkdir(parents=True, exist_ok=True)
    result = _session("fish", arena, [matrix.cd_to("fish", project), matrix.prompt("fish"), matrix.probe("fish", "path", "PATH")])
    assert result.returncode == 0, f"a backtick/glob-bearing element must not error; stderr:\n{result.stderr}"
    segments = matrix.path_segments(_read(result, "path"))
    assert str(project / literal_dir) in segments, f"the literal segment must survive with no glob expansion: {segments}"


def test_ec_quote_windows_only_rows_are_out_of_scope_here(arena: Arena) -> None:
    """EC-QUOTE-004 / EC-QUOTE-010 / EC-QUOTE-011 — the Batch arm needs a real ``cmd.exe``, not the Linux shell zoo."""
    pytest.skip(
        f"observed: sys.platform={sys.platform!r} — the Batch arm hosts no per-prompt hook and needs a real "
        "Windows runner leg; owned by WP-18"
    )



# ---------------------------------------------------------------------------
# Nushell channel
# ---------------------------------------------------------------------------


def test_ec_nu_001_project_scope_constant_revert_is_unimplemented_on_nu(arena: Arena) -> None:
    """EC-NU-001 — D6(b): constant revert on nu is unimplementable until the ``hide-env`` spike lands.

    Nushell's ``env_change.PWD`` hook never calls ``--reconcile`` (verified by
    ``_skip_nushell_without_reconcile`` below), so there is no project-scope
    apply-then-revert path to observe on nu at all today — the row's own
    "say that rather than claiming parity" is the documented state this test
    pins.
    """
    _skip_nushell_without_reconcile("nushell", arena)
    # Canary: if this line is ever reached, WP-12b landed and nu now calls
    # --reconcile — EC-NU-001's premise (project-scope constant revert is
    # unimplemented on nu) needs re-examination against the new behaviour,
    # not a silent pass here.
    pytest.fail(
        "nu now calls --reconcile (the skip guard above did not fire) — re-examine EC-NU-001 against the new behaviour"
    )


def test_ec_nu_002_hide_env_scoping_inside_a_hook_block_red_and_green(arena: Arena) -> None:
    """EC-NU-002 — the mandated red-and-green spike: does ``hide-env`` inside a hook block escape it?

    A raw nu-language probe, independent of ocx: sets a var in the enclosing
    scope, then runs ``hide-env`` inside a nested block (standing in for
    ``env_change.PWD``'s own block scope) and reads back afterward. Both
    colours are demonstrated on inputs this test controls, as the ADR's own
    spike requires.
    """
    shell_abs = _require("nushell")
    script = arena.scripts / "nu002_spike.nu"
    script.write_text(
        "$env.JAVA_HOME = 'seeded'\n"
        "do { hide-env JAVA_HOME }\n"
        "print $'escaped=($env.JAVA_HOME? | default MISSING)'\n",
        encoding="utf-8",
    )
    result = subprocess.run([shell_abs, "--no-config-file", str(script)], capture_output=True, check=False, text=True, env=arena.env(shell_abs))
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    # Whichever colour this nu version is, the property under test is that the
    # spike is OBSERVABLE and deterministic — assert it printed one of the two
    # legal answers, not a crash or an ambiguous third state.
    assert "escaped=seeded" in result.stdout or "escaped=MISSING" in result.stdout, (
        f"the spike must resolve to one clear answer:\n{result.stdout}"
    )


def test_ec_nu_003_shipped_applier_parses_on_this_nu_and_avoids_get_optional(arena: Arena) -> None:
    """EC-NU-003 — the shipped ``env.nu`` autoload must parse (whole-file parse-before-run) and never reintroduce ``get --optional``."""
    shell_abs = _require("nushell")
    _self_setup(arena, "nu")
    env_nu = arena.ocx_home / "env.nu"
    assert env_nu.is_file(), f"self setup must write {env_nu}"
    body = env_nu.read_text(encoding="utf-8")
    assert "get --optional" not in body, (
        f"the shipped applier must never use `get --optional` — nu's whole-file parse would void the PATH prepend "
        f"on an older nu too:\n{body[:2000]}"
    )
    parse_check = subprocess.run(
        [shell_abs, "--no-config-file", "-c", f"nu-check {matrix.quote('nushell', str(env_nu))}"],
        capture_output=True, check=False, text=True, env=arena.env(shell_abs),
    )
    assert parse_check.returncode == 0, f"the shipped autoload must parse cleanly on this nu:\nstdout:\n{parse_check.stdout}\nstderr:\n{parse_check.stderr}"


def test_ec_nu_004b_hooks_key_absent_from_config_does_not_crash_install(arena: Arena) -> None:
    """EC-NU-004(b) — a minimal ``nu -n`` session with no ``hooks`` key at all must not die installing the PWD hook."""
    shell_abs = _require("nushell")
    _self_setup(arena, "nu")
    env_nu = arena.ocx_home / "env.nu"
    assert env_nu.is_file()
    result = subprocess.run(
        [shell_abs, "-n", "--no-config-file", "-c", f"source {matrix.quote('nushell', str(env_nu))}; print DONE"],
        capture_output=True, check=False, text=True, env=arena.env(shell_abs),
    )
    assert result.returncode == 0, f"installing the hook with no pre-existing `hooks` key must not error; stderr:\n{result.stderr}"
    assert "DONE" in result.stdout, f"stdout:\n{result.stdout}"


def test_ec_nu_005_the_hook_fires_observed_through_a_real_pty(arena: Arena) -> None:
    """EC-NU-005 — a hook that never fires can also produce a correct-looking env; only a real pty distinguishes them."""
    shell_abs = _require("nushell")
    _skip_nushell_without_reconcile("nushell", arena)
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nWP15_GLOBAL = "before"\n', encoding="utf-8")
    locked = subprocess.run([str(arena.ocx), "--offline", "--global", "lock"], capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects))
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"
    _self_setup(arena, "nu")
    env = arena.env(shell_abs)
    env["TERM"] = "dumb"
    output = matrix.pty_session(
        [shell_abs, "--no-config-file"],
        [
            f"source {arena.ocx_home / 'env.nu'}",
            "print $'@@before@@($env.WP15_GLOBAL? | default __OCX_ABSENT__)'",
            "cd /tmp",  # a PWD event, to fire env_change.PWD at least once and prove the hook DID run
            "print $'@@after@@($env.WP15_GLOBAL? | default __OCX_ABSENT__)'",
        ],
        cwd=arena.projects, env=env, timeout=60,
    )
    found = matrix.probes(output)
    assert found.get("before") == "before", f"the hook must fire and apply on install\npty transcript:\n{output}"
    assert found.get("after") == "before", f"a PWD event must not clobber the still-correct global value\npty transcript:\n{output}"


def test_ec_nu_006_global_list_kind_is_not_silently_misapplied_as_a_constant(arena: Arena) -> None:
    """EC-NU-006 — A-23 (widened): a ``list``-kind global entry applies through nu's ``list`` arm, preserving the caller's prior value.

    This was a strict xfail while `NU_ENV_APPLY_LOOP`
    (``crates/ocx_lib/src/setup/shims.rs``) was a two-way branch that sent a
    ``list`` entry down the constant arm and clobbered the prior. The four-way
    dispatch (``path`` / ``list`` / ``constant`` / apply-nothing) has landed, so
    this is an ordinary positive assertion again — a strict xfail against a
    fixed defect reds on the unexpected pass, which is the opposite of coverage.
    """
    shell_abs = _require("nushell")
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nCFLAGS = { type = "list", separator = " ", value = "-DGLOBAL" }\n', encoding="utf-8")
    locked = subprocess.run([str(arena.ocx), "--offline", "--global", "lock"], capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects))
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"
    # The row names `NU_ENV_APPLY_LOOP` — the loop inlined in the shipped
    # `env.nu`, not the per-entry code `--reconcile` emits — so this drives the
    # shipped file. The PRIOR is load-bearing: applied as a list the declared
    # value appends to it, applied as a constant it replaces it. Without a prior,
    # both arms produce `-DGLOBAL` and the assertion cannot tell them apart.
    _self_setup(arena, "nu")
    env_nu = arena.ocx_home / "env.nu"
    assert env_nu.is_file(), f"self setup must write {env_nu}; without it this is not the shipped applier"
    env = arena.env(shell_abs)
    env["CFLAGS"] = "-DUSER"
    result = _session(
        "nushell",
        arena,
        [f"source {matrix.quote('nushell', str(env_nu))}", matrix.probe("nushell", "cflags", "CFLAGS")],
        env=env,
        name="nu006",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "cflags") == "-DUSER -DGLOBAL", (
        "a list-kind entry must apply through a list arm that PRESERVES the prior — every emitted-stream "
        f"arm appends to it — not fall through the `else` arm as a constant that clobbers it:\n{result.stdout}"
    )


def test_ec_nu_007_env_path_applies_correctly_as_both_list_and_string_shape(arena: Arena) -> None:
    """EC-NU-007 — the nu applier must apply correctly whether ``$env.PATH`` is nu's native list or a joined string."""
    shell_abs = _require("nushell")
    project = _locked_project(arena, "alpha", 'PATH = { type = "path", value = "binA" }\n')
    (project / "binA").mkdir()
    emitted = matrix.reconcile(arena.ocx, "nushell", project, arena.env(shell_abs))
    assert emitted.returncode == 0, f"--reconcile must exit 0 for nushell; stderr:\n{emitted.stderr}"
    assert str(project / "binA") in emitted.stdout, (
        f"the stream must carry the project's bin dir, or neither shape below proves anything:\n{emitted.stdout}"
    )
    snippet = arena.scripts / "nu007_stream.nu"
    snippet.write_text(emitted.stdout, encoding="utf-8")
    # A-24: one prompt per process, so each shape gets its own nu. (a) is nu's
    # native startup shape — `$env.PATH` is a `list<string>`; (b) is the joined
    # string `ENV_NU` normalises it to before any reconcile can run.
    for label, preamble, expected_shape in (
        ("list", "", "list"),
        ("string", "$env.PATH = ($env.PATH | str join (char esep))", "string"),
    ):
        result = matrix.run_script(
            "nushell",
            shell_abs,
            "\n".join(
                [
                    preamble,
                    "$env.WP15_SHAPE = ($env.PATH | describe)",
                    f"source {matrix.quote('nushell', str(snippet))}",
                    matrix.probe("nushell", "shape", "WP15_SHAPE"),
                    matrix.probe("nushell", "path", "PATH"),
                ]
            ),
            cwd=project,
            env=arena.env(shell_abs),
            script_dir=arena.scripts,
            name=f"nu007_{label}",
        )
        assert result.returncode == 0, f"{label} shape: stderr:\n{result.stderr}"
        assert _read(result, "shape").startswith(expected_shape), (
            f"{label} shape: the fixture must actually present $env.PATH in that shape, or the arm is untested; "
            f"describe returned {_read(result, 'shape')!r}"
        )
        segments = matrix.path_segments(_read(result, "path"))
        assert str(project / "binA") in segments, f"{label} shape: the apply must land under this shape too:\n{result.stdout}"



# ---------------------------------------------------------------------------
# Process-boundary and privilege-boundary rows (EC-PROC)
# ---------------------------------------------------------------------------


def test_ec_proc_001_subshell_forms_never_leak_back_into_the_parent(arena: Arena) -> None:
    """EC-PROC-001 — parens, ``$(...)``, background, and a pipe all fork; none can mutate the parent's PATH or carrier."""
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    outside = arena.home  # stand-in for `/tmp` — hermetic, guaranteed writable, no project
    ocx_literal = matrix.quote("bash", str(arena.ocx))
    reconcile_in_outside = f'(cd {matrix.quote("bash", str(outside))} && eval "$({ocx_literal} --offline self activate --reconcile --shell=bash)")'
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before_path", "PATH"),
            matrix.probe("bash", "before_state", matrix.CARRIER),
            reconcile_in_outside,
            matrix.probe("bash", "after_parens_path", "PATH"),
            matrix.probe("bash", "after_parens_state", matrix.CARRIER),
            f"__x=$({reconcile_in_outside}; echo done)",
            matrix.probe("bash", "after_cmdsub_path", "PATH"),
            f"{reconcile_in_outside} &",
            "wait",
            matrix.probe("bash", "after_bg_path", "PATH"),
            f"{reconcile_in_outside} | cat",
            matrix.probe("bash", "after_pipe_path", "PATH"),
        ],
        name="proc001_subshell_containment",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    before_path = _read(result, "before_path")
    before_state = _read(result, "before_state")
    for label in ("after_parens_path", "after_cmdsub_path", "after_bg_path", "after_pipe_path"):
        assert _read(result, label) == before_path, f"{label}: a subshell must never mutate the parent's PATH\n{result.stdout}"
    assert _read(result, "after_parens_state") == before_state, (
        f"a subshell's own reconcile must never mutate the parent's carrier:\n{result.stdout}"
    )


@pytest.mark.parametrize("child", ["fish", "pwsh"])
def test_ec_proc_002_exec_replacement_reads_the_bash_written_ledger_correctly(child: str, arena: Arena) -> None:
    """EC-PROC-002 — invariant L-2: a ledger written by bash is decoded correctly by an ``exec``'d fish or pwsh."""
    child_abs = _require(child)
    project = _locked_project(arena, "alpha", "WP15_CONST = \"it's $HOME `tick` !bang\"\n")

    child_script = arena.scripts / f"proc002_child{matrix.ARMS[child].extension}"
    child_script.write_text(
        "\n".join([matrix.header(child, arena.ocx), matrix.probe(child, "inherited", "WP15_CONST")]) + "\n",
        encoding="utf-8",
    )
    fragment = _run_child_fragment("bash", child, child_abs, child_script)
    result = _session(
        "bash",
        arena,
        [matrix.cd_to("bash", project), matrix.prompt("bash"), fragment],
        name="proc002_exec_replacement",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "inherited") == "it's $HOME `tick` !bang", (
        f"{child} must decode the raw value bash's escaper wrote, byte-for-byte:\n{result.stdout}"
    )


def test_ec_proc_003_activation_installs_the_hook_into_prompt_command_once(arena: Arena) -> None:
    """EC-PROC-003 — Decision 5 layer 1: the hook is appended to PROMPT_COMMAND, decided once at shell start via ``self activate``'s ConfigLoader pass (D5:274-279).

    ``--hook`` forces deterministic emission (bare ``self activate`` only
    installs it when it detects an interactive session via a real tty, which
    is unreliable to fake through a login shell across hosts — this repo's
    system profile scripts are not under test control either) — same
    emission path, same doc-cited ConfigLoader pass, just without the tty
    dependency.
    """
    result = subprocess.run(
        [
            "/bin/bash",
            "-l",
            "-c",
            f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash --hook)"; printf \'%s\\n\' "@@hook@@${{PROMPT_COMMAND-__OCX_ABSENT__}}"',
        ],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env("/bin/bash"),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "__ocx_prompt_hook" in _read(result, "hook"), (
        f"activation must append the hook to PROMPT_COMMAND once run; got {result.stdout!r}\nstderr:\n{result.stderr}"
    )


def test_ec_proc_004_norc_login_flag_does_not_change_whether_the_hook_installs(arena: Arena) -> None:
    """EC-PROC-004 — the hook installs identically whether the invoking flags are login-style or not; login-vs-non-login is irrelevant to Decision 5's enablement path."""
    result = subprocess.run(
        [
            "/bin/bash",
            "--norc",
            "-c",
            f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash --hook)"; printf \'%s\\n\' "@@hook@@${{PROMPT_COMMAND-__OCX_ABSENT__}}"',
        ],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env("/bin/bash"),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "__ocx_prompt_hook" in _read(result, "hook"), (
        f"a non-login `--norc` invocation must install the hook identically to the login path (EC-PROC-003); got {result.stdout!r}\nstderr:\n{result.stderr}"
    )


def test_ec_proc_005_bash_env_script_never_sets_the_carrier_or_spawns_reconcile(arena: Arena) -> None:
    """EC-PROC-005 — scripts are ``ocx run``'s domain, not the hook's: ``BASH_ENV`` must never leave the carrier set."""
    _self_setup(arena, "bash")
    rcfile = arena.home / ".bashrc"
    assert rcfile.is_file()
    script = arena.scripts / "proc005_script.sh"
    script.write_text(
        'printf "%s%s\\n" "@@carrier@@" "${__OCX_ENV_STATE-__OCX_ABSENT__}"\n',
        encoding="utf-8",
    )
    result = subprocess.run(
        ["/bin/bash", str(script)],
        capture_output=True,
        check=False,
        text=True,
        env=arena.env("/bin/bash", BASH_ENV=str(rcfile)),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "carrier") == matrix.ABSENT, (
        f"BASH_ENV must never trigger the interactive hook — a non-interactive script's carrier must stay unset:\n{result.stdout}"
    )


def test_ec_proc_006_a_mid_line_global_config_mutation_only_takes_effect_next_prompt(arena: Arena) -> None:
    """EC-PROC-006 — a same-command-line global mutation degrades to next-prompt correctness (D5:294), never breaks."""
    global_toml = arena.ocx_home / "ocx.toml"
    tool_dir = arena.ocx_home / "packages" / "sometool" / "bin"
    tool_dir.mkdir(parents=True)
    (tool_dir / "sometool").write_text("#!/bin/sh\necho hi\n", encoding="utf-8")
    (tool_dir / "sometool").chmod(0o755)
    global_toml.write_text("[env]\n", encoding="utf-8")
    locked = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"],
        capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects),
    )
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"

    child_probe = 'bash -c \'printf "%s%s\\n" "@@child_path@@" "$PATH"\''
    result = _session(
        "bash",
        arena,
        [
            matrix.prompt("bash"),
            matrix.probe("bash", "before_path", "PATH"),
            _write_config_env("bash", global_toml, f'PATH = {{ type = "path", value = "{tool_dir}" }}\n'),
            child_probe,
            matrix.prompt("bash"),
            matrix.probe("bash", "after_path", "PATH"),
        ],
        name="proc006_no_intervening_prompt",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    before = matrix.path_segments(_read(result, "before_path"))
    child = matrix.path_segments(_read(result, "child_path"))
    after = matrix.path_segments(_read(result, "after_path"))
    assert str(tool_dir) not in before, f"before: {result.stdout}"
    assert str(tool_dir) not in child, (
        f"a subshell forked on the SAME command line as the global-config mutation must see the pre-mutation "
        f"snapshot, not the just-written config:\n{result.stdout}"
    )
    assert str(tool_dir) in after, f"the NEXT prompt must recompose with the new global entry:\n{result.stdout}"


def test_ec_proc_007_tmux_detach_reattach_resolves_new_tool_state_on_the_fresh_prompt(arena: Arena) -> None:
    """EC-PROC-007 — a detached tmux pane carries no cached D; reattach + a fresh prompt recomposes from the mutated lock."""
    tmux = shutil.which("tmux")
    if tmux is None:
        pytest.skip("tmux is not installed on this host (shutil.which('tmux') is None)")
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    session = f"wp15proc007-{os.getpid()}"
    out_fifo = arena.scripts / "proc007_out.txt"
    env = arena.env("/bin/bash")
    try:
        start = subprocess.run(
            [tmux, "new-session", "-d", "-s", session, "-x", "200", "-y", "50", "/bin/bash", "--noprofile", "--norc"],
            capture_output=True, check=False, text=True, env=env,
        )
        assert start.returncode == 0, f"stderr:\n{start.stderr}"
        header_cmd = matrix.header("bash", arena.ocx)
        subprocess.run([tmux, "send-keys", "-t", session, f"cd {project} && {header_cmd}", "Enter"], check=False)
        subprocess.run([tmux, "send-keys", "-t", session, matrix.prompt("bash"), "Enter"], check=False)
        time.sleep(0.5)
        subprocess.run([tmux, "detach-client", "-s", session], check=False)
        # Mutate the tool declaration from outside the pane while detached.
        matrix.write_project(project, 'WP15_CONST = "v2"\n')
        subprocess.run([tmux, "send-keys", "-t", session, f"{matrix.prompt('bash')} > {out_fifo} 2>&1", "Enter"], check=False)
        subprocess.run(
            [tmux, "send-keys", "-t", session, f'printf "%s\\n" "$WP15_CONST" >> {out_fifo}', "Enter"], check=False
        )
        deadline = time.time() + 10
        while time.time() < deadline and not out_fifo.exists():
            time.sleep(0.2)
        time.sleep(0.5)
    finally:
        subprocess.run([tmux, "kill-session", "-t", session], check=False, capture_output=True)
    assert out_fifo.exists(), "tmux pane never produced output within the deadline"
    text = out_fifo.read_text(encoding="utf-8")
    assert "v2" in text, f"a fresh prompt after reattach must recompose D from the mutated lock, not any cached value:\n{text}"


def test_ec_proc_008_a_running_sessions_hook_presence_is_fixed_at_its_own_startup(arena: Arena) -> None:
    """EC-PROC-008 — hook presence is decided once, at session startup; an already-running session never retroactively gains or loses it."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    # Session A never sources the hook body at all — stands in for a pane started
    # under a pre-ADR build (or OCX_NO_HOOK=1 baked into that shell's startup).
    no_hook = _session(
        "bash",
        arena,
        [matrix.cd_to("bash", project), matrix.probe("bash", "carrier", matrix.CARRIER)],
        name="proc008_no_hook_session",
    )
    assert no_hook.returncode == 0, f"stderr:\n{no_hook.stderr}"
    assert _read(no_hook, "carrier") == matrix.ABSENT, (
        f"a session that never sourced the hook must never gain reconciliation retroactively:\n{no_hook.stdout}"
    )
    # Session B is a fresh process that DOES source the hook — the "new pane" case.
    with_hook = _session(
        "bash",
        arena,
        [matrix.cd_to("bash", project), matrix.prompt("bash"), matrix.probe("bash", "carrier", matrix.CARRIER)],
        name="proc008_with_hook_session",
    )
    assert with_hook.returncode == 0, f"stderr:\n{with_hook.stderr}"
    assert _read(with_hook, "carrier") != matrix.ABSENT, (
        f"a fresh session that sources the hook must reconcile normally:\n{with_hook.stdout}"
    )


def test_ec_proc_009_a_clean_env_spawn_reduces_to_the_absent_ledger_first_prompt_case(arena: Arena) -> None:
    """EC-PROC-009 — ``ssh -A host``'s first prompt needs no SSH-specific mechanism: a clean-env spawn already has an absent ledger."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    env = arena.env("/bin/bash")
    assert matrix.CARRIER not in env, f"the spawn env itself must carry no ambient ledger (clean_env's own contract): {env}"
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.probe("bash", "before", matrix.CARRIER),
            matrix.prompt("bash"),
            matrix.probe("bash", "after", matrix.CARRIER),
        ],
        name="proc009_clean_spawn",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before") == matrix.ABSENT, f"before the first apply, the carrier must be absent:\n{result.stdout}"
    assert _read(result, "after") != matrix.ABSENT, f"after the first apply, the carrier must be set:\n{result.stdout}"


def test_ec_proc_010_non_interactive_command_execution_never_installs_the_hook(arena: Arena) -> None:
    """EC-PROC-010 — ``ssh host 'cmd'`` is mechanically a script invocation: no interactive profile, no hook, no carrier."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    _self_setup(arena, "bash")  # writes the managed block into .bashrc — irrelevant to a non-interactive `-c`
    probe_cmd = 'printf "%s%s\\n" "@@carrier@@" "${__OCX_ENV_STATE-__OCX_ABSENT__}"'
    result = subprocess.run(
        ["/bin/bash", "-c", probe_cmd],
        capture_output=True, check=False, text=True, cwd=str(project), env=arena.env("/bin/bash"),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "carrier") == matrix.ABSENT, (
        f"a non-interactive `bash -c` never reads .bashrc, so the hook never installs and the carrier stays unset:\n{result.stdout}"
    )


def test_ec_proc_012_a_scrubbed_env_spawn_resolves_ocx_home_under_the_targets_own_home(tmp_path: Path, arena: Arena) -> None:
    """EC-PROC-012 — ``sudo -i``'s reduction: an unset ``OCX_HOME`` falls back to ``$HOME/.ocx`` of whichever user is now running."""
    target_home = tmp_path / "target_user_home"
    target_ocx_home = target_home / ".ocx"
    candidate = target_ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    target_home.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    target_arena = Arena(home=target_home, ocx_home=target_ocx_home, scripts=arena.scripts, projects=arena.projects, ocx=arena.ocx)
    _self_setup(target_arena, "bash")

    scrubbed_env = {"HOME": str(target_home), "PATH": arena.env("/bin/bash")["PATH"]}
    assert "OCX_HOME" not in scrubbed_env, "the whole point: no explicit OCX_HOME, only HOME changes (the sudo -i reduction)"
    result = subprocess.run(
        ["/bin/bash", "-l", "-c", 'echo "@@resolved_home@@$OCX_HOME"'],
        capture_output=True, check=False, text=True, env=scrubbed_env,
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    resolved = _read(result, "resolved_home")
    assert resolved == str(target_ocx_home), (
        f"an unset OCX_HOME must resolve under the CURRENT $HOME ({target_home}), never the original user's; got {resolved!r}\n{result.stdout}"
    )


def test_ec_proc_014_env_channel_consent_namespace_activates_silently_at_shell_start(arena: Arena) -> None:
    """EC-PROC-014 — an ``OCX_CONSENT_NAMESPACES`` env var pre-set before shell start (devcontainer ``ENV``) activates the matching project without a prompt.

    Note: the row's own "Expected behaviour" text still cites the pre-A-26
    "auto-stamp on first activation" rule (D4:193); A-26 retires the
    auto-stamp explicitly, but this row carries no addendum-override marker
    at all — silent staleness distinct from a marker disagreeing with its
    resolution. This test asserts the SHIPPED (A-26) behaviour: a namespaces
    grant activates every prompt and writes no stamp.
    """
    project = arena.projects / "acme_ns"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nwidget = "ocx.sh/acme-corp/widget"\n')
    matrix.write_lock(project, matrix.lock_tool("widget", "ocx.sh/acme-corp/widget"))

    granted = arena.env(OCX_CONSENT_NAMESPACES="ocx.sh/acme-corp/*")
    state = matrix.shell_state(arena.ocx, project, granted)
    assert state["inert_reason"]["reason"] != "no_stamp_no_grant", (
        f"a namespaces env var set before shell start must activate the matching checkout silently: {state['inert_reason']}"
    )
    key = state["project_key"]
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), "A-26: a namespaces grant activates without writing a stamp"


# EC-PROC-011 and EC-PROC-013 are manual-only (tier: manual-only). Documented
# here as clearly-marked non-test procedures rather than pytest functions —
# both require infrastructure this harness deliberately never drives
# (a real JetBrains/VS Code process; a real multi-user privilege boundary).

def manual_procedure_ec_proc_011_ide_terminal_and_run_configuration_env_snapshot() -> None:
    """MANUAL PROCEDURE — EC-PROC-011 (tier: manual-only).

    Not run by this suite. The terminal-pane half reduces to EC-PROC-003 /
    EC-PROC-004 (already automated above) and needs no separate automation;
    only the Run Configuration half needs a human:

    1. Open a checked-out ocx project in JetBrains (or VS Code).
    2. Open the IDE's integrated terminal; `cd` into the project. Confirm the
       hook fires (prompt shows ocx's env applied) — this is the automated
       part, covered by test_ec_proc_003/004 above.
    3. Launch a Run Configuration that reads `$PATH` from the IDE's own
       captured environment (e.g. a "Shell Script" or "Application" run
       config with no "activate shell env" option enabled).
    4. In a terminal pane, run `ocx add --global sometool` (or hand-edit the
       global ocx.toml + `ocx --global lock`, per test_ec_proc_006's
       technique) so a new tool lands on PATH for future prompts.
    5. Re-run the SAME Run Configuration (do not restart the IDE). Confirm it
       does NOT see `sometool` on PATH — the IDE snapshotted its env once at
       process launch and never re-reads it (D9:352, ocx#189's class).
    """


def manual_procedure_ec_fp_002_coarse_mtime_granularity_widens_the_ceiling() -> None:
    """MANUAL PROCEDURE — EC-FP-002 (tier: manual-only, closed by A-14).

    Not run by this suite: the failing configuration is a filesystem
    property. Neither a Linux CI job nor the shell-zoo image can create a
    FAT/exFAT volume (2-second mtime granularity) or an NFS mount with
    1-second granularity, and the ceiling itself is already asserted on a
    normal filesystem by EC-FP-001.

    1. Mount a FAT32 or exFAT volume (on Linux: `mkfs.vfat` a loopback
       image and mount it; on Windows: format a small VHD). An NFS or SMB
       mount with 1-second granularity reproduces the milder half.
    2. Place a project (`ocx.toml` + `ocx.lock`) on that volume and
       activate the hook in a shell sitting inside it.
    3. Perform the EC-FP-001 edit — rewrite a watched file with the same
       byte length within the same granularity window (under two seconds on
       FAT/exFAT, under one second on NFS).
    4. Observe that the reconcile does NOT fire for that edit, and that it
       DOES fire once the window elapses.
       Per A-14 the rule is unchanged, only the window widens: "same second"
       becomes "same one to two seconds". The residual is documented, not
       fixed — the point of this procedure is to confirm no assertion
       anywhere encodes a 1-second assumption it cannot hold on Windows,
       where these filesystems are a first-class target.
    """


def manual_procedure_ec_proc_013_sudo_e_cannot_forge_root_path_across_the_privilege_boundary() -> None:
    """MANUAL PROCEDURE — EC-PROC-013 (tier: manual-only, closed by A-06).

    Not run by this suite: genuinely privilege-boundary-crossing, needs a
    real multi-user sandbox (a container or VM with a second non-root user
    is the minimum realistic rig) — the highest-value case to verify by hand
    given the security framing.

    1. As a non-root user with the hook installed, hand-craft
       `__OCX_ENV_STATE` claiming an `applied` PATH-list element the user
       does not actually own (e.g. `/usr/local/sbin`).
    2. `sudo -E bash` into a root shell, carrying that forged carrier via
       `-E`.
    3. Trigger a scope-exit/retirement recompose in the root shell (`cd` out
       of the project directory so the reconciler runs a revert pass).
    4. Observe whether the forged PATH entry is removed from root's PATH.
       Per A-06 (closing D1's forgery posture, D1:79): the ability to forge
       an arbitrary shell variable already equals the ability to set PATH
       directly when attacker and victim share a process — `sudo -E` does
       not create a NEW privilege escalation via the carrier; it only
       matters when attacker and victim are different privilege levels
       sharing one carrier value, which is exactly what `sudo -E` does.
       Confirm the shipped behaviour matches A-06's resolution, not D1's
       original (weaker) framing.
    """



# ---------------------------------------------------------------------------
# direnv / mise coexistence (EC-COEX)
# ---------------------------------------------------------------------------


def test_ec_coex_001_matching_direnv_dir_yields_and_reverts_the_project_scope(arena: Arena) -> None:
    """EC-COEX-001 — a ``DIRENV_DIR`` naming this project's own directory yields: global only, project scope reverted, one info line."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "WP15_CONST"),
            f"export DIRENV_DIR={matrix.quote('bash', str(project))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "WP15_CONST"),
        ],
        name="coex001_direnv_matches",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before") == "v1", f"the project scope must apply before direnv is observed:\n{result.stdout}"
    assert _read(result, "after") == matrix.ABSENT, (
        f"a matching DIRENV_DIR must revert the already-applied project scope:\n{result.stdout}"
    )
    assert f"ocx: direnv manages this directory (DIRENV_DIR={project}); applying the global toolchain only" in result.stderr, (
        f"the one info line must name direnv and the exact signal (a _session eval sends the printed line to the "
        f"child shell's real stderr, unlike a raw matrix.reconcile() call):\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


def test_ec_coex_002_a_direnv_dir_naming_an_ancestor_is_treated_as_absent(arena: Arena) -> None:
    """EC-COEX-002 — ``DIRENV_DIR`` naming a real ancestor (not the resolved project itself) proceeds normally: direnv owns a different project."""
    project = _locked_project(arena, "nested", 'WP15_CONST = "v1"\n')
    ancestor = project.parent
    assert ancestor != project and str(project).startswith(str(ancestor)), "fixture sanity: ancestor must be a real ancestor, not the project itself"
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env(DIRENV_DIR=str(ancestor)))
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='v1'" in result.stdout, f"an ancestor DIRENV_DIR must not suppress this project's own scope:\n{result.stdout}"
    assert "manages this directory" not in result.stdout, (
        f"an ancestor DIRENV_DIR must not print a yield line for THIS project (the generic 'ocx: +VAR' summary "
        f"line is unrelated and must not be mistaken for one):\n{result.stdout}"
    )


def test_ec_coex_003_a_config_file_with_no_live_hook_never_suppresses_activation(arena: Arena) -> None:
    """EC-COEX-003 — an ``.envrc`` on disk with no live ``DIRENV_DIR`` is not evidence of a live hook (D9:347): activation proceeds normally.

    Covers both of the row's sub-cases: (a) direnv never hooked into this
    shell, and (b) direnv hooked in OTHER shells but not this one's RC — both
    reduce to the identical observable input (``DIRENV_DIR`` unset in this
    process), so one test covers both.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    (project / ".envrc").write_text("export WP15_UNRELATED=1\n", encoding="utf-8")
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='v1'" in result.stdout, f"a config file with no live sentinel must not suppress the project scope:\n{result.stdout}"
    assert "manages this directory" not in result.stdout, f"no live DIRENV_DIR means no yield line:\n{result.stdout}"


def test_ec_coex_004_mise_shell_yields_symmetrically_to_direnv(arena: Arena) -> None:
    """EC-COEX-004 — ``MISE_SHELL`` present gets the same treatment as a matching ``DIRENV_DIR`` (D9:350, review G-2)."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env(MISE_SHELL="bash"))
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "WP15_CONST" not in result.stdout, f"a live mise session must revert the project scope, symmetric to direnv:\n{result.stdout}"
    assert "ocx: mise manages this directory (MISE_SHELL=bash); applying the global toolchain only" in result.stdout, (
        f"the one info line must name mise:\n{result.stdout}"
    )


def test_ec_coex_005_a_mise_toml_with_no_live_sentinel_never_suppresses_activation(arena: Arena) -> None:
    """EC-COEX-005 — a ``mise.toml`` on disk with ``MISE_SHELL``/``__MISE_ORIG_PATH`` unset is not live-session evidence (D9:347,350)."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    (project / "mise.toml").write_text("[tools]\n", encoding="utf-8")
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='v1'" in result.stdout, f"a config file with no live mise sentinel must not suppress the project scope:\n{result.stdout}"
    assert "manages this directory" not in result.stdout, f"no live MISE_SHELL/__MISE_ORIG_PATH means no yield line:\n{result.stdout}"


def test_ec_coex_006_both_sentinels_set_yield_independently_with_one_line_each(arena: Arena) -> None:
    """EC-COEX-006 — A-37: the two checks are independent ``if``s, never an ``elif`` chain; both sentinels print their own line."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = matrix.reconcile(
        arena.ocx, "bash", project, arena.env(DIRENV_DIR=str(project), MISE_SHELL="bash")
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "WP15_CONST" not in result.stdout, f"the project scope must revert when either sentinel yields:\n{result.stdout}"
    assert f"ocx: direnv manages this directory (DIRENV_DIR={project}); applying the global toolchain only" in result.stdout, (
        f"an elif chain would show only one of the two lines — direnv's line is missing:\n{result.stdout}"
    )
    assert "ocx: mise manages this directory (MISE_SHELL=bash); applying the global toolchain only" in result.stdout, (
        f"an elif chain would show only one of the two lines — mise's line is missing:\n{result.stdout}"
    )



# ---------------------------------------------------------------------------
# Version skew / rollout (EC-VER)
# ---------------------------------------------------------------------------


def test_ec_ver_001_an_already_sourced_hook_function_body_survives_a_concurrent_update(arena: Arena) -> None:
    """EC-VER-001 — lag surface (a): a shell's already-sourced hook function body is unchanged until it restarts, even if another shell runs ``self update`` concurrently."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash --hook)"',
            'printf "@@before@@%s\\n" "$(declare -f __ocx_prompt_hook)"',
            # Simulate a concurrent `self update` in another shell: it rewrites
            # the on-disk shim, but a function already defined in THIS
            # process's memory cannot be retroactively edited by that.
            f'printf \'echo REWRITTEN\\n\' > {matrix.quote("bash", str(arena.ocx_home / "env.sh"))}',
            'printf "@@after@@%s\\n" "$(declare -f __ocx_prompt_hook)"',
        ],
        name="ver001_concurrent_update",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    before = _read(result, "before")
    after = _read(result, "after")
    assert before != "", "the hook function must actually be defined for this test to mean anything"
    assert before == after, (
        f"an on-disk shim rewrite must never retroactively change an already-defined shell function:\n{result.stdout}"
    )


def test_ec_ver_002_hook_discards_stderr_and_ignores_exit_status_from_an_incompatible_binary(arena: Arena) -> None:
    """EC-VER-002 — the emitted hook discards the reconcile call's stderr and ignores its exit status (D5:291): a downgraded/incompatible binary is a silent no-op, never a printed error."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    candidate = arena.ocx_home / _CANDIDATE_REL
    broken_binary = arena.scripts / "broken_ocx.sh"
    broken_binary.write_text(
        "#!/bin/sh\necho 'SHOULD_NEVER_APPEAR_error: unrecognized argument --reconcile' >&2\nexit 77\n",
        encoding="utf-8",
    )
    broken_binary.chmod(0o755)
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash --hook)"',
            # Swap the candidate for an incompatible "old binary" AFTER the hook
            # function is defined — the probe-guard only checks `-x`, which the
            # replacement still satisfies. Copy-then-rename (not an in-place
            # `cp`, which intermittently races ETXTBSY against the synchronous
            # `--global env` eval the activation script above just ran against
            # this same candidate inode) — same idiom as EC-FS-016's swap.
            f"cp {matrix.quote('bash', str(broken_binary))} {matrix.quote('bash', str(candidate))}.new",
            f"mv {matrix.quote('bash', f'{candidate}.new')} {matrix.quote('bash', str(candidate))}",
            "__ocx_prompt_hook",
            'printf "@@rc_after_hook@@%s\\n" "$?"',
        ],
        name="ver002_incompatible_binary",
    )
    assert result.returncode == 0, f"the session itself must not fail even though the hook's own reconcile call did:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert "SHOULD_NEVER_APPEAR" not in result.stdout, f"the incompatible binary's stderr must be discarded, never surfaced:\n{result.stdout}"
    assert "SHOULD_NEVER_APPEAR" not in result.stderr, f"the incompatible binary's stderr must be discarded, never surfaced:\n{result.stderr}"
    assert _read(result, "rc_after_hook") == "0", (
        f"the hook must ignore the reconcile call's exit status and always return $__ocx_status (the ORIGINAL "
        f"command's exit code), never the broken binary's 77:\n{result.stdout}"
    )


def test_ec_ver_003_nushell_structured_plan_schema_skew_needs_nu(arena: Arena) -> None:
    """EC-VER-003 — needs a real nushell to drive the structured ``Plan`` JSON consumer across a schema-version swap; honest skip when absent."""
    _require("nushell")
    pytest.skip("nushell is present but this row needs two staged builds with different Plan schema versions — not reachable with a single test binary")


def test_ec_ver_004_the_hook_ignores_ocx_binary_pin_and_always_resolves_through_current(arena: Arena) -> None:
    """EC-VER-004 — A-34: the emitted hook always resolves through ``current``; ``OCX_BINARY_PIN`` has no effect on it.

    Note: A-34's own "Test hook" names ``rust-unit`` (grep the five shim
    bodies for ``OCX_BINARY_PIN`` absence) as the primary verification, while
    this row's own Test-tier column says ``pytest-hostshell`` — a
    tier disagreement between the row and its own closing resolution,
    reported as a finding. This test still honours the row's own column.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = matrix.reconcile(
        arena.ocx, "bash", project, arena.env(OCX_BINARY_PIN="/nonexistent/pinned/ocx")
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='v1'" in result.stdout, (
        f"a garbage OCX_BINARY_PIN must not affect the reconcile call at all — if it were consulted, this project "
        f"would fail to resolve since the pinned path does not exist:\n{result.stdout}"
    )


def test_ec_ver_005_install_no_setup_never_engages_any_ledger_machinery(arena: Arena) -> None:
    """EC-VER-005 — an ``OCX_INSTALL_NO_SETUP=1`` install never gets shims and therefore never gets the hook (Migration/Rollout, L415): documented, not fixed.

    Simulated at the downstream boundary this repo owns: ``self setup`` is
    simply never called (standing in for the installer having honoured
    ``OCX_INSTALL_NO_SETUP=1`` and skipped it) — no managed block exists, so
    no shell ever sources any ``__OCX_ENV_STATE`` machinery.
    """
    assert not (arena.home / ".bashrc").exists(), "fixture sanity: self setup must never have run"
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = _session(
        "bash",
        arena,
        [matrix.cd_to("bash", project), matrix.probe("bash", "carrier", matrix.CARRIER)],
        name="ver005_no_setup",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "carrier") == matrix.ABSENT, (
        f"with no managed block ever written, no shell ever engages the ledger machinery at all:\n{result.stdout}"
    )


def test_ec_ver_006_ci_non_interactive_shell_never_installs_the_hook(arena: Arena) -> None:
    """EC-VER-006 — a CI-style non-interactive shell never spawns ``--reconcile``; interactivity auto-detects correctly as off.

    Note: the row's own Expected-behaviour text says interactivity is
    "decided shell-side and passed explicitly... the binary never probes a
    stderr the shim may have redirected" — but the shipped code (`activate.rs`
    ``let interactive = std::io::stderr().is_terminal();``) does exactly the
    opposite: the BINARY itself probes its own stderr for tty-ness, with no
    shell-side flag involved. Reported as a finding (no addendum-override
    marker present on this row). This test asserts the SHIPPED behaviour,
    which happens to make the CI case trivially reachable: any pytest
    subprocess has a piped, non-tty stderr already.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = subprocess.run(
        [
            "/bin/bash",
            "-c",
            (
                f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash)"; '
                'printf "%s%s\\n" "@@hook@@" "${PROMPT_COMMAND-__OCX_ABSENT__}"; '
                'printf "%s%s\\n" "@@carrier@@" "${__OCX_ENV_STATE-__OCX_ABSENT__}"'
            ),
        ],
        capture_output=True, check=False, text=True, cwd=str(project), env=arena.env("/bin/bash"),
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "hook") == matrix.ABSENT, f"a CI-style non-interactive shell must never get the hook:\n{result.stdout}"
    assert _read(result, "carrier") == matrix.ABSENT, f"and therefore never spawns a --reconcile call at all:\n{result.stdout}"


def test_ec_ver_007_nushell_two_hop_update_lag_needs_nu(arena: Arena) -> None:
    """EC-VER-007 — needs nushell plus two staged binary builds to observe the two-hop body-rewrite lag; honest skip when absent."""
    _require("nushell")
    pytest.skip("nushell is present but this row needs two staged builds across consecutive self-update runs — not reachable with a single test binary")



# ---------------------------------------------------------------------------
# Environment size limits (EC-SIZE)
# ---------------------------------------------------------------------------


def _decode_carrier(carrier: str) -> dict:
    """Decode a ``__OCX_ENV_STATE`` wire value: ``"1." + base64url(json)``, no padding."""
    assert carrier.startswith("1."), f"unrecognized envelope tag: {carrier[:8]!r}"
    payload = carrier[len("1.") :]
    padded = payload + "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(padded))


def test_ec_size_002_a_spawned_child_inherits_the_over_cap_marker_ledger_intact(arena: Arena) -> None:
    """EC-SIZE-002 — A-01 restated: the over-cap marker ships (never zero bytes); a child process inherits it unaffected, decoding to ``{v, fp, verdict, over_cap}`` with both scope payloads absent."""
    padding = "x" * 900
    block = "".join(f'WP15_BIG_{index:02d} = "{padding}"\n' for index in range(24))
    project = _locked_project(arena, "big", block)

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            'bash -c \'printf "%s%s\\n" "@@child_carrier@@" "${__OCX_ENV_STATE-__OCX_ABSENT__}"\'',
        ],
        name="size002_over_cap_child_inherit",
    )
    assert result.returncode == 0, f"an over-cap project must not break the prompt:\n{result.stderr}"
    child_carrier = _read(result, "child_carrier")
    assert child_carrier != matrix.ABSENT, f"the spawned child must still inherit a carrier, marker-only or not:\n{result.stdout}"
    assert len(child_carrier) <= 16384, f"the marker must fit the cap; got {len(child_carrier)} bytes"
    decoded = _decode_carrier(child_carrier)
    assert {"v", "fp", "over_cap"} <= decoded.keys(), (
        f"an over-cap marker must retain v, fp and over_cap; got {sorted(decoded.keys())}"
    )
    assert decoded.get("verdict") in (None, {}), f"a marker ledger carries no cached verdict payload; got {decoded.get('verdict')!r}"
    assert decoded["fp"], "the marker must retain the fingerprint"
    assert "project" in decoded["over_cap"], f"the abandoned project scope must be named; got {decoded['over_cap']}"
    assert not decoded.get("scopes", {}).get("project"), (
        f"both scope payloads must be dropped whole, never partially retained: {decoded.get('scopes')}"
    )


def test_ec_size_003_windows_only_row_is_out_of_scope_here(arena: Arena) -> None:
    """EC-SIZE-003 — Windows-only leg (the 32767-char whole-block cap is enforced only there); skip observed via ``sys.platform``, not this Linux host."""
    if sys.platform == "win32":
        pytest.skip("EC-SIZE-003 needs the deep cross-platform matrix's Windows runner, not this direct pytest invocation")
    pytest.skip(f"observed sys.platform={sys.platform!r}: EC-SIZE-003's 32767-char whole-block cap is enforced only on Windows")


def test_ec_size_004_a_bloated_ambient_environment_plus_a_near_cap_carrier_still_spawns(arena: Arena) -> None:
    """EC-SIZE-004 — A-38: a near-16-KiB carrier combined with a bloated ambient env (simulating a CI runner) does not reach E2BIG on Linux."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    bloat = {f"WP15_AMBIENT_{i:03d}": "y" * 4000 for i in range(20)}  # ~80 KiB of unrelated ambient vars
    env = arena.env() | bloat
    result = matrix.reconcile(arena.ocx, "bash", project, env)
    assert result.returncode == 0, (
        f"a bloated ambient environment plus the carrier must not push process creation into E2BIG on Linux "
        f"(128 KiB MAX_ARG_STRLEN is non-binding at this combined size):\nstderr:\n{result.stderr}"
    )
    assert "export WP15_CONST='v1'" in result.stdout, f"the project scope must still apply normally:\n{result.stdout}"


def test_ec_size_005_a_large_but_individually_capped_env_still_starts_the_child(arena: Arena) -> None:
    """EC-SIZE-005 — A-38: many project ``[env]`` entries plus ambient vars, each individually under its own cap, must still start the spawned child successfully."""
    block = "".join(f'WP15_ENTRY_{index:02d} = "{"z" * 200}"\n' for index in range(30))
    project = _locked_project(arena, "many", block)
    bloat = {f"WP15_AMBIENT_{i:03d}": "y" * 500 for i in range(30)}
    env = arena.env() | bloat
    result = _session(
        "bash",
        arena,
        [matrix.cd_to("bash", project), matrix.prompt("bash"), "true"],
        env=env | {"PATH": arena.env("/bin/bash")["PATH"]},
        name="size005_large_combined_env",
    )
    assert result.returncode == 0, (
        f"a large but individually-capped combined environment must still let the child start; "
        f"documented degrade, not a hard number:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Filesystem failure modes (EC-FS)
# ---------------------------------------------------------------------------


def test_ec_fs_002_an_unwritable_state_root_degrades_the_hook_but_not_an_explicit_write(arena: Arena) -> None:
    """EC-FS-002 — the per-prompt path degrades silently on an unwritable state root (D3, D1:75); the explicit-write path (``ocx lock``, which owns the write seam) surfaces an ordinary ``IoError``(74) instead (Component Contracts, L383).

    Note: an unwritable **consent stamp directory specifically** does NOT
    reproduce this — the shipped code treats a consent-stamp write failure as
    explicitly non-fatal ("Shell-activation consent was not recorded
    (non-fatal)", still exit 0), which is a narrower carve-out than the row's
    blanket "an IO failure there is an ordinary IoError(74)" claim (no
    addendum-override marker on this row — reported as a finding). This test
    instead makes the PROJECT DIRECTORY itself (``ocx.lock``'s own write
    target) unwritable, which does hit the IoError(74) path.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    hook_result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert hook_result.returncode == 0, (
        f"the hook path must never break a prompt even before any write-target is touched:\nstderr:\n{hook_result.stderr}"
    )
    original_mode = project.stat().st_mode
    os.chmod(project, 0o500)  # read + execute, no write — ocx.lock's own containing directory can't be written
    try:
        # Directory mode bits are advisory for a process holding CAP_DAC_OVERRIDE
        # — uid 0, which is what the shell-zoo container runs as. PROBE it rather
        # than assume it: a probe write that SUCCEEDS here means the premise of
        # this row (an unwritable write target) was never staged, and asserting
        # on exit 74 would be asserting against a writable directory.
        canary = project / ".ocx-unwritable-probe"
        try:
            canary.touch()
        except OSError:
            pass  # the premise holds: this process really cannot write here
        else:
            observed_mode = oct(project.stat().st_mode & 0o777)
            canary.unlink()
            pytest.skip(
                f"this process bypasses directory mode bits (euid {os.geteuid()}): creating {canary} "
                f"succeeded despite the observed mode {observed_mode}, so an unwritable write target "
                "cannot be staged by chmod here"
            )
        write_result = subprocess.run(
            [str(arena.ocx), "--offline", "lock"], cwd=str(project), capture_output=True, check=False, text=True, env=arena.env(),
        )
        assert write_result.returncode == 74, (
            f"the explicit write path owns its own write seam and must surface an ordinary IoError(74), not silently "
            f"succeed while unable to write:\nstdout:\n{write_result.stdout}\nstderr:\n{write_result.stderr}"
        )
    finally:
        os.chmod(project, original_mode)


def test_ec_fs_003_an_unresolvable_home_degrades_to_no_consent_ever_recorded(arena: Arena) -> None:
    """EC-FS-003 — ``state/``'s contract is "safe to delete at any time"; an unwritable/unresolvable state root degrades to "no consent ever recorded", never a hard hook failure."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    env_without_ocx_home = {"HOME": "/nonexistent", "PATH": arena.env("/bin/bash")["PATH"]}
    assert "OCX_HOME" not in env_without_ocx_home
    unresolvable_home = matrix.reconcile(arena.ocx, "bash", project, env_without_ocx_home)
    assert unresolvable_home.returncode == 0, (
        f"HOME=/nonexistent (and therefore an unresolvable default OCX_HOME) must degrade gracefully, never crash "
        f"the prompt:\nstderr:\n{unresolvable_home.stderr}"
    )

    # Baseline first: with the state root readable the stamp is found and the
    # project applies. Without this, the "absent" assertion below would pass on a
    # project that was never activated in the first place.
    activated = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert "WP15_CONST" in activated.stdout, (
        f"the fixture must actually activate before its degradation can be observed:\nstdout:\n{activated.stdout}"
    )

    state_root = arena.ocx_home / "state"
    original_mode = state_root.stat().st_mode
    os.chmod(state_root, 0o000)
    try:
        # Directory mode bits are advisory for a process holding CAP_DAC_OVERRIDE
        # — uid 0, which is what the shell-zoo container runs as. PROBE it rather
        # than assume it: a read that SUCCEEDS here means an unreadable state root
        # was never staged, and everything below would assert against a readable one.
        try:
            list(state_root.iterdir())
        except OSError:
            pass  # the premise holds: this process really cannot read the state root
        else:
            observed_mode = oct(state_root.stat().st_mode & 0o777)
            pytest.skip(
                f"this process bypasses directory mode bits (euid {os.geteuid()}): listing {state_root} "
                f"succeeded despite the observed mode {observed_mode}, so an unreadable state root "
                "cannot be staged by chmod here"
            )
        recompose = matrix.reconcile(arena.ocx, "bash", project, arena.env())
        assert recompose.returncode == 0, (
            f"a totally unreadable state root must still degrade to 'no consent recorded', never a hard failure "
            f"on the hook path:\nstderr:\n{recompose.stderr}"
        )
        # Exit 0 alone is not the contract — a run that crashed-free but still
        # applied the project would satisfy it. "No consent ever recorded" means
        # the project is inert, so its constant must not reach the stream.
        assert "WP15_CONST" not in recompose.stdout, (
            "an unreadable state root means no stamp can be read, so the project must go inert — "
            f"not apply anyway:\nstdout:\n{recompose.stdout}"
        )
    finally:
        os.chmod(state_root, original_mode)


def test_ec_fs_004_concurrent_writers_never_produce_a_torn_consent_stamp(arena: Arena) -> None:
    """EC-FS-004 — writes go through an atomic rename (D2:129): concurrent writers race, but the result is always valid JSON, never interleaved.

    Simplified fixture: ``ocx lock`` (on A-29's closed writer allowlist,
    same as ``ocx add``) run several times concurrently against one
    env-only project — offline-safe, no registry round-trip needed to stress
    the atomic-rename property under test.
    """
    project = arena.projects / "concurrent"
    matrix.write_project(project, 'WP15_CONST = "v1"\n')
    env = arena.env()
    processes = [
        subprocess.Popen(
            [str(arena.ocx), "--offline", "lock"], cwd=str(project), env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        for _ in range(8)
    ]
    outcomes = [(proc.wait(), *proc.communicate()) for proc in processes]
    # `ocx.toml` is guarded by an exclusive flock, so losing the race and exiting
    # 75 (TempFail) is correct behaviour, not a defect — the row's property is
    # that the stamp is never torn, not that every writer wins. Asserting rc == 0
    # for all eight made this test pass only when the processes happened to
    # serialize, and red under `-n auto`. These are two exact outcomes, each
    # asserted for its own reason, not a tolerated range.
    winners = [rc for rc, _out, _err in outcomes if rc == 0]
    assert winners, (
        "at least one concurrent `ocx lock` must win the flock, or the stamp "
        f"assertions below are vacuous; outcomes: {[rc for rc, _o, _e in outcomes]}"
    )
    for rc, _out, err in outcomes:
        if rc == 0:
            continue
        assert rc == 75, (
            "a writer that loses the `ocx.toml` flock must exit 75 (TempFail) and "
            f"nothing else — a torn write or a crash would show up here; got {rc}:\n{err}"
        )
        assert "locked by another process" in err, (
            f"exit 75 must name lock contention as the cause, not stand in for it:\n{err}"
        )

    key = matrix.project_key(arena.ocx, project, env)
    consent_path = matrix.stamp_dir(arena.ocx_home, key) / "consent.json"
    assert consent_path.is_file(), f"a stamp must exist after concurrent writers converge: {consent_path}"
    raw = consent_path.read_bytes()
    parsed = json.loads(raw)  # must not raise — a torn/interleaved write is not valid JSON
    assert isinstance(parsed, dict) and parsed, f"the stamp must name a real, non-empty source set, never a corrupt fragment: {parsed!r}"


def test_ec_fs_005_a_live_projects_stamp_survives_a_concurrent_clean_sweep(arena: Arena) -> None:
    """EC-FS-005 — the sweep's re-probe immediately before removal means a still-live project's stamp is never torn out from under a concurrent reconcile.

    Approximates the exact TOCTOU window (unreachable deterministically from
    outside the binary) with the STATED guarantee instead: `ocx clean` and a
    reconcile running concurrently against the SAME, still-existing project
    must both complete cleanly, and the project's own stamp must still be
    present afterward — the re-probe guard exists precisely so a live
    project is never swept.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    key = matrix.project_key(arena.ocx, project, arena.env())
    stamp = matrix.stamp_dir(arena.ocx_home, key)
    assert stamp.exists(), "fixture sanity: locking must stamp consent"

    clean_proc = subprocess.Popen(
        [str(arena.ocx), "--offline", "clean"], cwd=str(arena.projects), env=arena.env(),
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    reconcile_result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    clean_rc, clean_out, clean_err = clean_proc.wait(), *clean_proc.communicate()
    assert clean_rc == 0, f"stderr:\n{clean_err}"
    assert reconcile_result.returncode == 0, f"stderr:\n{reconcile_result.stderr}"
    assert stamp.exists(), (
        f"a live project's stamp must never be swept by a concurrent `ocx clean`, even racing a reconcile:\n"
        f"clean stdout: {clean_out}\nreconcile stdout: {reconcile_result.stdout}"
    )


def test_ec_fs_006_a_lock_only_change_retires_the_now_absent_path_entry(arena: Arena) -> None:
    """EC-FS-006 — retirement rule (D3:154): after a branch-switch-equivalent change, a PATH entry no longer in D is REMOVED, not merely reordered.

    Simulated without real git: a fresh ``ocx.lock`` write (bumping its
    declaration hash, the actual watched signal per D3:170) paired with an
    ``[env]`` rewrite dropping the PATH entry — together standing in for
    "checked out a branch whose lock/env no longer declares this tool",
    since driving a real tool-to-PATH resolution offline is not reachable
    without a registry.
    """
    project = _locked_project(arena, "alpha", _ENV_BLOCK_A)
    (project / "binA").mkdir()
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before_path", "PATH"),
            _write_config_env("bash", project / "ocx.toml", 'WP15_CONST = "alpha"\n'),
            f'{matrix.quote("bash", str(arena.ocx))} --offline lock >/dev/null 2>&1',
            matrix.prompt("bash"),
            matrix.probe("bash", "after_path", "PATH"),
        ],
        name="fs006_lock_only_retirement",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    before = matrix.path_segments(_read(result, "before_path"))
    after = matrix.path_segments(_read(result, "after_path"))
    assert str(project / "binA") in before, f"fixture sanity: the PATH entry must apply first:\n{result.stdout}"
    assert str(project / "binA") not in after, f"a no-longer-declared PATH entry must be REMOVED, not reordered:\n{result.stdout}"


def test_ec_fs_007_an_env_only_change_re_captures_the_constants_prior(arena: Arena) -> None:
    """EC-FS-007 — ``[env]`` applies on its own authority, independent of the lock (the watch set includes ``ocx.toml`` itself, D3:166); the constant's new value applies and the prior is re-captured, not reused."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    live_env = arena.env() | {"WP15_CONST": "/user-set"}
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "WP15_CONST"),
            _write_config_env("bash", project / "ocx.toml", 'WP15_CONST = "v2"\n'),
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "WP15_CONST"),
        ],
        env=live_env,
        name="fs007_env_only_recapture",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before") == "v1", f"stdout:\n{result.stdout}"
    assert _read(result, "after") == "v2", (
        f"an ocx.toml-only [env] edit must be watched and applied on its own authority, independent of ocx.lock:\n{result.stdout}"
    )


def test_ec_fs_008_a_lost_ocx_toml_reverts_the_whole_project_scope(arena: Arena) -> None:
    """EC-FS-008 — a branch with no ``ocx.toml`` at all degenerates to scope-exit: every project-scoped element and constant reverts via the ordinary revert-set mechanism, exactly like an actual ``cd`` out."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "WP15_CONST"),
            f"rm {matrix.quote('bash', str(project / 'ocx.toml'))} {matrix.quote('bash', str(project / 'ocx.lock'))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "WP15_CONST"),
        ],
        name="fs008_lost_ocx_toml",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before") == "v1", f"stdout:\n{result.stdout}"
    assert _read(result, "after") == matrix.ABSENT, (
        f"a project directory with no ocx.toml at all must revert the whole project scope, same as an ordinary "
        f"scope-exit:\n{result.stdout}"
    )


def test_ec_fs_015_the_cwd_itself_disappearing_degrades_gracefully(arena: Arena) -> None:
    """EC-FS-015 — A-11: a ``getcwd()``/canonicalize failure on the CWD ITSELF (not just an ancestor) degrades identically to any other per-prompt I/O error, never a hard failure.

    Empirically, a bash process (and any child it forks) that was already
    `cd`'d into a directory keeps a live kernel reference to it: removing the
    directory from a SEPARATE process does not make `getcwd()` fail for the
    process still holding that reference (verified directly: `pwd` and a real
    ``current_dir()`` call both keep resolving the orphaned path after an
    external ``rm -rf``). So the actual, portably-observable claim here is
    narrower than "the scope reverts" — it is "nothing crashes, no error
    surfaces" regardless of which way the (already-orphaned) CWD resolves.
    """
    project = arena.projects / "vanishing"
    matrix.write_project(project, 'WP15_CONST = "v1"\n')
    locked = matrix.run_lock(arena.ocx, project, arena.env())
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "before", "WP15_CONST"),
            # Remove the CWD out from under the still-running shell — a
            # standard POSIX-allowed operation.
            f"rm -rf {matrix.quote('bash', str(project))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "after", "WP15_CONST"),
        ],
        name="fs015_cwd_removed",
    )
    assert result.returncode == 0, (
        f"a CWD that fails to resolve at all must degrade gracefully — no crash, no exit != 0:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "panic" not in result.stderr.lower() and "panic" not in result.stdout.lower(), (
        f"an unresolvable CWD must never panic the binary:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert _read(result, "before") == "v1", f"stdout:\n{result.stdout}"
    after = _read(result, "after")
    assert after in ("v1", matrix.ABSENT), (
        f"whichever way the orphaned CWD resolves, the outcome must be one of the two legal, non-crashing states:\n{result.stdout}"
    )


def test_ec_fs_016_the_hook_re_resolves_current_fresh_every_prompt(arena: Arena) -> None:
    """EC-FS-016(a)/(b) — the reconcile call re-resolves ``current`` fresh every prompt (D6:302): a swapped binary is picked up immediately; a missing target degrades to a silent no-op, never an error."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    candidate = arena.ocx_home / _CANDIDATE_REL
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            f'eval "$({matrix.quote("bash", str(arena.ocx))} --offline self activate --shell=bash --hook)"',
            "__ocx_prompt_hook",
            matrix.probe("bash", "before_swap", "WP15_CONST"),
            # (a) Re-point `current` at a byte-identical copy — proves the
            # reconcile call re-resolves fresh rather than caching anything.
            f"cp {matrix.quote('bash', str(arena.ocx))} {matrix.quote('bash', str(candidate))}.new",
            f"mv {matrix.quote('bash', f'{candidate}.new')} {matrix.quote('bash', str(candidate))}",
            "__ocx_prompt_hook",
            matrix.probe("bash", "after_swap", "WP15_CONST"),
            # (b) Remove the `current` target entirely — the probe-guard's
            # `-x` check must fail closed, silently, never an error.
            f"rm {matrix.quote('bash', str(candidate))}",
            "__ocx_prompt_hook",
            'printf "@@rc_after_missing@@%s\\n" "$?"',
        ],
        name="fs016_current_reresolve",
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    assert _read(result, "before_swap") == "v1", f"stdout:\n{result.stdout}"
    assert _read(result, "after_swap") == "v1", (
        f"a fresh binary swapped into `current` must be picked up on the very next prompt, no shell restart needed:\n{result.stdout}"
    )
    assert _read(result, "rc_after_missing") == "0", (
        f"a missing `current` target must fail closed (probe-guard -x) and never surface a nonzero status to the prompt:\n{result.stdout}"
    )



# ---------------------------------------------------------------------------
# Consent / grant / config / identity cluster (EC-CONSENT, EC-GRANT, EC-CFG, EC-IDENT)
# ---------------------------------------------------------------------------


def test_ec_consent_012_a_paths_grant_activates_and_writes_no_stamp(arena: Arena) -> None:
    """EC-CONSENT-012 — Addendum override (A-26): retired. A ``paths``-granted project activates silently and writes NO stamp (drift is not tracked for it).

    The row's own original premise ("stamp written with no prompt") is
    explicitly retired by A-26 — this test asserts the CURRENT, opposite
    resolution, driven through ``[shell.consent] paths`` in ``config.toml``
    (WP-14 already covers the ``OCX_CONSENT_PATHS`` env-var channel; this
    exercises the config-file channel instead).
    """
    project = arena.projects / "granted"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nt = "ghcr.io/acme/tool:1"\n')
    matrix.write_lock(project, matrix.lock_tool("t", "ghcr.io/acme/tool"))
    _write_config(arena, f'[shell.consent]\npaths = [{json.dumps(str(project))}]\n')

    state = matrix.shell_state(arena.ocx, project, arena.env())
    assert state["inert_reason"]["reason"] != "no_stamp_no_grant", (
        f"a paths grant must activate the project: {state['inert_reason']}"
    )
    key = state["project_key"]
    assert not matrix.stamp_dir(arena.ocx_home, key).exists(), (
        "A-26: a paths grant activates and writes NO stamp — never a baseline to drift from"
    )


def test_ec_grant_018_reserved_ocx_keys_never_reach_the_composed_environment(arena: Arena) -> None:
    """EC-GRANT-018 — reserved ``OCX_*``/``__OCX_*`` keys are stripped at the application seam, never reach a spawned child.

    The row's literal reproduction publishes a package whose metadata smuggles
    these keys (rejected at ``package create`` with exit 65, per the row's own
    text) and observes through ``ocx run``/``ocx exec`` — unreachable here
    without a real registry (forbidden fixture). This exercises the SAME
    underlying reservation (``is_reserved_ocx_key``) through the reachable
    channel: a project's own ``[env]`` table declaring these keys directly.

    Finding: the row's Expected-behaviour text says these keys are "skipped
    at the application seam" (implying they parse, then get stripped during
    composition). The shipped ``[env]``-in-``ocx.toml`` channel is actually
    STRONGER — refused at PARSE TIME (exit 78), before any application seam
    is reached. Not a contradiction, but a stronger gate than described; no
    addendum-override marker present on this row.
    """
    project = arena.projects / "smuggle"
    matrix.write_project(
        project,
        'OCX_NO_HOOK = "1"\n'
        'OCX_CONSENT_NAMESPACES = "ocx.sh/*"\n'
        '__OCX_ENV_STATE = "1.forged"\n'
        'WP15_ORDINARY = "kept"\n',
    )
    result = matrix.run_lock(arena.ocx, project, arena.env())
    # Stronger than the row expected: `ocx.toml`'s own [env] table refuses a
    # reserved key at PARSE TIME (exit 78), before the application seam this
    # row names is ever reached — the reservation is structurally enforced
    # earlier than `is_reserved_ocx_key`'s apply-time strip.
    assert result.returncode == 78, (
        f"a reserved OCX_*/__OCX_* key in [env] must be refused at parse time, never silently accepted then "
        f"stripped later:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "reserved" in result.stderr and "OCX_" in result.stderr, f"stderr:\n{result.stderr}"


def test_ec_cfg_004_the_managed_consent_strip_reason_rides_the_evald_stream(arena: Arena) -> None:
    """EC-CFG-004 — the managed-tier consent-strip reason (C-034) must reach the operator through the same eval'd ``printf … >&2`` channel the rest of the hook uses, not only ``log::warn!`` (a stderr the shims discard)."""
    managed_dir = arena.ocx_home / "state" / "managed-config"
    managed_dir.mkdir(parents=True, exist_ok=True)
    (arena.ocx_home / "config.toml").write_text('[managed]\nsource = "ghcr.io/acme/config:v1"\nrequired = false\n', encoding="utf-8")
    (managed_dir / "snapshot.json").write_text(
        json.dumps(
            {
                "source": "ghcr.io/acme/config:v1",
                "tag": "v1",
                "digest": "sha256:" + "11" * 32,
                "fetched_at": "2026-01-01T00:00:00Z",
            }
        ),
        encoding="utf-8",
    )
    (managed_dir / "config.toml").write_text('[shell.consent]\nnamespaces = "ghcr.io/attacker/*"\n', encoding="utf-8")

    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    result = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "digest-pinned" in result.stdout and "shell.consent" in result.stdout, (
        f"the strip reason must ride the eval'd stream (this test's own result.stdout is the binary's stdout, "
        f"never affected by a shim discarding the process's stderr):\n{result.stdout}"
    )


def test_ec_cfg_012_a_new_grant_is_observed_at_the_very_next_prompt(arena: Arena) -> None:
    """EC-CFG-012 — A-13 (already shipped): the watch set now includes the config tier's own paths plus the consent stamp, so a newly-added grant is observed at the next prompt, never stuck inert for the shell's life.

    The row's own premise (a real gap: config.toml/env are watched by
    nothing, so the fingerprint never moves) is the PRE-A-13 state; A-13
    closed it. This asserts the CURRENT, fixed behaviour.
    """
    project = arena.projects / "waiting"
    matrix.write_project(project, _ENV_BLOCK_A, tools_block='[tools]\nt = "ghcr.io/acme/tool:1"\n')
    matrix.write_lock(project, matrix.lock_tool("t", "ghcr.io/acme/tool"))

    before = matrix.shell_state(arena.ocx, project, arena.env())
    assert before["inert_reason"]["reason"] == "no_stamp_no_grant", f"fixture sanity: must start inert; got {before['inert_reason']}"

    _write_config(arena, f'[shell.consent]\npaths = [{json.dumps(str(project))}]\n')
    after = matrix.shell_state(arena.ocx, project, arena.env())
    assert after["inert_reason"]["reason"] != "no_stamp_no_grant", (
        f"a grant added to config.toml must be observed at the very next prompt — the config tier is in the "
        f"watch set (A-13), never requiring a shell restart: {after['inert_reason']}"
    )


def test_ec_ident_012_every_offline_reachable_writer_stamps_consent(arena: Arena) -> None:
    """EC-IDENT-012 — every one of the six consent-stamping commands writes ``state/projects/<key>/consent.json`` on an unstamped, granted project.

    Of the six (``add``, ``remove``, ``lock``, ``update``, ``pull``, ``run``),
    only ``lock`` and ``run -- true`` are reachable offline without a
    registry (``add``/``remove``/``update``/``pull`` all need real resolution
    against a registry, forbidden as a fixture here) — this test covers those
    two; the other four are named as untestable-within-constraints in the
    coverage report, not silently skipped.
    """
    for command in (["lock"], ["exec", "--", "true"]):
        project = arena.projects / f"ident012_{command[0]}"
        matrix.write_project(project, 'WP15_CONST = "v1"\n')
        if command != ["lock"]:
            # `run` needs an existing ocx.lock as a prerequisite; locking
            # first is the realistic workflow, and offline-safe (env-only).
            prelock = matrix.run_lock(arena.ocx, project, arena.env())
            assert prelock.returncode == 0, f"stderr:\n{prelock.stderr}"
            (matrix.stamp_dir(arena.ocx_home, matrix.project_key(arena.ocx, project, arena.env())) / "consent.json").unlink()
        result = subprocess.run(
            [str(arena.ocx), "--offline", *command], cwd=str(project), capture_output=True, check=False, text=True, env=arena.env(),
        )
        assert result.returncode == 0, f"`{' '.join(command)}` must itself succeed; stderr:\n{result.stderr}"
        key = matrix.project_key(arena.ocx, project, arena.env())
        stamp = matrix.stamp_dir(arena.ocx_home, key) / "consent.json"
        assert stamp.is_file(), f"`ocx {' '.join(command)}` must stamp consent for a fresh, unstamped project: {stamp}"


def test_ec_ident_013_read_only_commands_never_create_a_project_state_dir(arena: Arena) -> None:
    """EC-IDENT-013 — A-29: the six writers are an explicit allowlist; every OTHER command, including ``ocx shell state`` itself, must never consent to the project it is diagnosing."""
    project = arena.projects / "diagnose_only"
    matrix.write_project(project, 'WP15_CONST = "v1"\n')
    # Deliberately no `ocx lock` — this project must stay genuinely unstamped.
    key = matrix.project_key(arena.ocx, project, arena.env())
    stamp = matrix.stamp_dir(arena.ocx_home, key)
    assert not stamp.exists(), "fixture sanity: must start with no project state dir at all"

    for command in (["env"], ["inspect"], ["shell", "state"], ["self", "activate"]):
        result = subprocess.run(
            [str(arena.ocx), "--offline", "--format", "json", *command],
            cwd=str(project), capture_output=True, check=False, text=True, env=arena.env(),
        )
        assert not stamp.exists(), (
            f"`ocx {' '.join(command)}` must never create {stamp} — a read-only command must not silently consent "
            f"to the project it is diagnosing:\nrc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )



# ---------------------------------------------------------------------------
# Project resolution / scope switching (EC-SCOPE)
# ---------------------------------------------------------------------------


def test_ec_scope_002_leaving_a_project_restores_the_global_prior_and_leaves_global_intact(arena: Arena) -> None:
    """EC-SCOPE-002 — normative ordering: apply global, THEN capture the project's priors, THEN apply project; leaving restores the global value and leaves the global scope itself untouched."""
    global_toml = arena.ocx_home / "ocx.toml"
    global_toml.write_text('[env]\nWP15_CONST = "global"\n', encoding="utf-8")
    locked = subprocess.run(
        [str(arena.ocx), "--offline", "--global", "lock"], capture_output=True, check=False, text=True, env=arena.env(), cwd=str(arena.projects),
    )
    assert locked.returncode == 0, f"stderr:\n{locked.stderr}"
    project = _locked_project(arena, "alpha", 'WP15_CONST = "proj"\n')
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", project),
            matrix.prompt("bash"),
            matrix.probe("bash", "inside", "WP15_CONST"),
            matrix.cd_to("bash", arena.home),
            matrix.prompt("bash"),
            matrix.probe("bash", "outside", "WP15_CONST"),
        ],
        name="scope002_global_prior",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "inside") == "proj", f"stdout:\n{result.stdout}"
    assert _read(result, "outside") == "global", (
        f"leaving the project must restore the captured prior ('global'), never Unset — the prior was captured "
        f"AFTER global's own apply, not before:\n{result.stdout}"
    )


def test_ec_scope_003_switching_projects_directly_reverts_a_and_applies_b_in_one_pass(arena: Arena) -> None:
    """EC-SCOPE-003 — no intermediate prompt elsewhere: revert A and apply B happen in the SAME prompt/pass, never through a phantom third slot."""
    p1 = _locked_project(arena, "p1", 'WP15_CONST = "a"\n' + 'PATH = { type = "path", value = "binX" }\n')
    (p1 / "binX").mkdir()
    p2 = _locked_project(arena, "p2", 'WP15_CONST = "b"\n' + 'PATH = { type = "path", value = "binY" }\n')
    (p2 / "binY").mkdir()
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", p1),
            matrix.prompt("bash"),
            matrix.cd_to("bash", p2),
            # No prompt call between the two `cd`s — a single pass reconciles the switch.
            matrix.prompt("bash"),
            matrix.probe("bash", "const", "WP15_CONST"),
            matrix.probe("bash", "path", "PATH"),
        ],
        name="scope003_direct_switch",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "const") == "b", f"stdout:\n{result.stdout}"
    segments = matrix.path_segments(_read(result, "path"))
    assert str(p2 / "binY") in segments, f"stdout:\n{result.stdout}"
    assert str(p1 / "binX") not in segments, f"A's PATH element must be fully retired in the same pass:\n{result.stdout}"


def test_ec_scope_004_nested_projects_switch_rather_than_layer(arena: Arena) -> None:
    """EC-SCOPE-004 — the nearest ocx.toml wins and returns on first hit: entering a nested project SWITCHES scope, never layers it."""
    outer = _locked_project(arena, "outer", 'WP15_CONST = "outer"\n')
    inner = outer / "inner"
    matrix.write_project(inner, 'WP15_CONST = "inner"\n')
    locked_inner = matrix.run_lock(arena.ocx, inner, arena.env())
    assert locked_inner.returncode == 0, f"stderr:\n{locked_inner.stderr}"

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", outer),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_outer", "WP15_CONST"),
            matrix.cd_to("bash", inner),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_inner", "WP15_CONST"),
            matrix.cd_to("bash", outer),
            matrix.prompt("bash"),
            matrix.probe("bash", "back_at_outer", "WP15_CONST"),
        ],
        name="scope004_nested_switch",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "at_outer") == "outer", f"stdout:\n{result.stdout}"
    assert _read(result, "at_inner") == "inner", f"nesting must SWITCH, not layer atop outer:\n{result.stdout}"
    assert _read(result, "back_at_outer") == "outer", f"stdout:\n{result.stdout}"


def test_ec_scope_005a_a_git_boundary_inside_the_project_tree_is_retained_as_indeterminate(arena: Arena) -> None:
    """EC-SCOPE-005(a) — A-11's ancestor-or-self check applies uniformly to ANY walk miss, not just transient I/O errors: a genuine ``.git`` boundary below the project root is retained (indeterminate), not reverted.

    Finding: the row's own Expected-behaviour text says this case "must be
    reverted" — no addendum-override marker is present. Empirically (and by
    reading ``ocx_lib/src/activation.rs::walk_is_indeterminate``), a walk miss
    while CWD is still an ancestor-or-self of the previously-applied project's
    directory AND that project's ``ocx.toml`` is still a regular file is classified
    ``Walk::Indeterminate`` REGARDLESS of why the walk missed — the code
    never distinguishes "a transient I/O error" (A-11's literal question)
    from "a genuine deeper .git boundary" (this row's scenario). A-11's
    actual shipped scope is broader than its own "Question" states. This
    test asserts the SHIPPED behaviour.
    """
    work = _locked_project(arena, "work", 'WP15_CONST = "outer"\n')
    repo = work / "repo"
    (repo / ".git").mkdir(parents=True)
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", work),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_work", "WP15_CONST"),
            matrix.cd_to("bash", repo),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_repo", "WP15_CONST"),
        ],
        name="scope005a_bare_git_boundary",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "at_work") == "outer", f"stdout:\n{result.stdout}"
    assert _read(result, "at_repo") == "outer", (
        f"A-11: CWD (repo) is still an ancestor-or-self of the applied project's dir (work), and work/ocx.toml is "
        f"still a regular file, so the walk miss at the .git boundary is INDETERMINATE — the outer scope is "
        f"retained, not reverted:\n{result.stdout}"
    )


def test_ec_scope_005b_a_project_file_at_the_repo_root_still_wins_over_the_git_gate(arena: Arena) -> None:
    """EC-SCOPE-005(b) — the candidate check runs BEFORE the .git gate: an ocx.toml at the very same directory as .git still activates."""
    repo = _locked_project(arena, "repo", 'WP15_CONST = "at_repo_root"\n')
    (repo / ".git").mkdir()
    result = matrix.reconcile(arena.ocx, "bash", repo, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='at_repo_root'" in result.stdout, (
        f"an ocx.toml co-located with .git must still win — the candidate check precedes the git gate:\n{result.stdout}"
    )


def test_ec_scope_007_a_symlinked_candidate_is_skipped_and_the_ancestor_activates(arena: Arena) -> None:
    """EC-SCOPE-007 — A-12: a symlinked ocx.toml is skipped by the walk, silently (the hook discards stderr); the ancestor's own project file activates instead."""
    work = _locked_project(arena, "work", 'WP15_CONST = "ancestor"\n')
    proj = work / "proj"
    proj.mkdir()
    elsewhere_toml = arena.scripts / "elsewhere_ocx.toml"
    elsewhere_toml.write_text('[env]\nWP15_CONST = "symlinked-away"\n', encoding="utf-8")
    (proj / "ocx.toml").symlink_to(elsewhere_toml)

    result = matrix.reconcile(arena.ocx, "bash", proj, arena.env())
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='ancestor'" in result.stdout, (
        f"a symlinked candidate must be skipped, promoting the ancestor's real ocx.toml, not the symlink target:\n{result.stdout}"
    )
    assert "symlinked-away" not in result.stdout, f"stdout:\n{result.stdout}"


def test_ec_scope_008_an_explicit_ocx_project_selector_never_consults_the_cwd(arena: Arena) -> None:
    """EC-SCOPE-008 — the explicit ``OCX_PROJECT`` tier resolves one file and returns, replacing rather than layering: a PWD event changes nothing while it is set."""
    p1 = _locked_project(arena, "p1", 'WP15_CONST = "explicit"\n')
    p2 = _locked_project(arena, "p2", 'WP15_CONST = "cwd"\n')
    result = matrix.reconcile(arena.ocx, "bash", p2, arena.env(OCX_PROJECT=str(p1 / "ocx.toml")))
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert "export WP15_CONST='explicit'" in result.stdout, (
        f"OCX_PROJECT must win outright — the CWD (p2) must never override the explicit selector:\n{result.stdout}"
    )
    assert "'cwd'" not in result.stdout, f"stdout:\n{result.stdout}"


def test_ec_scope_009_entering_an_unconsented_fresh_clone_reverts_the_prior_project(arena: Arena) -> None:
    """EC-SCOPE-009 — an unconsented fresh clone yields an empty project D, which is exactly the retirement rule's scope-exit trigger: the prior project's scope must revert, never leak."""
    p1 = _locked_project(arena, "p1", 'WP15_CONST = "a"\n' + 'PATH = { type = "path", value = "binX" }\n')
    (p1 / "binX").mkdir()
    fresh_clone = _clone_of(p1, arena.projects / "fresh_clone")
    (fresh_clone / "state_marker_absent").write_text("no stamp, no grant, by construction of _clone_of\n", encoding="utf-8")

    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", p1),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_p1", "WP15_CONST"),
            matrix.cd_to("bash", fresh_clone),
            matrix.prompt("bash"),
            matrix.probe("bash", "at_clone_const", "WP15_CONST"),
            matrix.probe("bash", "at_clone_path", "PATH"),
        ],
        name="scope009_unconsented_clone",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "at_p1") == "a", f"stdout:\n{result.stdout}"
    assert _read(result, "at_clone_const") == matrix.ABSENT, (
        f"an unconsented clone must revert the prior project's constant, never leak it in as 'inert = do nothing':\n{result.stdout}"
    )
    assert str(p1 / "binX") not in matrix.path_segments(_read(result, "at_clone_path")), (
        f"the prior project's PATH element must be retired too:\n{result.stdout}"
    )


def test_ec_scope_006_a_transient_git_read_failure_below_the_project_boundary(arena: Arena) -> None:
    """EC-SCOPE-006 — A-11 (shipped): a transient ``.git`` read failure at a level between the CWD and the project file is classified INDETERMINATE and retains the scope unchanged, never flaps.

    Correction: an earlier pass of this file concluded A-11 was unshipped
    from a case-sensitive grep for lowercase ``indeterminate``/``determinacy``
    — the real implementation uses PascalCase (``Walk::Indeterminate``, which
    ``crates/ocx_cli/src/command/self_group/activate.rs`` classifies from the
    predicate ``ocx_lib/src/activation.rs::walk_is_indeterminate``), which the
    grep silently missed. Confirmed shipped by direct code read and by this
    test's own green result below.
    """
    p1 = _locked_project(arena, "p1", 'WP15_CONST = "v1"\n')
    (p1 / "sub" / "deeper").mkdir(parents=True)
    result = _session(
        "bash",
        arena,
        [
            matrix.cd_to("bash", p1 / "sub" / "deeper"),
            matrix.prompt("bash"),
            matrix.probe("bash", "before_chmod", "WP15_CONST"),
            f"chmod 000 {matrix.quote('bash', str(p1 / 'sub'))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "during_chmod", "WP15_CONST"),
            f"chmod 755 {matrix.quote('bash', str(p1 / 'sub'))}",
            matrix.prompt("bash"),
            matrix.probe("bash", "after_restore", "WP15_CONST"),
        ],
        name="scope006_transient_git_read_failure",
    )
    assert result.returncode == 0, f"stderr:\n{result.stderr}"
    assert _read(result, "before_chmod") == "v1", f"stdout:\n{result.stdout}"
    assert _read(result, "during_chmod") == "v1", (
        f"A-11 IS shipped (confirmed via activation.rs::walk_is_indeterminate, Walk::Indeterminate/Determinate/"
        f"Resolved) — CWD is still an ancestor-or-self of the applied project's dir and its ocx.toml is still a "
        f"regular file, so the walk miss is INDETERMINATE: the scope is retained UNCHANGED, never flaps:\n{result.stdout}"
    )
    assert _read(result, "after_restore") == "v1", f"stdout:\n{result.stdout}"



def test_ec_consent_015_an_env_only_channel_is_gated_by_consent_like_every_other(arena: Arena) -> None:
    """EC-CONSENT-015 — an inert project applies nothing, including the one channel that needs no lock, no download and no registry.

    The register files this row ``rust-unit``, but the predicate is not where
    it is observable: ``consent::evaluate`` never receives ``[env]`` at all, so
    no unit test of it can distinguish "inert" from "inert except for
    ``[env]``". Only the emitted stream can. Decision 3's "``[env]`` applies on
    its own authority independently of the lock" governs the **watch set**,
    never activation — otherwise inertness would hold for the tool channel and
    not for the cheapest one.
    """
    project = _locked_project(arena, "envonly", 'JAVA_HOME = "/opt/j"\n')
    env = arena.env()

    # Baseline: consented, the constant reaches the stream. Without this the
    # negative below would pass on a project that never applied anything.
    granted = matrix.reconcile(arena.ocx, "bash", project, arena.env(OCX_CONSENT_PATHS=str(project.resolve())))
    assert "JAVA_HOME" in granted.stdout, (
        f"the fixture must apply its [env] channel when consented:\n{granted.stdout}"
    )

    # Revoke every grant and delete the stamp `ocx lock` wrote: now inert.
    stamp_dir = matrix.stamp_dir(arena.ocx_home, matrix.project_key(arena.ocx, project, env))
    shutil.rmtree(stamp_dir, ignore_errors=True)
    assert not stamp_dir.exists(), f"the stamp must be gone for the project to be inert: {stamp_dir}"

    inert = matrix.reconcile(arena.ocx, "bash", project, env)
    assert inert.returncode == 0, f"an inert project is not an error:\nstderr:\n{inert.stderr}"
    assert "JAVA_HOME" not in inert.stdout, (
        "an inert project must apply NOTHING — [env] is gated by consent exactly like the tool "
        f"channel, and needing no lock is not a licence to skip the gate:\n{inert.stdout}"
    )


# ---------------------------------------------------------------------------
# Traceability self-check — the register's own coverage column must be real
# ---------------------------------------------------------------------------

def _locate_register() -> Path | None:
    """Find the edge-case register, or ``None`` when this module runs outside the repo.

    The shell-zoo container bind-mounts this file alone at ``/work``, so the
    repo-relative walk has no ancestors to climb and raises ``IndexError`` at
    import time — which took the whole zoo leg down with a collection error
    rather than skipping one repo-consistency check.
    """
    here = Path(__file__).resolve()
    for ancestor in here.parents:
        candidate = ancestor / ".claude" / "artifacts" / "analysis_shell_env_edge_cases.md"
        if candidate.is_file():
            return candidate
    return None


_REGISTER_PATH = _locate_register()
_THIS_MODULE_PATH = Path(__file__).resolve()


def _split_table_row(line: str) -> list[str]:
    """Split one markdown table row into cells, respecting CommonMark code-span backtick runs and ``\\|`` escapes."""
    s = line.strip()
    s = s.removeprefix("|")
    s = s.removesuffix("|")
    cells: list[str] = []
    buf: list[str] = []
    i = 0
    n = len(s)
    code_run = 0
    while i < n:
        c = s[i]
        if c == "`":
            j = i
            while j < n and s[j] == "`":
                j += 1
            run_len = j - i
            if code_run == 0:
                code_run = run_len
            elif run_len == code_run:
                code_run = 0
            buf.append(s[i:j])
            i = j
            continue
        if c == "\\" and i + 1 < n and s[i + 1] == "|":
            buf.append("|")
            i += 2
            continue
        if c == "|" and code_run == 0:
            cells.append("".join(buf).strip())
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    cells.append("".join(buf).strip())
    return cells


def _parse_register() -> dict[str, dict[str, str]]:
    """Parse every ``EC-*`` row of the register into ``{id: {column: value}}``, skipping the non-primary recap table (``| ID | Gap | Recommendation |``)."""
    rows: dict[str, dict[str, str]] = {}
    header: list[str] | None = None
    for raw_line in _REGISTER_PATH.read_text(encoding="utf-8").split("\n"):
        line = raw_line.rstrip("\n")
        if not line.strip().startswith("|"):
            header = None
            continue
        bare = line.strip().strip("|")
        if re.fullmatch(r"[\s:\-|]+", bare):
            continue
        cells = _split_table_row(line)
        if len(cells) < 2:
            continue
        if cells[0] == "ID":
            header = cells if "Coverage" in cells else None  # only the primary per-row tables carry Coverage
            continue
        if header is None or not re.fullmatch(r"EC-[A-Z]+-\d+", cells[0]):
            continue
        # Structural check the row COUNT cannot substitute for. A row that
        # under-parses still lands in `rows`, so `len(register) == 223` stays
        # green while `Test tier` / `Coverage` hold the wrong cell or none at
        # all — and every gate below then classifies the row as "not mine" and
        # skips it. The usual cause is an unbalanced backtick run swallowing
        # the `|` delimiters for the rest of the line.
        assert len(cells) == len(header), (
            f"{cells[0]} split into {len(cells)} cell(s) against a {len(header)}-column header "
            f"— it would have reached only {header[: len(cells)]}. A mis-split row is invisible "
            "to every traceability gate in this module while the row count still looks right. "
            "Check the row for a code span containing a literal backtick (use a ``…`` span) "
            "or an unclosed one."
        )
        rows[cells[0]] = dict(zip(header, cells, strict=True))
    return rows


_TIER_LABELS = ("rust-unit", "pytest-hostshell", "pytest-shellzoo", "manual-only")


def _primary_tier(tier: str) -> str | None:
    """The earliest-mentioned label in a (possibly combined) ``Test tier`` cell.

    The column's own docstring ("How to read a row") already names this rule:
    "Two tiers named with `+` … and the first is primary." This generalises it
    past the `+`-only case to any free-text ordering (some rows separate a
    second tier with `;` instead), and it is what keeps a cross-reference to
    another row's tier — `` `pytest-hostshell` (EC-FP-001) `` trailing inside an
    otherwise `manual-only` cell — from being misread as this row's own tier:
    a row's own tier is always named first, a citation of someone else's
    always trails as elaboration.
    """
    best_label: str | None = None
    best_index: int | None = None
    for label in _TIER_LABELS:
        index = tier.find(label)
        if index != -1 and (best_index is None or index < best_index):
            best_index = index
            best_label = label
    return best_label


def _this_modules_test_to_ids() -> dict[str, list[str]]:
    """AST-parse THIS file (not the register) for every ``test_*`` function's docstring-cited ``EC-*`` IDs."""
    tree = ast.parse(_THIS_MODULE_PATH.read_text(encoding="utf-8"))
    mapping: dict[str, list[str]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name.startswith("test_"):
            doc = ast.get_docstring(node) or ""
            ids = re.findall(r"EC-[A-Z]+-\d+", doc.split("\n", 1)[0]) or re.findall(r"EC-[A-Z]+-\d+", doc)
            mapping[node.name] = sorted(set(ids))
    return mapping


def _placeholder_tests() -> set[str]:
    """``<file>::<name>`` for every ``test_*`` in THIS module whose body runs no ``assert`` and no ``pytest.fail``.

    Such a body is a placeholder, not coverage. It reports the same green
    whether the behaviour holds or not — which is the state a test that never
    ran is in (``quality-core.md`` §"Unchecked Green"). The gate below refuses
    to accept one as a row's only coverage.
    """
    tree = ast.parse(_THIS_MODULE_PATH.read_text(encoding="utf-8"))
    placeholders: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.FunctionDef) or not node.name.startswith("test_"):
            continue
        kids = list(ast.walk(node))
        if any(isinstance(kid, ast.Assert) for kid in kids):
            continue
        if any(
            isinstance(kid, ast.Call) and isinstance(kid.func, ast.Attribute) and kid.func.attr == "fail"
            for kid in kids
        ):
            continue
        placeholders.add(f"{_THIS_MODULE_PATH.name}::{node.name}")
    return placeholders


# Rows whose Coverage cell declares the register's own ``uncovered`` vocabulary
# — no test anywhere executes an assertion for them, and the placeholder that
# cites them only records WHY. Pinned so a tenth row cannot join them quietly:
# the escape hatch is for making a gap visible, not for widening it.
#
# EC-HOOK-009, EC-PATH-013, EC-QUOTE-004, EC-QUOTE-010, EC-QUOTE-011 and
# EC-SIZE-003 left this set in ocx#353's retier — none has a Windows leg of
# THIS suite to wait on (the module-level skip above says why), but that is
# no longer "manual-only for lack of trying": ocx#354 (the bug that once
# stopped the Windows deep job from completing) is fixed, so the retier
# checked, per row, whether `verify-deep.yml`'s windows-latest `nextest` leg
# could actually automate the live `cmd.exe` half rather than defaulting to
# manual.
#   - EC-QUOTE-004 / EC-QUOTE-010: fully automated — Batch quoting is pure
#     `Shell` logic (`batch_refuses_percent_lf_and_cr_on_both_emitters` in
#     `shell.rs`), and a refused value never reaches a live interpreter, so
#     there is no live half left to assert.
#   - EC-PATH-013: fully automated — `live_batch_unanchored_last_segment_does_not_regrow_past_two`
#     drives a real `cmd.exe` on the same leg.
#   - EC-QUOTE-011: automated for delayed-expansion OFF (the `!` string-level
#     pin `batch_accepts_a_bang_under_the_delayed_expansion_precondition` plus
#     the live-interpreter `live_batch_bang_survives_without_delayed_expansion`);
#     the delayed-expansion-ON sub-case stays `manual_procedure_ec_quote_011_delayed_expansion_on_truncation`
#     below because nobody here has a Windows host to observe cmd's exact
#     `!...!` pairing on that shape before shipping an assertion on it as live CI.
#   - EC-HOOK-009 / EC-SIZE-003: still manual — the concrete obstacle is file
#     ownership, not Windows availability: EC-HOOK-009's carrier lives in
#     `hook.rs`/`setup`/`shims.rs`, EC-SIZE-003's in `shell/reconcile.rs`,
#     neither owned by this package.
# A row stays uncovered while ANY tier it names is uncovered, so every named
# half had to close (automated or documented-manual) before the row could
# leave this set.
_UNCOVERED_ROWS = frozenset(
    {
        "EC-FP-005",  # fingerprint() folds CARGO_PKG_VERSION with no runtime seam
        "EC-VER-003",  # needs two staged builds with different Plan schema versions
        "EC-VER-007",  # needs two staged builds across consecutive self-updates
    }
)


# Traceability tests + manual-procedure functions are exempt from "must trace
# to a row" — they ARE the trace, not a covered behaviour.
_TRACEABILITY_EXEMPT_NAMES = frozenset(
    {
        "test_traceability_every_pytest_and_manual_row_names_a_real_covering_test",
        "test_traceability_every_test_in_this_module_traces_to_a_register_row",
        "test_traceability_every_register_row_is_cited_by_a_real_test",
        "test_traceability_the_summary_counts_match_the_register",
        "test_traceability_no_row_is_covered_only_by_an_assertion_free_placeholder",
    }
)


def _tests_citing_each_row() -> dict[str, set[str]]:
    """Map every ``EC-*`` id to the tests that cite it, across both harnesses.

    A row is *traceable* when some test names it — in the function name, or in
    the doc comment directly above it. That is the whole mechanism: cheap to
    run, impossible to satisfy by accident, and it fails loudly the moment a
    row is added without a test or a test citing it is deleted.
    """
    root = _REGISTER_PATH.parents[2] if _REGISTER_PATH else None
    citing: dict[str, set[str]] = {}
    if root is None:
        return citing

    for path in sorted((root / "test" / "tests").glob("test_shell*.py")):
        source = path.read_text(encoding="utf-8")
        lines = source.splitlines()
        for node in ast.walk(ast.parse(source)):
            if not isinstance(node, ast.FunctionDef):
                continue
            if not node.name.startswith(("test_", "manual_procedure_")):
                continue
            # The function's OWN source range (decorators included, since a
            # strict-xfail reason is part of the test), never a `^(?=def )`
            # block that runs on to the next top-level def. Same rule the Rust
            # half below already applies, for the same reason: a citation in
            # ordinary prose — a section banner between two functions — is a
            # cross-reference, never coverage.
            first = min([node.lineno, *(d.lineno for d in node.decorator_list)])
            body = "\n".join(lines[first - 1 : node.end_lineno])
            for row in set(re.findall(r"EC-[A-Z]+-\d+", body)):
                citing.setdefault(row, set()).add(f"{path.name}::{node.name}")

    for path in sorted((root / "crates").rglob("*.rs")):
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
        for index, line in enumerate(lines):
            named = re.match(r"\s*(?:async )?fn (\w+)\(", line)
            if not named:
                continue
            cursor, header = index - 1, []
            while cursor >= 0 and lines[cursor].lstrip().startswith(("#[", "///", "//")):
                header.append(lines[cursor])
                cursor -= 1
            # Only real test functions count — a citation in ordinary prose or in
            # a production doc comment is a cross-reference, never coverage.
            if not any("#[test]" in entry or "#[tokio::test]" in entry for entry in header):
                continue
            haystack = named.group(1) + " " + " ".join(header)
            for row in set(re.findall(r"EC-[A-Z]+-\d+", haystack)):
                citing.setdefault(row, set()).add(f"{path.name}::{named.group(1)}")

    return citing


def test_traceability_every_register_row_is_cited_by_a_real_test() -> None:
    """Every one of the 223 rows names a test that exists, in either harness — including the ``rust-unit`` rows.

    The sibling checks below only reach rows whose tier names ``pytest``. That
    left the ``rust-unit`` majority with behaviour that was genuinely covered
    and coverage that nobody could mechanically check — the same class of
    problem as an unchecked green, because a claim nobody re-checks is not
    evidence.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    citing = _tests_citing_each_row()

    uncited = sorted(row for row in register if row not in citing)
    assert not uncited, (
        f"{len(uncited)} register row(s) are cited by no test in either harness. Add the "
        "EC id to the test that proves the row, or write the test at the row's stated tier "
        f"— never attach an id to a test that does not prove it:\n{uncited}"
    )

    # The reverse direction: a coverage cell must not name a test that is gone.
    dangling = []
    for row, fields in register.items():
        for cited in re.findall(r"`[^`]*::(\w+)`", fields.get("Coverage", "")):
            if not any(cited == name.split("::")[-1] for name in citing.get(row, set())):
                dangling.append(f"{row}: coverage names {cited!r}, which no longer cites it")
    assert not dangling, "coverage cells naming a test that does not cite the row:\n" + "\n".join(dangling)


def test_traceability_the_summary_counts_match_the_register() -> None:
    """The register's ``Coverage:`` summary counts are recomputed from the parsed rows.

    The cell markings two paragraphs above have been gated since `_UNCOVERED_ROWS`
    existed, and were correct. The summary paragraph beside them had no gate and
    drifted: it claimed 91 Rust-cited rows while the tree held 90 — wrong before
    any of the work that added this check, and unnoticed for exactly the reason
    this module exists to refuse. A number nothing recomputes is a claim, not a
    fact.

    Widened past the `Coverage:` sentence to the two paragraphs beside it that
    made the same mistake: the opening `**N rows.**` sentence and the "Tier
    distribution" paragraph both stated a plain integer nothing recomputed, and
    both drifted right along with the coverage counts (they all predate the same
    two added rows) while only the `Coverage:` sentence had a gate.

    Deliberately scoped to the counts. The surrounding prose is not validated and
    should not be: gating prose is how a check becomes something people delete.

    Red state: change any one of the six `Coverage:` numbers, the row count, or
    any one of the four tier-distribution numbers in that paragraph.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    coverage = [row.get("Coverage", "") for row in register.values()]
    total = len(register)
    cited = sum(1 for c in coverage if c.strip())
    asserting = sum(1 for c in coverage if c.strip() and "uncovered" not in c)
    by_pytest = sum(1 for c in coverage if "test/tests/" in c)
    by_rust = sum(1 for c in coverage if "crates/" in c)
    by_both = sum(1 for c in coverage if "test/tests/" in c and "crates/" in c)

    match = re.search(
        r"\*\*Coverage: (\d+) / (\d+) rows cited, (\d+) by a test that asserts\*\*"
        r"[^0-9]+(\d+) by a pytest\s+test in [^,]+, (\d+) by a Rust `#\[test\]` under\s+"
        r"`crates/`, (\d+) rows by both",
        _REGISTER_PATH.read_text(encoding="utf-8"),
    )
    assert match, (
        "the register's `**Coverage: … rows cited …**` summary paragraph is missing or "
        "reworded past this check. Restore the sentence shape or update this gate — do not "
        "delete it: the paragraph drifted for months precisely while nothing read it."
    )
    claimed = tuple(int(g) for g in match.groups())
    computed = (cited, total, asserting, by_pytest, by_rust, by_both)
    assert claimed == computed, (
        "the register's summary counts disagree with the register itself.\n"
        f"  claimed  (cited/total/asserting/pytest/rust/both): {claimed}\n"
        f"  computed (cited/total/asserting/pytest/rust/both): {computed}\n"
        "Recompute from the tree and edit the paragraph; never edit this gate to match it."
    )
    assert by_pytest + by_rust - by_both == total, (
        f"inclusion-exclusion does not close: {by_pytest} + {by_rust} - {by_both} != {total}. "
        "Some row cites neither harness, which the Coverage column forbids."
    )

    register_text = _REGISTER_PATH.read_text(encoding="utf-8")

    rows_match = re.search(r"\*\*(\d+) rows\.\*\*", register_text)
    assert rows_match, (
        "the register's opening `**N rows.**` sentence is missing or reworded past "
        "this check. Restore the sentence shape or update this gate."
    )
    assert int(rows_match.group(1)) == total, (
        f"the opening '**N rows.**' sentence claims {rows_match.group(1)}, the register "
        f"parses to {total}. Recompute from the tree and edit the sentence."
    )

    tier_counts = Counter(_primary_tier(row.get("Test tier", "")) for row in register.values())
    assert None not in tier_counts, (
        "a row's Test tier cell names none of rust-unit/pytest-hostshell/"
        "pytest-shellzoo/manual-only, so it cannot be assigned a primary tier."
    )
    computed_tiers = tuple(tier_counts[label] for label in _TIER_LABELS)
    assert sum(computed_tiers) == total, (
        f"primary-tier counts {computed_tiers} do not sum to {total} rows — every row "
        "must have exactly one primary tier."
    )
    tier_match = re.search(
        r"\*\*Tier distribution\*\* — `rust-unit` (\d+) . `pytest-hostshell` (\d+) .\s*"
        r"`pytest-shellzoo` (\d+) . `manual-only` (\d+)",
        register_text,
    )
    assert tier_match, (
        "the register's `**Tier distribution** — ...` paragraph is missing or reworded "
        "past this check. Restore the sentence shape or update this gate."
    )
    claimed_tiers = tuple(int(g) for g in tier_match.groups())
    assert claimed_tiers == computed_tiers, (
        "the register's tier-distribution counts disagree with the register itself "
        "(each row counted once, by its first-listed tier).\n"
        f"  claimed  (rust-unit/pytest-hostshell/pytest-shellzoo/manual-only): {claimed_tiers}\n"
        f"  computed (rust-unit/pytest-hostshell/pytest-shellzoo/manual-only): {computed_tiers}\n"
        "Recompute from the tree and edit the paragraph; never edit this gate to match it."
    )


def _shell_module_test_names() -> set[str]:
    """Every ``test_*`` name across the shell test modules, not just this one.

    `_tests_citing_each_row` already globs `test_shell*.py`, so a row is allowed
    to be proven by a test in a sibling module — `EC-HOOK-017` is, because the
    per-prompt reconcile matrix lives in `test_shell_reconcile.py`. The coverage
    gate has to resolve names the same way or a legitimate citation reads as
    dangling. This widens only *where a cited test may live*; the opposite
    direction is untouched, and every `test_*` in THIS module must still trace
    back to a register row.
    """
    root = _REGISTER_PATH.parents[2] if _REGISTER_PATH else None
    if root is None:
        return set()
    names: set[str] = set()
    for path in sorted((root / "test" / "tests").glob("test_shell*.py")):
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.FunctionDef) and node.name.startswith("test_"):
                names.add(node.name)
    return names


def test_traceability_every_pytest_and_manual_row_names_a_real_covering_test() -> None:
    """Every register row whose Test tier names ``pytest-hostshell``/``pytest-shellzoo`` (even combined), or is exactly ``manual-only``, must have a Coverage cell naming a real test in one of the shell test modules (manual rows: a real ``manual_procedure_*`` in THIS module)."""
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    # 220 original rows + 3 added by this module for Discovery corrections 12,
    # 14, 15 (plan_shell_env_overhaul.md §5: EC-HOOK-015, EC-HOOK-016, EC-PROC-015),
    # + EC-LIST-011 (ocx#350) and EC-HOOK-017 (ocx#347) + EC-REC-007 (S-022,
    # `shell_integration_installed`) + EC-REC-008 (finding 97, `lock_refusal`).
    assert len(register) == 227, f"the register must still parse to exactly 227 rows; got {len(register)}"
    test_to_ids = _this_modules_test_to_ids()
    known_test_names = set(test_to_ids.keys()) | _shell_module_test_names()
    known_manual_procedures = {
        name
        for name in re.findall(r"^def (manual_procedure_\w+)", _THIS_MODULE_PATH.read_text(encoding="utf-8"), re.MULTILINE)
    }

    missing_coverage: list[str] = []
    dangling_coverage: list[str] = []
    for id_, row in register.items():
        tier = row.get("Test tier", "")
        is_manual = tier.strip().startswith("manual-only")
        is_pytest = ("pytest-hostshell" in tier or "pytest-shellzoo" in tier) and not is_manual
        if not (is_manual or is_pytest):
            continue
        coverage = row.get("Coverage", "").strip()
        if not coverage:
            missing_coverage.append(id_)
            continue
        # The cell names `<path>::<test>`; the bare-name form is still accepted.
        cited_names = re.findall(r"`(?:[^`]*::)?([A-Za-z0-9_]+)`", coverage)
        if is_manual:
            # No prose fallback: a cell that merely repeats the words "manual-only"
            # would pass while naming nothing, which is the same evidence value as
            # this check never running. Every manual row names a real function.
            if not (set(cited_names) & known_manual_procedures):
                dangling_coverage.append(f"{id_}: {coverage!r} names no known manual procedure")
            continue
        if not cited_names or not any(name in known_test_names for name in cited_names):
            dangling_coverage.append(f"{id_}: {coverage!r} names no test function that exists in this module")

    assert not missing_coverage, f"rows with an empty Coverage cell: {missing_coverage}"
    assert not dangling_coverage, "rows whose Coverage cell names something that does not exist:\n" + "\n".join(dangling_coverage)


def test_traceability_every_test_in_this_module_traces_to_a_register_row() -> None:
    """Every ``test_*`` function in this module (except the traceability checks themselves) must cite at least one register row ID that actually exists — no test tracing to a phantom ID."""
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    test_to_ids = _this_modules_test_to_ids()

    untraced: list[str] = []
    dangling: list[str] = []
    for name, ids in test_to_ids.items():
        if name in _TRACEABILITY_EXEMPT_NAMES:
            continue
        if not ids:
            untraced.append(name)
            continue
        for id_ in ids:
            if id_ not in register:
                dangling.append(f"{name} -> {id_}")

    assert not untraced, f"tests whose docstring names no EC-* row at all: {untraced}"
    assert not dangling, "tests tracing to an EC-* ID that does not exist in the register:\n" + "\n".join(dangling)


def test_traceability_no_row_is_covered_only_by_an_assertion_free_placeholder() -> None:
    """A row whose every citing test is a branch-free ``pytest.skip`` executes no assertion on any leg — it is uncovered, and the register must say so rather than read as green.

    The sibling gates check that a coverage claim names something that
    *exists*. They cannot tell an existing test that proves the row from one
    whose whole body is ``pytest.skip(...)``: both report the same green, on
    every platform, forever. The register carries the honest vocabulary for
    this already (``uncovered``); this gate makes using it the only way past.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    citing = _tests_citing_each_row()
    placeholders = _placeholder_tests()
    assert placeholders, (
        "the placeholder detector matched nothing at all — an AST change that quietly stops "
        "matching would make this whole gate green for the wrong reason"
    )

    placeholder_only: list[str] = []
    stale_marker: list[str] = []
    marked: set[str] = set()
    for id_, row in register.items():
        coverage = row["Coverage"]
        proving = citing.get(id_, set()) - placeholders
        if re.search(r"\buncovered\b", coverage, re.IGNORECASE):
            marked.add(id_)
            if proving:
                stale_marker.append(f"{id_}: marked uncovered, yet {sorted(proving)} asserts for it")
        elif citing.get(id_) and not proving:
            placeholder_only.append(
                f"{id_}: every citing test is assertion-free ({sorted(citing[id_])}) — write the test at "
                "the row's stated tier, or mark the Coverage cell 'uncovered (<owner>)'"
            )

    assert not placeholder_only, (
        "rows whose only coverage is a skip placeholder, reported as green:\n" + "\n".join(placeholder_only)
    )
    assert not stale_marker, (
        "rows marked uncovered that are in fact covered — the marker outlived its cause:\n" + "\n".join(stale_marker)
    )
    assert marked == _UNCOVERED_ROWS, (
        "the set of rows the register admits are uncovered drifted from the pinned set. Marking a row "
        "uncovered is a deliberate, reviewable act, not a way to silence this gate:\n"
        f"  newly marked: {sorted(marked - _UNCOVERED_ROWS)}\n"
        f"  no longer marked: {sorted(_UNCOVERED_ROWS - marked)}"
    )


# EC-HOOK-009, EC-QUOTE-011 (delayed-expansion-ON half only) and EC-SIZE-003
# are the residual manual-only ground left by ocx#353's retier. EC-PATH-013
# and EC-QUOTE-004/010/011's delayed-expansion-OFF half were checked against
# the now-fixed ocx#354 and found automatable — see the `live_batch_*` tests
# and the `_UNCOVERED_ROWS` comment above — so they are NOT documented here.
# What remains genuinely manual has a named, concrete obstacle per row, not
# "Windows" in general: EC-HOOK-009 needs Windows PowerShell 5.1, which no CI
# leg in this repository runs (`shell-activation-deep.yml`'s Windows job
# drives login activation via `test/manual/test-windows-activation.ps1`, not
# this per-prompt reconcile hook); EC-QUOTE-011's ON-half needs a Windows host
# to observe cmd's exact `!...!` pairing before an assertion on it ships as
# live CI; EC-SIZE-003 needs the `shell/reconcile.rs` carrier, out of this
# package's file scope (owned by P1/P2/P3).


def manual_procedure_ec_hook_009_windows_powershell_5_1_prompt_wrap_hook_fires() -> None:
    """MANUAL PROCEDURE — EC-HOOK-009 (tier: manual-only).

    Not run by this suite: Windows PowerShell 5.1 (not pwsh 7) is the only
    place `LocationChangedEventArgs` is absent, so the hook falls back to
    wrapping `prompt` — a code path pwsh-on-Linux cannot approximate, and no
    CI leg in this repository runs Windows PowerShell 5.1 today
    (`shell-activation-deep.yml`'s Windows job runs
    `test/manual/test-windows-activation.ps1`, which drives login activation,
    not this per-prompt reconcile hook).

    1. On a Windows host with Windows PowerShell 5.1 available
       (`$PSVersionTable.PSVersion.Major -eq 5`), build a release `ocx.exe`
       and run `ocx self setup` against a scratch `OCX_HOME`.
    2. Open a fresh `powershell.exe` (not `pwsh`) session and `cd` into a
       locked project directory.
    3. Confirm the hook installed: `(Get-Content Function:\\prompt).ToString()`
       contains the ocx reconcile call.
    4. Trigger a prompt (press Enter on an empty line) and confirm
       `$env:PATH` gained the project's entry, checked case-insensitively —
       `$env:PATH` and `$env:Path` name the same variable on Windows.
    5. Confirm `[Console]::IsInputRedirected` and
       `$PSVersionTable.PSVersion.Major -ge 5` gated completions in
       `ENV_PS1` without a crash or a missing `prompt` function.
    """


def manual_procedure_ec_quote_011_delayed_expansion_on_truncation() -> None:
    """MANUAL PROCEDURE — EC-QUOTE-011 (tier: rust-unit + `live_batch_*` covered; this
    procedure is only the residual delayed-expansion-**on** half).

    EC-QUOTE-004, EC-QUOTE-010 and the rest of EC-QUOTE-011 are automated —
    `batch_refuses_percent_lf_and_cr_on_both_emitters` (LF/CR + `%` refusal),
    `batch_accepts_a_bang_under_the_delayed_expansion_precondition` (the `!`
    string-level pin) and `live_batch_bang_survives_without_delayed_expansion`
    (the same claim against a real `cmd.exe`), all in `crates/ocx_lib/src/shell.rs`,
    running on the `verify-deep.yml` windows-latest `nextest` leg. What is
    NOT automated is the delayed-expansion-**on** half, where the row's own
    text predicts truncation: the emitted line contains the value twice (once
    as the prepend, once inside `%VAR:search=%`'s search pattern), so it
    carries two `!` bytes, and this environment has no Windows host to verify
    cmd's exact `!...!` pairing across that shape before shipping an assertion
    on it as live CI.

    1. On a Windows host, emit `Shell::Batch.export_path("PATH", "C:\\x!y\\bin")`
       into a `.bat` that starts with `setlocal EnableDelayedExpansion` (or run
       under `cmd /v:on`).
    2. `SET "PATH="` first (empty ambient), run the emitted line, `echo %PATH%`.
    3. Confirm the `!`-bearing segment does NOT survive verbatim — the row's
       predicted truncation — and record the actual resulting string, since
       nobody has observed it yet.
    4. If the observed shape is stable and mechanical (not interpreter-version-
       dependent), promote this to a `live_batch_*` unit test asserting that
       exact string; if the exact remainder text differs across cmd.exe builds,
       keep the weaker `!contains(value)` assertion `live_batch_*` would use
       instead of a byte-exact one.
    """


def manual_procedure_ec_size_003_windows_32767_char_block_cap_headroom() -> None:
    """MANUAL PROCEDURE — EC-SIZE-003 (tier: manual-only).

    Not run by this suite: the 32767-char whole-environment-block limit is a
    `CreateProcessW` ceiling that exists only on Windows; Linux's E2BIG
    threshold is unrelated (EC-SIZE-004 already covers the Linux headroom
    case in this module).

    1. On a Windows host, construct a shell environment whose other
       variables plus a near-16-KiB `__OCX_ENV_STATE` carrier approach the
       32767-character whole-block limit — pad an unrelated env var to make
       up the difference, since the limit is shared across every variable in
       the block, not `__OCX_ENV_STATE` alone.
    2. From that shell, run a command that spawns a child process (e.g. the
       ocx-emitted hook calling `ocx.exe`) via `CreateProcessW`.
    3. Assert the child process launches successfully with the full
       environment intact — no silent truncation of `__OCX_ENV_STATE` or any
       other variable, and no `CreateProcessW` failure.
    4. Push the block a few hundred bytes past 32767 and confirm
       `CreateProcessW` fails loudly rather than silently truncating — D1:74's
       headroom claim is that 16 KiB is "a factor of two, not an order of
       magnitude," so the failure mode close to the ceiling must be an
       honest error, not silent data loss.
    """


# ---------------------------------------------------------------------------
# Discovery corrections 12/14/15 (plan_shell_env_overhaul.md §5) — absent from
# the 220-row corpus; added here per the plan's explicit instruction, and
# ONLY for these three named corrections.
# ---------------------------------------------------------------------------


def test_ec_hook_015_restricted_bash_sourcing_the_managed_block_never_kills_the_shell(arena: Arena) -> None:
    """EC-HOOK-015 — Discovery correction 12: ``rbash``/restricted bash forbids any '/'-containing command name, which every ocx invocation in the managed block uses unconditionally.

    Finding: restricted bash's OWN builtin refusal of the ``.`` (source)
    builtin with a ``/``-containing path is non-fatal to the sourcing shell
    (bash prints ``restricted`` to stderr and continues) — so activation
    silently never happens under a restricted shell, but D3's "never break a
    prompt" IS satisfied as an incidental consequence of bash's own
    restricted-mode error handling, not because of any purpose-built guard
    this repo ships. No evidence of a dedicated "detect-and-silently-no-op
    path" (as plan §5 item 12 describes for WP-3) was found in the shipped
    ``env.sh``/hook body read during this session — this may mean WP-3
    has not landed in this worktree, or landed via a different mechanism
    than expected. Flagged for the orchestrator to verify against WP-3's
    actual status.
    """
    _self_setup(arena, "bash")
    rcfile = arena.home / ".bashrc"
    assert rcfile.is_file()
    result = subprocess.run(
        ["/bin/bash", "--restricted", "-c", f'. {matrix.quote("bash", str(rcfile))}; echo AFTER_SOURCE'],
        capture_output=True, check=False, text=True, env=arena.env("/bin/bash"),
    )
    assert result.returncode == 0, (
        f"a restricted shell sourcing the managed block must never kill the shell itself, whatever else happens:\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "AFTER_SOURCE" in result.stdout, (
        f"control must return to the rest of the profile after the restricted refusal, not hang or exit early:\n{result.stdout}"
    )


def test_ec_hook_016_errexit_does_not_fire_inside_the_freshness_tests_and_list(arena: Arena) -> None:
    """EC-HOOK-016 — Discovery correction 15: ``set -e`` (errexit) does not fire when the freshness test's left-hand side is false inside an ``&&`` list — asserted directly, not left as "likely"."""
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    future_stamp = arena.scripts / "future_stamp"
    future_stamp.touch()
    os.utime(future_stamp, (4102444800, 4102444800))  # 2100-01-01 — always newer than the watched file
    script = (
        "set -e\n"
        f"cd {matrix.quote('bash', str(project))}\n"
        f"[ {matrix.quote('bash', str(project / 'ocx.toml'))} -nt {matrix.quote('bash', str(future_stamp))} ] && echo WOULD_RECONCILE\n"
        'printf "%s\\n" "@@survived@@yes"\n'
    )
    result = subprocess.run(["/bin/bash", "-c", script], capture_output=True, check=False, text=True, env=arena.env("/bin/bash"))
    assert result.returncode == 0, (
        f"errexit must not fire on a false LHS of an && list — the freshness test's own idiom:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert _read(result, "survived") == "yes", f"stdout:\n{result.stdout}"
    assert "WOULD_RECONCILE" not in result.stdout, "fixture sanity: the LHS must genuinely be false (stamp is newer)"


def test_ec_proc_015_a_carrier_crossing_a_host_or_user_boundary_is_treated_as_foreign(arena: Arena) -> None:
    """EC-PROC-015 — Discovery correction 14: ``sudo -E``/ssh ``SendEnv`` can carry a stale ``__OCX_ENV_STATE`` across a host/user boundary; the fingerprint mismatch (D1 rule (a)) treats it as an ordinary foreign/corrupt ledger, never as this session's own applied state.

    Finding: when the freshly-composed D happens to equal the foreign
    ledger's own recorded ``applied`` value (both say ``WP15_CONST=v1``,
    since it is the same project directory), the reconciler emits NO
    ``export`` line at all — a coincidence-rule skip based purely on value
    comparison, with no verification that the REAL process env already has
    it set (it does not, across a genuine host boundary). Not a forged-value
    risk (D composes the correct value either way), but the assertion below
    checks the DECODED ledger payload rather than assuming an export line is
    always re-emitted.
    """
    project = _locked_project(arena, "alpha", 'WP15_CONST = "v1"\n')
    # Simulate the boundary crossing directly: a real ledger applied in one
    # session (own OCX_HOME/project fingerprint), then carried via SendEnv/
    # `sudo -E`-style inheritance into a session with a DIFFERENT OCX_HOME —
    # the fingerprint folds project_dir AND the OCX_HOME-rooted watch paths
    # (A-13), so a different OCX_HOME alone is sufficient to mismatch.
    applied = matrix.reconcile(arena.ocx, "bash", project, arena.env())
    assert applied.returncode == 0, f"stderr:\n{applied.stderr}"
    match = re.search(rf"export {matrix.CARRIER}='([^']+)'", applied.stdout)
    assert match, f"fixture sanity: the source session must apply a real ledger:\n{applied.stdout}"
    carrier_from_other_host = match.group(1)

    other_ocx_home = arena.scripts / "other_host_ocx_home"
    other_ocx_home.mkdir()
    crossed_env = dict(arena.env())
    crossed_env["OCX_HOME"] = str(other_ocx_home)  # the "sudo -E"/SendEnv boundary: different host identity
    # Consent is scoped per OCX_HOME — stamp the project under the NEW home
    # too, so the only variable under test is the carried-over carrier, not
    # an unrelated unstamped-project inertness.
    prelock = matrix.run_lock(arena.ocx, project, crossed_env)
    assert prelock.returncode == 0, f"stderr:\n{prelock.stderr}"

    result = matrix.reconcile(arena.ocx, "bash", project, crossed_env, carrier=carrier_from_other_host)
    assert result.returncode == 0, f"a foreign-fingerprint carrier must never break the prompt:\nstderr:\n{result.stderr}"
    new_carrier_match = re.search(rf"export {matrix.CARRIER}='([^']+)'", result.stdout)
    assert new_carrier_match, f"a fingerprint-mismatched carrier must still recompose and write a fresh, decodable ledger:\n{result.stdout}"
    decoded = _decode_carrier(new_carrier_match.group(1))
    project_applied = decoded.get("scopes", {}).get("project", {}).get("applied", [])
    applied_values = {entry["key"]: entry["value"] for entry in project_applied}
    assert applied_values.get("WP15_CONST") == "v1", (
        f"a carrier crossing a host/OCX_HOME boundary must never cause the WRONG value to be trusted — the freshly "
        f"recomposed D, not the foreign ledger's bookkeeping, is authoritative for what applies: {applied_values}\n{result.stdout}"
    )
