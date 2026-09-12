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
    "ACTIVE_LINK",
    "DEFAULT_GROUP",
    "DEFAULT_SHELL",
    "EXIT_SUCCESS",
    "LINKS_DIR",
    "SHELLS_DIR",
    "ToolchainProject",
    "TwoBranchCheckout",
    "active_link",
    "assert_key_and_binary_namespaces_stay_disjoint",
    "back_reference_entries",
    "bin_entries",
    "entry_link",
    "git",
    "link_entries",
    "links_group",
    "links_root",
    "locked_project",
    "path_facing_bin",
    "read_render_stamp",
    "render_stamp_path",
    "resolved_toolchain_bin",
    "resolved_toolchain_home",
    "run_in",
    "sha256_of",
    "shell_bin",
    "snapshot_tree",
    "toolchain_home",
    "two_branch_checkout",
    "write_toolchain_dir_config",
]

EXIT_SUCCESS = 0

#: The reserved group whose entries ``bin/`` exposes (C-045).
DEFAULT_GROUP = "default"

#: The tree's own depth-1 names, mirrored from the grammar
#: (``file_structure/toolchain_store.rs`` ``LINKS_DIR``/``SHELLS_DIR``/
#: ``ACTIVE_LINK``/``DEFAULT_SHELL``). Every user-supplied name lives one level
#: *below* ``links/``, which is what retires the old depth-1 reservation.
LINKS_DIR = "links"
SHELLS_DIR = "shells"
ACTIVE_LINK = "active"
DEFAULT_SHELL = "default"

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
        # Detached, never inherited -- see `OcxRunner.run`, which states the
        # SIGTTIN failure this closes.
        stdin=subprocess.DEVNULL,
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
        # Detached: git prompts for credentials on a terminal. These rows drive
        # a purely local repository, so a prompt could only ever be a hang.
        stdin=subprocess.DEVNULL,
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

    Only correct when no ``toolchain_dir`` is in effect — use
    :func:`resolved_toolchain_home` whenever the test configures one.
    """
    return project / ".ocx" / "toolchain"


def resolved_toolchain_home(
    ocx: OcxRunner, cwd: Path, env_extra: dict[str, str] | None = None
) -> Path:
    """The home ``ocx`` itself resolves for ``cwd`` (C-056).

    Reads the ``toolchain_home`` contract field of ``ocx --format json shell
    state``, which is the supported discovery path when ``toolchain_dir``
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


def links_root(home: Path) -> Path:
    """``<home>/links`` — the one depth-1 directory holding group directories.

    Every ``<group>/<entry>`` question goes through this or through
    :func:`links_group` / :func:`entry_link`, never through a literal join onto
    the home root: a test that spells the layout itself keeps passing after the
    layout moves, because it moved its own expectation with it.
    """
    return home / LINKS_DIR


def links_group(home: Path, group: Path | str) -> Path:
    """``<home>/links/<group>`` — one group's directory of entry links."""
    return links_root(home) / str(group)


def entry_link(home: Path, group: Path | str, entry: Path | str) -> Path:
    """``<home>/links/<group>/<entry>`` — the link a ``[tools]`` key renders as.

    Built on :func:`links_group` rather than beside it, for the reason the
    grammar's own ``entry()`` is built on ``links_group()``: nothing but this
    construction makes ``entry_link(h, g, e).parent == links_group(h, g)`` true
    by definition.
    """
    return links_group(home, group) / str(entry)


def active_link(home: Path) -> Path:
    """``<home>/active`` — the depth-1 link every ``PATH`` route resolves
    through (C-078). A link, never a directory; its one legal target is
    ``shells/<shell>``."""
    return home / ACTIVE_LINK


def shell_bin(home: Path, shell: str = DEFAULT_SHELL) -> Path:
    """``<home>/shells/<shell>/bin`` — the **physical** trampoline directory.

    Where the renderer writes. Every observation of *what a render produced*
    uses this and never :func:`path_facing_bin`, so a repointed ``active``
    cannot make a stale tree look freshly rendered.
    """
    return home / SHELLS_DIR / shell / "bin"


