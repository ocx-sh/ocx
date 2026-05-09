# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Project-tier config bootstrap acceptance tests (plan Phase 1).

Phase 1 only exercises path *discovery* — Phase 2+ adds schema parsing. The
tests here assert that the CLI accepts ``--project`` / ``OCX_PROJECT``,
surfaces missing-path failures with the correct exit code, and that
``OCX_NO_PROJECT=1`` does not prune an explicit ``--project`` flag.

Scope boundary:
    - Covered here: path discovery at the CLI boundary for explicit sources.
    - NOT covered: project config *contents* (no parsing yet), project config
      *semantics* (no commands consume the path yet), end-to-end CWD-walk
      behavior (unit tests in ``crates/ocx_lib/src/config/loader.rs`` cover it).

# TODO(Phase 2+): once ``ProjectConfig`` parsing lands, add content-driven
# acceptance tests (valid schema → success; malformed → exit 78; unknown
# fields → reject). Once ``ocx exec -g`` lands (Phase 4), add end-to-end
# workflow tests.

Phase 1 impl gate: these tests run against today's ``Context::try_init``
which does NOT yet call ``ConfigLoader::project_path`` — only wires
``explicit_project_path`` into ``ConfigInputs``. They are expected to FAIL
(not skip, not xfail) so Phase 1 impl flips them to pass.
"""
from __future__ import annotations

import json
import subprocess
import uuid
from pathlib import Path

import pytest

from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _unique_missing_path(prefix: str) -> str:
    """Return an absolute path that reliably does not exist."""
    return f"/tmp/ocx-project-test-missing-{prefix}-{uuid.uuid4().hex}.toml"


def _run_with_env(
    ocx: OcxRunner,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with the runner's isolated env plus optional extras."""
    env = {**ocx.env}
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary)] + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ---------------------------------------------------------------------------
# --project flag
# ---------------------------------------------------------------------------


