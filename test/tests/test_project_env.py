# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for user-declarable project environment variables
(``[env]`` / ``[group.<name>.env]`` in ``ocx.toml``, and ``ocx run --env``).

Scope
-----
Encodes the Component Contracts (CLI, Precedence, Composition) and the
Config-shape-fault table in
``.claude/state/plans/plan_project_env_declaration.md``, plus the ratified
decisions (S1-S9b, C1-C5, L1-L3, Q1-Q7, R1-R3, X1-X3) in
``.claude/artifacts/adr_project_env_declaration.md``.

Test inventory
---------------
CLI (``ocx run --env KEY[:TYPE]=VALUE``):
  test_env_flag_is_highest_precedence_constant
  test_env_flag_splits_on_first_equals_only
  test_env_flag_bare_key_without_equals_exits_64
  test_env_flag_repeated_last_wins
  test_env_flag_survives_clean
  test_env_flag_rejects_ocx_prefixed_key_exits_64
  test_env_flag_path_type_prepends_instead_of_replacing
  test_env_flag_without_type_replaces_path
  test_env_flag_relative_path_resolves_against_cwd
  test_env_flag_rejects_unknown_and_empty_type_exits_64
  test_env_flag_reserved_key_rejected_with_type_qualifier_exits_64
  test_env_flag_on_env_subcommand_matches_what_run_executes
  test_env_flag_on_direnv_export_reaches_the_exported_lines
  test_group_flag_on_direnv_export_selects_group_env
  test_direnv_export_rejects_unknown_group_exits_64
  test_direnv_export_empty_group_segment_exits_64

Precedence:
  test_ambient_value_loses_to_project_env
  test_project_env_constant_overrides_package_constant
  test_group_env_overrides_project_env_for_same_key
  test_group_env_later_selected_group_wins
  test_repeated_group_contributes_its_env_once
  test_project_env_path_entry_precedes_package_path_entry
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
  test_env_config_shape_fault_exits_78 (invalid key / non-string value /
    unknown value field)

Boundary-pinning (guard constraints whose consuming feature has not shipped):
  test_run_package_composed_env_byte_identical_with_and_without_clean  (R3)
  test_project_env_override_reaches_generated_entrypoint_launcher      (R1)
  test_project_env_override_survives_nested_launcher_hop               (R1)
  test_env_flag_path_type_survives_generated_entrypoint_launcher       (R1/L1)
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


def _export_value(lines: str, key: str) -> str | None:
    """Return the effective value of ``export KEY="value"`` in a shell block.

    Two deliberate choices:

    * Reads the value rather than matching the whole line, so a change in the
      emitter's quoting style does not fail tests that are about composition.
    * Returns the **last** assignment, not the first. The export block is a
      replay of the composed entry vector — a constant declared at a later
      stage is emitted as a second ``export`` line, and a shell evaluating the
      block top to bottom keeps the last one. Reading the first would report a
      value the sourced environment never has.
    """
    prefix = f"export {key}="
    effective: str | None = None
    for line in lines.splitlines():
        if not line.startswith(prefix):
            continue
        value = line[len(prefix) :]
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        effective = value
    return effective


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


