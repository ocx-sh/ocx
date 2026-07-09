# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx update`` (whole-file and scoped lock model).

``ocx update`` replaces ``ocx upgrade`` — a hard rename, no alias. Default
resolution is against the LIVE registry: a declared tag re-resolves to
whatever it points at right now, with no prior ``ocx index update`` needed.
``ocx update`` writes ONLY ``ocx.lock`` — it never writes a local index tag
pointer under ``$OCX_HOME/tags/``. ``--remote`` is accepted but redundant
(already the default). ``--frozen`` restricts resolution to the local
snapshot only — a tag never captured by a prior ``ocx index update`` is
unresolvable under ``--frozen`` and exits 81 (PolicyBlocked); ``--offline``
forbids all network and also exits 81 on an uncached moving tag.

``ocx update`` with no arguments is the whole-file bump verb: it re-resolves
EVERY declared tag, always, even when the lock is already current.

``ocx update`` also accepts binding names and ``-g``/``--group`` to advance
only part of the toolchain and freeze the rest. A scoped update re-resolves
only the named bindings' declared tags and carries every other pin forward
verbatim, so a single tool can move while every other pin stays frozen.

1. ``ocx update`` (no args)      — re-resolves the whole file (bumps every tag)
2. ``ocx update NAME``           — advances only the named binding(s)
3. ``ocx update -g GROUP``       — advances only the named group(s)
4. ``ocx update`` scoped, no lock — exit 78 (needs a predecessor to carry forward)
5. ``ocx update`` scoped, drift   — exit 65 (``ocx.toml`` changed since ``ocx lock``)
6. ``ocx update UNKNOWN``        — exit 64 (unknown binding name)

Guarantee preservation: only the explicitly named bindings re-resolve against
the live index (no laundering); untouched pins are carried forward, never
live-re-resolved (no drift); the freshness gate refuses a drifted ``ocx.toml``.
See ``.claude/artifacts/adr_scoped_upgrade.md``.
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
EXIT_USAGE = 64        # clap unknown-arg / unknown group or name → EX_USAGE
EXIT_DATA = 65         # scoped update on a drifted ocx.toml (StaleLockOnPartial)
EXIT_CONFIG = 78       # scoped update with no predecessor ocx.lock
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


def _run_add(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "add", *extra),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _run_update(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "update", *extra),
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


def _tags_snapshot(ocx: OcxRunner) -> list[tuple[str, bytes]]:
    """Return a sorted ``(relative_path, bytes)`` snapshot of every tag
    pointer file under ``$OCX_HOME/tags/`` — ``[]`` when the directory is
    absent. Used to prove a command never writes a local index tag pointer.
    """
    tags_root = Path(ocx.env["OCX_HOME"]) / "tags"
    if not tags_root.exists():
        return []
    return sorted(
        (str(p.relative_to(tags_root)), p.read_bytes())
        for p in tags_root.rglob("*.json")
    )


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
# 1. ``ocx update`` (whole file) — bumps every moving tag to its new digest
# ---------------------------------------------------------------------------


def test_update_bumps_every_tag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update`` (no args) re-resolves EVERY declared tag. With two
    tools each declared at a moving ``:latest`` that has since advanced
    upstream — with NO ``ocx index update`` ever run — both tools' leaf
    digests change against the LIVE registry. Update is whole-file, never
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
    # Remote-default resolution means bare ``ocx update`` sees this directly —
    # no ``ocx index update`` snapshot step is needed.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=False, cascade=True)

    # Bare ``ocx update`` re-resolves the whole file; both tags advance.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_a = _leaves_for(after_text, "a")
    after_b = _leaves_for(after_text, "b")
    assert after_a and after_b, "both tools must still record leaf digests"

    assert after_a != initial_a, "'a' leaves must change — update bumps every tag"
    assert after_b != initial_b, "'b' leaves must change — update bumps every tag"


# ---------------------------------------------------------------------------
# 2. ``ocx update`` (no args) — full re-resolution, equivalent to ``ocx lock``
# ---------------------------------------------------------------------------


def test_update_no_args_full_resolution_equivalent_to_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update`` with no args on an unchanged ``ocx.toml`` → produces
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

    # `ocx update` (no args) re-resolves everything against the same
    # ocx.toml — every leaf digest must match the initial lock and the
    # declaration_hash must be unchanged.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_text = _read_lock_text(project)
    after_leaves = sorted(_LEAF_RE.findall(after_text))
    after_hash = _declaration_hash(after_text)

    assert after_leaves == initial_leaves, (
        "no-args `ocx update` must keep every tool's leaf digests equal to `ocx lock`"
    )
    assert after_hash == initial_hash, (
        "declaration_hash must be unchanged when ocx.toml has not changed"
    )