def test_project_flag_accepts_valid_file(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --project <valid>`` reaches the command without a config error.

    Uses ``index catalog`` as the command-under-test: it runs through
    ``Context::try_init`` (so ``project_path`` resolution fires) but does
    not actually consume the project config. A zero exit code proves the
    flag was accepted without producing a ``NotFound`` or ``ConfigError``.
    """
    project_file = tmp_path / "ocx.toml"
    project_file.write_text("")

    result = _run_with_env(ocx, "--project", str(project_file), "index", "catalog")

    assert result.returncode == 0, (
        f"--project <valid> should not abort the command; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_project_flag_missing_file_exits_79(ocx: OcxRunner) -> None:
    """``ocx --project <missing>`` exits 79 (NotFound).

    Plan line 69, Amendment G7: missing explicit project path is distinct
    from parse errors. Uses ``index catalog`` because it exercises
    ``Context::try_init`` fully (``--version`` / ``--help`` short-circuit
    before config loads).
    """
    missing = _unique_missing_path("flag")

    result = _run_with_env(ocx, "--project", missing, "index", "catalog")

    assert result.returncode == 79, (
        f"--project <missing> should exit 79 (NotFound), got {result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert missing in combined or "ocx-project-test-missing" in combined, (
        f"error should name the missing path, got: stdout={result.stdout!r} "
        f"stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# OCX_PROJECT env var
# ---------------------------------------------------------------------------


def test_project_env_var_accepts_valid_file(ocx: OcxRunner, tmp_path: Path) -> None:
    """``OCX_PROJECT=<valid>`` reaches the command without a config error."""
    project_file = tmp_path / "ocx.toml"
    project_file.write_text("")

    result = _run_with_env(
        ocx,
        "index",
        "catalog",
        extra_env={"OCX_PROJECT": str(project_file)},
    )

    assert result.returncode == 0, (
        f"OCX_PROJECT=<valid> should not abort the command; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_project_env_var_missing_file_exits_79(ocx: OcxRunner) -> None:
    """``OCX_PROJECT=<missing>`` exits 79 (NotFound).

    Symmetric with ``--project <missing>``: Amendment G7 applies to both
    explicit sources.
    """
    missing = _unique_missing_path("env")

    result = _run_with_env(
        ocx,
        "index",
        "catalog",
        extra_env={"OCX_PROJECT": missing},
    )

    assert result.returncode == 79, (
        f"OCX_PROJECT=<missing> should exit 79 (NotFound), got "
        f"{result.returncode}; stderr={result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert missing in combined or "ocx-project-test-missing" in combined, (
        f"error should name the missing path, got: stdout={result.stdout!r} "
        f"stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# OCX_NO_PROJECT kill switch interaction
# ---------------------------------------------------------------------------


def test_ocx_no_project_does_not_block_explicit_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_NO_PROJECT=1`` + ``--project <valid>`` still loads.

    Mirrors the ``OCX_NO_CONFIG`` + ``--config`` interaction: the kill
    switch prunes discovery + env var, but NEVER the explicit CLI flag.
    """
    project_file = tmp_path / "ocx.toml"
    project_file.write_text("")

    result = _run_with_env(
        ocx,
        "--project",
        str(project_file),
        "index",
        "catalog",
        extra_env={"OCX_NO_PROJECT": "1"},
    )

    assert result.returncode == 0, (
        f"OCX_NO_PROJECT=1 must not block --project <valid>; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Precedence: --project flag wins over OCX_PROJECT env var
# ---------------------------------------------------------------------------


def test_project_flag_wins_over_env_var(ocx: OcxRunner, tmp_path: Path) -> None:
    """``OCX_PROJECT=A --project=B`` reads ``B`` and ignores ``A``.

    Plan_review_fixes_project_toolchain.md Phase 3 (Cluster B) — explicit
    flag must override env var. Both files exist and are valid. The test
    relies on observable state: the flagged file ``B`` parses successfully
    (exit 0) even when the env var ``A`` points at a malformed file that
    would exit 78 if it were the one consumed.
    """
    # File A (env var target): malformed TOML — would exit 78 if read.
    file_a = tmp_path / "envfile.toml"
    file_a.write_text("not valid toml = = =\n[\n")

    # File B (flag target): valid (empty `[tools]`).
    file_b = tmp_path / "flagfile.toml"
    file_b.write_text("[tools]\n")

    result = _run_with_env(
        ocx,
        "--project",
        str(file_b),
        "index",
        "catalog",
        extra_env={"OCX_PROJECT": str(file_a)},
    )

    # If the flag wins, parsing succeeds (exit 0); if the env var won
    # instead, parsing the malformed file would exit 78 (ConfigError).
    assert result.returncode == 0, (
        f"--project must override OCX_PROJECT; got rc={result.returncode}, "
        f"stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Custom config-file name end-to-end (Codex H3 regression)
# ---------------------------------------------------------------------------


def test_project_flag_resolves_custom_filename(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``--project=<dir>/custom.toml`` is honoured by mutators end-to-end.

    Plan_review_fixes_project_toolchain.md Phase 3 (Cluster B), Codex H3:
    pre-fix, mutators reconstructed ``<root>/ocx.toml`` from the resolved
    config path's parent and clobbered the wrong file. After the fix,
    ``ocx add`` writes to the supplied custom-named config and never
    creates a sibling ``ocx.toml``.

    Note: ``ocx add`` resolves the registry tag, so pointing at a real
    registry would cost network. The first thing the mutator does — flock
    + load — exercises the path-threading contract, so we check for the
    early failure mode (config-file presence) rather than the final
    write. If the mutator silently created a sibling ``ocx.toml`` (the
    pre-fix bug shape), this test would observe the file on disk.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    custom = project_dir / "custom-name.toml"
    custom.write_text("[tools]\n")

    # Run `ocx add` against an unreachable identifier so the resolver
    # short-circuits before touching the network in a way the test
    # environment cannot satisfy. The path-threading contract fires
    # during the load phase, before resolve.
    result = _run_with_env(
        ocx,
        "--project",
        str(custom),
        "add",
        "ocx.sh/this-tool-does-not-exist:1.0",
    )

    # Whether `ocx add` ultimately succeeds or fails on resolve, the
    # pre-fix bug shape is the observable presence of a sibling
    # `ocx.toml` that the mutator inadvertently created.
    sibling_ocx_toml = project_dir / "ocx.toml"
    assert not sibling_ocx_toml.exists(), (
        f"--project=<custom>.toml must NOT cause the mutator to create "
        f"<dir>/ocx.toml as a side effect; found: "
        f"{sorted(p.name for p in project_dir.iterdir())}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    # And the custom file must still exist (was not deleted or moved).
    assert custom.is_file(), (
        f"custom config file must survive the mutator invocation; "
        f"contents: {sorted(p.name for p in project_dir.iterdir())}"
    )


def test_project_flag_canonicalized_for_registry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Aliased project paths collapse to one entry in ``projects.json``.

    Plan_review_fixes_project_toolchain.md Phase 3 (Cluster B), Security
    Warn: pre-fix, ``ProjectRegistry::register`` accepted any absolute
    path verbatim, so two writers reaching the same physical lock file
    through different aliased paths (e.g. one canonical, one with a ``./``
    segment) registered twice. After the fix, lock-write sites
    canonicalize before registering — both paths land on a single entry.

    Two ``ocx lock`` invocations targeting the same project file via
    distinct path forms must produce one entry in ``$OCX_HOME/projects.json``.
    """
    home = tmp_path / "home"
    home.mkdir()

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    (project_dir / "ocx.toml").write_text("[tools]\n")

    # Path A: absolute, canonical.
    path_a = project_dir / "ocx.toml"
    # Path B: absolute path with a `./` segment that resolves to the same
    # file. Per POSIX, `<project_dir>/./ocx.toml` is the same file as
    # `<project_dir>/ocx.toml`; canonicalize collapses them.
    path_b = Path(str(project_dir) + "/./ocx.toml")

    env = {**ocx.env, "OCX_HOME": str(home)}

    # First lock via canonical path.
    r1 = subprocess.run(
        [str(ocx.binary), "--project", str(path_a), "lock"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert r1.returncode == 0, (
        f"first ocx lock must succeed; rc={r1.returncode}, stderr={r1.stderr!r}"
    )

    # Second lock via aliased path.
    r2 = subprocess.run(
        [str(ocx.binary), "--project", str(path_b), "lock"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert r2.returncode == 0, (
        f"second ocx lock must succeed; rc={r2.returncode}, stderr={r2.stderr!r}"
    )

    registry_file = home / "projects.json"
    assert registry_file.is_file(), (
        f"projects.json must exist after two `ocx lock` invocations; "
        f"home contents: {sorted(p.name for p in home.iterdir())}"
    )
    doc = json.loads(registry_file.read_text())
    entries = doc.get("entries", [])
    assert len(entries) == 1, (
        f"aliased lock paths must collapse to a single registry entry; "
        f"got {len(entries)} entries: {entries}"
    )


# ---------------------------------------------------------------------------
# Phase 2 bootstrap — ProjectConfig parsing via the CLI boundary
#
# Phase 2 lands the schema (``ProjectConfig``, ``ProjectLock``, declaration
# hash) but NO CLI command consumes the project config yet. End-to-end
# parsing acceptance is deferred to Phase 3 (``ocx lock``) and Phase 4
# (``ocx exec -g``). These xfail tests encode the fixture shapes so the
# implementer can flip the marker once a CLI surface is available.
# ---------------------------------------------------------------------------


def _write_fixture(tmp_path: Path, body: str) -> Path:
    """Write a valid ``ocx.toml`` fixture into a nested fixture dir."""
    fixture_dir = tmp_path / "fixture"
    fixture_dir.mkdir()
    project_file = fixture_dir / "ocx.toml"
    project_file.write_text(body)
    return project_file


VALID_OCX_TOML = """\
[tools]
cmake = "ocx.sh/cmake:3.28"
ninja = "ocx.sh/ninja:1.11"
mytool = "ghcr.io/acme/mytool:1.0"

[group.ci]
shellcheck = "ocx.sh/shellcheck:0.10"
"""

# Bare-tag form is rejected at the schema layer per plan_project_toolchain.md
# Phase 2.1 finding F1: identifiers must be fully qualified
# (registry/repo[:tag][@digest]) — no env-var expansion of the default
# registry at parse time, otherwise a checked-in `ocx.toml` would resolve
# differently across machines that set `OCX_DEFAULT_REGISTRY` to different
# values. The diagnostic surfaces as exit 78 (ConfigError) and names both
# the offending key and value.
BARE_TAG_OCX_TOML = """\
[tools]
cmake = "3.28"
"""


# xfail until Phase 3 (see plan_project_toolchain.md § Phase 3 — `ocx lock`
# command — adds the first CLI surface that parses ProjectConfig end-to-end).
@pytest.mark.xfail(
    reason=(
        "Phase 2 has no CLI surface that parses ProjectConfig; landing in "
        "Phase 3 with `ocx lock` / Phase 4 with `ocx exec -g`. Bootstrap "
        "test encoded now so the fixture shape is locked in."
    ),
    strict=False,
)
def test_project_config_loads_via_project_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --project fixture/ocx.toml <cmd>`` parses without erroring.

    Once a CLI surface lands that consumes the project config (e.g.
    ``ocx lock``), this test should replace ``index catalog`` with the
    real subcommand and assert on its output. The fixture shape captures
    the Phase 2 contract: flat ``[tools]``, a named ``[group.*]``.
    """
    project_file = _write_fixture(tmp_path, VALID_OCX_TOML)

    result = _run_with_env(ocx, "--project", str(project_file), "index", "catalog")

    # When a real CLI surface exists, this should assert structured
    # output. For now, non-zero exit with a config-parse trace is the
    # signal we're past the path-discovery layer.
    assert result.returncode == 0, (
        f"valid ocx.toml via --project should be accepted; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


# xfail until Phase 3 (see plan_project_toolchain.md § Phase 3 — `ocx lock`
# command — adds the first CLI surface that parses ProjectConfig end-to-end).
@pytest.mark.xfail(
    reason=(
        "Phase 2 has no CLI surface that parses ProjectConfig; landing in "
        "Phase 3 with `ocx lock` / Phase 4 with `ocx exec -g`. Bootstrap "
        "test encoded now so the env-var path is locked in."
    ),
    strict=False,
)
def test_project_config_loads_via_env_var(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_PROJECT=fixture/ocx.toml`` parses without erroring.

    Symmetric with ``--project``: the env-var resolution path must reach
    the parse layer once a consumer exists.
    """
    project_file = _write_fixture(tmp_path, VALID_OCX_TOML)

    result = _run_with_env(
        ocx,
        "index",
        "catalog",
        extra_env={"OCX_PROJECT": str(project_file)},
    )

    assert result.returncode == 0, (
        f"valid ocx.toml via OCX_PROJECT should be accepted; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


# xfail until Phase 3 wires `ProjectConfig::from_path` into a CLI surface
# that actually parses the file. Today's `Context::try_init` only resolves
# the project *path* via `ConfigLoader::project_path`; nothing reads the
# bytes. Once `ocx lock` lands, this test should flip to passing.
@pytest.mark.xfail(
    reason=(
        "Phase 2 has no CLI surface that parses ProjectConfig; the bare-tag "
        "rejection diagnostic only fires once Phase 3 (`ocx lock`) routes "
        "the file through `ProjectConfig::from_path`. Bootstrap test encoded "
        "now so the diagnostic shape is locked in."
    ),
    strict=False,
)
def test_bare_tag_in_project_config_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bare-tag identifier in ``ocx.toml`` exits 78 (ConfigError).

    Plan §3 / finding F1: full identifiers only — bare ``cmake = "3.28"``
    is a parse error with a diagnostic naming the offending key/value.
    A future ``default_registry = "..."`` field *inside* ``ocx.toml`` is
    deferred; today the only correct form is fully qualified.
    """
    project_file = _write_fixture(tmp_path, BARE_TAG_OCX_TOML)

    result = _run_with_env(ocx, "--project", str(project_file), "index", "catalog")

    assert result.returncode == 78, (
        f"bare-tag ocx.toml should exit 78 (ConfigError), got "
        f"{result.returncode}; stderr={result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "cmake" in combined and "3.28" in combined, (
        f"diagnostic should name the offending key and value, got: "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Home-tier fallback (Phase 9)
#
# Plan plan_project_toolchain.md Phase 9 (lines 830–855).  When the CWD walk
# finds nothing, the resolver falls back to ``$OCX_HOME/ocx.toml``.  When a
# project ``ocx.toml`` exists in the walk, the home tier MUST NOT compose
# (Amendment C — wholesale replacement).
#
# These tests are observable at the CLI boundary through ``ocx lock``: a
# project-tier file makes ``ocx lock`` succeed (exit 0); absence of any
# project-tier file makes ``ocx lock`` exit 64 ("no ocx.toml found").
# Sub-process invocations cd into a sibling dir so the CWD walk cannot
# rediscover the home file from above.
#
# ``ocx lock`` against a real registry needs published packages in the
# fixture; that infrastructure is in test_lock.py.  Here we only need the
# *path discovery* surface to fire — an empty ``[tools]`` table is enough
# to drive ``project_path()``'s home-tier branch.  If the empty-tools case
# turns out to require a tool entry (Phase 5 may tighten this), the test
# is marked ``xfail`` so Phase 5 implementation can flip it.
# ---------------------------------------------------------------------------


def _run_lock_in(
    ocx: OcxRunner,
    cwd: Path,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx lock`` from ``cwd`` so the resolver's CWD walk fires.

    Mirrors test_lock.py::_run_lock but takes optional ``extra_env`` so we
    can override ``OCX_HOME`` for the home-tier tests.
    """
    env = {**ocx.env}
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary), "lock"]
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)


def test_home_tier_ocx_toml_loads_when_no_project(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``$OCX_HOME/ocx.toml`` is consumed when CWD has no project file.

    ``ocx lock`` exits 0 when the home tier provides a valid (empty)
    ``[tools]`` table; without the fallback it would exit 64 ("no
    ocx.toml found").  This pins down the project-tier observability of
    the home-tier branch.
    """
    cwd = tmp_path / "sibling"
    cwd.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    (home / "ocx.toml").write_text("[tools]\n")

    result = _run_lock_in(ocx, cwd, extra_env={"OCX_HOME": str(home)})

    assert result.returncode == 0, (
        f"home-tier ocx.toml should drive `ocx lock` to success; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    # The lock writes next to the home file, not the cwd.
    assert (home / "ocx.lock").is_file(), (
        f"ocx.lock should be written to $OCX_HOME, not cwd; "
        f"home contents: {sorted(p.name for p in home.iterdir())}"
    )


def test_home_tier_ocx_toml_skipped_when_project_present(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Project ``ocx.toml`` at cwd beats ``$OCX_HOME/ocx.toml``.

    Both files exist and parse cleanly. The lock must be written next to
    the project file, not the home file (proves the walk hit was
    consumed; Amendment C — the home tier is never composed).
    """
    cwd = tmp_path / "project"
    cwd.mkdir()
    (cwd / "ocx.toml").write_text("[tools]\n")
    home = tmp_path / "home"
    home.mkdir()
    (home / "ocx.toml").write_text("[tools]\n")

    result = _run_lock_in(ocx, cwd, extra_env={"OCX_HOME": str(home)})

    assert result.returncode == 0, (
        f"`ocx lock` should succeed against project file; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (cwd / "ocx.lock").is_file(), (
        "lock must be written next to the project file (walk wins per "
        "Amendment C)"
    )
    assert not (home / "ocx.lock").exists(), (
        f"home-tier lock must NOT be written when a project file is "
        f"present; home contents: "
        f"{sorted(p.name for p in home.iterdir())}"
    )