def test_env_flag_survives_clean(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--clean`` strips the AMBIENT environment, never the ``--env``
    overrides typed on the same invocation.

    ``--clean`` decides what the child inherits from the surrounding shell;
    ``--env`` is a stage-6 declaration of this invocation. A ``--clean`` that
    also dropped stage 6 would make the two flags mutually exclusive in
    practice — the hermetic invocation is exactly the one that needs to name
    its own variables. Sibling of
    ``test_run_package_composed_env_byte_identical_with_and_without_clean``,
    which pins the same independence for the package-composed layer.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    # `--clean` starts from an empty ambient env, so `PATH` carries only what
    # the toolchain composes — nothing to resolve a bare `env` against.
    env_binary = shutil.which("env")
    assert env_binary is not None, "the POSIX `env` binary must be available"

    ambient = "T_PENV_CLEAN_AMBIENT_MUST_NOT_SURVIVE"
    result = _run(
        ocx,
        project,
        "run",
        "--clean",
        "--env",
        "FOO=1",
        "--",
        env_binary,
        extra_env={ambient: "ambient-value"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --clean --env must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "FOO") == "1", (
        f"--env must still apply under --clean; stdout:\n{result.stdout}"
    )
    # Discriminates "--clean did nothing" from "--clean kept stage 6": the
    # ambient value has to be gone in the same dump that carries FOO=1.
    assert _env_value(result.stdout, ambient) is None, (
        f"--clean must still strip the ambient environment; stdout:\n{result.stdout}"
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


def test_env_flag_path_type_prepends_instead_of_replacing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--env PATH:path=<dir>`` puts ``<dir>`` at the FRONT of the composed
    ``PATH`` and keeps every package directory behind it (L1).

    This is the capability the qualifier exists for: a caller that builds an
    argv array cannot inject ``$PATH`` into a value, so "prepend a directory
    for this invocation" is otherwise inexpressible from the CLI.
    """
    repo, tag = _published_tool(ocx, tmp_path, "flagpath")

    project = tmp_path / "proj"
    project.mkdir()
    extra = project / "flag_bin"
    extra.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    baseline = _run(ocx, project, "run", "--", "env")
    assert baseline.returncode == EXIT_SUCCESS, baseline.stderr
    baseline_path = _env_value(baseline.stdout, "PATH")
    assert baseline_path is not None, f"PATH must be present; stdout:\n{baseline.stdout}"

    result = _run(ocx, project, "run", "--env", f"PATH:path={extra}", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --env PATH:path=... must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    path_value = _env_value(result.stdout, "PATH")
    assert path_value is not None, f"PATH must be present; stdout:\n{result.stdout}"

    segments = path_value.split(":")  # POSIX separator; module is Linux-only
    assert segments[0] == str(extra), (
        f"a :path override must be the FRONT of PATH; got {segments[0]!r}, "
        f"expected {str(extra)!r}; full PATH:\n{path_value}"
    )
    missing = [seg for seg in baseline_path.split(":") if seg not in segments]
    assert not missing, (
        f"a :path override must PREPEND, leaving every composed directory in "
        f"place; these disappeared: {missing}\nbefore:\n{baseline_path}\n"
        f"after:\n{path_value}"
    )


def test_env_flag_without_type_replaces_path(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--env PATH=<dir>`` (no ``:path``) still REPLACES the composed value.

    Pins the deliberate footgun so it cannot change silently: ``--env`` is a
    stage-6 constant by default, and there is no name-based special case for
    ``PATH``. The child must be named absolutely, because clobbering ``PATH``
    is exactly what leaves nothing on it to resolve against.
    """
    repo, tag = _published_tool(ocx, tmp_path, "flagclobber")

    project = tmp_path / "proj"
    project.mkdir()
    only = project / "only_bin"
    only.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    env_binary = shutil.which("env")
    assert env_binary is not None, "the POSIX `env` binary must be available"

    result = _run(ocx, project, "run", "--env", f"PATH={only}", "--", env_binary)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --env PATH=... must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "PATH") == str(only), (
        f"an unqualified --env PATH must REPLACE the composed value, dropping "
        f"every package directory — the documented hazard `:path` exists to "
        f"avoid; stdout:\n{result.stdout}"
    )


def test_env_flag_relative_path_resolves_against_cwd(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A relative ``:path`` value anchors to the INVOCATION directory, not the
    project root — the one behavior that distinguishes the flag's ``:path``
    from the ``ocx.toml`` form, which anchors to the project root so a
    checked-in file means the same thing from any subdirectory.
    """
    project = tmp_path / "proj"
    project.mkdir()
    subdirectory = project / "sub"
    subdirectory.mkdir()
    # Both candidates exist, so the assertion discriminates between the two
    # bases rather than merely observing which one happens to be present.
    (project / "rel_bin").mkdir()
    (subdirectory / "rel_bin").mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, subdirectory, "run", "--env", "PATH:path=rel_bin", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run from a subdirectory must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    path_value = _env_value(result.stdout, "PATH")
    assert path_value is not None, f"PATH must be present; stdout:\n{result.stdout}"
    first_segment = path_value.split(":")[0]  # POSIX separator; module is Linux-only
    assert first_segment == str(subdirectory / "rel_bin"), (
        f"a relative --env :path must resolve against the invocation directory, "
        f"not the project root; got {first_segment!r}, expected "
        f"{str(subdirectory / 'rel_bin')!r}; full PATH:\n{path_value}"
    )


def test_env_flag_rejects_unknown_and_empty_type_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A ``TYPE`` naming no modifier is CLI misuse, exit 64.

    Deliberately NOT the file form's exit 78: a bogus ``type`` in ``ocx.toml``
    is a config-shape fault in a file, whereas this is a mistyped flag.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    for argument in ("X:bogus=v", "X:=v"):
        result = _run(ocx, project, "run", "--env", argument, "--", "echo", "hi")
        assert result.returncode == EXIT_USAGE, (
            f"--env {argument} must exit {EXIT_USAGE}; got {result.returncode}\n"
            f"stderr:\n{result.stderr}"
        )
        assert "constant" in result.stderr and "path" in result.stderr, (
            f"the error must name the accepted types — the user has no file to "
            f"look at; stderr:\n{result.stderr}"
        )


def test_env_flag_reserved_key_rejected_with_type_qualifier_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--env OCX_FOO:path=x`` is still rejected, exit 64 (X1).

    The security-relevant case: the reserved-namespace gate runs on the key
    AFTER the ``:TYPE`` qualifier is stripped, so adding a qualifier is not a
    way past it.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    for argument in ("OCX_INDEX:path=/tmp/evil", "__OCX_TESTING_X:constant=1"):
        result = _run(ocx, project, "run", "--env", argument, "--", "echo", "hi")
        assert result.returncode == EXIT_USAGE, (
            f"--env {argument} must exit {EXIT_USAGE} (reserved OCX_*/__OCX_* "
            f"namespace); got {result.returncode}\nstderr:\n{result.stderr}"
        )


def test_env_flag_on_env_subcommand_matches_what_run_executes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx env --env`` prints exactly what ``ocx run --env`` executes with.

    This is the whole point of the flag existing on an export surface: OCX is
    a backend tool for tools, and a caller that builds an argv array must be
    able to *export* the environment it would otherwise *execute* in. A test
    asserting each side independently would pass even if the two composed
    different environments, so this asserts the two against each other.
    """
    repo, tag = _published_tool(ocx, tmp_path, "exportparity")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"

[env]
FROM_FILE = "file-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    overrides = ["--env", "FROM_FLAG=flag-value", "--env", "FROM_FILE=flag-wins"]

    executed = _run(ocx, project, "run", *overrides, "--", "env")
    assert executed.returncode == EXIT_SUCCESS, executed.stderr

    exported = _run(ocx, project, "env", *overrides, "--shell=bash")
    assert exported.returncode == EXIT_SUCCESS, exported.stderr

    for key, expected in (
        ("FROM_FLAG", "flag-value"),
        ("FROM_FILE", "flag-wins"),
    ):
        assert _env_value(executed.stdout, key) == expected, (
            f"ocx run --env must apply {key}={expected}; stdout:\n{executed.stdout}"
        )
        assert _export_value(exported.stdout, key) == expected, (
            f"ocx env --env must export the same {key}={expected} that ocx run "
            f"applies; export lines:\n{exported.stdout}"
        )


def test_env_flag_on_direnv_export_reaches_the_exported_lines(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx direnv export --env`` emits the override, so a hand-edited
    ``.envrc`` can carry one.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "direnv", "export", "--env", "DIRENV_FLAG=on")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _export_value(result.stdout, "DIRENV_FLAG") == "on", (
        f"--env must reach the exported lines; stdout:\n{result.stdout}"
    )


def test_group_flag_on_direnv_export_selects_group_env(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx direnv export -g <group>`` composes that group's ``[env]``.

    Without ``-g`` the command exports the default group only, so a group's
    ``[env]`` was previously unreachable from an ``.envrc`` at all.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[env]
ALWAYS = "base"

[group.ci.env]
CI_ONLY = "ci-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    default_scope = _run(ocx, project, "direnv", "export")
    assert default_scope.returncode == EXIT_SUCCESS, default_scope.stderr
    assert _export_value(default_scope.stdout, "ALWAYS") == "base"
    assert _export_value(default_scope.stdout, "CI_ONLY") is None, (
        f"the ci group's [env] must not leak into the default scope; "
        f"stdout:\n{default_scope.stdout}"
    )

    ci_scope = _run(ocx, project, "direnv", "export", "-g", "ci")
    assert ci_scope.returncode == EXIT_SUCCESS, ci_scope.stderr
    assert _export_value(ci_scope.stdout, "CI_ONLY") == "ci-value", (
        f"-g ci must compose [group.ci.env]; stdout:\n{ci_scope.stdout}"
    )


def test_direnv_export_rejects_unknown_group_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A ``-g`` naming no declared group is an argv typo in a hand-edited
    ``.envrc``, so it fails loudly rather than silently exporting nothing.

    The never-fail-the-prompt contract covers transient toolchain state (a
    missing lock, an unmaterialised tool), not a misspelled argument.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "direnv", "export", "-g", "nope")
    assert result.returncode == EXIT_USAGE, (
        f"an unknown -g must exit {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


@pytest.mark.parametrize(
    "group_value",
    ["ci,,lint", ",ci", "ci,", ",,"],
    ids=["middle", "leading", "trailing", "degenerate"],
)
def test_direnv_export_empty_group_segment_exits_64(
    ocx: OcxRunner, tmp_path: Path, group_value: str
) -> None:
    """A stray comma in ``direnv export -g`` is a typo, exit 64 — the same
    parse-level rejection ``ocx run`` and ``ocx env`` already give it.

    clap's ``value_delimiter`` turns each of these into a list carrying at
    least one empty name, which would otherwise select nothing silently. The
    never-fail-the-prompt contract covers transient toolchain state, not a
    malformed argument in a hand-edited ``.envrc``; a third export surface
    that quietly disagreed with the other two would be the worst of both.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[group.ci.env]
CI_ONLY = "ci-value"

[group.lint.env]
LINT_ONLY = "lint-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    result = _run(ocx, project, "direnv", "export", "-g", group_value)
    assert result.returncode == EXIT_USAGE, (
        f"an empty -g comma segment ({group_value!r}) must exit {EXIT_USAGE}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
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


def test_group_env_overrides_project_env_for_same_key(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A selected group's ``[env]`` (stage 5) beats the project's top-level
    ``[env]`` (stage 4) for the same key (C2).

    ``test_env_flag_is_highest_precedence_constant`` declares both stages on
    the same key but asserts only that ``--env`` beats them, so it passes
    whichever of 4 and 5 wins. Dropping the flag is what makes the ordering
    between the two file-declared stages observable at all: a group is a
    narrowing of the project scope, so the narrower declaration is the one the
    user expects to see.
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

    result = _run(ocx, project, "run", "-g", "g", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -g g must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert _env_value(result.stdout, "X") == "group-value", (
        f"a selected group's [env] (stage 5) must override the project's "
        f"top-level [env] (stage 4) for the same key; stdout:\n{result.stdout}"
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


def test_repeated_group_contributes_its_env_once(ocx: OcxRunner, tmp_path: Path) -> None:
    """``-g a -g b -g a`` applies group ``a`` ONCE, at its last position.

    "Later group wins" is implemented by keeping only a repeated group's LAST
    occurrence, so naming it twice re-orders it rather than re-applying it.
    The composed entry vector is where that is observable: the JSON report
    replays the vector verbatim, so a lost dedup emits ``a``'s declaration
    twice — once ahead of ``b``, once behind it.

    ``PATH`` alone cannot discriminate the two: the applier prepends with
    move-to-front semantics (``Env::add_path``), so re-applying the same
    directory never duplicates it. Both surfaces are asserted anyway — the
    entry-vector count is the guard, and the ``PATH`` assertion pins the
    user-visible promise that guard exists to protect.
    """
    project = tmp_path / "proj"
    project.mkdir()
    (project / "a_bin").mkdir()
    _write_ocx_toml(
        project,
        """\
[tools]

[group.a.env]
PATH = { type = "path", value = "a_bin" }

[group.b.env]
B_MARKER = "b-value"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    a_bin = str(project / "a_bin")

    reported = _run(ocx, project, "--format", "json", "env", "-g", "a", "-g", "b", "-g", "a")
    assert reported.returncode == EXIT_SUCCESS, (
        f"ocx env -g a -g b -g a must succeed; rc={reported.returncode}\n"
        f"stderr:\n{reported.stderr}"
    )
    entries = json.loads(reported.stdout)["entries"]
    occurrences = [e for e in entries if e["key"] == "PATH" and e["value"] == a_bin]
    assert len(occurrences) == 1, (
        f"a group named twice must contribute its [env] once, at its last "
        f"position; got {len(occurrences)} occurrences of {a_bin!r}; "
        f"entries={entries}"
    )
    assert any(e["key"] == "B_MARKER" for e in entries), (
        f"the group between the repeats must still contribute; entries={entries}"
    )

    executed = _run(ocx, project, "run", "-g", "a", "-g", "b", "-g", "a", "--", "env")
    assert executed.returncode == EXIT_SUCCESS, (
        f"ocx run -g a -g b -g a must succeed; rc={executed.returncode}\n"
        f"stderr:\n{executed.stderr}"
    )
    path_value = _env_value(executed.stdout, "PATH")
    assert path_value is not None, f"PATH must be present; stdout:\n{executed.stdout}"
    segments = path_value.split(":")  # POSIX separator; module is Linux-only
    assert segments.count(a_bin) == 1, (
        f"the repeated group's directory must appear exactly once on PATH; "
        f"full PATH:\n{path_value}"
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


@pytest.mark.parametrize(
    ("body", "tokens"),
    [
        ('[tools]\n\n[env]\n1FOO = "x"\n', ("1FOO",)),
        ("[tools]\n\n[env]\nX = 42\n", ("X", "integer")),
        (
            '[tools]\n\n[env]\nX = { type = "path", value = "bin", required = true }\n',
            ("X", "required"),
        ),
    ],
    ids=["invalid-key", "non-string-value", "unknown-value-field"],
)
def test_env_config_shape_fault_exits_78(
    ocx: OcxRunner, tmp_path: Path, body: str, tokens: tuple[str, ...]
) -> None:
    """Every ``[env]`` shape fault is a config error (78) naming what is wrong.

    The three cases are the remaining fault classes beside the bogus-``type``
    one above: a key outside the POSIX environment-name grammar (``1FOO``), a
    value that is neither a string nor a ``{ type, value }`` table (``42``),
    and a table carrying a field the grammar does not define (``required`` —
    a deferred feature, so accepting-and-ignoring it would silently discard a
    fail-if-absent intent).

    Exit 78 is the contract a caller scripts against: 78 says "fix the file",
    64 would say "fix the command line". All three are faults in a file, so
    the code must not drift to the flag's 64 — which is what an exit-code
    assertion at this level pins that a variant-level unit assertion cannot.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, body)

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_CONFIG, (
        f"a malformed [env] declaration must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    for token in tokens:
        assert token in result.stderr, (
            f"stderr must name {token!r} so the user knows what to edit; "
            f"got:\n{result.stderr}"
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


def test_env_flag_path_type_survives_generated_entrypoint_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A ``--env PATH:path=<dir>`` override reaches a tool invoked THROUGH a
    generated entrypoint launcher, still prepending rather than replacing.

    Pins the modifier's survival across the ``OCX_ENV`` wire: the payload
    carries a ``"type"`` per entry, and a decode that dropped or defaulted it
    would turn this into a constant — leaving ``<dir>`` as the whole of
    ``PATH`` instead of its front. Asserting on the package directories that
    remain behind it is what discriminates the two.
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
    )
    ocx.plain("package", "install", pkg.short)

    project = tmp_path / "proj"
    project.mkdir()
    extra = project / "launcher_bin"
    extra.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{pkg.fq}"
""",
    )
    assert _run_lock(ocx, project).returncode == EXIT_SUCCESS

    baseline = _run(ocx, project, "run", "--", "showenv")
    assert baseline.returncode == EXIT_SUCCESS, baseline.stderr
    baseline_path = _env_value(baseline.stdout, "PATH")
    assert baseline_path is not None, f"PATH must be present; stdout:\n{baseline.stdout}"

    result = _run(ocx, project, "run", "--env", f"PATH:path={extra}", "--", "showenv")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run --env PATH:path=... -- showenv must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    path_value = _env_value(result.stdout, "PATH")
    assert path_value is not None, f"PATH must be present; stdout:\n{result.stdout}"

    segments = path_value.split(":")  # POSIX separator; module is Linux-only
    assert segments[0] == str(extra), (
        f"the forwarded :path override must be the FRONT of the launcher's "
        f"PATH; got {segments[0]!r}, expected {str(extra)!r}; "
        f"full PATH:\n{path_value}"
    )
    missing = [seg for seg in baseline_path.split(":") if seg not in segments]
    assert not missing, (
        f"the forwarded entry must keep its `path` modifier across OCX_ENV — a "
        f"constant would have replaced these instead of prepending: {missing}\n"
        f"before:\n{baseline_path}\nafter:\n{path_value}"
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
