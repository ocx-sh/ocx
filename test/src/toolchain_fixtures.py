# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Shared construction and observation helpers for the rendered toolchain tree.

Owned by WP-12a of ``plan_toolchain_activation.md``; imported by
``test/tests/test_toolchain_render.py`` and by the wave-6 suites that build on
it (WP-12b, WP-12d).

Two properties are load-bearing for every consumer, and both are enforced *by
construction* here rather than left to each call site to remember.

Identity, not spelling
----------------------
``render_toolchain`` keys ``bin/<name>`` on the **exposed binary name** and
``<group>/<entry>`` on the **``[tools]`` key**. Those are two different
namespaces, and a test that spells them the same cannot tell which one an
assertion actually observed. :func:`locked_project` therefore mints three
distinct, UUID-prefixed spellings per tool — the registry repository, the
``[tools]`` key and the declared binary name — so no assertion about one can be
satisfied by another. :func:`assert_key_and_binary_namespaces_stay_disjoint` is
the explicit negative.

Per-test isolation under xdist
------------------------------
``task test:parallel`` runs ``pytest -n auto --dist loadgroup``. Every helper
here builds its state under the **calling test's own** ``tmp_path``;
:func:`two_branch_checkout` in particular creates a fresh bare repository *and*
a fresh clone per call, never a shared working tree that two workers would
``git checkout`` in opposite directions.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package, write_ocx_toml
from src.runner import OcxRunner, PackageInfo

__all__ = [
    "DEFAULT_GROUP",
    "EXIT_SUCCESS",
    "ToolchainProject",
    "TwoBranchCheckout",
    "assert_key_and_binary_namespaces_stay_disjoint",
    "back_reference_entries",
    "bin_entries",
    "git",
    "link_entries",
    "locked_project",
    "read_render_stamp",
    "render_stamp_path",
    "resolved_toolchain_home",
    "run_in",
    "sha256_of",
    "snapshot_tree",
    "toolchain_home",
    "two_branch_checkout",
    "write_toolchain_dir_config",
]

EXIT_SUCCESS = 0

#: The reserved group whose entries ``bin/`` exposes (C-045).
DEFAULT_GROUP = "default"

#: Every subprocess here is bounded. ``OcxRunner`` passes no ``timeout=`` and
#: the suite configures no ``pytest-timeout``, so a regression that reintroduced
#: the trampoline re-entry loop (D-V1) would hang the run instead of failing it.
_TIMEOUT = 180


# ---------------------------------------------------------------------------
# Running things
# ---------------------------------------------------------------------------


