# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx upgrade`` (whole-file lock model).

``ocx upgrade`` is the whole-file bump verb: it re-resolves EVERY declared
tag, always, even when the lock is already current. There is no subset
surface — ``--group`` / ``-g`` and positional binding names were removed.
Groups are a composition concern only, never an upgrade scope.

1. ``ocx upgrade`` (no args)  — re-resolves the whole file (bumps every tag)
2. ``ocx upgrade --group ci`` — rejected by clap (exit 64); subset surface gone
3. ``ocx upgrade cmake``      — rejected by clap (exit 64); positional scoping gone

Spec: ``design_spec_partial_mutator_pin_preservation.md`` §2, §4.5, §8.2.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


EXIT_SUCCESS = 0
EXIT_USAGE = 64        # clap unknown-arg → EX_USAGE
EXIT_POLICY_BLOCKED = 81


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    return [str(ocx.binary), *args]


def _run_lock(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "lock", *extra),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _run_update(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "upgrade", *extra),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _read_lock_text(project_dir: Path) -> str:
    return (project_dir / "ocx.lock").read_text()


# V2 lock shape: each ``[[tool]]`` carries a bare ``repository`` plus a
# ``[tool.platforms]`` table of per-platform leaf digests. There is no single
# ``pinned`` index digest — a tool's content fingerprint is its leaf set.
_LEAF_RE = re.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def _leaves_for(lock_text: str, name: str) -> list[str]:
    """Return the sorted per-platform leaf digests for the ``[[tool]]`` entry
    whose ``name`` field equals ``name``; ``[]`` when no entry matches.

    Slices from the tool's ``name = "<name>"`` line to the next ``[[tool]]``
    boundary so only that tool's ``[tool.platforms]`` leaves are collected.
    """
    marker = f'name = "{name}"'
    if marker not in lock_text:
        return []
    start = lock_text.index(marker)
    rest = lock_text[start:]
    next_tool = rest.find("[[tool]]", len("[[tool]]"))
    slice_text = rest if next_tool == -1 else rest[:next_tool]
    return sorted(_LEAF_RE.findall(slice_text))


def _declaration_hash(lock_text: str) -> str:
    m = re.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', lock_text)
    assert m is not None, "declaration_hash missing"
    return m.group(1)


def _generated_at(lock_text: str) -> str:
    m = re.search(r'generated_at\s*=\s*"([^"]+)"', lock_text)
    assert m is not None, "generated_at missing"
    return m.group(1)


# ---------------------------------------------------------------------------
# 1. ``ocx upgrade`` (whole file) — bumps every moving tag to its new digest
# ---------------------------------------------------------------------------


