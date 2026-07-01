# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx remove`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx remove`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx remove")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §3.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists, assert_symlink_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


EXIT_SUCCESS = 0
# StaleLockOnPartial → DataError (65) per error.rs ClassifyExitCode
EXIT_DATA_ERROR = 65
# BindingNotFound → NotFound (79) per error.rs ClassifyExitCode
EXIT_NOT_FOUND = 79

_LEAF_RE_REM = re.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def _leaves_for_remove(lock_text: str, name: str) -> list[str]:
    """Return the sorted per-platform leaf digests for the ``[[tool]]`` entry
    whose ``name`` field equals ``name``; ``[]`` when no entry matches."""
    marker = f'name = "{name}"'
    if marker not in lock_text:
        return []
    start = lock_text.index(marker)
    rest = lock_text[start:]
    next_tool = rest.find("[[tool]]", len("[[tool]]"))
    slice_text = rest if next_tool == -1 else rest[:next_tool]
    return sorted(_LEAF_RE_REM.findall(slice_text))


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> None:
    (project_dir / "ocx.toml").write_text(body)


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def _setup_project_with_tool(
    ocx: OcxRunner, tmp_path: Path, repo: str, tag: str
) -> tuple[Path, Path]:
    """Create project dir + ocx.toml + lock + install for one tool.

    Returns (project_dir, candidate_symlink_path).
    """
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    _write_ocx_toml(
        project_dir,
        f'[tools]\n{repo} = "{ocx.registry}/{repo}:{tag}"\n',
    )
    # Establish lock + install baseline.
    lock_r = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"
    install_r = subprocess.run(
        [str(ocx.binary), "package", "install", f"{ocx.registry}/{repo}:{tag}"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert install_r.returncode == EXIT_SUCCESS, f"baseline install failed: {install_r.stderr}"
    return project_dir, _candidate_path(ocx, repo, tag)


def test_remove_drops_binding_and_uninstalls(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove <name>`` removes the binding from ``ocx.toml``, rewrites
    ``ocx.lock``, and uninstalls the package (candidate symlink absent).

    Spec: Unit 7 §3 bullet 1.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_rem_drop"
    project_dir, candidate = _setup_project_with_tool(ocx, tmp_path, repo, "1.0.0")

    assert_symlink_exists(candidate)  # sanity: installed before remove

    result = _run_cmd(ocx, project_dir, "remove", repo)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo not in toml_content, (
        f"binding for {repo!r} must be gone from ocx.toml after remove; got:\n{toml_content}"
    )

    lock_text = (project_dir / "ocx.lock").read_text()
    assert repo not in lock_text, (
        f"lock entry for {repo!r} must be removed from ocx.lock; got:\n{lock_text}"
    )

    assert_not_exists(candidate)


def test_remove_rejects_unknown_binding(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove nonexistent`` exits with NotFound (79) when the binding
    is not declared in any group. ``ocx.toml`` and ``ocx.lock`` stay unchanged.

    Spec: Unit 7 §3 bullet 2. Error variant: ``BindingNotFound`` →
    ``NotFound`` (79).
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "remove", "nonexistent")
    assert result.returncode == EXIT_NOT_FOUND, (
        f"ocx remove nonexistent should exit {EXIT_NOT_FOUND}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when remove target not found"
    )
    # Lock file must also remain unchanged (or absent if never created).
    lock_path = project_dir / "ocx.lock"
    if lock_path.exists():
        assert "nonexistent" not in lock_path.read_text()


def test_remove_from_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove <name>`` finds a binding in a named group (``[group.ci]``)
    and removes it — no explicit ``--group`` flag required.

    Spec: Unit 7 §3 bullet 3. ``mutate::remove_binding`` searches both
    default and named groups.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_rem_grp"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n\n[group.ci]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n',
    )

    # Lock the group so ocx.lock reflects the ci-group entry.
    lock_r = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"

    result = _run_cmd(ocx, project_dir, "remove", repo)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove from named group failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    # The binding value line must be gone; the [group.ci] header may remain empty.
    assert f'{repo} = ' not in toml_content, (
        f"binding for {repo!r} must be removed from [group.ci]; got:\n{toml_content}"
    )


# ---------------------------------------------------------------------------
# Whole-file model: remove preserves untouched pins; freshness gate (spec §8.2)
# ---------------------------------------------------------------------------
#
# CONTRACT CHANGE (spec §4.4): `ocx remove` previously re-resolved every
# survivor's live tag (a silent bump). It now carries every survivor forward
# VERBATIM and re-resolves nothing. The two tests below assert the new
# verbatim-preservation contract directly. No existing remove acceptance test
# asserted survivor re-resolution (the prior tests only checked binding/lock
# absence for single-tool projects), so there is no prior assertion to flip —
# these are the net-new contract anchors.


def test_remove_preserves_untouched_pins_when_upstream_moved(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove B`` must carry survivor A's pin forward verbatim — even
    when A's upstream moving tag advanced since the lock. A's leaf digests stay
    byte-identical; remove re-resolves NOTHING (contract change, spec §4.4).
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_rempres_a"
    repo_b = f"t_{short}_rempres_b"

    # A on a moving ``:latest`` tag; B as the binding to remove.
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=True)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    # Use repo basenames as the TOML keys so `ocx remove <repo>` resolves the
    # binding (the binding key is the repo basename of the identifier).
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:latest"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    lock_r = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"
    initial_a = _leaves_for_remove((project_dir / "ocx.lock").read_text(), repo_a)
    assert initial_a, "survivor A must record leaf digests"

    # Advance A's upstream ``:latest`` to a new digest, refresh the local index.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    refresh = subprocess.run(
        [str(ocx.binary), "index", "update", f"{ocx.registry}/{repo_a}"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    # Remove B. Survivor A must be carried forward verbatim (no re-resolve).
    result = _run_cmd(ocx, project_dir, "remove", repo_b)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    after_text = (project_dir / "ocx.lock").read_text()
    assert _leaves_for_remove(after_text, repo_b) == [], "removed binding B must be gone from the lock"
    after_a = _leaves_for_remove(after_text, repo_a)
    assert after_a == initial_a, (
        "survivor A must keep its old pin verbatim on remove even though its "
        f"upstream tag moved; before={initial_a}, after={after_a}"
    )


def test_remove_fails_when_toml_handedited_since_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove`` fails with exit 65 when ``ocx.toml`` drifted from
    ``ocx.lock`` before this remove (a hand-edit since the last lock). The
    freshness gate anchors on the pre-mutation snapshot; drift refuses the
    verbatim carry-forward.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_remhe_a"
    repo_b = f"t_{short}_remhe_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    # Use repo basenames as the TOML keys so `ocx remove <repo>` resolves the
    # binding (the binding key is the repo basename of the identifier).
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    lock_r = subprocess.run(
        [str(ocx.binary), "lock", "--no-pull"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"

    # Hand-edit survivor A's tag since the lock — declaration_hash now drifts.
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:2.0.0"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    result = _run_cmd(ocx, project_dir, "remove", repo_b)
    assert result.returncode == EXIT_DATA_ERROR, (
        f"ocx remove on a hand-edited ocx.toml must exit {EXIT_DATA_ERROR}; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    combined = (result.stderr + result.stdout).lower()
    assert "ocx lock" in combined or "`ocx lock`" in combined, (
        "error must direct the user to run `ocx lock` to reconcile; "
        f"stderr:\n{result.stderr}\nstdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# Multi-identifier `ocx remove a b` (fail-fast, all-or-nothing)
# ---------------------------------------------------------------------------


def test_remove_multiple_bindings_in_one_call(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove A B`` drops both bindings from ``ocx.toml`` + ``ocx.lock``
    and uninstalls both (candidate symlinks absent)."""
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_multi_a"
    repo_b = f"t_{short}_multi_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )
    lock_r = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"
    for repo in (repo_a, repo_b):
        subprocess.run(
            [str(ocx.binary), "package", "install", f"{ocx.registry}/{repo}:1.0.0"],
            capture_output=True,
            text=True,
            env=ocx.env,
            check=True,
        )
        assert_symlink_exists(_candidate_path(ocx, repo, "1.0.0"))

    result = _run_cmd(ocx, project_dir, "remove", repo_a, repo_b)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove A B failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    lock_text = (project_dir / "ocx.lock").read_text()
    for repo in (repo_a, repo_b):
        assert repo not in toml_content, f"{repo!r} must be gone from ocx.toml; got:\n{toml_content}"
        assert repo not in lock_text, f"{repo!r} must be gone from ocx.lock; got:\n{lock_text}"
        assert_not_exists(_candidate_path(ocx, repo, "1.0.0"))


def test_remove_fails_fast_when_one_absent(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove present absent`` fails fast with NotFound (79) and removes
    NOTHING — the present binding stays in ``ocx.toml`` and installed."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_ff_present"
    project_dir, candidate = _setup_project_with_tool(ocx, tmp_path, repo, "1.0.0")
    assert_symlink_exists(candidate)

    original_toml = (project_dir / "ocx.toml").read_text()
    result = _run_cmd(ocx, project_dir, "remove", repo, "nonexistent")
    assert result.returncode == EXIT_NOT_FOUND, (
        f"remove with one absent binding should exit {EXIT_NOT_FOUND}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when a batch remove hits a missing binding"
    )
    assert_symlink_exists(candidate)
