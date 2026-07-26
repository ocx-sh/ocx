# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for user-declarable project environment variables
(``[env]`` / ``[group.<name>.env]`` in ``ocx.toml``, and ``ocx run --env``).

Specification mode (contract-first TDD)
---------------------------------------
Encodes the Component Contracts (CLI, Precedence, Composition) and the
Config-shape-fault table in
``.claude/state/plans/plan_project_env_declaration.md``, plus the ratified
decisions (S1-S9b, C1-C5, L1-L3, Q1-Q7, R1-R3, X1-X3) in
``.claude/artifacts/adr_project_env_declaration.md``. Several call sites
still return ``unimplemented!()`` (``crates/ocx_lib/src/project/env.rs``,
``crates/ocx_lib/src/project/config.rs``, ``crates/ocx_cli/src/command/
run.rs``'s ``parse_env_overrides``), and the ``OCX_ENV`` launcher-forwarding
wire (R1) has no decode-side implementation at all yet — every test in this
file is expected to FAIL against today's binary.

Test inventory
---------------
CLI (``ocx run --env``):
  test_env_flag_is_highest_precedence_constant
  test_env_flag_splits_on_first_equals_only
  test_env_flag_bare_key_without_equals_exits_64
  test_env_flag_repeated_last_wins
  test_env_flag_rejects_ocx_prefixed_key_exits_64
  test_env_flag_absent_on_env_subcommand
  test_env_flag_absent_on_package_exec_subcommand

Precedence:
  test_ambient_value_loses_to_project_env
  test_project_env_constant_overrides_package_constant
  test_group_env_later_selected_group_wins
  test_project_env_path_entry_precedes_package_path_entry
  test_self_flag_does_not_affect_project_env
  test_global_env_applies_to_global_tier_resolution
  test_global_env_applies_without_any_global_lock
  test_global_env_applies_when_locked_tool_not_materialised
  test_global_env_never_composes_into_project_run

Config-shape faults (exit 78):
  test_group_direct_tool_binding_rejected_names_group_and_tools_subtable
  test_group_unknown_key_rejected_by_deny_unknown_fields
  test_project_env_ocx_prefixed_key_rejected
  test_group_env_dunder_ocx_prefixed_key_rejected
  test_env_value_bogus_modifier_type_rejected

Boundary-pinning (guard constraints whose consuming feature has not shipped):
  test_run_package_composed_env_byte_identical_with_and_without_clean  (R3)
  test_project_env_override_reaches_generated_entrypoint_launcher      (R1)
  test_project_env_override_survives_nested_launcher_hop               (R1)
  test_launcher_forged_ocx_env_fails_closed_on_whole_payload           (R1/X1/X2)
  test_run_strips_stale_ambient_ocx_env                                (R1)

Misc:
  test_init_emits_ocx_toml_its_own_parser_accepts                     (Q1)
  test_env_string_shorthand_validates_against_generated_schema        (S9a)
  test_env_table_form_validates_against_generated_schema               (S9a)
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE)
EXIT_CONFIG = 78  # ConfigError (sysexits EX_CONFIG)
EXIT_DATA = 65  # DataError (sysexits EX_DATAERR) — malformed/rejected OCX_ENV payload

PROJECT_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_BINARY = PROJECT_ROOT / "target" / "release" / "ocx_schema"