def run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    env_extra: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` from ``cwd`` so the ``ocx.toml`` CWD walk finds the project.

    ``OcxRunner`` exposes no ``cwd=``, and the project selector *is* the working
    directory for every command under test here — so this drops to
    ``subprocess.run`` rather than reaching for a runner method that does not
    exist. This is the one capability the shipped runner genuinely lacks; do not
    copy it a fourth time.
    """
    env = dict(ocx.env)
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_TIMEOUT,
    )


def git(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run ``git`` in ``cwd`` under a hermetic identity and no ambient config.

    ``GIT_CONFIG_GLOBAL``/``GIT_CONFIG_SYSTEM`` are pinned at ``/dev/null`` so a
    developer's ``~/.gitconfig`` — hooks, ``core.hooksPath``, a signing key, an
    ``init.defaultBranch`` — cannot change what a staleness row observes.
    """
    executable = shutil.which("git")
    assert executable, "git is required for the branch-switch staleness rows"
    return subprocess.run(
        [executable, *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
        timeout=_TIMEOUT,
        env={
            **os.environ,
            "GIT_AUTHOR_NAME": "ocx tests",
            "GIT_AUTHOR_EMAIL": "tests@ocx.invalid",
            "GIT_COMMITTER_NAME": "ocx tests",
            "GIT_COMMITTER_EMAIL": "tests@ocx.invalid",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_SYSTEM": "/dev/null",
        },
    )


def _git_ok(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    result = git(cwd, *args)
    assert result.returncode == EXIT_SUCCESS, (
        f"git {' '.join(args)} failed (rc={result.returncode})\n{result.stderr}"
    )
    return result


# ---------------------------------------------------------------------------
# Locating the home and its stamp
# ---------------------------------------------------------------------------


def toolchain_home(project: Path) -> Path:
    """The default, un-configured home (C-002): ``<project>/.ocx/toolchain``.

    Only correct when no ``toolchain-dir`` is in effect — use
    :func:`resolved_toolchain_home` whenever the test configures one.
    """
    return project / ".ocx" / "toolchain"


def resolved_toolchain_home(
    ocx: OcxRunner, cwd: Path, env_extra: dict[str, str] | None = None
) -> Path:
    """The home ``ocx`` itself resolves for ``cwd`` (C-056).

    Reads the ``toolchain_home`` contract field of ``ocx --format json shell
    state``, which is the supported discovery path when ``toolchain-dir``
    relocates the tree — so a relocation case never has to re-derive a 16-hex
    project key by hand.
    """
    result = run_in(ocx, cwd, "--format", "json", "shell", "state", env_extra=env_extra)
    assert result.returncode == EXIT_SUCCESS, (
        f"`ocx shell state` must report the resolved home; rc={result.returncode}\n"
        f"{result.stderr}"
    )
    payload = json.loads(result.stdout)
    return Path(payload["toolchain_home"])


def render_stamp_path(ocx: OcxRunner, home: Path) -> Path | None:
    """The render stamp describing ``home``, or ``None`` when none was written.

    Both tiers are searched, because the path differs by scope: a project's
    stamp lives at ``$OCX_HOME/state/projects/<key>/render_stamp.json`` and the
    global tree's at ``$OCX_HOME/state/render_stamp.json``. The match is on the
    stamp's own recorded ``home``, never on a key the test re-derives.
    """
    state = Path(ocx.env["OCX_HOME"]) / "state"
    if not state.exists():
        return None
    candidates = [
        state / "render_stamp.json",
        *sorted(state.rglob("render_stamp.json")),
    ]
    for candidate in candidates:
        if not candidate.exists():
            continue
        try:
            payload = json.loads(candidate.read_text())
        except json.JSONDecodeError:
            continue
        if payload.get("home") == str(home):
            return candidate
    return None


def read_render_stamp(ocx: OcxRunner, home: Path) -> dict | None:
    """The parsed render stamp for ``home``, or ``None`` when none was written."""
    path = render_stamp_path(ocx, home)
    return json.loads(path.read_text()) if path else None


def write_toolchain_dir_config(ocx: OcxRunner, value: str | Path) -> Path:
    """Declare ``toolchain-dir = <value>`` in the home-tier ``config.toml``.

    The tier a refusal names is ``config.toml `toolchain-dir```; the environment
    tier is reached instead by passing ``OCX_TOOLCHAIN_DIR`` through
    ``env_extra``.
    """
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    text = value.as_posix() if isinstance(value, Path) else str(value)
    path.write_text(f'toolchain-dir = "{text}"\n')
    return path


# ---------------------------------------------------------------------------
# Observing the tree
# ---------------------------------------------------------------------------


def sha256_of(path: Path) -> str:
    """Hex sha256 of a file's bytes — the digest the stamp's ``content_hash``
    carries for each ``bin/`` entry."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot_tree(root: Path) -> dict[str, tuple[str, str]]:
    """A recursive **byte-identity** snapshot: path → ``(kind, identity)``.

    ``kind`` is ``file``/``symlink``/``dir``; ``identity`` is the sha256 of a
    file's bytes, a symlink's raw target, or ``""`` for a directory. An absent
    root snapshots as ``{}`` — the state a render that wrote nothing leaves.

    ``lstat``-shaped throughout: a snapshot must record the link itself, never
    what it currently points at, because a repoint is a write. mtime and inode
    are deliberately **absent** — C-047's claim is byte-identity, and folding in
    a timestamp would make an untouched tree compare unequal for the exact
    reason C-003 refuses to stamp mtime.
    """
    out: dict[str, tuple[str, str]] = {}
    if not root.exists() and not root.is_symlink():
        return out
    for path in sorted(root.rglob("*")):
        key = str(path.relative_to(root))
        if path.is_symlink():
            out[key] = ("symlink", os.readlink(path))
        elif path.is_dir():
            out[key] = ("dir", "")
        else:
            out[key] = ("file", sha256_of(path))
    return out


def bin_entries(home: Path) -> list[str]:
    """The on-disk entry names under ``<home>/bin/`` — the set
    :external+ocx:``RenderStamp::names`` claims to mirror."""
    directory = home / "bin"
    return sorted(p.name for p in directory.iterdir()) if directory.is_dir() else []


def link_entries(home: Path) -> dict[str, str]:
    """Every ``<group>/<entry>`` link in ``home``, as ``"group/entry" → target``.

    ``bin/`` is skipped: it holds trampolines, not links, and folding the two
    together would let an assertion about one be satisfied by the other.
    """
    out: dict[str, str] = {}
    if not home.is_dir():
        return out
    for group in sorted(home.iterdir()):
        if not group.is_dir() or group.name == "bin":
            continue
        for entry in sorted(group.iterdir()):
            if entry.is_symlink():
                out[f"{group.name}/{entry.name}"] = os.readlink(entry)
    return out


def back_reference_entries(ocx: OcxRunner) -> list[str]:
    """Every install back-reference under ``$OCX_HOME/packages/**/refs/symlinks/``.

    C-052 forbids one for any toolchain link: a `<group>/<entry>` link is
    written with ``symlink::replace_atomic``, never through ``ReferenceManager``,
    because a back-reference would pin every rendered package forever. Returned
    as store-relative strings so a failure names *which* reference exists.
    """
    packages = Path(ocx.env["OCX_HOME"]) / "packages"
    if not packages.is_dir():
        return []
    return sorted(
        str(entry.relative_to(packages))
        for refs in packages.rglob("refs/symlinks")
        if refs.is_dir()
        for entry in refs.iterdir()
    )


# ---------------------------------------------------------------------------
# Building a project
# ---------------------------------------------------------------------------


def _unique_label(label: str | None) -> str:
    """``<label>`` as a readable prefix on a fresh UUID — unique *per call*.

    Every name these builders mint reaches the shared registry: repositories
    ``t_<label>_tcpkg`` / ``t_<label>_tcgrp`` / ``t_<label>_branch``, all pushed
    at tag ``1.0.0``. A caller's label identifies the *row's intent*, so a
    parametrized test passes one constant label for all of its rows — and
    ``pytest -n auto`` runs those rows on different workers at the same time,
    pushing different bundle bytes to one ``repository:tag``. The loser reads
    back a manifest the winner has already replaced and fails in fixture setup
    with ``manifest not found`` (exit 79), a different row each run.

    Uniqueness therefore cannot be the caller's to remember: it is minted here,
    where every name is built. The label survives in the spelling so a failure
    still names which row wrote the tree.
    """
    return f"{label}{uuid4().hex[:8]}" if label else uuid4().hex[:8]


@dataclasses.dataclass(frozen=True)
class ToolchainProject:
    """A pulled project, and the three disjoint spellings of each of its tools.

    ``key`` is what ``<group>/<entry>`` is named after; ``binary`` is what
    ``bin/<name>`` is named after; ``package.repo`` is neither. Keeping the
    three apart is what makes an assertion about one of them discriminating —
    see the module docstring.
    """

    directory: Path
    home: Path
    group: str

    default_key: str
    default_binary: str
    default_package: PackageInfo

    group_key: str
    group_binary: str
    group_package: PackageInfo

    @property
    def default_link(self) -> Path:
        """``<home>/default/<[tools] key>`` — the default group's link."""
        return self.home / DEFAULT_GROUP / self.default_key

    @property
    def group_link(self) -> Path:
        """``<home>/<group>/<key>`` — the named group's link."""
        return self.home / self.group / self.group_key

    @property
    def default_trampoline(self) -> Path:
        """``<home>/bin/<binary name>`` — keyed on the exposed name, never the
        ``[tools]`` key."""
        return self.home / "bin" / self.default_binary

    def config_body(
        self, *, pinned: bool | None = None, group: str | None = None
    ) -> str:
        """The project's ``ocx.toml`` text, optionally with a different group
        name (for the rename rows) or a ``pinned`` declaration."""
        header = "" if pinned is None else f"pinned = {str(pinned).lower()}\n\n"
        selected = group or self.group
        return (
            f"{header}[tools]\n"
            f'{self.default_key} = "{self.default_package.fq}"\n\n'
            f"[group.{selected}.tools]\n"
            f'{self.group_key} = "{self.group_package.fq}"\n'
        )


def locked_project(
    ocx: OcxRunner,
    tmp_path: Path,
    *,
    group: str = "ci",
    directory: Path | None = None,
    pull: bool = True,
    label: str | None = None,
    env_extra: dict[str, str] | None = None,
) -> ToolchainProject:
    """A locked (and by default pulled) project with one default-group tool and
    one tool in ``[group.<group>]``.

    The two tools exist together because most rows need a *second real digest
    root* to point a poisoned link at — a dangling target is a different arm of
    C-067 and would not discriminate the same mutation.

    Every namespace gets its own spelling: repository ``t_<label>_tcpkg``,
    ``[tools]`` key ``tckey<label>``, binary ``tcbin<label>``. Do not collapse
    them.

    ``label`` is a *readable prefix*, never the whole name — see
    :func:`_unique_label`.

    ``env_extra`` reaches ``ocx lock`` and ``ocx pull`` — pass it whenever the
    caller has configured a tier (``OCX_MANAGED_CONFIG``, ``OCX_TOOLCHAIN_DIR``)
    that must already be in effect while the first tree renders, or the project
    renders once at the un-relocated home first.
    """
    label = _unique_label(label)
    default_package = make_package(
        ocx,
        f"t_{label}_tcpkg",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"tcbin{label}"],
    )
    group_package = make_package(
        ocx,
        f"t_{label}_tcgrp",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"grpbin{label}"],
    )

    project_dir = directory or (tmp_path / f"proj-{label}")
    project_dir.mkdir(parents=True, exist_ok=True)

    project = ToolchainProject(
        directory=project_dir,
        home=toolchain_home(project_dir),
        group=group,
        default_key=f"tckey{label}",
        default_binary=f"tcbin{label}",
        default_package=default_package,
        group_key=f"grpkey{label}",
        group_binary=f"grpbin{label}",
        group_package=group_package,
    )
    write_ocx_toml(project_dir, project.config_body())

    lock = run_in(ocx, project_dir, "lock", env_extra=env_extra)
    assert lock.returncode == EXIT_SUCCESS, f"ocx lock failed:\n{lock.stderr}"
    if pull:
        pulled = run_in(ocx, project_dir, "pull", env_extra=env_extra)
        assert pulled.returncode == EXIT_SUCCESS, f"ocx pull failed:\n{pulled.stderr}"
    return project


def assert_key_and_binary_namespaces_stay_disjoint(project: ToolchainProject) -> None:
    """The identity-not-spelling negative, asserted rather than assumed.

    ``bin/`` is keyed on exposed **binary names** and ``<group>/`` on
    ``[tools]`` **keys**. A renderer that confused the two would still produce a
    plausible-looking tree, and every "``bin/<X>`` exists" assertion in the suite
    would pass against the wrong namespace. This states the negative once, so
    every consumer inherits it.
    """
    names = bin_entries(project.home)
    links = link_entries(project.home)

    for key in (project.default_key, project.group_key):
        assert key not in names, (
            f"a `[tools]` key must never appear as a `bin/` entry; {key!r} did, "
            f"in {names}"
        )
    for binary in (project.default_binary, project.group_binary):
        assert not any(entry.endswith(f"/{binary}") for entry in links), (
            f"an exposed binary name must never appear as a `<group>/<entry>` "
            f"link; {binary!r} did, in {sorted(links)}"
        )


# ---------------------------------------------------------------------------
# The two-branch checkout (branch-switch staleness rows)
# ---------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class TwoBranchCheckout:
    """One test's private clone of a two-branch repository.

    ``main`` locks ``package_main``, ``other`` locks ``package_other`` — same
    ``[tools]`` key, same binary name, two different digests. Switching branches
    is therefore exactly S-006's shape: the tree on disk still describes the
    branch that was pulled.
    """

    directory: Path
    home: Path
    key: str
    binary: str
    package_main: PackageInfo
    package_other: PackageInfo
    main_branch: str = "main"
    other_branch: str = "other"

    @property
    def link(self) -> Path:
        """``<home>/default/<key>`` — the link a branch switch makes stale."""
        return self.home / DEFAULT_GROUP / self.key


def two_branch_checkout(
    ocx: OcxRunner, tmp_path: Path, *, label: str | None = None
) -> TwoBranchCheckout:
    """Build a bare repository with two branches and hand back a fresh clone.

    **A bare repository plus a per-call clone, never a shared working tree.**
    ``task test:parallel`` runs ``pytest -n auto``; two workers driving different
    staleness rows against one checkout would ``git checkout`` opposite branches
    under each other. Both the bare repository and the clone live under the
    calling test's own ``tmp_path``.
    """
    label = _unique_label(label)
    key = f"brkey{label}"
    package_main = make_package(
        ocx,
        f"t_{label}_branch",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"brbin{label}"],
    )
    package_other = make_package(
        ocx,
        f"t_{label}_branch",
        "2.0.0",
        tmp_path,
        cascade=False,
        bins=[f"brbin{label}"],
    )

    seed = tmp_path / f"seed-{label}"
    seed.mkdir()
    _git_ok(seed, "init", "-q", "-b", "main")
    for branch, package in ((None, package_main), ("other", package_other)):
        if branch:
            _git_ok(seed, "checkout", "-q", "-b", branch)
        write_ocx_toml(seed, f'[tools]\n{key} = "{package.fq}"\n')
        lock = run_in(ocx, seed, "lock")
        assert lock.returncode == EXIT_SUCCESS, (
            f"seeding `ocx lock` failed:\n{lock.stderr}"
        )
        _git_ok(seed, "add", "-A")
        _git_ok(seed, "commit", "-qm", f"lock {package.tag}")
    _git_ok(seed, "checkout", "-q", "main")

    bare = tmp_path / f"origin-{label}.git"
    _git_ok(tmp_path, "clone", "-q", "--bare", str(seed), str(bare))
    checkout = tmp_path / f"checkout-{label}"
    _git_ok(tmp_path, "clone", "-q", str(bare), str(checkout))

    return TwoBranchCheckout(
        directory=checkout,
        home=toolchain_home(checkout),
        key=key,
        binary=f"brbin{label}",
        package_main=package_main,
        package_other=package_other,
    )
