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

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Exit code constants — align with crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64       # --group unknown / empty segment / no ocx.toml
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


# Matches ``pinned = "<registry>/<repo>@sha256:<hex>"`` — the advisory
# tag is stripped at write time, so the on-disk shape is canonical.
_PINNED_RE = re.compile(
    r'pinned\s*=\s*"([^"@]+)@sha256:([0-9a-f]{64})"'
)


def _pinned_values(lock_text: str) -> list[tuple[str, str]]:
    """Return ``[(registry/repo, digest_hex), ...]`` for every locked tool."""
    return [(m.group(1), m.group(2)) for m in _PINNED_RE.finditer(lock_text)]


# ---------------------------------------------------------------------------
# 1. Happy path — two tools → well-shaped lock
# ---------------------------------------------------------------------------


def test_lock_two_tools_produces_valid_lock_file(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` against a fixture registry with two tools → produces
    a correctly shaped ``ocx.lock`` (3-field ``[[tool]]`` entries,
    sorted by ``(group, name)``, valid declaration hash).
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

    # Metadata header: required fields present.
    assert "lock_version = 1" in lock_text, "lock_version = 1 missing"
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
    pinned = _pinned_values(lock_text)
    assert len(pinned) == 2, f"expected 2 tool entries, got {len(pinned)}"
    alpha_idx = lock_text.index('name = "alpha"')
    beta_idx = lock_text.index('name = "beta"')
    assert alpha_idx < beta_idx, "entries must be sorted by (group, name)"

    # Each entry is the 3-field shape (name, group, pinned).
    for name, repo in [("alpha", repo_a), ("beta", repo_b)]:
        entry = re.search(
            r'\[\[tool\]\]\s*\n'
            rf'name\s*=\s*"{re.escape(name)}"\s*\n'
            r'group\s*=\s*"default"\s*\n'
            rf'pinned\s*=\s*"{re.escape(ocx.registry + "/" + repo)}@sha256:[0-9a-f]{{64}}"\s*\n',
            lock_text,
        )
        assert entry is not None, (
            f"missing or malformed [[tool]] entry for {name}; full lock:\n{lock_text}"
        )


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


# ---------------------------------------------------------------------------
# 3. Tag change rewrites only that entry
# ---------------------------------------------------------------------------


def test_lock_tag_change_rewrites_only_that_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Change a tool's tag in ``ocx.toml``, re-run → only that tool's
    ``pinned`` field changes; ``generated_at`` and ``declaration_hash``
    both update.
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
    first_pinned_a = re.search(
        rf'name\s*=\s*"a".*?pinned\s*=\s*"{re.escape(ocx.registry + "/" + repo_a)}@(sha256:[0-9a-f]{{64}})"',
        first_text,
        re.DOTALL,
    ).group(1)
    first_pinned_b = re.search(
        rf'name\s*=\s*"b".*?pinned\s*=\s*"{re.escape(ocx.registry + "/" + repo_b)}@(sha256:[0-9a-f]{{64}})"',
        first_text,
        re.DOTALL,
    ).group(1)

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
    second_pinned_a = re.search(
        rf'name\s*=\s*"a".*?pinned\s*=\s*"{re.escape(ocx.registry + "/" + repo_a)}@(sha256:[0-9a-f]{{64}})"',
        second_text,
        re.DOTALL,
    ).group(1)
    second_pinned_b = re.search(
        rf'name\s*=\s*"b".*?pinned\s*=\s*"{re.escape(ocx.registry + "/" + repo_b)}@(sha256:[0-9a-f]{{64}})"',
        second_text,
        re.DOTALL,
    ).group(1)

    assert second_pinned_a != first_pinned_a, (
        "'a' digest must change when its tag changes"
    )
    assert second_pinned_b == first_pinned_b, (
        "'b' digest must be unchanged (its tag did not change)"
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
    first_pinned = re.search(
        r'pinned\s*=\s*"([^"]+)"', first_text
    ).group(1)

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
    second_pinned = re.search(
        r'pinned\s*=\s*"([^"]+)"', second_text
    ).group(1)

    assert first_pinned == second_pinned, (
        "pinned digest must be identical when tags point to the same manifest"
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
# 7. --group filter includes only the named group
# ---------------------------------------------------------------------------


def test_lock_group_filter_only_includes_selected_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --group ci`` → only ci-group tools in output; tools in
    ``[tools]`` and other groups are left out.
    """
    short = uuid4().hex[:8]
    repo_default = f"t_{short}_default"
    repo_ci = f"t_{short}_ci"
    repo_lint = f"t_{short}_lint"
    make_package(ocx, repo_default, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_ci, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_lint, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool_default = "{ocx.registry}/{repo_default}:1.0.0"

[group.ci]
tool_ci = "{ocx.registry}/{repo_ci}:1.0.0"

[group.lint]
tool_lint = "{ocx.registry}/{repo_lint}:1.0.0"
""",
    )

    result = _run_lock(ocx, project, "--group", "ci")

    assert result.returncode == EXIT_SUCCESS, (
        f"--group ci must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    lock_text = _read_lock_text(project)

    assert 'name = "tool_ci"' in lock_text, "ci-group tool must be present"
    assert 'name = "tool_default"' not in lock_text, (
        "[tools] entries must NOT be included when --group ci is passed"
    )
    assert 'name = "tool_lint"' not in lock_text, (
        "lint-group entries must NOT be included when --group ci is passed"
    )


# ---------------------------------------------------------------------------
# 8. --group unknown → exit 64
# ---------------------------------------------------------------------------


def test_lock_unknown_group_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock --group nonexistent`` → exit 64 (UsageError)."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_unknown_group"
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

    result = _run_lock(ocx, project, "--group", "nonexistent")

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for unknown group; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx.lock must not be written when --group validation fails"
    )


# ---------------------------------------------------------------------------
# 9. -g ci,,lint empty segment → exit 64
# ---------------------------------------------------------------------------


def test_lock_empty_group_segment_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock -g ci,,lint`` → exit 64 (empty segment).

    Companion to the Rust unit test
    ``group_flag_clap_parses_comma_values_and_empties`` which asserts
    that clap passes the empty string through at the parse layer. The
    runtime validator is responsible for rejecting it with exit 64.
    """
    short = uuid4().hex[:8]
    repo_ci = f"t_{short}_ci_seg"
    repo_lint = f"t_{short}_lint_seg"
    make_package(ocx, repo_ci, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_lint, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[group.ci]
a = "{ocx.registry}/{repo_ci}:1.0.0"

[group.lint]
b = "{ocx.registry}/{repo_lint}:1.0.0"
""",
    )

    result = _run_lock(ocx, project, "-g", "ci,,lint")

    assert result.returncode == EXIT_USAGE, (
        f"expected exit {EXIT_USAGE} for empty group segment; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx.lock must not be written when --group validation fails"
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
# W16-2. -g default combined with -g <named-group> resolves both
# ---------------------------------------------------------------------------


def test_lock_default_group_combined_with_named_group_resolves_both(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock -g default -g dev`` must resolve both the root ``[tools]``
    table AND the ``[group.dev]`` table, producing a merged lockfile.

    Control assertions:
    - ``ocx lock -g dev`` alone excludes root ``[tools]`` entries.
    - ``ocx lock`` (no ``-g``) resolves everything (root + all groups).
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

    # --- Combined: -g default -g dev → both tools present ---
    combined = _run_lock(ocx, project, "-g", "default", "-g", "dev")
    assert combined.returncode == EXIT_SUCCESS, (
        f"-g default -g dev must succeed; rc={combined.returncode}\nstderr:\n{combined.stderr}"
    )
    combined_text = _read_lock_text(project)
    assert 'name = "cmake"' in combined_text, (
        "root [tools] cmake must be in lockfile with -g default -g dev"
    )
    assert 'name = "ruff"' in combined_text, (
        "[group.dev] ruff must be in lockfile with -g default -g dev"
    )

    # Reset lock before next run.
    (project / "ocx.lock").unlink()

    # --- Control: -g dev alone → only dev group, no root tools ---
    dev_only = _run_lock(ocx, project, "-g", "dev")
    assert dev_only.returncode == EXIT_SUCCESS, (
        f"-g dev must succeed; rc={dev_only.returncode}\nstderr:\n{dev_only.stderr}"
    )
    dev_text = _read_lock_text(project)
    assert 'name = "ruff"' in dev_text, (
        "[group.dev] ruff must be in lockfile with -g dev"
    )
    assert 'name = "cmake"' not in dev_text, (
        "root [tools] cmake must NOT be in lockfile with -g dev only"
    )

    # Reset lock before next run.
    (project / "ocx.lock").unlink()

    # --- No -g flag → all groups resolved (root + dev) ---
    all_groups = _run_lock(ocx, project)
    assert all_groups.returncode == EXIT_SUCCESS, (
        f"lock with no -g must succeed; rc={all_groups.returncode}\nstderr:\n{all_groups.stderr}"
    )
    all_text = _read_lock_text(project)
    assert 'name = "cmake"' in all_text, (
        "root [tools] cmake must be present when no -g filter is given"
    )
    assert 'name = "ruff"' in all_text, (
        "[group.dev] ruff must be present when no -g filter is given"
    )


# ---------------------------------------------------------------------------
# W16-3. Duplicate tool across groups exits 78 (ConfigError), no lock written
# ---------------------------------------------------------------------------


def test_lock_duplicate_tool_across_groups_exits_78_no_lock_written(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Declaring the same tool name in ``[tools]`` and ``[group.dev]`` must
    be rejected with exit code 78 (ConfigError) and no ``ocx.lock`` written.

    ``ProjectErrorKind::DuplicateToolAcrossSections`` maps to
    ``ExitCode::ConfigError`` (78) in the error classifier.  The plan
    description said 64 (UsageError), but the implemented mapping is 78 —
    this test asserts the *actual* exit-code contract.

    Stderr must name the duplicated tool so users can diagnose the conflict.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_dup"
    make_package(ocx, repo, "3.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    # Same tool name "cmake" in root [tools] AND [group.dev].
    _write_ocx_toml(
        project,
        f"""\
[tools]
cmake = "{ocx.registry}/{repo}:3.0.0"

[group.dev]
cmake = "{ocx.registry}/{repo}:3.0.0"
""",
    )

    result = _run_lock(ocx, project, "-g", "default", "-g", "dev")

    assert result.returncode == EXIT_CONFIG, (
        f"duplicate tool must exit {EXIT_CONFIG} (ConfigError); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "cmake" in result.stderr, (
        f"stderr must name the duplicated tool 'cmake'; got:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx.lock must NOT be written when duplicate tool validation fails"
    )