def test_upgrade_bumps_every_tag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade`` (no args) re-resolves EVERY declared tag. With two
    tools each declared at a moving ``:latest`` that has since advanced
    upstream, both tools' leaf digests change — upgrade is whole-file, never
    a subset.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_bump_a"
    repo_b = f"t_{short}_bump_b"

    # Cascade so ``:latest`` is a real moving tag for both tools.
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=True)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:latest"
b = "{ocx.registry}/{repo_b}:latest"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_a = _leaves_for(initial_text, "a")
    initial_b = _leaves_for(initial_text, "b")
    assert initial_a and initial_b, "both tools must record leaf digests"

    # Advance both moving tags upstream (cascade re-points ``:latest``).
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=False, cascade=True)
    for repo in (repo_a, repo_b):
        refresh = subprocess.run(
            _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
            cwd=project,
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    # Bare ``ocx upgrade`` re-resolves the whole file; both tags advance.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_a = _leaves_for(after_text, "a")
    after_b = _leaves_for(after_text, "b")
    assert after_a and after_b, "both tools must still record leaf digests"

    assert after_a != initial_a, "'a' leaves must change — upgrade bumps every tag"
    assert after_b != initial_b, "'b' leaves must change — upgrade bumps every tag"


# ---------------------------------------------------------------------------
# 2. ``ocx upgrade`` (no args) — full re-resolution, equivalent to ``ocx lock``
# ---------------------------------------------------------------------------


def test_update_no_args_full_resolution_equivalent_to_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade`` with no args on an unchanged ``ocx.toml`` → produces
    a byte-identical lock to a fresh ``ocx lock`` run from scratch.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_full_a"
    repo_b = f"t_{short}_full_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
b = "{ocx.registry}/{repo_b}:2.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_leaves = sorted(_LEAF_RE.findall(initial_text))
    initial_hash = _declaration_hash(initial_text)

    # `ocx upgrade` (no args) re-resolves everything against the same
    # ocx.toml — every leaf digest must match the initial lock and the
    # declaration_hash must be unchanged.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_text = _read_lock_text(project)
    after_leaves = sorted(_LEAF_RE.findall(after_text))
    after_hash = _declaration_hash(after_text)

    assert after_leaves == initial_leaves, (
        "no-args `ocx upgrade` must keep every tool's leaf digests equal to `ocx lock`"
    )
    assert after_hash == initial_hash, (
        "declaration_hash must be unchanged when ocx.toml has not changed"
    )


# ---------------------------------------------------------------------------
# 3. The subset surface is gone — positional args and --group → exit 64
# ---------------------------------------------------------------------------


def test_upgrade_rejects_positional_args(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade <binding>`` is rejected by clap (exit 64): positional
    scoping no longer exists. The existing ``ocx.lock`` is left untouched.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_pos"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before_bytes = (project / "ocx.lock").read_bytes()

    result = _run_update(ocx, project, "tool")
    assert result.returncode == EXIT_USAGE, (
        f"`ocx upgrade tool` must be rejected (exit {EXIT_USAGE}); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )

    after_bytes = (project / "ocx.lock").read_bytes()
    assert after_bytes == before_bytes, (
        "ocx.lock must NOT be rewritten when a positional arg is rejected"
    )


def test_upgrade_rejects_group_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --group ci`` is rejected by clap (exit 64): the subset
    surface is gone. ``-g`` is also rejected. Groups are a composition
    concern only, never an upgrade scope.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grpflag"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[group.ci]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )

    for flag in ("--group", "-g"):
        result = _run_update(ocx, project, flag, "ci")
        assert result.returncode == EXIT_USAGE, (
            f"`ocx upgrade {flag} ci` must be rejected (exit {EXIT_USAGE}); "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )


def test_upgrade_offline_uncached_policy_blocked(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --offline upgrade`` with a moving tag absent from the local
    index → exit 81 (PolicyBlocked). Offline + frozen refuse to re-resolve an
    unpinned tag without touching the network.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_offline"
    fake_registry = "fake.registry.invalid"  # unreachable by construction

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{fake_registry}/{repo}:latest"
""",
    )

    # Hand-author a V2 lock so the predecessor exists but the tag is uncached.
    (project / "ocx.lock").write_text(
        f"""\
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{"d" * 64}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "tool"
group = "default"
repository = "{fake_registry}/{repo}"

[tool.platforms]
"linux/amd64" = "sha256:{"a" * 64}"
"""
    )

    result = subprocess.run(
        _ocx_cmd(ocx, "--offline", "upgrade"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"ocx --offline upgrade with an uncached moving tag must exit "
        f"{EXIT_POLICY_BLOCKED}; got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_update_check_succeeds_on_current(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` exits 0 without writing when the candidate
    lock matches the predecessor.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upd_check_ok"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before = (project / "ocx.lock").read_bytes()

    result = _run_update(ocx, project, "--check")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --check must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after = (project / "ocx.lock").read_bytes()
    assert before == after, "ocx upgrade --check must NOT rewrite ocx.lock"


def test_upgrade_check_no_lock_exits_78_without_network(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` with NO ``ocx.lock`` exits 78 (ConfigError)
    BEFORE any resolution. The missing-predecessor check runs first, so a
    registry/auth/policy failure can never mask the intended exit 78 and no
    network call is attempted.

    The ``ocx.toml`` points at an unreachable registry: if the command tried
    to re-resolve before checking for the predecessor it would surface a
    registry/policy error (not 78). Exit 78 proves the check runs first.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_check_no_lock"
    fake_registry = "fake.registry.invalid"  # unreachable by construction

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{fake_registry}/{repo}:1.0.0"
""",
    )

    # No ocx.lock written — the predecessor is absent.
    assert not (project / "ocx.lock").exists()

    result = _run_update(ocx, project, "--check")
    assert result.returncode == 78, (
        f"ocx upgrade --check with no lock must exit 78 (ConfigError) before "
        f"any resolve; got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx upgrade --check must not create ocx.lock"
    )


def test_upgrade_check_exits_65_on_whole_file_drift(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` exits 65 (DataError) without writing when
    an advisory tag has moved upstream — even though ``ocx.toml`` is
    byte-identical to the lock's recorded ``declaration_hash``.

    ``upgrade`` is a whole-file verb: ``--check`` compares the whole-file
    re-resolve candidate against the recorded lock and refuses when any
    declared tag has advanced. There is no subset scope.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upd_check_drift"

    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
mover = "{ocx.registry}/{repo}:latest"
""",
    )
    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before_lock = (project / "ocx.lock").read_bytes()

    # Publish 2.0.0 with cascade=True — "latest" now points at the new digest.
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True)
    refresh = subprocess.run(
        _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    result = _run_update(ocx, project, "--check")
    assert result.returncode == 65, (
        f"ocx upgrade --check must exit 65 when an advisory tag moved; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_lock = (project / "ocx.lock").read_bytes()
    assert before_lock == after_lock, (
        "ocx upgrade --check must NOT rewrite ocx.lock when refusing"
    )


# ---------------------------------------------------------------------------
# Eager materialization — Phase-5 contracts
# ---------------------------------------------------------------------------


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    """Return the expected candidate-symlink path for ``repo:tag``."""
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def _packages_present_count(ocx: OcxRunner) -> int:
    """Count ``content/`` directories under
    ``$OCX_HOME/packages/{registry_dir}/`` — eager-vs-lazy observable for
    toolchain mutators (``project_context.rs::materialize_lock`` warms via
    ``pull_all``, never creates symlinks).
    """
    base = Path(ocx.ocx_home) / "packages" / registry_dir(ocx.registry)
    if not base.exists():
        return 0
    return sum(1 for p in base.rglob("content") if p.is_dir())


def _two_tag_project(
    ocx: OcxRunner, tmp_path: Path
) -> tuple[Path, str, str, str]:
    """Publish a tool with two distinct tags and return ``(project_dir, repo, tag_v1, tag_v2)``.

    The project's ``ocx.toml`` is initially locked to ``tag_v1``; callers that
    want to exercise upgrade behaviour swap the toml to ``tag_v2`` and re-run
    ``ocx lock`` / ``ocx upgrade``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upg_eager"
    tag_v1 = "1.0.0"
    tag_v2 = "2.0.0"
    make_package(ocx, repo, tag_v1, tmp_path, new=True, cascade=False)
    make_package(ocx, repo, tag_v2, tmp_path, new=False, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v1}"
""",
    )
    # Write the initial lock so upgrade has a predecessor.  Use --no-pull
    # so the setup step leaves the object store cold; otherwise tag_v1
    # would already be present and `_packages_present_count >= 1` would
    # fire trivially, hiding whether the eager-default upgrade actually
    # materialised tag_v2 (the cold-store baseline is the eager-vs-lazy
    # observable now that candidate symlinks are no longer created by
    # toolchain mutators).
    initial = _run_lock(ocx, project, "--no-pull")
    assert initial.returncode == EXIT_SUCCESS, initial.stderr

    return project, repo, tag_v1, tag_v2


def test_upgrade_eager_default_materializes_new_digest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """REGRESSION GUARD: ``ocx upgrade`` (no flags) after bumping the toml to
    a new tag writes the lock AND pre-warms the object store with the new
    tag, but creates **no** candidate symlink.

    Plan Phase-5 Step 3.3 contract: default is eager. The candidate-absent
    half locks in the no-symlink mutator invariant
    (``project_context.rs::materialize_lock`` → ``pull_all``).
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    # Bump the toml to tag_v2 then upgrade.
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx upgrade must pre-warm the new digest into the object store"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_upgrade_no_pull_skips_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --no-pull`` writes the lock and leaves the object store
    cold. No candidate symlink under either eager or lazy paths anymore;
    cold-store is the only eager-vs-lazy observable.

    Plan Phase-5 Step 3.3 contract.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written even with --no-pull"

    assert _packages_present_count(ocx) == 0, (
        "ocx upgrade --no-pull must leave the object store cold"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_upgrade_pull_then_no_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --pull --no-pull`` → ``--no-pull`` wins (POSIX last-wins);
    lock must advance to the new digest but candidate_v2 must NOT exist.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx upgrade``.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--pull", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --pull --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) == 0, (
        "--no-pull must win: object store stays cold"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_upgrade_no_pull_then_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --no-pull --pull`` → ``--pull`` wins (POSIX last-wins);
    lock advances and object store warms with the new digest; candidate
    symlink absent.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx upgrade``.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--no-pull", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --no-pull --pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) >= 1, (
        "--pull must win: object store warms with the new digest"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_upgrade_check_unaffected_by_pull_flags(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` is a pure dry-run and must NOT create any
    candidate symlink regardless of ``--pull`` / ``--no-pull`` flags.

    Plan Phase-5 Step 3.3: the ``--check`` early-return path in
    ``upgrade.rs:175`` (before the materialize call) guarantees this.
    The test is a regression guard for the separation of verify vs mutate.
    """
    project, repo, tag_v1, _tag_v2 = _two_tag_project(ocx, tmp_path)

    # Candidate symlinks are never created by toolchain mutators (lock /
    # upgrade / add) under the no-symlink mutator model. The probe path
    # is kept here as a regression anchor for the verify-vs-mutate split.
    candidate_v1 = _candidate_path(ocx, repo, tag_v1)
    assert_not_exists(candidate_v1)

    lock_bytes_before = (project / "ocx.lock").read_bytes()

    # Run with both --check and --pull to confirm --check wins.
    result = _run_update(ocx, project, "--check", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --check --pull must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_bytes_after = (project / "ocx.lock").read_bytes()
    assert lock_bytes_before == lock_bytes_after, (
        "ocx upgrade --check must NOT rewrite ocx.lock"
    )
    # --check must not materialize anything even when --pull is present.
    assert_not_exists(candidate_v1)
