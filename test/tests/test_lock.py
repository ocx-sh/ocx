# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx lock`` (plan Phase 3).

These tests trace one-to-one to the 11 acceptance-test bullets at plan
lines 614–625.  They exercise the full CLI boundary against the fixture
Docker-compose registry: ``ocx lock`` walks CWD for ``ocx.toml``,
resolves every tool's advisory tag to an OCI index-manifest digest, and
writes a deterministic ``ocx.lock`` next to the config.

Specification mode (contract-first TDD)
---------------------------------------
All tests below run against the current Phase 3 *stub*.  ``ocx lock``
calls ``unimplemented!()`` in both the CLI command (``command/lock.rs``)
and the resolver (``project/resolve.rs::resolve_lock``).  Every test in
this file is therefore expected to FAIL against the stub — the contract
they encode is the Phase 3 implementation target.

F3 compliance: classification is asserted via **exit codes** (stable,
sysexits-aligned) and **file side effects** (written / not written).
No test asserts on not-yet-existing ``ProjectErrorKind`` variants.

Gitattributes coverage (test 11) is the ONLY Python-only coverage point
for the note — the unit-test counterpart is tombstoned in
``crates/ocx_lib/src/project/resolve.rs`` (see docstring there).
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


# ---------------------------------------------------------------------------
# Exit code constants — align with crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64       # no ocx.toml / dropped flag (--group, -g, --upgrade)
EXIT_UNAVAILABLE = 69
EXIT_CONFIG = 78      # corrupt ocx.lock
EXIT_NOT_FOUND = 79   # tag unresolvable


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    """Build an argv list for ``ocx`` with the runner's isolated env."""
    return [str(ocx.binary), *args]