# ---------------------------------------------------------------------------
# Helpers (DAMP — self-contained, mirrors idiom in test_project_run.py /
# test_toolchain_env.py rather than importing from either)
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` from ``cwd`` with the runner's isolated environment."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> None:
    (project_dir / "ocx.toml").write_text(body)


def _run_lock(ocx: OcxRunner, cwd: Path) -> subprocess.CompletedProcess[str]:
    return _run(ocx, cwd, "lock")


def _published_tool(
    ocx: OcxRunner, tmp_path: Path, label: str, env: list[dict] | None = None
) -> tuple[str, str]:
    """Publish a single test package and return ``(repo, tag)``.

    ``label`` is embedded in the repo name so failure messages are
    traceable. Mirrors ``test_project_run.py::_published_tool``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_penv_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, env=env)
    return repo, tag


def _env_value(dump: str, key: str) -> str | None:
    """Return the value of a ``KEY=value`` line in an ``env``-dumped block.

    Naive line-based parsing (no embedded-newline support) — matches the
    established idiom across this test suite (e.g.
    ``test_toolchain_env.py``'s security regression tests); all values
    used here are single-line by construction.
    """
    prefix = f"{key}="
    for line in dump.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return None


# =============================================================================
# CLI contract — `ocx run --env`
# =============================================================================


def test_env_flag_is_highest_precedence_constant(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env`` (stage 6) beats a project ``[env]`` entry (stage 4) and a
    selected group's ``[env]`` entry (stage 5) for the same key (C2).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[env]
X = "project-value"

[group.g.env]
X = "group-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "-g", "g", "--env", "X=flag-value", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --env must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "X") == "flag-value", (
        f"--env must be the highest-precedence layer, beating project and "
        f"group [env] for the same key; stdout:\n{result.stdout}"
    )


def test_env_flag_splits_on_first_equals_only(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env FOO=a=b`` sets ``FOO`` to ``a=b`` — split on the FIRST ``=`` only (L1)."""
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--env", "FOO=a=b", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --env FOO=a=b must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "FOO") == "a=b", (
        f"--env must split on the first '=' only; stdout:\n{result.stdout}"
    )


def test_env_flag_bare_key_without_equals_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """A bare ``--env FOO`` (no ``=``) is a usage error, exit 64 (L2).

    Ambient pass-through is not accepted in v1 — it has meaning only under
    ``--clean``, and admitting it later is purely additive.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--env", "FOO", "--", "echo", "hi")
    assert result.returncode == EXIT_USAGE, (
        f"bare --env FOO (no '=') must exit {EXIT_USAGE}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_env_flag_repeated_last_wins(ocx: OcxRunner, tmp_path: Path) -> None:
    """A repeated ``--env`` for the same key applies all; the later wins."""
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(
        ocx, project, "run", "--env", "FOO=1", "--env", "FOO=2", "--env", "BAR=b", "--", "env"
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"repeated --env must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "FOO") == "2", (
        f"a later --env for the same key must win; stdout:\n{result.stdout}"
    )
    assert _env_value(result.stdout, "BAR") == "b", (
        f"a different --env key must also apply; stdout:\n{result.stdout}"
    )


def test_env_flag_rejects_ocx_prefixed_key_exits_64(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env OCX_FOO=x`` is rejected, exit 64 — X1 applies to the flag too."""
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    for key in ("OCX_FOO", "__OCX_BAR"):
        result = _run(ocx, project, "run", "--env", f"{key}=x", "--", "echo", "hi")
        assert result.returncode == EXIT_USAGE, (
            f"--env {key}=x must exit {EXIT_USAGE} (reserved OCX_*/__OCX_* namespace); "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )


def test_env_flag_absent_on_env_subcommand(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env`` does not exist on ``ocx env`` — ``run``-only in v1 (L3)."""
    result = _run(ocx, tmp_path, "env", "--env", "FOO=bar")
    assert result.returncode == EXIT_USAGE, (
        f"ocx env --env must be rejected as an unrecognized flag, exit {EXIT_USAGE}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_env_flag_absent_on_package_exec_subcommand(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env`` does not exist on ``ocx package exec`` — the OCI-tier exec
    command; the ambient shell is the caller's own concern there (L3).
    """
    fake_id = f"{ocx.registry}/does-not-exist:1.0.0"
    result = _run(
        ocx, tmp_path, "package", "exec", "--env", "FOO=bar", fake_id, "--", "true"
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx package exec --env must be rejected as an unrecognized flag, "
        f"exit {EXIT_USAGE}; got {result.returncode}\nstderr:\n{result.stderr}"
    )


# =============================================================================
# Precedence contract (C2, Q6, S6, Q2)
# =============================================================================


def test_ambient_value_loses_to_project_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """Ambient ``CI=true`` loses to a project ``[env] CI = "1"`` declaration
    (Q6 — no Cargo-style ``force``; the project file, not the ambient shell,
    states what the project needs).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[env]
CI = "1"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "env", extra_env={"CI": "true"})
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "CI") == "1", (
        f"project [env] must win over an ambient value for the same key (Q6); "
        f"stdout:\n{result.stdout}"
    )


def test_project_env_constant_overrides_package_constant(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project ``[env]`` constant overrides a package-declared constant of
    the same key (C2); the shadowing must not surface as a warning (C4 —
    shadowing is the feature's declared intent, not a misconfiguration).
    """
    shared_key = "T_PENV_SHARED_CONST"
    repo, tag = _published_tool(
        ocx,
        tmp_path,
        "pkgshadow",
        env=[{"key": shared_key, "type": "constant", "value": "package-value", "visibility": "public"}],
    )

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"

[env]
{shared_key} = "project-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, shared_key) == "project-value", (
        f"project [env] must override a package constant of the same key (C2); "
        f"stdout:\n{result.stdout}"
    )
    assert "warn" not in result.stderr.lower(), (
        f"project-over-package shadowing is declared intent and must not warn (C4); "
        f"stderr:\n{result.stderr}"
    )


def test_group_env_later_selected_group_wins(ocx: OcxRunner, tmp_path: Path) -> None:
    """Two groups both declare ``X``; with ``-g a,b`` the later group (``b``)
    wins (C2 stage 5 — later ``-g`` selection wins).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[group.a.env]
X = "value-a"

[group.b.env]
X = "value-b"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "-g", "a,b", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -g a,b must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "X") == "value-b", (
        f"the later-selected group must win for a shared key (C2); stdout:\n{result.stdout}"
    )


def test_project_env_path_entry_precedes_package_path_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project ``type = "path"`` entry lands ahead of a package's own
    ``PATH`` entry for the same key (C1a/C2 — a stage-4 path entry is
    applied after stage-2 package paths, and path entries prepend).
    """
    repo, tag = _published_tool(ocx, tmp_path, "pathprec")

    project = tmp_path / "proj"
    project.mkdir()
    (project / "local_bin").mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"

[env]
PATH = {{ type = "path", value = "local_bin" }}
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    path_value = _env_value(result.stdout, "PATH")
    assert path_value is not None, f"PATH must be present in env dump; stdout:\n{result.stdout}"
    first_segment = path_value.split(":")[0]  # POSIX separator; module is Linux-only
    expected = str(project / "local_bin")
    assert first_segment == expected, (
        f"the project's [env] path entry must be the FRONT of PATH, ahead of "
        f"the package's own path entry; got first segment {first_segment!r}, "
        f"expected {expected!r}; full PATH:\n{path_value}"
    )


def test_self_flag_does_not_affect_project_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--self`` on and off produce identical output for a project
    ``[env]`` entry (S6 — project env has no visibility axis; a project is
    never a dependency of anything, so there is no edge to gate).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[env]
PROJECT_ONLY_VAR = "same-either-way"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    consumer = _run(ocx, project, "run", "--", "env")
    assert consumer.returncode == EXIT_SUCCESS, consumer.stderr
    self_view = _run(ocx, project, "run", "--self", "--", "env")
    assert self_view.returncode == EXIT_SUCCESS, self_view.stderr

    assert _env_value(consumer.stdout, "PROJECT_ONLY_VAR") == "same-either-way", (
        f"project [env] must be visible without --self; stdout:\n{consumer.stdout}"
    )
    assert _env_value(self_view.stdout, "PROJECT_ONLY_VAR") == "same-either-way", (
        f"project [env] must be visible identically with --self (S6 — no "
        f"visibility axis); stdout:\n{self_view.stdout}"
    )


def test_global_env_applies_to_global_tier_resolution(ocx: OcxRunner, tmp_path: Path) -> None:
    """``$OCX_HOME/ocx.toml``'s own ``[env]`` applies when the global tier
    is the one being resolved — ``ocx --global env`` (Q2). ``[env]`` is
    excluded from ``declaration_hash`` (H1), so hand-editing it in after
    ``--global add`` does not stale the just-written global lock.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_globalenv"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    fq = f"{ocx.registry}/{repo}:1.0.0"

    empty = tmp_path / "no_project"
    empty.mkdir()
    add = _run(ocx, empty, "--global", "add", fq)
    assert add.returncode == EXIT_SUCCESS, (
        f"ocx --global add must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    global_toml.write_text(
        global_toml.read_text() + '\n[env]\nGLOBAL_ENV_MARKER = "global-value"\n'
    )

    result = _run(ocx, empty, "--global", "--format", "json", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global env must succeed after hand-editing [env] into the "
        f"global ocx.toml; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    data = json.loads(result.stdout)
    entries = {e["key"]: e["value"] for e in data["entries"]}
    assert entries.get("GLOBAL_ENV_MARKER") == "global-value", (
        f"the global tier's own [env] must apply to `ocx --global env` (Q2); "
        f"entries={entries}"
    )


def test_global_env_applies_without_any_global_lock(ocx: OcxRunner, tmp_path: Path) -> None:
    """A global ``ocx.toml`` carrying only ``[env]`` — no lock, no tools —
    still exports its declarations (Q2).

    A declaration's effect must not depend on unrelated package
    availability: the global env resolver is lenient about AVAILABILITY, and
    "nothing is installed" is not a reason to drop what the file declares.
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    (ocx_home / "ocx.toml").write_text('[tools]\n\n[env]\nGLOBAL_ONLY_MARKER = "global-value"\n')
    assert not (ocx_home / "ocx.lock").exists(), (
        "test setup: this case is specifically 'declared [env], no lock at all'"
    )

    empty = tmp_path / "no_project"
    empty.mkdir()
    result = _run(ocx, empty, "--global", "--format", "json", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global env must succeed with a lock-less global ocx.toml; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    entries = {e["key"]: e["value"] for e in json.loads(result.stdout)["entries"]}
    assert entries.get("GLOBAL_ONLY_MARKER") == "global-value", (
        f"a global [env] declaration must apply even when no global tool is "
        f"locked or installed (Q2); entries={entries}"
    )


def test_global_env_applies_when_locked_tool_not_materialised(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A global ``[env]`` still exports when the global lock's tools are
    locked but no longer materialised (Q2).

    Sibling of the lock-less case above, and the one that actually reaches
    the resolver's tool loop: the lock is read, every entry fails the offline
    store lookup and is skipped, and the declared ``[env]`` must survive that
    empty package set rather than being short-circuited away with it.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_globalunmaterialised"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    empty = tmp_path / "no_project"
    empty.mkdir()
    add = _run(ocx, empty, "--global", "add", f"{ocx.registry}/{repo}:1.0.0")
    assert add.returncode == EXIT_SUCCESS, (
        f"ocx --global add must succeed; rc={add.returncode}\nstderr:\n{add.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    global_toml.write_text(
        global_toml.read_text() + '\n[env]\nUNMATERIALISED_MARKER = "global-value"\n'
    )
    # "added, then the object store was cleaned" — the lock keeps its pins,
    # the packages they name are gone.
    shutil.rmtree(ocx_home / "packages")
    assert (ocx_home / "ocx.lock").exists(), (
        "test setup: the lock must survive so the resolver still walks its tools"
    )

    result = _run(ocx, empty, "--global", "--format", "json", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global env must stay lenient about unmaterialised tools; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    entries = {e["key"]: e["value"] for e in json.loads(result.stdout)["entries"]}
    assert entries.get("UNMATERIALISED_MARKER") == "global-value", (
        f"a global [env] declaration must apply even when every locked tool "
        f"fails the offline lookup (Q2); entries={entries}"
    )


def test_global_env_never_composes_into_project_run(ocx: OcxRunner, tmp_path: Path) -> None:
    """A global ``[env]`` entry never composes into a project-tier
    resolution — ``ocx run`` inside a project stays isolated from
    ``$OCX_HOME/ocx.toml``'s own declarations (Q2 — strict isolation).
    """
    ocx_home = Path(ocx.env["OCX_HOME"])
    global_toml = ocx_home / "ocx.toml"
    global_toml.write_text('[tools]\n\n[env]\nSHARED_MARKER = "global-value"\n')

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "SHARED_MARKER") is None, (
        f"the global tier's [env] must NEVER compose into a project-tier "
        f"resolution (Q2), regardless of a global ocx.toml existing; "
        f"stdout:\n{result.stdout}"
    )


# =============================================================================
# Config-shape faults (exit 78)
# =============================================================================


def test_group_direct_tool_binding_rejected_names_group_and_tools_subtable(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A tool binding declared directly under ``[group.<name>]`` is a parse
    error naming the group and pointing at ``[group.<name>.tools]`` (S8).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[group.ci]
bar = "ocx.sh/bar:1"
""",
    )

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_CONFIG, (
        f"a direct tool binding under [group.ci] must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "ci" in result.stderr, f"stderr must name the group 'ci'; got:\n{result.stderr}"
    assert "[group.ci.tools]" in result.stderr, (
        f"stderr must point at [group.ci.tools] (S8's carried fix); got:\n{result.stderr}"
    )


def test_group_unknown_key_rejected_by_deny_unknown_fields(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``[group.ci.tolos]`` (typo) is rejected by ``deny_unknown_fields``,
    naming the offending key — no hand-written unknown-key branch (S2).
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[group.ci.tolos]
foo = "ocx.sh/foo:1"
""",
    )

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_CONFIG, (
        f"[group.ci.tolos] must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "tolos" in result.stderr, (
        f"stderr must name the unknown key 'tolos'; got:\n{result.stderr}"
    )


def test_project_env_ocx_prefixed_key_rejected(ocx: OcxRunner, tmp_path: Path) -> None:
    """``OCX_*`` / ``__OCX_*`` keys are rejected in top-level ``[env]``,
    exit 78 (X1).
    """
    for key in ("OCX_FOO", "__OCX_BAR"):
        project = tmp_path / f"proj_{key.lower()}"
        project.mkdir()
        _write_ocx_toml(project, f'[tools]\n\n[env]\n{key} = "1"\n')

        result = _run_lock(ocx, project)
        assert result.returncode == EXIT_CONFIG, (
            f"[env] {key} must exit {EXIT_CONFIG}; "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )
        assert key in result.stderr, f"stderr must name {key!r}; got:\n{result.stderr}"


def test_group_env_dunder_ocx_prefixed_key_rejected(ocx: OcxRunner, tmp_path: Path) -> None:
    """``OCX_*`` / ``__OCX_*`` keys are rejected in ``[group.<name>.env]``
    too, exit 78 (X1).
    """
    for key in ("OCX_FOO", "__OCX_BAR"):
        project = tmp_path / f"proj_g_{key.lower()}"
        project.mkdir()
        _write_ocx_toml(project, f'[group.ci.env]\n{key} = "1"\n')

        result = _run_lock(ocx, project)
        assert result.returncode == EXIT_CONFIG, (
            f"[group.ci.env] {key} must exit {EXIT_CONFIG}; "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )
        assert key in result.stderr, f"stderr must name {key!r}; got:\n{result.stderr}"


def test_env_value_bogus_modifier_type_rejected(ocx: OcxRunner, tmp_path: Path) -> None:
    """``X = { type = "bogus", value = "v" }`` is a parse error naming the
    key and the bad type.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[env]
X = { type = "bogus", value = "v" }
""",
    )

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_CONFIG, (
        f"a bogus env value type must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "X" in result.stderr, f"stderr must name the key 'X'; got:\n{result.stderr}"
    assert "bogus" in result.stderr, (
        f"stderr must name the bad type 'bogus'; got:\n{result.stderr}"
    )


# =============================================================================
# Boundary-pinning tests — guard constraints whose consuming feature has not
# shipped yet. These are the point: they encode a boundary the deferred
# interpolation work (#175) will depend on, not merely a feature detail.
# =============================================================================


def test_run_package_composed_env_byte_identical_with_and_without_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Package-composed env is byte-identical with and without ``--clean``,
    for the same lock and the same digests (R3 hermeticity pin).

    No existing test to build on: the nearest shape,
    ``test_run_clean_strips_inherited_env`` (``test_project_run.py``), proves
    ambient stripping but never diffs the package-composed subset. This
    assertion is meaningful TODAY (it should already hold) — it exists to
    fail loudly the moment ambient state leaks into package env resolution,
    which is the boundary deferred interpolation (`${env.VAR}`, #175) must
    respect: package env values may only ever resolve against a
    package-only accumulator, never against ``std::env`` or the process env
    under construction. ``--clean`` controls only what the CHILD inherits
    from ambient; it must have no effect on what the packages themselves
    contribute — ``--clean`` is NOT the hermeticity boundary (docs corollary
    in the ADR).
    """
    home_key = "T_PENV_CLEAN_PARITY_HOME"
    repo, tag = _published_tool(
        ocx,
        tmp_path,
        "cleanparity",
        env=[{"key": home_key, "type": "constant", "value": "${installPath}", "visibility": "public"}],
    )

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\n{repo} = "{ocx.registry}/{repo}:{tag}"\n')
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    dirty = _run(ocx, project, "run", "--", "env")
    assert dirty.returncode == EXIT_SUCCESS, (
        f"ocx run (no --clean) must succeed; rc={dirty.returncode}\nstderr:\n{dirty.stderr}"
    )
    clean = _run(ocx, project, "run", "--clean", "--", "env")
    assert clean.returncode == EXIT_SUCCESS, (
        f"ocx run --clean must succeed; rc={clean.returncode}\nstderr:\n{clean.stderr}"
    )

    dirty_value = _env_value(dirty.stdout, home_key)
    clean_value = _env_value(clean.stdout, home_key)
    assert dirty_value is not None, f"{home_key} must be present without --clean"
    assert dirty_value == clean_value, (
        f"the package-composed {home_key} constant must be byte-identical "
        f"with and without --clean, for the same lock and digests (R3); "
        f"without --clean: {dirty_value!r}; with --clean: {clean_value!r}"
    )


def test_project_env_override_reaches_generated_entrypoint_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A project ``[env]`` override reaches a tool invoked THROUGH a
    generated entrypoint launcher, not only through ``ocx run``'s own
    direct child (R1 — the launcher is the PRIMARY path: synth-PATH is
    pushed last so ``entrypoints/`` shadows ``bin/``, so a package with
    entrypoints resolves through its launcher under ``ocx run``).

    Without ``OCX_ENV`` forwarding (R1 option 2), the launcher's
    ``Env::new()`` inherits nothing from the parent's ``[env]`` decision,
    and the package's own env re-applies on top — silently reverting
    exactly the override C4 names as the feature's declared intent. This
    test is the regression guard for that silent failure, not merely a
    functional check.
    """
    var_name = f"T_{unique_repo.upper().replace('-', '_')}_LAUNCH_OVERRIDE"
    base_pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[
            {
                "key": var_name,
                "type": "constant",
                "value": "package-value",
                "visibility": "public",
            },
        ],
    )
    ocx.plain("package", "install", base_pkg.short)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{base_pkg.fq}"

[env]
{var_name} = "project-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "showenv")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -- showenv must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, var_name) == "project-value", (
        f"a project [env] override must survive launcher re-entry — the "
        f"PRIMARY path for a package with entrypoints (R1); without OCX_ENV "
        f"forwarding, the package's own value silently wins instead; "
        f"env dump:\n{result.stdout}"
    )


def test_project_env_override_survives_nested_launcher_hop(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A project ``[env]`` override still wins at the SECOND launcher hop —
    when an entrypoint dispatches to another package's generated launcher
    (R1, nested case).

    The first launcher decodes ``OCX_ENV`` and applies the payload, then
    ``apply_ocx_config`` strips the key (the stale-payload defense). If
    nothing re-emits the validated payload afterwards, the second launcher
    sees no project env at all: it re-applies its OWN package env on top of
    the inherited value, and ``package-value`` beats the project override
    that stage 4-6 precedence promises. Inheriting the value is not enough —
    ``inner``'s package constant overwrites it — so this discriminates
    between "forwarded" and "merely inherited".
    """
    inner_repo = f"{unique_repo}_inner"
    var_name = f"T_{unique_repo.upper().replace('-', '_')}_NESTED_OVERRIDE"

    # Hop 2: declares the conflicting package constant, dispatches to `env`.
    inner_pkg = make_package_with_entrypoints(
        ocx,
        inner_repo,
        tmp_path,
        entrypoints={"inner": {"command": "env"}},
        file_prefix="inner",
        env=[
            {
                "key": var_name,
                "type": "constant",
                "value": "package-value",
                "visibility": "public",
            },
        ],
    )
    # Hop 1: dispatches to `inner`, which resolves to the OTHER package's
    # launcher on the composed PATH — the nested-launcher shape.
    outer_pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"outer": {"command": "inner"}},
        file_prefix="outer",
    )
    ocx.plain("package", "install", inner_pkg.short)
    ocx.plain("package", "install", outer_pkg.short)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
outer = "{outer_pkg.fq}"
inner = "{inner_pkg.fq}"

[env]
{var_name} = "project-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "run", "--", "outer")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -- outer must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, var_name) == "project-value", (
        f"a project [env] override must survive a SECOND launcher hop — the "
        f"first launcher has to re-emit the validated OCX_ENV payload after "
        f"apply_ocx_config strips it, or the nested package's own value wins; "
        f"env dump:\n{result.stdout}"
    )


def test_launcher_forged_ocx_env_fails_closed_on_whole_payload(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A forged ``OCX_ENV`` envelope carrying an ``OCX_DEFAULT_REGISTRY``
    entry must fail closed on the WHOLE payload (R1 decode-side gating) —
    not merely skip the one dangerous key. A benign entry in the SAME
    payload must not apply either, which is what proves the rejection is
    payload-wide and not a per-entry skip-and-warn (the X2 behavior the CI
    flavor writers use, deliberately NOT reused here per the ADR).

    The payload below is **well-formed**: correct envelope, correct
    sentinel, every entry structurally valid. That is deliberate and is the
    whole point — a malformed payload would be rejected for the wrong
    reason and would prove nothing about key gating. The only thing wrong
    with it is that one key is reserved.

    Envelope per R1a: an object whose ``entries`` array is the mandatory
    sentinel (a bare array has nowhere to carry one). The modifier field is
    spelled ``type``, matching ``ocx --format json env``, the ``[env]``
    table grammar, and ``Modifier``'s serde tag.
    """
    base_pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints={"showenv": {"command": "env"}}
    )
    ocx.plain("package", "install", base_pkg.short)
    which = ocx.json("package", "which", base_pkg.short)
    pkg_root = Path(which[base_pkg.short])

    forged = json.dumps(
        {
            "entries": [
                {"key": "OCX_ENV_TEST_BENIGN", "value": "should-not-apply", "type": "constant"},
                {"key": "OCX_DEFAULT_REGISTRY", "value": "evil.example.com", "type": "constant"},
            ]
        }
    )
    env = {**ocx.env, "OCX_NO_CONFIG": "1", "OCX_ENV": forged}
    result = subprocess.run(
        [str(ocx.binary), "launcher", "exec", str(pkg_root), "--", "showenv"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    # The payload is well-formed, so a rejection here can only be the
    # reserved-key gate firing. Asserting the exit code — not merely
    # "the key is absent" — is what makes this test discriminate: a decoder
    # that silently dropped the reserved entry and applied the rest would
    # exit 0 and fail here.
    assert result.returncode == EXIT_DATA, (
        f"a well-formed OCX_ENV payload carrying a reserved OCX_* key must be "
        f"rejected whole (exit {EXIT_DATA}), not partially applied; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert "OCX_ENV_TEST_BENIGN" not in result.stdout, (
        f"a benign entry from a payload that ALSO carries a forged "
        f"OCX_DEFAULT_REGISTRY key must not apply either — whole-payload "
        f"fail-closed, not per-entry skip (R1); env dump:\n{result.stdout}"
    )
    assert "OCX_DEFAULT_REGISTRY=evil.example.com" not in result.stdout, (
        f"a forged OCX_DEFAULT_REGISTRY in OCX_ENV must never reach the "
        f"child's real environment (X1 applies on decode too); "
        f"env dump:\n{result.stdout}"
    )


def test_run_strips_stale_ambient_ocx_env(ocx: OcxRunner, tmp_path: Path) -> None:
    """A stale ``OCX_ENV`` inherited from the ambient shell must not leak
    into an unrelated ``ocx run`` invocation (R1: ``apply_ocx_config``
    needs an ``OCX_ENV`` remove branch so a stale shell export cannot leak
    into an invocation that never intended to forward it). ``ocx run``
    always computes its own payload for its child; the ambient value must
    be overwritten, never passed through verbatim.
    """
    repo, tag = _published_tool(ocx, tmp_path, "staleenv")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f'[tools]\n{repo} = "{ocx.registry}/{repo}:{tag}"\n')
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    stale_marker = "STALE_OCX_ENV_MARKER_MUST_NOT_LEAK"
    result = _run(ocx, project, "run", "--", "env", extra_env={"OCX_ENV": stale_marker})
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run must succeed even with a garbage ambient OCX_ENV; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    ocx_env_value = _env_value(result.stdout, "OCX_ENV")
    assert ocx_env_value != stale_marker, (
        f"a stale ambient OCX_ENV must not leak verbatim into the child's "
        f"OCX_ENV — it must be overwritten by this invocation's own payload "
        f"(or absent); got: {ocx_env_value!r}"
    )


# =============================================================================
# Misc
# =============================================================================


def test_init_emits_ocx_toml_its_own_parser_accepts(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` emits a file its own parser accepts (Q1) — guards the
    hand-written literal template in ``project/mutate.rs`` from diverging
    from the parser as the ``Group``/``ProjectEnv`` schema evolves.
    """
    project = tmp_path / "proj"
    project.mkdir()
    init_result = _run(ocx, project, "init")
    assert init_result.returncode == EXIT_SUCCESS, (
        f"ocx init must succeed; rc={init_result.returncode}\nstderr:\n{init_result.stderr}"
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx init's emitted ocx.toml must parse under its own schema — "
        f"a subsequent `ocx lock` must not fail; rc={lock.returncode}\n"
        f"stderr:\n{lock.stderr}\nemitted content:\n"
        f"{(project / 'ocx.toml').read_text()}"
    )


# ---------------------------------------------------------------------------
# S9a — both env value forms validate green against the generated `project`
# JSON Schema. schemars cannot infer a string-or-table union from the
# normalized `EnvValue` struct (the shipped `[mirrors]` schema has exactly
# this defect, confirmed by execution per the ADR); this pins the required
# hand-written `oneOf` fragment. Uses the freshly generated schema (not the
# hosted URL) via a local `#:schema` file-path directive, so an uncommitted
# schema change is caught — mirrors `test_taplo_project_toolchain.py`'s
# skip-if-absent idiom (this file must not import from it — DAMP).
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def _taplo_binary() -> str:
    path = shutil.which("taplo")
    if path is None:
        pytest.skip(
            "taplo not available — pin via "
            "`ocx index update taplo && ocx install --select taplo` "
            "to enable this test"
        )
    return path


@pytest.fixture(scope="module")
def _project_schema_path(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """Build ``ocx_schema`` if needed, emit the ``project`` schema kind to
    a temp file, and return its path for a local ``#:schema`` binding.
    """
    if not SCHEMA_BINARY.exists():
        build = subprocess.run(
            ["cargo", "build", "--release", "-p", "ocx_schema"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        if build.returncode != 0 or not SCHEMA_BINARY.exists():
            pytest.skip(f"failed to build ocx_schema binary: {build.stderr.strip()}")

    result = subprocess.run(
        [str(SCHEMA_BINARY), "project"], capture_output=True, text=True, check=False
    )
    assert result.returncode == 0, (
        f"ocx_schema project failed (exit {result.returncode})\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    schema_dir = tmp_path_factory.mktemp("project_schema")
    schema_path = schema_dir / "project-v1.json"
    schema_path.write_text(result.stdout)
    return schema_path


def _taplo_check(taplo_binary: str, schema_path: Path, toml_body: str, tmp_path: Path):
    fixture = tmp_path / "ocx.toml"
    fixture.write_text(f"#:schema {schema_path}\n{toml_body}", encoding="utf-8")
    return subprocess.run(
        [taplo_binary, "check", str(fixture)],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )


def test_env_string_shorthand_validates_against_generated_schema(
    _taplo_binary: str, _project_schema_path: Path, tmp_path: Path
) -> None:
    """The bare-string constant shorthand — the common case, ``CI = "1"``
    — must validate green against the freshly generated ``project`` schema
    (S9a). A derived schema would red-underline almost every correct
    ``[env]`` file in the user's editor.
    """
    result = _taplo_check(
        _taplo_binary, _project_schema_path, '[tools]\n\n[env]\nCI = "1"\n', tmp_path
    )
    assert result.returncode == 0, (
        f"the bare-string env constant form must validate against the "
        f"generated project schema (S9a); taplo stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )


def test_env_table_form_validates_against_generated_schema(
    _taplo_binary: str, _project_schema_path: Path, tmp_path: Path
) -> None:
    """The explicit table form, ``CI = { type = "path", value = "bin" }``,
    must also validate green against the generated ``project`` schema (S9a).
    """
    result = _taplo_check(
        _taplo_binary,
        _project_schema_path,
        '[tools]\n\n[env]\nCI = { type = "path", value = "bin" }\n',
        tmp_path,
    )
    assert result.returncode == 0, (
        f"the table env form must validate against the generated project "
        f"schema (S9a); taplo stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