# ---------------------------------------------------------------------------
# Remote-by-default & local index immutability
# ---------------------------------------------------------------------------


def test_update_default_resolves_against_live_registry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Remote-by-default: a moving tag that has advanced purely upstream —
    with NO ``ocx index update`` ever run against it — still advances the
    lock on a bare ``ocx update``. Proves resolution reads the live registry
    directly rather than requiring a prior local-index snapshot.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_live_default"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    added = _run_add(ocx, project, f"{ocx.registry}/{repo}:latest")
    assert added.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={added.returncode}\nstderr:\n{added.stderr}"
    )
    initial_leaves = _leaves_for(_read_lock_text(project), repo)
    assert initial_leaves, "ocx add must record leaf digests for the new binding"

    # Upstream moves the tag — index=False keeps the local snapshot stale
    # (`make_package` otherwise runs `ocx index update` as part of publishing).
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True, index=False)

    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update must resolve against the live registry by default; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_leaves = _leaves_for(_read_lock_text(project), repo)
    assert after_leaves and after_leaves != initial_leaves, (
        "ocx update (remote-by-default) must advance the lock to the tag's "
        "new digest without any prior `ocx index update`"
    )


def test_update_does_not_write_local_tag_pointers(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update`` writes ONLY ``ocx.lock`` — it must never write a local
    index tag pointer under ``$OCX_HOME/tags/``, even when the update
    actually performs a live re-resolution that advances the lock. Blob
    additions under ``blobs/`` are fine; only ``tags/`` must stay untouched.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_no_tag_write"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:latest"
""",
    )
    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_leaves = _leaves_for(_read_lock_text(project), "tool")

    # Upstream moves — index=False keeps the local snapshot stale, so the
    # live re-resolve below would be the only thing that could write tags/.
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True, index=False)

    before = _tags_snapshot(ocx)

    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_leaves = _leaves_for(_read_lock_text(project), "tool")
    assert after_leaves != initial_leaves, (
        "precondition: the update must have actually resolved live (tag advanced)"
    )

    after = _tags_snapshot(ocx)
    assert before == after, (
        "ocx update must never write $OCX_HOME/tags/ — it writes only "
        f"ocx.lock. Before: {[n for n, _ in before]}, after: {[n for n, _ in after]}"
    )


# ---------------------------------------------------------------------------
# 3. Scoped update — advance only the named binding(s) / group(s)
# ---------------------------------------------------------------------------


def test_update_single_tool_bumps_only_named(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update a`` with two tools both on a moving ``:latest`` that has
    advanced upstream: only ``a``'s leaf digests change; ``b`` is carried
    forward verbatim (frozen). This is the load-bearing scoped-update
    contract — advance one tool without moving the rest.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_scoped_a"
    repo_b = f"t_{short}_scoped_b"
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

    # Advance both moving tags upstream (cascade re-points ``:latest``) —
    # no ``ocx index update`` snapshot needed under remote-default.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=False, cascade=True)

    # Scoped update of ONLY ``a``.
    result = _run_update(ocx, project, "a")
    assert result.returncode == EXIT_SUCCESS, (
        f"scoped ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_a = _leaves_for(after_text, "a")
    after_b = _leaves_for(after_text, "b")
    assert after_a != initial_a, "'a' was named — its leaves must advance"
    assert after_b == initial_b, "'b' was not named — its leaves must stay frozen"


def test_update_scoped_preserves_untouched_pins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The untouched ``[[tool]]`` entry is carried forward byte-for-byte.

    Complements ``test_update_single_tool_bumps_only_named`` by asserting the
    stronger property: the frozen tool's on-disk lock slice is identical, not
    merely resolving to the same digest.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_preserve_a"
    repo_b = f"t_{short}_preserve_b"
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

    def _tool_slice(lock_text: str, name: str) -> str:
        marker = f'name = "{name}"'
        start = lock_text.index(marker)
        rest = lock_text[start:]
        nxt = rest.find("[[tool]]", len("[[tool]]"))
        return rest if nxt == -1 else rest[:nxt]

    before_b_slice = _tool_slice(_read_lock_text(project), "b")

    # Advance only ``a`` upstream, then scoped-update ``a``. No ``ocx index
    # update`` snapshot needed under remote-default.
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=True)

    result = _run_update(ocx, project, "a")
    assert result.returncode == EXIT_SUCCESS, result.stderr

    after_b_slice = _tool_slice(_read_lock_text(project), "b")
    assert after_b_slice == before_b_slice, (
        "the untouched tool 'b' must be carried forward byte-for-byte"
    )


def test_update_group_scopes_to_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update -g ci`` advances every binding in the ``ci`` group and
    freezes the default ``[tools]`` table.
    """
    short = uuid4().hex[:8]
    repo_default = f"t_{short}_grp_def"
    repo_ci = f"t_{short}_grp_ci"
    make_package(ocx, repo_default, "1.0.0", tmp_path, new=True, cascade=True)
    make_package(ocx, repo_ci, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo_default}:latest"

[group.ci]
citool = "{ocx.registry}/{repo_ci}:latest"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_default = _leaves_for(initial_text, "tool")
    initial_ci = _leaves_for(initial_text, "citool")
    assert initial_default and initial_ci, "both tools must record leaf digests"

    # No ``ocx index update`` snapshot needed under remote-default.
    make_package(ocx, repo_default, "2.0.0", tmp_path, new=False, cascade=True)
    make_package(ocx, repo_ci, "2.0.0", tmp_path, new=False, cascade=True)

    result = _run_update(ocx, project, "-g", "ci")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update -g ci failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    assert _leaves_for(after_text, "citool") != initial_ci, (
        "the ci-group tool must advance — it is in scope"
    )
    assert _leaves_for(after_text, "tool") == initial_default, (
        "the default-group tool must stay frozen — it is out of scope"
    )


def test_update_scoped_no_lock_errors_78(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A scoped ``ocx update tool`` with NO ``ocx.lock`` exits 78: there is
    no predecessor to carry untouched pins forward from. ``ocx.lock`` is never
    created.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_scoped_nolock"
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
    assert not (project / "ocx.lock").exists()

    result = _run_update(ocx, project, "tool")
    assert result.returncode == EXIT_CONFIG, (
        f"scoped ocx update with no lock must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "scoped ocx update must not create ocx.lock when it fails"
    )


def test_update_scoped_stale_toml_errors_65(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A scoped ``ocx update a`` on an ``ocx.toml`` hand-edited since the last
    ``ocx lock`` exits 65 (the freshness gate refuses to carry a stale lock
    forward) and leaves ``ocx.lock`` untouched.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_stale_a"
    repo_b = f"t_{short}_stale_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
""",
    )
    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before_bytes = (project / "ocx.lock").read_bytes()

    # Hand-edit ocx.toml AFTER locking: add a binding so it drifts from the lock.
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    result = _run_update(ocx, project, "a")
    assert result.returncode == EXIT_DATA, (
        f"scoped ocx update on a drifted ocx.toml must exit {EXIT_DATA}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    after_bytes = (project / "ocx.lock").read_bytes()
    assert after_bytes == before_bytes, (
        "scoped ocx update must NOT rewrite ocx.lock when refusing a stale toml"
    )


def test_update_unknown_name_errors_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update does-not-exist`` exits 64 (the name matches no binding)
    and leaves ``ocx.lock`` untouched.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_unknown_name"
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

    result = _run_update(ocx, project, "does-not-exist")
    assert result.returncode == EXIT_USAGE, (
        f"`ocx update does-not-exist` must exit {EXIT_USAGE}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )
    after_bytes = (project / "ocx.lock").read_bytes()
    assert after_bytes == before_bytes, (
        "ocx.lock must NOT be rewritten when a binding name is unknown"
    )


# ---------------------------------------------------------------------------
# Network policy: ``--offline`` and ``--frozen``
# ---------------------------------------------------------------------------


def test_update_offline_uncached_policy_blocked(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --offline update`` with a moving tag absent from the local
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
        _ocx_cmd(ocx, "--offline", "update"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"ocx --offline update with an uncached moving tag must exit "
        f"{EXIT_POLICY_BLOCKED}; got {result.returncode}\nstderr:\n{result.stderr}"
    )


def test_update_frozen_unsnapshotted_tag_exits_81(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx --frozen update`` with a declared tag that was NEVER snapshotted
    via ``ocx index update`` exits 81 (PolicyBlocked). ``--frozen`` refuses to
    resolve against anything but the local snapshot, and none exists for this
    tag — distinct from ``--offline``: the registry is reachable, but frozen
    still refuses.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_frozen_ceiling"
    # index=False: the package must exist on the registry but stay
    # un-snapshotted locally — `make_package` otherwise runs `ocx index
    # update` as part of publishing.
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, index=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )

    # Hand-author a V2 lock so a predecessor exists but the tag is uncached
    # locally (mirrors ``test_update_offline_uncached_policy_blocked``).
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
repository = "{ocx.registry}/{repo}"

[tool.platforms]
"linux/amd64" = "sha256:{"a" * 64}"
"""
    )

    result = subprocess.run(
        _ocx_cmd(ocx, "--frozen", "update"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"ocx --frozen update with an unsnapshotted tag must exit "
        f"{EXIT_POLICY_BLOCKED}; got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# ``--check`` (dry-run) tests
# ---------------------------------------------------------------------------


def test_update_check_succeeds_on_current(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --check`` exits 0 without writing when the candidate
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
        f"ocx update --check must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after = (project / "ocx.lock").read_bytes()
    assert before == after, "ocx update --check must NOT rewrite ocx.lock"


def test_update_check_no_lock_exits_78_without_network(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --check`` with NO ``ocx.lock`` exits 78 (ConfigError)
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
        f"ocx update --check with no lock must exit 78 (ConfigError) before "
        f"any resolve; got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not (project / "ocx.lock").exists(), (
        "ocx update --check must not create ocx.lock"
    )


def test_update_check_exits_65_on_whole_file_drift(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --check`` exits 65 (DataError) without writing when
    an advisory tag has moved upstream — even though ``ocx.toml`` is
    byte-identical to the lock's recorded ``declaration_hash``.

    ``update`` is a whole-file verb: ``--check`` compares the whole-file
    re-resolve candidate against the recorded lock and refuses when any
    declared tag has advanced. There is no subset scope.

    Also exercises the snapshot path: after ``ocx index update`` snapshots
    the moved tag (2.0.0), the upstream tag advances AGAIN (3.0.0) with no
    further snapshot. A subsequent ``ocx --frozen update`` still succeeds and
    resolves to the SNAPSHOTTED digest (2.0.0), not the further-moved live
    one — proving ``--frozen`` reads the local snapshot, never the registry.
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

    # Publish 2.0.0 with cascade=True — "latest" now points at the new
    # digest — and snapshot it via ``ocx index update``.
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True)
    snapshot = subprocess.run(
        _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert snapshot.returncode == EXIT_SUCCESS, snapshot.stderr

    result = _run_update(ocx, project, "--check")
    assert result.returncode == 65, (
        f"ocx update --check must exit 65 when an advisory tag moved; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_lock = (project / "ocx.lock").read_bytes()
    assert before_lock == after_lock, (
        "ocx update --check must NOT rewrite ocx.lock when refusing"
    )

    # Frozen update resolves from the just-snapshotted tag (2.0.0).
    frozen_first = subprocess.run(
        _ocx_cmd(ocx, "--frozen", "update"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert frozen_first.returncode == EXIT_SUCCESS, (
        f"ocx --frozen update must resolve from the local snapshot; "
        f"rc={frozen_first.returncode}\nstderr:\n{frozen_first.stderr}"
    )
    snapshotted_leaves = _leaves_for(_read_lock_text(project), "mover")
    assert snapshotted_leaves, "frozen update must record leaf digests"
    assert snapshotted_leaves != _leaves_for(before_lock.decode(), "mover"), (
        "frozen update must advance to the snapshotted (2.0.0) digest"
    )

    # Upstream moves AGAIN, past the snapshot — index=False so no further
    # `index update` runs (`make_package` snapshots by default).
    make_package(ocx, repo, "3.0.0", tmp_path, new=False, cascade=True, index=False)

    frozen_second = subprocess.run(
        _ocx_cmd(ocx, "--frozen", "update"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert frozen_second.returncode == EXIT_SUCCESS, (
        f"ocx --frozen update must resolve from the local snapshot even when "
        f"upstream has moved further; rc={frozen_second.returncode}\n"
        f"stderr:\n{frozen_second.stderr}"
    )
    assert _leaves_for(_read_lock_text(project), "mover") == snapshotted_leaves, (
        "frozen update must keep resolving the OLD snapshot even though "
        "upstream moved further; it must never re-resolve live"
    )


def test_update_scoped_check_isolates_scope(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Scoped ``ocx update NAME --check`` verifies only the named binding.

    Two tools are locked at ``:latest``; only ``b`` advances upstream. A scoped
    ``--check`` on the still-current ``a`` exits 0 (``b``'s drift is out of
    scope and carried forward frozen), while a scoped ``--check`` on the moved
    ``b`` exits 65. Neither writes ``ocx.lock``. This is the scoped counterpart
    to ``test_update_check_exits_65_on_whole_file_drift``: a whole-file
    ``--check`` here would exit 65 because ``b`` moved.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_chk_a"
    repo_b = f"t_{short}_chk_b"
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
    before_lock = (project / "ocx.lock").read_bytes()

    # Only ``b`` advances upstream; ``a`` stays put. No ``ocx index update``
    # snapshot needed under remote-default.
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=False, cascade=True)

    # ``a`` is still current → scoped --check on ``a`` passes; ``b``'s drift is
    # out of scope and carried forward frozen.
    in_scope_current = _run_update(ocx, project, "a", "--check")
    assert in_scope_current.returncode == EXIT_SUCCESS, (
        f"scoped --check on the still-current 'a' must exit 0; "
        f"rc={in_scope_current.returncode}\nstderr:\n{in_scope_current.stderr}"
    )

    # ``b`` moved → scoped --check on ``b`` refuses with 65.
    in_scope_moved = _run_update(ocx, project, "b", "--check")
    assert in_scope_moved.returncode == EXIT_DATA, (
        f"scoped --check on the moved 'b' must exit {EXIT_DATA}; "
        f"rc={in_scope_moved.returncode}\nstderr:\n{in_scope_moved.stderr}"
    )

    after_lock = (project / "ocx.lock").read_bytes()
    assert before_lock == after_lock, (
        "scoped ocx update --check must never rewrite ocx.lock"
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
    want to exercise update behaviour swap the toml to ``tag_v2`` and re-run
    ``ocx lock`` / ``ocx update``.
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
    # Write the initial lock so update has a predecessor.  Use --no-pull
    # so the setup step leaves the object store cold; otherwise tag_v1
    # would already be present and `_packages_present_count >= 1` would
    # fire trivially, hiding whether the eager-default update actually
    # materialised tag_v2 (the cold-store baseline is the eager-vs-lazy
    # observable now that candidate symlinks are no longer created by
    # toolchain mutators).
    initial = _run_lock(ocx, project, "--no-pull")
    assert initial.returncode == EXIT_SUCCESS, initial.stderr

    return project, repo, tag_v1, tag_v2


def test_update_eager_default_materializes_new_digest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """REGRESSION GUARD: ``ocx update`` (no flags) after bumping the toml to
    a new tag writes the lock AND pre-warms the object store with the new
    tag, but creates **no** candidate symlink.

    Plan Phase-5 Step 3.3 contract: default is eager. The candidate-absent
    half locks in the no-symlink mutator invariant
    (``project_context.rs::materialize_lock`` → ``pull_all``).
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    # Bump the toml to tag_v2 then update.
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx update must pre-warm the new digest into the object store"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_update_no_pull_skips_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --no-pull`` writes the lock and leaves the object store
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
        f"ocx update --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written even with --no-pull"

    assert _packages_present_count(ocx) == 0, (
        "ocx update --no-pull must leave the object store cold"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_update_pull_then_no_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --pull --no-pull`` → ``--no-pull`` wins (POSIX last-wins);
    lock must advance to the new digest but candidate_v2 must NOT exist.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx update``.
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
        f"ocx update --pull --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) == 0, (
        "--no-pull must win: object store stays cold"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_update_no_pull_then_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --no-pull --pull`` → ``--pull`` wins (POSIX last-wins);
    lock advances and object store warms with the new digest; candidate
    symlink absent.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx update``.
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
        f"ocx update --no-pull --pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) >= 1, (
        "--pull must win: object store warms with the new digest"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_update_check_unaffected_by_pull_flags(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --check`` is a pure dry-run and must NOT create any
    candidate symlink regardless of ``--pull`` / ``--no-pull`` flags.

    Plan Phase-5 Step 3.3: the ``--check`` early-return path in
    ``update.rs:175`` (before the materialize call) guarantees this.
    The test is a regression guard for the separation of verify vs mutate.
    """
    project, repo, tag_v1, _tag_v2 = _two_tag_project(ocx, tmp_path)

    # Candidate symlinks are never created by toolchain mutators (lock /
    # update / add) under the no-symlink mutator model. The probe path
    # is kept here as a regression anchor for the verify-vs-mutate split.
    candidate_v1 = _candidate_path(ocx, repo, tag_v1)
    assert_not_exists(candidate_v1)

    lock_bytes_before = (project / "ocx.lock").read_bytes()

    # Run with both --check and --pull to confirm --check wins.
    result = _run_update(ocx, project, "--check", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update --check --pull must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_bytes_after = (project / "ocx.lock").read_bytes()
    assert lock_bytes_before == lock_bytes_after, (
        "ocx update --check must NOT rewrite ocx.lock"
    )
    # --check must not materialize anything even when --pull is present.
    assert_not_exists(candidate_v1)