def path_facing_bin(home: Path) -> Path:
    """``<home>/active/bin`` — the spelling that goes on ``PATH`` (C-078).

    Equal to :func:`shell_bin` only *through* the link, never by string
    equality. Use it only where the property under test is the indirection
    itself.
    """
    return active_link(home) / "bin"


def resolved_toolchain_bin(
    ocx: OcxRunner, cwd: Path, env_extra: dict[str, str] | None = None
) -> Path:
    """The launcher directory ``ocx`` itself reports for ``cwd`` (G-1, S-004).

    Reads the ``toolchain_bin`` contract field of ``ocx --format json shell
    state`` — the supported discovery path for a consumer that needs the
    directory to put on ``PATH``. It is *not* ``toolchain_home + "/bin"``: that
    concatenation names a directory which does not exist under this layout and
    still exits 0, which is the silent failure the field replaces.
    """
    result = run_in(ocx, cwd, "--format", "json", "shell", "state", env_extra=env_extra)
    assert result.returncode == EXIT_SUCCESS, (
        f"`ocx shell state` must report the launcher directory; "
        f"rc={result.returncode}\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert "toolchain_bin" in payload, (
        f"RUL-51 — `toolchain_bin` is present whenever `toolchain_home` is; "
        f"got keys {sorted(payload)}"
    )
    return Path(payload["toolchain_bin"])


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
    """Declare ``toolchain_dir = <value>`` in the home-tier ``config.toml``.

    The tier a refusal names is ``config.toml `toolchain_dir```; the environment
    tier is reached instead by passing ``OCX_TOOLCHAIN_DIR`` through
    ``env_extra``.
    """
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    text = value.as_posix() if isinstance(value, Path) else str(value)
    path.write_text(f'toolchain_dir = "{text}"\n')
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


def bin_entries(home: Path, shell: str = DEFAULT_SHELL) -> list[str]:
    """The on-disk entry names under :func:`shell_bin` — the set
    :external+ocx:``RenderStamp::names`` claims to mirror.

    The **physical** directory, deliberately: reading through ``active`` would
    make "the render wrote these names" and "``active`` currently points at a
    tree holding these names" the same observation, and the second is true of a
    stale tree a repoint aimed at. :func:`path_facing_bin` is the other
    spelling, for the rows whose subject *is* the indirection.
    """
    directory = shell_bin(home, shell)
    return sorted(p.name for p in directory.iterdir()) if directory.is_dir() else []