def _run_lock(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx lock`` with ``cwd`` driving the ``ocx.toml`` CWD-walk.

    We bypass ``OcxRunner.run`` because it does not expose ``cwd=`` and
    ``ocx lock`` is the first command in the suite that needs it.
    """
    cmd = _ocx_cmd(ocx, "lock", *extra)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _run_lock_with_project(
    ocx: OcxRunner,
    cwd: Path,
    project_path: Path,
    *extra: str,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx --project <project_path> lock`` from ``cwd``.

    Passes the ``--project`` global flag before the ``lock`` subcommand so
    the resolver uses the explicit config rather than the CWD-walk result.
    """
    cmd = [str(ocx.binary), "--project", str(project_path), "lock", *extra]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _two_pushed_tools(
    ocx: OcxRunner,
    tmp_path: Path,
) -> tuple[str, str, str, str]:
    """Publish two distinct tool packages (each with one tag) and return
    their ``(registry/repo:tag)`` identifier strings along with the
    fully-qualified tool binding values to write into ``ocx.toml``.

    Returns ``(repo_a, tag_a, repo_b, tag_b)``. Callers interpolate
    ``{ocx.registry}/{repo}:{tag}`` as the TOML value — always the full
    form because Phase 2 requires ``registry/repo:tag`` in ``ocx.toml``.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_tool_a"
    repo_b = f"t_{short}_tool_b"
    tag_a = "1.0.0"
    tag_b = "2.0.0"
    make_package(ocx, repo_a, tag_a, tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, tag_b, tmp_path, new=True, cascade=False)
    return repo_a, tag_a, repo_b, tag_b


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    """Write an ``ocx.toml`` into ``project_dir`` and return the path."""
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _read_lock_text(project_dir: Path) -> str:
    return (project_dir / "ocx.lock").read_text()


def _read_lock_bytes(project_dir: Path) -> bytes:
    return (project_dir / "ocx.lock").read_bytes()


# V2 lock shape: each ``[[tool]]`` carries a bare ``repository`` coordinate
# (no tag, no digest) and a ``[tool.platforms]`` table mapping a lossless
# platform key to a per-platform leaf digest. The outer image-index digest is
# NOT stored (ADR per-platform leaf pinning). There is no ``pinned`` line.
_REPOSITORY_RE = re.compile(r'repository\s*=\s*"([^"@:]+(?::\d+)?/[^"@:]+)"')
_LEAF_RE = re.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def _repository_values(lock_text: str) -> list[str]:
    """Return ``[registry/repo, ...]`` for every locked tool's ``repository``."""
    return _REPOSITORY_RE.findall(lock_text)


def _leaf_digests(lock_text: str) -> list[str]:
    """Return every per-platform leaf digest hex recorded under
    ``[tool.platforms]``."""
    return _LEAF_RE.findall(lock_text)


def _tool_leaf_digests(lock_text: str, name: str) -> list[str]:
    """Return the sorted per-platform leaf digests for the ``[[tool]]`` entry
    whose ``name`` is ``name``.

    Slices from the tool's ``name = "<name>"`` line to the next ``[[tool]]``
    boundary (or end of file) and collects every leaf digest in that tool's
    ``[tool.platforms]`` table. A V2 entry has no single ``pinned`` digest —
    "did this tool's content change?" is answered by its leaf set.
    """
    start = lock_text.index(f'name = "{name}"')
    rest = lock_text[start:]
    next_tool = rest.find("[[tool]]", len("[[tool]]"))
    slice_text = rest if next_tool == -1 else rest[:next_tool]
    return sorted(_LEAF_RE.findall(slice_text))


# ---------------------------------------------------------------------------
# 1. Happy path — two tools → well-shaped lock
# ---------------------------------------------------------------------------


def test_lock_two_tools_produces_valid_lock_file(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` against a fixture registry with two tools → produces
    a correctly shaped V2 ``ocx.lock`` (``[[tool]]`` entries with a bare
    ``repository`` + ``[tool.platforms]`` leaf map, sorted by ``(group,
    name)``, valid declaration hash, ``lock_version = 2``).
    """
    repo_a, tag_a, repo_b, tag_b = _two_pushed_tools(ocx, tmp_path)
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    result = _run_lock(ocx, project)

    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock was not written"

    lock_text = _read_lock_text(project)

    # Metadata header: required fields present. The writer only emits V2.
    assert "lock_version = 2" in lock_text, "lock_version = 2 missing"
    assert "declaration_hash_version = 1" in lock_text, (
        "declaration_hash_version = 1 missing"
    )
    assert re.search(r'declaration_hash\s*=\s*"sha256:[0-9a-f]{64}"', lock_text), (
        "declaration_hash must be sha256:<64-hex>"
    )
    assert re.search(r'generated_by\s*=\s*"ocx\s+[^"]+"', lock_text), (
        "generated_by must be 'ocx <version>'"
    )
    assert re.search(r'generated_at\s*=\s*"[0-9T:\-+.Z]+"', lock_text), (
        "generated_at must be an RFC3339 timestamp"
    )

    # Exactly two tool entries, sorted by (group, name) → alpha before beta.
    repositories = _repository_values(lock_text)
    assert len(repositories) == 2, f"expected 2 tool entries, got {len(repositories)}"
    alpha_idx = lock_text.index('name = "alpha"')
    beta_idx = lock_text.index('name = "beta"')
    assert alpha_idx < beta_idx, "entries must be sorted by (group, name)"

    # Each entry is the V2 shape: name, group, a BARE repository (no tag, no
    # digest), and a [tool.platforms] table with at least one leaf digest.
    for name, repo in [("alpha", repo_a), ("beta", repo_b)]:
        entry = re.search(
            r'\[\[tool\]\]\s*\n'
            rf'name\s*=\s*"{re.escape(name)}"\s*\n'
            r'group\s*=\s*"default"\s*\n'
            rf'repository\s*=\s*"{re.escape(ocx.registry + "/" + repo)}"\s*\n',
            lock_text,
        )
        assert entry is not None, (
            f"missing or malformed V2 [[tool]] entry for {name}; full lock:\n{lock_text}"
        )
        # The repository coordinate must be bare — no tag, no embedded digest.
        assert f'repository = "{ocx.registry}/{repo}@' not in lock_text, (
            f"V2 repository for {name} must not carry a digest"
        )
        assert f'repository = "{ocx.registry}/{repo}:' not in lock_text, (
            f"V2 repository for {name} must not carry a tag"
        )

    # The platforms map must carry at least one leaf digest per tool, and the
    # legacy ``pinned`` index-digest line must be absent everywhere.
    assert "[tool.platforms]" in lock_text, "V2 lock must carry a [tool.platforms] table"
    assert len(_leaf_digests(lock_text)) >= 2, (
        f"expected at least one leaf digest per tool; full lock:\n{lock_text}"
    )
    assert "pinned =" not in lock_text, "V2 lock must NOT carry a legacy `pinned` line"


# ---------------------------------------------------------------------------
# 2. Idempotent — byte-identical on rerun
# ---------------------------------------------------------------------------


def test_lock_idempotent_byte_identical_on_rerun(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Second ``ocx lock`` run on unchanged ``ocx.toml`` → byte-identical
    output (``generated_at`` preserved via ``PinnedIdentifier::eq_content``).
    """
    repo_a, tag_a, repo_b, tag_b = _two_pushed_tools(ocx, tmp_path)
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:{tag_a}"
b = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    first = _run_lock(ocx, project)
    assert first.returncode == EXIT_SUCCESS, (
        f"first lock failed: rc={first.returncode}\nstderr:\n{first.stderr}"
    )
    first_bytes = _read_lock_bytes(project)

    second = _run_lock(ocx, project)
    assert second.returncode == EXIT_SUCCESS, (
        f"second lock failed: rc={second.returncode}\nstderr:\n{second.stderr}"
    )
    second_bytes = _read_lock_bytes(project)

    assert first_bytes == second_bytes, (
        "ocx.lock must be byte-identical across idempotent runs"
    )


def test_lock_clean_does_not_bump_moved_tag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` on a CLEAN lock (``ocx.toml`` byte-identical, its
    ``declaration_hash`` already matching the lock) leaves the pin UNCHANGED
    even when the upstream advisory tag has moved.

    A clean ``ocx lock`` is a reconcile, not a bump: it must carry the
    predecessor forward verbatim and produce a byte-identical lock. The
    lock-vs-upgrade distinction is the whole point — advancing a moved tag on
    a clean lock collapses ``ocx lock`` into ``ocx upgrade``. Use ``ocx
    upgrade`` to force-advance.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_clean_noop"
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
    initial_bytes = _read_lock_bytes(project)
    initial_leaves = _tool_leaf_digests(_read_lock_text(project), "mover")
    assert initial_leaves, "mover must record leaf digests"

    # Advance the moving tag upstream (cascade re-points ``:latest``). The
    # ``ocx.toml`` text is unchanged, so the lock is still CLEAN (its stored
    # declaration_hash still matches the config).
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True)
    refresh = subprocess.run(
        _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    # Clean reconcile: bare `ocx lock` must NOT bump the moved tag.
    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"clean ocx lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_bytes = _read_lock_bytes(project)
    after_leaves = _tool_leaf_digests(_read_lock_text(project), "mover")
    assert after_leaves == initial_leaves, (
        "a clean ocx lock must NOT re-resolve the moved tag (pin preserved); "
        "use `ocx upgrade` to force-advance"
    )
    assert after_bytes == initial_bytes, (
        "a clean ocx lock must produce a byte-identical lock (idempotent no-op)"
    )


def test_lock_dirty_reresolves_all_declared_tags(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A whole-file ``ocx lock`` reconcile on a DIRTY lock (``ocx.toml``
    changed since the last lock, so its ``declaration_hash`` no longer
    matches) re-resolves every declared tag. Here a second tool is appended
    to ``ocx.toml`` between the two lock runs: the config genuinely changed,
    so the whole file is reconciled and the pre-existing tool is re-resolved
    along with the new one.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_dirty_a"
    repo_b = f"t_{short}_dirty_b"
    # `a` cascades so its `:latest` is a real moving tag we can advance.
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=True)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:latest"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_a = _tool_leaf_digests(_read_lock_text(project), "a")
    assert initial_a, "tool 'a' must record leaf digests"

    # Advance `a`'s moving tag upstream, AND change `ocx.toml` by appending a
    # second tool. The config change makes the lock DIRTY (declaration_hash
    # mismatch), so a whole-file reconcile re-resolves every declared tag —
    # including the moved `a`.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    for repo in (repo_a, repo_b):
        refresh = subprocess.run(
            _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
            cwd=project,
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert refresh.returncode == EXIT_SUCCESS, refresh.stderr
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:latest"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    # Dirty whole-file reconcile: bare `ocx lock` re-resolves the moved tag
    # for `a` (the config changed, so the bump is the intended whole-file
    # behaviour) and locks the new tool `b`.
    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"dirty ocx lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_a = _tool_leaf_digests(_read_lock_text(project), "a")
    after_b = _tool_leaf_digests(_read_lock_text(project), "b")
    assert after_a != initial_a, (
        "a dirty whole-file lock must re-resolve the moved tag for 'a' "
        "(leaves advance)"
    )
    assert after_b, "the newly declared tool 'b' must be locked"


# ---------------------------------------------------------------------------
# 3. Tag change rewrites only that entry
# ---------------------------------------------------------------------------


def test_lock_tag_change_rewrites_only_that_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Change a tool's tag in ``ocx.toml``, re-run → only that tool's
    per-platform leaf digests change; ``generated_at`` and
    ``declaration_hash`` both update.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_swap_a"
    repo_b = f"t_{short}_swap_b"

    # Tool 'a': two different tags (distinct digests).
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=False)
    # Tool 'b': single tag, unchanging.
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    first = _run_lock(ocx, project)
    assert first.returncode == EXIT_SUCCESS, first.stderr
    first_text = _read_lock_text(project)
    first_hash = re.search(
        r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', first_text
    ).group(1)
    first_generated_at = re.search(
        r'generated_at\s*=\s*"([^"]+)"', first_text
    ).group(1)
    first_leaves_a = _tool_leaf_digests(first_text, "a")
    first_leaves_b = _tool_leaf_digests(first_text, "b")
    assert first_leaves_a, "tool 'a' must record at least one leaf digest"
    assert first_leaves_b, "tool 'b' must record at least one leaf digest"

    # Swap 'a' tag 1.0.0 → 2.0.0.
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:2.0.0"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )
    second = _run_lock(ocx, project)
    assert second.returncode == EXIT_SUCCESS, second.stderr
    second_text = _read_lock_text(project)
    second_hash = re.search(
        r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', second_text
    ).group(1)
    second_generated_at = re.search(
        r'generated_at\s*=\s*"([^"]+)"', second_text
    ).group(1)
    second_leaves_a = _tool_leaf_digests(second_text, "a")
    second_leaves_b = _tool_leaf_digests(second_text, "b")

    assert second_leaves_a != first_leaves_a, (
        "'a' leaf digests must change when its tag changes"
    )
    assert second_leaves_b == first_leaves_b, (
        "'b' leaf digests must be unchanged (its tag did not change)"
    )
    assert second_hash != first_hash, (
        "declaration_hash must update when ocx.toml changes"
    )
    assert second_generated_at != first_generated_at, (
        "generated_at must update when any tool's digest changes"
    )


# ---------------------------------------------------------------------------
# 4. Advisory tag change, same digest → generated_at preserved
# ---------------------------------------------------------------------------


def test_lock_advisory_tag_change_but_same_digest_preserves_generated_at(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Change the advisory tag but keep the digest the same (two tags
    pointing at the same manifest) → ``generated_at`` preserved via
    ``PinnedIdentifier::eq_content`` semantics.

    We publish a single package with ``cascade=True`` so ``1.0.0`` and
    ``1`` both resolve to the same image-index digest. Swapping the tag
    in ``ocx.toml`` between those two keeps the resolved digest
    identical.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_cascade"
    # Cascade pushes 1.0.0 and also tags 1, 1.0, latest at the same digest.
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )

    first = _run_lock(ocx, project)
    assert first.returncode == EXIT_SUCCESS, first.stderr
    first_text = _read_lock_text(project)
    first_generated_at = re.search(
        r'generated_at\s*=\s*"([^"]+)"', first_text
    ).group(1)
    first_leaves = _tool_leaf_digests(first_text, "tool")

    # Change advisory tag 1.0.0 → 1 (different tag, identical digest).
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1"
""",
    )
    second = _run_lock(ocx, project)
    assert second.returncode == EXIT_SUCCESS, second.stderr
    second_text = _read_lock_text(project)
    second_generated_at = re.search(
        r'generated_at\s*=\s*"([^"]+)"', second_text
    ).group(1)
    second_leaves = _tool_leaf_digests(second_text, "tool")

    assert first_leaves == second_leaves, (
        "leaf digests must be identical when tags point to the same manifest"
    )
    assert first_generated_at == second_generated_at, (
        "generated_at must be preserved when resolved content is unchanged"
    )


# ---------------------------------------------------------------------------
# 5. Non-existent tag → exit 79, transactional (no file written)
# ---------------------------------------------------------------------------


def test_lock_nonexistent_tag_exits_79_no_file_written(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` with a non-existent tag → exit 79 (NotFound);
    ``ocx.lock`` is **not** written (fully transactional).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_missing"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:does-not-exist"
""",
    )

    result = _run_lock(ocx, project)

    assert result.returncode == EXIT_NOT_FOUND, (
        f"expected exit {EXIT_NOT_FOUND} for non-existent tag; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx.lock must NOT be written when any tool fails to resolve"
    )


# ---------------------------------------------------------------------------
# 6. No ocx.toml → exit 64
# ---------------------------------------------------------------------------


def test_lock_no_toml_exits_64_usage_error(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` in a directory without ``ocx.toml`` (and no home
    fallback) → exit 64 with a ``no ocx.toml found`` message.

    The runner's ``OCX_HOME`` fixture is a fresh tmp dir, so the home
    fallback is guaranteed empty.
    """
    empty = tmp_path / "empty"
    empty.mkdir()

    result = _run_lock(ocx, empty)

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for missing ocx.toml; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    # Substring match, case-insensitive. The exact wording is the plan's
    # "no ocx.toml found" phrase but tolerant of surrounding context.
    assert "ocx.toml" in result.stderr.lower(), (
        f"stderr must mention ocx.toml; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 7. The subset surface is gone — --group / -g / --upgrade → exit 64
# ---------------------------------------------------------------------------


def test_lock_rejects_group_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --group ci`` is rejected by clap (exit 64): ``lock`` is a
    whole-file reconcile, never a subset. ``-g`` is also rejected. The
    existing ``ocx.lock`` (if any) is left untouched.

    Groups are a composition concern only, never a lock scope.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lock_grpflag"
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
        result = _run_lock(ocx, project, flag, "ci")
        assert result.returncode == EXIT_USAGE, (
            f"`ocx lock {flag} ci` must be rejected (exit {EXIT_USAGE}); "
            f"got {result.returncode}\nstderr:\n{result.stderr}"
        )
        assert not (project / "ocx.lock").exists(), (
            "ocx.lock must not be written when a dropped flag is rejected"
        )


def test_lock_rejects_upgrade_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --upgrade`` is rejected by clap (exit 64): the migration
    flag was folded — V1 → V2 migration is automatic on any write. Use
    ``ocx upgrade`` to force a re-resolve of every tag.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_lock_upgflag"
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

    result = _run_lock(ocx, project, "--upgrade")
    assert result.returncode == EXIT_USAGE, (
        f"`ocx lock --upgrade` must be rejected (exit {EXIT_USAGE}); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# 10. Corrupt existing ocx.lock → exit 78
# ---------------------------------------------------------------------------


def test_lock_corrupt_existing_lock_exits_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` when the existing ``ocx.lock`` is corrupt/unreadable
    → exit 78 (ConfigError); the lock file is **not** replaced.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_corrupt"
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

    # Write a corrupt lock: not valid TOML, also schema-invalid.
    corrupt_body = "!! not toml at all !!\n<<<<<>>>>> invalid\n"
    (project / "ocx.lock").write_text(corrupt_body)

    result = _run_lock(ocx, project)

    assert result.returncode == EXIT_CONFIG, (
        f"expected exit {EXIT_CONFIG} for corrupt ocx.lock; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    # Lock file must not be replaced — read back and confirm bytes.
    assert (project / "ocx.lock").read_text() == corrupt_body, (
        "corrupt ocx.lock must be preserved (not replaced) when parse fails"
    )


# ---------------------------------------------------------------------------
# 11. .gitattributes note — emitted when line missing, suppressed when present
# ---------------------------------------------------------------------------


def test_lock_gitattributes_note_emitted_when_line_missing(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``.gitattributes`` note on stderr:
       - appears when ``ocx.lock merge=union`` is absent (no file, or
         file present but line missing),
       - does NOT appear when the line is present.

    Each run is independent: the note is non-fatal and emitted every
    run without the line (plan line 599 clarifies it is NOT one-shot).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_gitattr"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    # --- Case A: no .gitattributes file at all → note emitted -----------
    proj_a = tmp_path / "proj_a"
    proj_a.mkdir()
    _write_ocx_toml(
        proj_a,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    a = _run_lock(ocx, proj_a)
    assert a.returncode == EXIT_SUCCESS, a.stderr
    assert "ocx.lock merge=union" in a.stderr, (
        "note must appear when .gitattributes is missing; "
        f"stderr:\n{a.stderr}"
    )

    # --- Case B: .gitattributes present but line absent → note emitted --
    proj_b = tmp_path / "proj_b"
    proj_b.mkdir()
    _write_ocx_toml(
        proj_b,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    (proj_b / ".gitattributes").write_text("*.rs diff=rust\n")
    b = _run_lock(ocx, proj_b)
    assert b.returncode == EXIT_SUCCESS, b.stderr
    assert "ocx.lock merge=union" in b.stderr, (
        "note must appear when .gitattributes exists but lacks the line; "
        f"stderr:\n{b.stderr}"
    )

    # --- Case C: .gitattributes contains the line → note suppressed -----
    proj_c = tmp_path / "proj_c"
    proj_c.mkdir()
    _write_ocx_toml(
        proj_c,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )
    (proj_c / ".gitattributes").write_text(
        "*.rs diff=rust\nocx.lock merge=union\n"
    )
    c = _run_lock(ocx, proj_c)
    assert c.returncode == EXIT_SUCCESS, c.stderr
    assert "ocx.lock merge=union" not in c.stderr, (
        "note must be suppressed when the line is already present; "
        f"stderr:\n{c.stderr}"
    )


# ---------------------------------------------------------------------------
# W16-1. --project flag is respected (context plumbing)
# ---------------------------------------------------------------------------


def test_lock_project_flag_uses_explicit_config_not_cwd(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --project /explicit/path/ocx.toml lock`` must use the config at
    the explicit path, write ``ocx.lock`` next to it, and NOT write anything
    next to the CWD-discovered config.

    Traces W16 (Context plumbing): the ``--project`` flag must flow all the
    way from ``ContextOptions`` through ``Context::project_path()`` into
    ``ProjectConfig::resolve``.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_proj_a"
    repo_b = f"t_{short}_proj_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    # CWD project: contains tool_a.
    cwd_proj = tmp_path / "default"
    cwd_proj.mkdir()
    _write_ocx_toml(
        cwd_proj,
        f"""\
[tools]
tool_a = "{ocx.registry}/{repo_a}:1.0.0"
""",
    )

    # Explicit project: contains tool_b.
    explicit_proj = tmp_path / "explicit"
    explicit_proj.mkdir()
    explicit_toml = _write_ocx_toml(
        explicit_proj,
        f"""\
[tools]
tool_b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    result = _run_lock_with_project(ocx, cwd_proj, explicit_toml)

    assert result.returncode == EXIT_SUCCESS, (
        f"--project lock must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # Lock must be written beside the explicit config.
    assert (explicit_proj / "ocx.lock").is_file(), (
        "ocx.lock must be written next to the explicit --project path"
    )
    explicit_lock = _read_lock_text(explicit_proj)
    assert 'name = "tool_b"' in explicit_lock, (
        "explicit project's tool must be in the lock file"
    )
    assert 'name = "tool_a"' not in explicit_lock, (
        "CWD project's tool must NOT be in the explicit lock file"
    )

    # No lock must be written in the CWD project directory.
    assert not (cwd_proj / "ocx.lock").exists(), (
        "ocx.lock must NOT be written in the CWD project directory"
    )


# ---------------------------------------------------------------------------
# W16-2. Whole-file lock resolves the root [tools] table AND every group
# ---------------------------------------------------------------------------


def test_lock_resolves_root_and_all_groups(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` (whole-file) resolves both the root ``[tools]`` table AND
    every ``[group.*]`` table, producing a merged lockfile. There is no
    subset surface — every declared binding is locked.
    """
    short = uuid4().hex[:8]
    repo_cmake = f"t_{short}_cmake"
    repo_ruff = f"t_{short}_ruff"
    make_package(ocx, repo_cmake, "3.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_ruff, "0.11.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
cmake = "{ocx.registry}/{repo_cmake}:3.0.0"

[group.dev]
ruff = "{ocx.registry}/{repo_ruff}:0.11.0"
""",
    )

    # Whole-file `ocx lock` → both root [tools] and [group.dev] resolved.
    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"whole-file lock must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    lock_text = _read_lock_text(project)
    assert 'name = "cmake"' in lock_text, (
        "root [tools] cmake must be present in the whole-file lock"
    )
    assert 'name = "ruff"' in lock_text, (
        "[group.dev] ruff must be present in the whole-file lock"
    )


# ---------------------------------------------------------------------------
# W16-3. Same binding name across groups: both group entries lock independently.
# ---------------------------------------------------------------------------


def test_lock_same_name_across_groups_locks_both_entries(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The same binding name in ``[tools]`` and ``[group.dev]`` with identical
    identifier content is not a parse-time error; whole-file ``ocx lock``
    succeeds and records a ``(group, name)`` entry for each.

    Identity in ``ocx.toml`` is now ``(group, name)`` — see plan D1.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_same"
    make_package(ocx, repo, "3.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
cmake = "{ocx.registry}/{repo}:3.0.0"

[group.dev]
cmake = "{ocx.registry}/{repo}:3.0.0"
""",
    )

    result = _run_lock(ocx, project)

    assert result.returncode == EXIT_SUCCESS, (
        f"same name across groups with identical content must succeed; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").exists(), "ocx.lock must be written"


# Compose-time conflict (different content across groups when both selected) is
# unit-tested in `crates/ocx_lib/src/project/compose.rs::tests::
# compose_errors_on_duplicate_binding_across_groups_with_different_content`.
# Acceptance coverage will land alongside the CLI command that calls
# `compose_tool_set` (env/exec); none of the current commands do.


# ---------------------------------------------------------------------------
# Cluster D.3 — `ocx lock --check` (no-op verification)
# ---------------------------------------------------------------------------


def test_lock_check_succeeds_on_current(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx lock --check`` against an `ocx.toml` whose `ocx.lock` is current
    (declaration_hash matches) exits 0 without writing anything.

    Cluster D.3 — CI primitive for "is the lock committed and current?"
    verification. No re-resolution, no writes.
    """
    repo_a, tag_a, repo_b, tag_b = _two_pushed_tools(ocx, tmp_path)
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    # First write the lock. Then snapshot its bytes; --check must not change them.
    write = _run_lock(ocx, project)
    assert write.returncode == EXIT_SUCCESS, write.stderr
    before = _read_lock_bytes(project)

    check = _run_lock(ocx, project, "--check")
    assert check.returncode == EXIT_SUCCESS, (
        f"ocx lock --check on a current lock must exit 0; "
        f"rc={check.returncode}\nstderr:\n{check.stderr}"
    )
    after = _read_lock_bytes(project)
    assert before == after, "ocx lock --check must NOT modify ocx.lock"


def test_lock_check_exits_65_on_drift(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx lock --check`` after `ocx.toml` mutates without `ocx lock`
    exits 65 (DataError) and does not write to the lock file.

    Cluster D.3 — drift signal for CI: catches "developer edited
    ocx.toml but forgot to commit ocx.lock".
    """
    repo_a, tag_a, repo_b, tag_b = _two_pushed_tools(ocx, tmp_path)
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
""",
    )
    write = _run_lock(ocx, project)
    assert write.returncode == EXIT_SUCCESS, write.stderr
    before = _read_lock_bytes(project)

    # Drift the manifest WITHOUT re-running `ocx lock`.
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
beta = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    check = _run_lock(ocx, project, "--check")
    assert check.returncode == 65, (
        f"ocx lock --check on a stale lock must exit 65 (DataError); "
        f"rc={check.returncode}\nstderr:\n{check.stderr}"
    )
    after = _read_lock_bytes(project)
    assert before == after, "ocx lock --check must NOT mutate ocx.lock on drift"


def test_lock_check_exits_78_on_missing_lock(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx lock --check`` when `ocx.lock` is absent exits 78 (ConfigError)
    and produces no lock file.

    Cluster D.3 — "lock missing" gate matches `ocx pull`/`ocx run`
    behaviour: `--check` is a verifier, not a creator.
    """
    repo_a, tag_a, _, _ = _two_pushed_tools(ocx, tmp_path)
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
alpha = "{ocx.registry}/{repo_a}:{tag_a}"
""",
    )

    check = _run_lock(ocx, project, "--check")
    assert check.returncode == 78, (
        f"ocx lock --check with no lock present must exit 78 (ConfigError); "
        f"rc={check.returncode}\nstderr:\n{check.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx lock --check must not create ocx.lock"
    )


# ---------------------------------------------------------------------------
# Eager materialization — Phase-5 contracts
# ---------------------------------------------------------------------------


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> "Path":
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
    ``$OCX_HOME/packages/{registry_dir}/`` — the eager-vs-lazy observable for
    toolchain mutators under the no-symlink model
    (``project_context.rs::materialize_lock`` calls ``pull_all``).
    """
    base = Path(ocx.ocx_home) / "packages" / registry_dir(ocx.registry)
    if not base.exists():
        return 0
    return sum(1 for p in base.rglob("content") if p.is_dir())


def _single_tool_project(
    ocx: OcxRunner, tmp_path: "Path"
) -> tuple["Path", str, str]:
    """Publish one tool and return ``(project_dir, repo, tag)``.

    The project directory contains only ``ocx.toml``; NO ``ocx.lock`` yet.
    We deliberately use `ocx lock` to create the lock rather than `ocx add`
    so the test setup is independent of `add`'s eager-default.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_eager"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag}"
""",
    )
    return project, repo, tag


def test_lock_eager_default_warms_object_store_without_symlinks(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """REGRESSION GUARD: ``ocx lock`` (no flags) pre-warms the object store
    after writing the lock, but creates **no** candidate or `current`
    symlinks.

    Locks in the new toolchain-mutator invariant:
    ``project_context.rs::materialize_lock`` calls ``pull_all``, never
    ``install_all``. Project-tier resolution walks ``ocx.lock``, so a
    candidate symlink here would only be a second, redundant GC root that
    breaks ``ocx clean --force`` (see test_clean_project_backlinks).
    """
    project, repo, tag = _single_tool_project(ocx, tmp_path)

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx lock must pre-warm the object store"
    )
    candidate = _candidate_path(ocx, repo, tag)
    assert_not_exists(candidate)


def test_lock_no_pull_skips_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --no-pull`` writes the lock and leaves the object store
    cold. No candidate symlink under either eager or lazy paths anymore;
    cold-store is the only eager-vs-lazy observable.

    Plan Phase-5 Step 3.2 contract.
    """
    project, repo, tag = _single_tool_project(ocx, tmp_path)

    result = _run_lock(ocx, project, "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written even with --no-pull"

    assert _packages_present_count(ocx) == 0, (
        "ocx lock --no-pull must not warm the object store"
    )
    candidate = _candidate_path(ocx, repo, tag)
    assert_not_exists(candidate)


def test_lock_pull_then_no_pull_last_wins_no_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --pull --no-pull`` → ``--no-pull`` wins (POSIX last-wins);
    object store stays cold, no candidate symlink.

    Plan Phase-5 Step 3.2 last-wins contract.
    """
    project, repo, tag = _single_tool_project(ocx, tmp_path)

    result = _run_lock(ocx, project, "--pull", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock --pull --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) == 0, (
        "--no-pull must win: object store stays cold"
    )
    candidate = _candidate_path(ocx, repo, tag)
    assert_not_exists(candidate)


def test_lock_no_pull_then_pull_last_wins_installs(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --no-pull --pull`` → ``--pull`` wins (POSIX last-wins);
    object store warms, candidate symlink remains absent.

    Plan Phase-5 Step 3.2 last-wins contract.
    """
    project, repo, tag = _single_tool_project(ocx, tmp_path)

    result = _run_lock(ocx, project, "--no-pull", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock --no-pull --pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) >= 1, (
        "--pull must win: object store warms"
    )
    candidate = _candidate_path(ocx, repo, tag)
    assert_not_exists(candidate)


# ---------------------------------------------------------------------------
# V1-read → V2-write forward migration (ADR: read both, write only V2)
# ---------------------------------------------------------------------------


def test_lock_rewrites_committed_v1_lock_to_v2(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A committed **V1** lock (``lock_version = 1`` with a single ``pinned``
    index digest per tool) is rewritten as **V2** the moment any write
    command touches it: ``ocx lock`` re-resolves and emits the V2 shape
    (bare ``repository`` + ``[tool.platforms]`` leaf map, ``lock_version =
    2``). No code path emits V1 (ADR: read both V1/V2, write only V2).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_v1mig"
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

    # Hand-author a legacy V1 lock with a single `pinned` index digest. The
    # digest does not need to be the live one — `ocx lock` re-resolves the
    # `ocx.toml` tag and overwrites it with the V2 shape.
    fake_index_digest = "0" * 64
    (project / "ocx.lock").write_text(
        f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{"d" * 64}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "tool"
group = "default"
pinned = "{ocx.registry}/{repo}@sha256:{fake_index_digest}"
"""
    )

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx lock on a V1 lock failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_text = _read_lock_text(project)
    assert "lock_version = 2" in lock_text, "the rewritten lock must be V2"
    assert "lock_version = 1" not in lock_text, "no V1 marker may survive the rewrite"
    assert "pinned =" not in lock_text, "the rewritten V2 lock must not carry a `pinned` line"
    assert "[tool.platforms]" in lock_text, "the rewritten lock must carry a [tool.platforms] table"
    assert f'repository = "{ocx.registry}/{repo}"' in lock_text, (
        "the rewritten lock must record the bare repository coordinate"
    )


# ---------------------------------------------------------------------------
# V1 → V2 migration is automatic on any write (no `--upgrade` verb)
# ---------------------------------------------------------------------------


def test_lock_auto_migrates_cached_v1_to_v2(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bare ``ocx lock`` against a committed V1 lock whose index blobs are
    cached locally rewrites it to V2 automatically. Migration-by-read happens
    on any write — there is no separate ``--upgrade`` verb.

    Setup: push a package → ``ocx lock`` to write a V2 lock and warm the
    object store → hand-author a V1 lock pointing at the live (cached) index
    digest → run bare ``ocx lock`` → assert exit 0, V2 shape, ``[tool.platforms]``
    present.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_automig"
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

    # Write the V2 lock (and warm the object store / blob cache).
    warm = _run_lock(ocx, project)
    assert warm.returncode == EXIT_SUCCESS, (
        f"setup lock failed: rc={warm.returncode}\nstderr:\n{warm.stderr}"
    )
    v2_text = _read_lock_text(project)

    # Extract the declaration_hash and one leaf digest so we can build a
    # syntactically-valid V1 lock that points at the cached index.
    decl_match = re.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_text)
    leaf_match = _LEAF_RE.search(v2_text)
    repo_match = _REPOSITORY_RE.search(v2_text)
    assert decl_match and leaf_match and repo_match, (
        f"V2 lock must carry declaration_hash + leaf + repository; got:\n{v2_text[:400]}"
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)
    decl_hash = decl_match.group(1)

    # Overwrite with a hand-authored V1 lock using the live leaf digest as
    # the `pinned` index digest.  Because ``ocx lock`` cached the blobs, the
    # automatic migration transcribes from cache without a network round-trip.
    (project / "ocx.lock").write_text(
        f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "tool"
group = "default"
pinned = "{bare_repo}@sha256:{leaf_hex}"
"""
    )

    result = _run_lock(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"bare ocx lock on a cached V1 lock must exit 0; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    upgraded = _read_lock_text(project)
    assert "lock_version = 2" in upgraded, (
        "migration must produce a V2 lock"
    )
    assert "pinned =" not in upgraded, (
        "migration must not carry a legacy `pinned` line"
    )
    assert "[tool.platforms]" in upgraded, (
        "migration must emit a [tool.platforms] table"
    )
    assert f'repository = "{ocx.registry}/{repo}"' in upgraded, (
        "migration must record the bare repository coordinate"
    )
    assert re.search(r'=\s*"sha256:[0-9a-f]{64}"', upgraded), (
        "migration must record at least one per-platform leaf digest"
    )


def test_fault_injected_add_rolls_back_committed_v1_lock_without_panic(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A fault-injected ``ocx add`` on a project carrying a committed **V1**
    lock must roll the lock back to its **byte-identical V1** predecessor —
    not panic in the V2 writer.

    Regression for F1: the rollback path used to re-``save`` the parsed
    predecessor through ``ProjectLock::to_toml_string`` → ``serialize_tool_views``,
    which hits ``unreachable!("LegacyIndex reached the V2 writer")`` for a V1
    (``LegacyIndex``) predecessor. The mutation guard now captures the
    predecessor lock's raw bytes at acquisition time and restores them verbatim,
    so a rolled-back mutation leaves a V1 lock exactly as it was (still V1).

    Setup mirrors ``test_lock_upgrade_pin_preserving_v1_to_v2``: publish a
    package, run ``ocx lock`` to warm the blob cache and learn the live index
    leaf, then overwrite ``ocx.lock`` with a hand-authored V1 lock pinned to
    that cached digest. Publishing a SECOND package and running a
    fault-injected ``ocx add`` (``OCX_TEST_FAULT=after_lock_write``) reaches the
    commit's post-lock-rename rollback boundary with a V1 predecessor in hand.
    """
    short = uuid4().hex[:8]
    repo_existing = f"t_{short}_v1pred"
    repo_added = f"t_{short}_v1add"
    make_package(ocx, repo_existing, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_added, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
existing = "{ocx.registry}/{repo_existing}:1.0.0"
""",
    )

    # Warm the object store / blob cache and learn the live index leaf so the
    # hand-authored V1 lock points at a cached (fetchable) index.
    warm = _run_lock(ocx, project)
    assert warm.returncode == EXIT_SUCCESS, (
        f"setup lock failed: rc={warm.returncode}\nstderr:\n{warm.stderr}"
    )
    v2_text = _read_lock_text(project)
    decl_match = re.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_text)
    leaf_match = _LEAF_RE.search(v2_text)
    repo_match = _REPOSITORY_RE.search(v2_text)
    assert decl_match and leaf_match and repo_match, (
        f"V2 lock must carry declaration_hash + leaf + repository; got:\n{v2_text[:400]}"
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)
    decl_hash = decl_match.group(1)

    # Overwrite with a hand-authored V1 lock pinned to the cached index digest.
    v1_lock_text = f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "existing"
group = "default"
pinned = "{bare_repo}@sha256:{leaf_hex}"
"""
    (project / "ocx.lock").write_text(v1_lock_text)
    v1_bytes_before = _read_lock_bytes(project)

    # Fault-injected add of the SECOND package: resolution succeeds (both
    # indexes are cached), the new lock is renamed into place, then the fault
    # fires before the manifest rewrite → the commit rolls the lock back to the
    # V1 predecessor. Old behaviour: panic in the V2 writer.
    cmd = [str(ocx.binary), "add", f"{ocx.registry}/{repo_added}:1.0.0"]
    failed = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_TEST_FAULT": "after_lock_write"},
    )

    assert failed.returncode != EXIT_SUCCESS, (
        f"fault-injected add must fail; got rc={failed.returncode}, "
        f"stderr={failed.stderr!r}"
    )
    # The fault path must not panic in the V2 writer.
    assert "unreachable" not in failed.stderr.lower(), (
        f"rollback must not hit the V2-writer unreachable!(); stderr:\n{failed.stderr}"
    )
    assert "panic" not in failed.stderr.lower(), (
        f"rollback must not panic; stderr:\n{failed.stderr}"
    )

    # The lock must be restored byte-for-byte to its V1 predecessor: still V1,
    # no orphaned binding in ocx.toml.
    v1_bytes_after = _read_lock_bytes(project)
    assert v1_bytes_after == v1_bytes_before, (
        "rollback must restore the committed V1 lock byte-for-byte; "
        f"before={v1_bytes_before!r}\nafter={v1_bytes_after!r}"
    )
    assert "lock_version = 1" in v1_bytes_after.decode(), (
        "the rolled-back lock must still be V1"
    )
    toml_after = (project / "ocx.toml").read_text()
    assert repo_added not in toml_after, (
        f"manifest must not contain the orphaned binding after rollback; got:\n{toml_after}"
    )


def test_lock_offline_uncached_dirty_exits_non_zero(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Bare ``ocx --offline lock`` against a dirty V1 lock whose index digest
    is NOT in the local cache exits non-zero without making any network call.

    Offline/frozen + uncached → ``PolicyResolutionBlocked`` (81) or
    ``TagNotFound`` (79); both prove no network call succeeded. The lock the
    fake declaration_hash makes dirty, so bare ``ocx lock`` re-resolves the
    declared tag — refused offline rather than silently fetched.

    Setup: hand-author an ``ocx.toml`` pointing at a fake registry path + a
    V1 lock with a stale declaration_hash and a fake (never-fetched) index
    digest.  No ``make_package`` or ``index update`` is called so OCX_HOME has
    no index data for this tool.

    We verify:
    - exit code classifies as a no-network refusal (79 or 81)
    - ``ocx.lock`` on disk is still V1 (no partial V2 was written)
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upggone"
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

    # Hand-author a V1 lock with a stale declaration_hash + fake index digest.
    fake_index_digest = "a" * 64
    v1_lock_text = f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{"d" * 64}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "tool"
group = "default"
pinned = "{fake_registry}/{repo}@sha256:{fake_index_digest}"
"""
    (project / "ocx.lock").write_text(v1_lock_text)

    cmd = [str(ocx.binary), "--offline", "lock"]
    result = subprocess.run(
        cmd,
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    # Pin the contract, not just "any error": offline + uncached must classify
    # as PolicyResolutionBlocked (81) or TagNotFound (79) — both prove no network
    # call succeeded.  A regression to generic Failure (1) or a 0 exit must fail
    # this test.
    assert result.returncode in (79, 81), (
        f"ocx --offline lock with uncached index must exit 79 or 81; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    # The lock on disk must still be V1 — no partial V2 was committed.
    lock_after = (project / "ocx.lock").read_text()
    assert "lock_version = 1" in lock_after, (
        "a failed lock must not rewrite the lock to V2; "
        f"lock after:\n{lock_after}"
    )


# ---------------------------------------------------------------------------
# Gap 2 — re-lock platform appears/disappears (ADR validation item d)
# ---------------------------------------------------------------------------


def test_relock_new_platform_lock_is_noop_upgrade_adds_key(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Pushing a second platform under the same tag is a moving-content change
    with a byte-identical ``ocx.toml``. A clean ``ocx lock`` must NOT pick it up
    (pin preserved); ``ocx upgrade`` does (whole-file forced bump adds the new
    platform key).

    ADR per-platform-lock-pinning validation item (d), revised for the
    lock-vs-upgrade distinction: a newly-shipped platform under an unchanged tag
    appears as an added key on ``ocx upgrade`` — not on a clean ``ocx lock``,
    which carries the existing pin forward verbatim.

    Setup: push platform A (``new=True``) → ``ocx lock --no-pull`` → record the
    set of leaf digests → push platform B under the same repo:tag
    (``new=False``) → re-index → assert a clean ``ocx lock --no-pull`` is a
    no-op (still one leaf) → ``ocx upgrade --no-pull`` → assert the lock now
    carries TWO platform keys and ``generated_at`` advanced (content changed).

    NOTE — ``test_lock_relock_dropped_platform_removes_key`` is NOT written
    here because the registry harness has no delete/untag capability: OCX's
    push pipeline calls ``retain(entry.platform != platform)`` on the existing
    index and re-points the tag, so platforms can only be *added* via the
    push API.  Simulating a drop would require the registry to expose a
    manifest-delete endpoint, which ``registry:2`` does not surface to OCX.
    The dropped-key signal is tested at the unit level by manipulating the
    in-memory platforms map directly.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_relock"
    tag = "1.0.0"

    # Push the first platform (linux/amd64 or whatever the host is).
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    first_lock = _run_lock(ocx, project, "--no-pull")
    assert first_lock.returncode == EXIT_SUCCESS, (
        f"first lock failed: rc={first_lock.returncode}\nstderr:\n{first_lock.stderr}"
    )
    first_text = _read_lock_text(project)
    first_leaves = set(_leaf_digests(first_text))
    first_at_match = re.search(r'generated_at\s*=\s*"([^"]+)"', first_text)
    assert first_at_match, "generated_at must be present in first lock"
    first_generated_at = first_at_match.group(1)
    assert len(first_leaves) == 1, (
        f"first push (one platform) must produce exactly one leaf; got {first_leaves}"
    )

    # Push a SECOND, distinct platform under the same repo:tag.  We use
    # ``linux/arm64`` as a portable non-host alternative; ``new=False`` tells
    # make_package to merge into the existing index rather than starting fresh.
    # A distinct ``tmp_path`` subdirectory is required so that make_package
    # does not try to re-create ``pkg-{repo}-{tag}/bin/`` which already
    # exists from the first call.
    second_platform = "linux/arm64"
    make_package(
        ocx, repo, tag, tmp_path / "second",
        new=False,
        cascade=False,
        platform=second_platform,
    )

    # Re-index so the local index sees the updated manifest.
    ocx.plain("index", "update", repo)

    # A clean `ocx lock` (ocx.toml unchanged → declaration_hash still matches)
    # must carry the existing pin forward verbatim — it must NOT pick up the
    # newly-shipped platform. That is the moving-content advance reserved for
    # `ocx upgrade`.
    clean_relock = _run_lock(ocx, project, "--no-pull")
    assert clean_relock.returncode == EXIT_SUCCESS, (
        f"clean relock failed: rc={clean_relock.returncode}\nstderr:\n{clean_relock.stderr}"
    )
    clean_leaves = set(_leaf_digests(_read_lock_text(project)))
    assert clean_leaves == first_leaves, (
        "a clean `ocx lock` must NOT pick up the newly-shipped platform "
        f"(pin preserved); got {clean_leaves}, expected {first_leaves}"
    )

    # `ocx upgrade` is the whole-file forced bump that re-resolves the tag and
    # picks up the new platform key.
    upgrade = subprocess.run(
        _ocx_cmd(ocx, "upgrade", "--no-pull"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert upgrade.returncode == EXIT_SUCCESS, (
        f"upgrade after adding platform failed: "
        f"rc={upgrade.returncode}\nstderr:\n{upgrade.stderr}"
    )
    second_text = _read_lock_text(project)
    second_leaves = set(_leaf_digests(second_text))

    assert len(second_leaves) == 2, (
        f"after adding {second_platform}, `ocx upgrade` must carry two platform "
        f"leaves; got {second_leaves}"
    )
    assert first_leaves.issubset(second_leaves), (
        "the original platform's leaf must be preserved when a new platform is added"
    )

    second_at_match = re.search(r'generated_at\s*=\s*"([^"]+)"', second_text)
    assert second_at_match, "generated_at must be present in the upgraded lock"
    second_generated_at = second_at_match.group(1)
    assert second_generated_at != first_generated_at, (
        "generated_at must advance when the platforms map content changes "
        f"(first={first_generated_at!r}, second={second_generated_at!r})"
    )