def link_entries(home: Path) -> dict[str, str]:
    """Every ``links/<group>/<entry>`` link in ``home``, as
    ``"group/entry" → target``.

    Keys stay ``"<group>/<entry>"`` with no ``links/`` prefix — that is the
    shape the render stamp's ``link_fingerprint`` uses, and re-spelling it here
    would make a stamp assertion compare two things this helper wrote.

    Only ``links/`` is walked. The tree's other depth-1 names hold no entry
    links, and folding them in would let an assertion about one namespace be
    satisfied by another.
    """
    out: dict[str, str] = {}
    root = links_root(home)
    if not root.is_dir():
        return out
    for group in sorted(root.iterdir()):
        if not group.is_dir() or group.is_symlink():
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
        """``<home>/links/default/<[tools] key>`` — the default group's link."""
        return entry_link(self.home, DEFAULT_GROUP, self.default_key)

    @property
    def group_link(self) -> Path:
        """``<home>/links/<group>/<key>`` — the named group's link."""
        return entry_link(self.home, self.group, self.group_key)

    @property
    def default_trampoline(self) -> Path:
        """``<home>/shells/default/bin/<binary name>`` — keyed on the exposed
        name, never the ``[tools]`` key, and named by the **physical**
        spelling, not through ``active``."""
        return shell_bin(self.home) / self.default_binary

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

    # Both helpers answer empty on a missing tree, so without these two lines
    # the whole negative is satisfied by a home that was never rendered — and
    # every consumer inherits that, which is the opposite of what the docstring
    # promises.
    assert names, (
        f"the negative is vacuous on an unrendered tree: no launchers under "
        f"{project.home}"
    )
    assert links, (
        f"the negative is vacuous on an unrendered tree: no links under "
        f"{project.home}"
    )

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

    The two branches differ in **three** independent ways, because a checkout
    that swaps ``ocx.toml`` and ``ocx.lock`` can change any of them and S-001
    asks that the rendered tree match the new lock *exactly*:

    ===================== ================================ ============================
    what differs          ``main``                         ``other``
    ===================== ================================ ============================
    a shared tool's digest ``key`` → ``package_main``       ``key`` → ``package_other``
    the default tool set   ``+ main_only_key``              ``+ other_only_key``
    the group set          ``[group.<main_group>]``         ``[group.<other_group>]``
    ===================== ================================ ============================

    The shared ``key`` is what makes a branch switch *stale* rather than merely
    different (S-006): same key, same exposed binary, two digests. The other two
    axes are what make a departed tool's trampoline and a departed group's
    directory observable at all — with only the shared key, both sets are
    identical on both branches and every prune assertion is vacuous.

    The two group tools point at ``package_main``/``package_other`` rather than
    at packages of their own: a group entry is a link and never a trampoline
    (C-045), so no exposed-binary collision is possible and two more registry
    pushes per call would buy nothing.
    """

    directory: Path
    home: Path

    key: str
    binary: str
    package_main: PackageInfo
    package_other: PackageInfo

    main_only_key: str
    main_only_binary: str
    main_only_package: PackageInfo

    other_only_key: str
    other_only_binary: str
    other_only_package: PackageInfo

    main_group: str
    main_group_key: str
    other_group: str
    other_group_key: str

    main_branch: str = "main"
    other_branch: str = "other"

    @property
    def link(self) -> Path:
        """``<home>/links/default/<key>`` — the link a branch switch makes
        stale."""
        return entry_link(self.home, DEFAULT_GROUP, self.key)

    @property
    def main_only_link(self) -> Path:
        """The default-group link that exists on ``main`` and nowhere else."""
        return entry_link(self.home, DEFAULT_GROUP, self.main_only_key)

    @property
    def other_only_link(self) -> Path:
        """The default-group link that exists on ``other`` and nowhere else."""
        return entry_link(self.home, DEFAULT_GROUP, self.other_only_key)

    @property
    def main_group_dir(self) -> Path:
        """``<home>/links/<main_group>`` — the group directory ``other`` does
        not declare."""
        return links_group(self.home, self.main_group)

    @property
    def other_group_dir(self) -> Path:
        """``<home>/links/<other_group>`` — the group directory ``main`` does
        not declare."""
        return links_group(self.home, self.other_group)

    def main_group_link_present(self) -> bool:
        """Whether ``main``'s group link is still on disk, as a **link**.

        ``exists()`` would follow it and answer about the digest root instead —
        and the digest root outlives the link, so that spelling is true in both
        states this predicate exists to tell apart.
        """
        return (self.main_group_dir / self.main_group_key).is_symlink()

    def expected_links(self, branch: str) -> set[str]:
        """The ``"group/entry"`` keys ``branch``'s ``ocx.lock`` declares.

        Derived from what this fixture *wrote into* ``ocx.toml``, never from the
        tree — an expectation read back off the tree would be satisfied by any
        tree at all.
        """
        if branch == self.main_branch:
            return {
                f"{DEFAULT_GROUP}/{self.key}",
                f"{DEFAULT_GROUP}/{self.main_only_key}",
                f"{self.main_group}/{self.main_group_key}",
            }
        return {
            f"{DEFAULT_GROUP}/{self.key}",
            f"{DEFAULT_GROUP}/{self.other_only_key}",
            f"{self.other_group}/{self.other_group_key}",
        }

    def expected_bin(self, branch: str) -> list[str]:
        """The exposed binary names ``branch`` puts in the trampoline directory.

        Default group only (C-045), so the group tool contributes nothing here —
        which is itself part of what the sync test pins.
        """
        if branch == self.main_branch:
            return sorted([self.binary, self.main_only_binary])
        return sorted([self.binary, self.other_only_binary])


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
    # One repository at two tags, not two repositories — and that is a
    # consent constraint, not an economy. `[shell.consent]` clause 1 grants a
    # stamped project only while its lock's source set stays a subset of the
    # stamped one, and a source is `<registry>/<first path segment>`
    # (`project/consent.rs`, `source_of`). Two repositories would make the
    # branch-only tool a *new source* on the far side of every checkout, so
    # the switch would answer with the consent refusal and no staleness row
    # would ever reach the question it is about. The tool set still differs
    # across the branches — different `[tools]` key, different exposed binary,
    # different digest — which is what S-001 asks of this fixture.
    main_only_package = make_package(
        ocx,
        f"t_{label}_only",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[f"mobin{label}"],
    )
    other_only_package = make_package(
        ocx,
        f"t_{label}_only",
        "2.0.0",
        tmp_path,
        cascade=False,
        bins=[f"oobin{label}"],
    )

    checkout = TwoBranchCheckout(
        directory=tmp_path / f"checkout-{label}",
        home=toolchain_home(tmp_path / f"checkout-{label}"),
        key=key,
        binary=f"brbin{label}",
        package_main=package_main,
        package_other=package_other,
        main_only_key=f"mokey{label}",
        main_only_binary=f"mobin{label}",
        main_only_package=main_only_package,
        other_only_key=f"ookey{label}",
        other_only_binary=f"oobin{label}",
        other_only_package=other_only_package,
        main_group=f"mgrp{label}",
        main_group_key=f"mgkey{label}",
        other_group=f"ogrp{label}",
        other_group_key=f"ogkey{label}",
    )

    def body(
        shared: PackageInfo,
        only_key: str,
        only: PackageInfo,
        group: str,
        group_key: str,
    ) -> str:
        return (
            f"[tools]\n"
            f'{key} = "{shared.fq}"\n'
            f'{only_key} = "{only.fq}"\n\n'
            f"[group.{group}.tools]\n"
            f'{group_key} = "{shared.fq}"\n'
        )

    seed = tmp_path / f"seed-{label}"
    seed.mkdir()
    _git_ok(seed, "init", "-q", "-b", "main")
    branches = (
        (
            None,
            body(
                package_main,
                checkout.main_only_key,
                main_only_package,
                checkout.main_group,
                checkout.main_group_key,
            ),
            package_main.tag,
        ),
        (
            "other",
            body(
                package_other,
                checkout.other_only_key,
                other_only_package,
                checkout.other_group,
                checkout.other_group_key,
            ),
            package_other.tag,
        ),
    )
    for branch, config, tag in branches:
        if branch:
            _git_ok(seed, "checkout", "-q", "-b", branch)
        write_ocx_toml(seed, config)
        lock = run_in(ocx, seed, "lock")
        assert lock.returncode == EXIT_SUCCESS, (
            f"seeding `ocx lock` failed:\n{lock.stderr}"
        )
        _git_ok(seed, "add", "-A")
        _git_ok(seed, "commit", "-qm", f"lock {tag}")
    _git_ok(seed, "checkout", "-q", "main")

    bare = tmp_path / f"origin-{label}.git"
    _git_ok(tmp_path, "clone", "-q", "--bare", str(seed), str(bare))
    _git_ok(tmp_path, "clone", "-q", str(bare), str(checkout.directory))

    return checkout
