# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A recording `git` shim for the `--transport git` acceptance suite.

Everything ocx writes into its own workspace lives inside a tempdir ocx removes
on every path that unwinds (C-033), so by the time a test looks, the evidence is
gone. This module generates a `git` executable that ocx resolves off `PATH`
instead of the real one; it records the invocation, **runs the real `git`**, then
records again — after the child ran and before the directory disappears.

Three properties are load-bearing and each has a reason a shortcut fails:

* **Wrap, never `execv`.** `subprocess.run(real_git, argv[1:])` with stdio
  inherited, then the post-run snapshot, then `sys.exit(rc)`. An `execv` shim
  replaces itself with `git` and can observe nothing after the child. Named
  residual: a signal-killed child surfaces here as `128 + n`, not as a signal
  death, which interacts with C-019's `kill_on_drop`.
* **Both paths are baked into the generated source.** C-035's child-environment
  allowlist passes no `__OCX_TESTING_*` variable, so an env-var channel telling
  the shim where to write does not survive into the child. The real `git` is
  resolved with `shutil.which` **before** the shim's directory reaches `PATH`,
  so the shim cannot find itself.
* **The shebang is `sys.executable`.** The uv venv interpreter is not on the
  child's `PATH`, so `#!/usr/bin/env python3` would resolve to a different (or
  no) interpreter inside a run whose `PATH` the test controls.

Placement is validated twice: the target must resolve **under the caller's
`tmp_path`**, and it must resolve outside every path in `DENIED_SHIM_ROOTS`. The
second check is not redundant — it is the half that still holds when a caller
passes the wrong root, which is the only failure mode the first cannot see. The
shim must never land in `test/bin/` or under `~/.ocx/`: this repository stages
`test/bin/ocx` and puts `~/.ocx/**` on `PATH` through direnv, so a `git` in
either would shadow the real `git` for every other acceptance module, for
`task verify`, and for the developer's own shell. The validation exists because
"there is no `git` in `test/bin/`" cannot be turned red without writing one.

**POSIX-only, by construction.** A `PATH` shim named `git` with no extension is
not found by Windows' `CreateProcess` unless `PATHEXT` lists extensionless
scripts, which it does not — git's own test suite compiles `test-fake-ssh.c`
for exactly this reason. `subsystem-tests.md` states the pytest leg is Linux-only
in CI, so a script is sufficient here; a Windows run needs a `.cmd` or compiled
variant and this module does not pretend otherwise.
"""

from __future__ import annotations

import base64
import dataclasses
import enum
import json
import os
import shutil
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path

#: File name of the generated executable. The point of the whole module is that
#: `git` resolves to it, so it is not configurable.
SHIM_NAME = "git"

#: The JSONL file the generated shim appends one object to per invocation. Its
#: absolute path is baked into the shim's source at generation time.
RECORD_NAME = "git-invocations.jsonl"

#: Files the shim snapshots out of the git directory **at exit**, relative to it
#: and glob-expanded. Every one is a surface S-030 names, and every one lives
#: inside the tempdir ocx removes on unwind, so the shim is the only observer.
#:
#: * `config` — the remote URL and any `extraHeader` written into it; also what
#:   `git remote -v` prints, so both of S-030's first two surfaces come from here.
#: * `logs/HEAD`, `logs/refs/**` — **the reflog**, the third surface, and the one
#:   that is not derivable from `config`: `git fetch <url>` writes the fetched URL
#:   into `logs/refs/remotes/**`, which is precisely the leak the clause exists
#:   for.
#:
#: A `Mapping` snapshot over a named set rather than one `git_config: bytes`
#: field: the reflog would otherwise need a second field minted later, and a
#: capture shape is a one-way door once the tester has written against it.
SNAPSHOT_PATHS: Sequence[str] = ("config", "logs/HEAD", "logs/refs/**/*")

#: Absolute roots a shim may never be written under, whatever `tmp_path` a
#: caller passes. The repository tree (this file's own worktree root) stages
#: `test/bin/ocx`; `~/.ocx` is on `PATH` through direnv. A `git` in either
#: shadows the real one for every other acceptance module and for the
#: developer's shell.
DENIED_SHIM_ROOTS: Sequence[Path] = (
    Path(__file__).resolve().parents[2],
    Path.home() / ".ocx",
)


class GitShimVariant(enum.StrEnum):
    """The four arrangements S-021 and C-075 need.

    `DELEGATE` is the ordinary recording shim. The two version variants answer
    `git --version` with a fixed string and delegate everything else, so the
    argv-boundary gate (C-065) sees a version the host does not have. `ABSENT`
    installs **no** `git` at all — an empty directory that becomes the child's
    whole `PATH`, which is the only way to exercise "no `git` resolvable".

    `VERSION_2_31_0` is not redundant with `DELEGATE`: C-075 requires the gate's
    **accept** side to be proved at the exact boundary release, because a gate
    that refuses everything passes every refusal test.
    """

    DELEGATE = "delegate"
    VERSION_2_30_9 = "version-2.30.9"
    VERSION_2_31_0 = "version-2.31.0"
    ABSENT = "absent"


#: The `git --version` line each spoofing variant answers with, verbatim. `None`
#: means "ask the real `git`".
VERSION_OVERRIDES: Mapping[GitShimVariant, str | None] = {
    GitShimVariant.DELEGATE: None,
    GitShimVariant.VERSION_2_30_9: "git version 2.30.9",
    GitShimVariant.VERSION_2_31_0: "git version 2.31.0",
    GitShimVariant.ABSENT: None,
}


class ShimPlacementError(ValueError):
    """Raised when a shim would be installed outside the test's `tmp_path`."""


#: The generated shim's imports. Split from `_SHIM_BODY` only so the baked-in
#: literals can sit between the two, where a reader looking for "where does this
#: write?" finds them without reading the logic.
_SHIM_PREAMBLE = '''\
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Generated by `test/tests/git_shim.py` — edit the renderer, never this file."""
import base64
import json
import os
import subprocess
import sys
from pathlib import Path
'''

#: The generated shim's logic: record at entry, run the real `git` with stdio
#: inherited, snapshot the git directory, append the record, exit with the
#: child's own code. Never `execv` — an `execv` shim replaces itself and can
#: observe nothing after the child.
_SHIM_BODY = '''\
SHIM_PATH = Path(__file__).resolve()


def dash_c_directory(arguments):
    """The first `-C <dir>` value, or None. git accepts several and composes
    them; the fixture drives one, and the first is the one C-033's mode check
    is about."""
    for index, argument in enumerate(arguments):
        if argument == "-C" and index + 1 < len(arguments):
            return arguments[index + 1]
    return None


def git_directory(directory):
    """`<dir>/.git` for a worktree, `<dir>` for a bare repository.

    A `.git` FILE — a submodule, or a linked worktree — is deliberately NOT
    followed, and neither is a repository that lives in a SUBdirectory of the
    `-C` target (`git -C <ws> clone <url> repo` puts it at `<ws>/repo/.git`).
    In both cases this answers the plain directory, `is_git_directory` then
    reads False, and the record says so — rather than quietly snapshotting
    nothing and leaving a later "the secret is in no snapshot" loop vacuous."""
    candidate = Path(directory)
    nested = candidate / ".git"
    return nested if nested.is_dir() else candidate


def is_git_directory(path):
    """Whether `path` is a git directory at all.

    `HEAD` is present in a bare repository and in a worktree's `.git`, and in
    no plain directory — so this is what separates "snapshotted nothing because
    nothing leaked" from "snapshotted nothing because it never looked at a
    repository"."""
    return (path / "HEAD").is_file()


def snapshot(directory):
    """Every `SNAPSHOT_PATHS` entry that exists, glob-expanded, as base64.

    Entries that do not exist are ABSENT rather than empty: a test asserting a
    secret is missing from a surface must first assert the surface is there,
    or it asserts a negative over nothing."""
    captured = {}
    for pattern in SNAPSHOT_PATHS:
        for match in sorted(directory.glob(pattern)):
            if match.is_file():
                key = match.relative_to(directory).as_posix()
                captured[key] = base64.b64encode(match.read_bytes()).decode("ascii")
    return captured


def main():
    arguments = sys.argv[1:]
    cwd = os.getcwd()
    dash_c = dash_c_directory(arguments)
    try:
        # Read at ENTRY, before the child can change it and long before ocx
        # removes the directory — the only moment C-033's 0700 is observable.
        mode = os.stat(dash_c if dash_c is not None else cwd).st_mode
    except OSError:
        mode = None

    if VERSION_OVERRIDE is not None and arguments == ["--version"]:
        sys.stdout.write(VERSION_OVERRIDE + "\\n")
        sys.stdout.flush()
        returncode = 0
    elif REAL_GIT is None:
        sys.stderr.write("git shim: no delegate was baked in\\n")
        returncode = 127
    else:
        returncode = subprocess.run([REAL_GIT] + arguments, check=False).returncode

    git_dir = git_directory(dash_c if dash_c is not None else cwd)
    looked = is_git_directory(git_dir)
    record = {
        "argv": [str(SHIM_PATH)] + arguments,
        "env": dict(os.environ),
        "cwd": cwd,
        "dash_c_dir": dash_c,
        "dir_mode": mode,
        "git_dir": str(git_dir),
        "git_dir_exists": looked,
        "snapshot": snapshot(git_dir) if looked else {},
        "returncode": returncode,
    }
    RECORD_PATH.parent.mkdir(parents=True, exist_ok=True)
    # 0600, not the umask default: this record carries the child's WHOLE
    # environment, which under C-034 is where the injected credential lives.
    # `os.open` is the only way to fix the mode at creation; `Path.open("a")`
    # creates 0644 and a later chmod leaves a window.
    handle = os.open(str(RECORD_PATH), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    with os.fdopen(handle, "a", encoding="utf-8") as log:
        log.write(json.dumps(record) + "\\n")
    sys.exit(returncode)


main()
'''


@dataclasses.dataclass(slots=True, frozen=True)
class GitInvocation:
    """One `git` invocation the shim observed, as evidence for S-030 and C-033.

    `dir_mode` is `os.stat(...).st_mode` of `dash_c_dir` (or `cwd` when the
    invocation carried no `-C`) read **at entry**, before the child could change
    it and long before ocx removes it — the only moment C-033's Unix-only `0700`
    is observable.

    `git_dir` is the directory `snapshot` was actually taken over, and
    `git_dir_exists` says whether that directory was a git directory at all.
    They are the **positive that guards the negative**: `git -C <ws> clone <url>
    repo` resolves `git_dir` to `<ws>` while the repository lands at
    `<ws>/repo/.git`, and so do a `-C` into a subdirectory, a linked worktree
    and a submodule whose `.git` is a file. In every one of those the snapshot
    is empty, and the loop shape S-030 invites — iterate invocations, assert the
    secret is in no snapshot value — is then vacuous on a whole run. Assert
    `git_dir_exists` on at least one invocation before trusting that loop.

    `snapshot` maps each `SNAPSHOT_PATHS` entry that existed to its raw bytes,
    read **at exit** — the files do not exist until the child has written them.
    It covers all three surfaces S-030 names: `.git/config` (which is also what
    `git remote -v` prints) and the reflog. Keys are relative to `git_dir`.

    Entries that did not exist are **absent**, not empty — so a test asserting a
    secret is absent from a surface must first assert that surface's key is
    present. `logs/HEAD` depends on `core.logAllRefUpdates` and a no-checkout
    workspace may never write it, in which case `secret not in
    snapshot["logs/HEAD"]` would be a negative over an empty set: green in every
    state, including the one where the secret leaked somewhere else.

    `env` is the child's **whole** environment, not a filtered view — C-035's
    tables are asserted by name against it, and a filtered capture could not
    tell "absent" from "filtered out by the fixture".

    It carries one residual of its own, beside the `128 + n` one the module
    docstring names for `returncode`: **PEP 538 locale coercion**. When the
    child environment carries no `LC_ALL` / `LC_CTYPE` / `LANG`, CPython adds
    `LC_CTYPE=C.UTF-8` to *its own* environment at interpreter init — and this
    shim is a CPython process — so the recorded `env` can hold a key ocx never
    set. C-033's shape (`LC_ALL=C`) suppresses it. That matters at exactly one
    moment: the mutation that must red S-031 is deleting `LC_ALL=C` from the
    allowlist table, which is also the mutation that grows the extra key. The
    failure then names `LC_CTYPE` and invites widening the expected set instead
    of restoring the row. There is no code fix — coercion happens before any
    line of the shim runs.

    `argv` is likewise the child's **whole** argv, slot zero included: the
    shim's own resolved path, then every argument it was given. Slot zero
    carries the resolved path rather than the spelling the caller used, because
    that is the only field that says *which binary ran* —
    `::test_shim_does_not_recurse_into_itself` reads it, and WP-17 scans the
    later slots for `-c http.followRedirects=false` and `-c credential.helper=`.
    A capture that dropped or filtered slots could not tell "the flag was
    absent" from "the fixture dropped it".
    """

    argv: Sequence[str]
    env: Mapping[str, str]
    cwd: str
    dash_c_dir: str | None
    dir_mode: int | None
    git_dir: str
    git_dir_exists: bool
    snapshot: Mapping[str, bytes]
    returncode: int


@dataclasses.dataclass(slots=True, frozen=True)
class GitShim:
    """An installed shim: where it lives, what it records, what it delegates to.

    `directory` is the **only** directory a test prepends to the child `PATH`;
    build the value with `child_path` rather than by hand, because the `ABSENT`
    variant's contract is that it *replaces* `PATH` instead of extending it.
    """

    directory: Path
    executable: Path | None
    record_path: Path
    real_git: Path | None
    variant: GitShimVariant

    def child_path(self, inherited: str | None = None) -> str:
        """The value to set as the child process's `PATH`.

        For every variant but `ABSENT` this is `directory` prepended to
        `inherited` (defaulting to the current process's `PATH`), so the shim
        shadows the real `git` while everything else still resolves. For
        `ABSENT` it is `directory` **alone** — an empty directory — because
        appending an inherited `PATH` would let the real `git` resolve and the
        "no `git` at all" case would silently become the ordinary one.
        """
        if self.variant is GitShimVariant.ABSENT:
            return str(self.directory)
        base = os.environ.get("PATH", "") if inherited is None else inherited
        if not base:
            return str(self.directory)
        return os.pathsep.join((str(self.directory), base))

    def invocations(self) -> list[GitInvocation]:
        """Every recorded invocation, in the order the shim observed them.

        Re-read from `record_path` on each call: the shim is a separate process
        that appends while the test is blocked in `ocx`, so a value cached at
        fixture time would always be empty.
        """
        if not self.record_path.exists():
            return []
        recorded: list[GitInvocation] = []
        for line in self.record_path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            payload = json.loads(line)
            recorded.append(
                GitInvocation(
                    argv=tuple(payload["argv"]),
                    env=dict(payload["env"]),
                    cwd=payload["cwd"],
                    dash_c_dir=payload["dash_c_dir"],
                    dir_mode=payload["dir_mode"],
                    git_dir=payload["git_dir"],
                    git_dir_exists=payload["git_dir_exists"],
                    # Snapshotted bytes travel base64-encoded: JSON carries no
                    # byte string, and `.git/config` is text only by convention.
                    snapshot={
                        name: base64.b64decode(blob)
                        for name, blob in payload["snapshot"].items()
                    },
                    returncode=payload["returncode"],
                )
            )
        return recorded


def resolve_real_git() -> Path:
    """The real `git`, resolved with `shutil.which` against the **current**
    `PATH`.

    Call this before any shim directory reaches `PATH`. Baking the result into
    the shim's source is what stops the shim resolving itself and recursing —
    `::test_shim_does_not_recurse_into_itself` is the control.

    Raises `FileNotFoundError` when no `git` is on `PATH`: a fixture that
    silently produced a shim with no delegate would make every "the real git ran"
    assertion pass against a shim that never ran anything.
    """
    found = shutil.which(SHIM_NAME)
    if found is None:
        raise FileNotFoundError(
            f"no {SHIM_NAME!r} on PATH: a shim with no delegate would make every "
            "'the real git ran' assertion pass against a shim that ran nothing"
        )
    return Path(found)


def render_shim_source(
    *,
    record_path: Path,
    real_git: Path | None,
    version_override: str | None,
) -> str:
    """The generated shim's complete source text.

    Public because the two properties that make the shim work are properties of
    this *string*, testable without installing anything: the first line is
    `#!` + `sys.executable`, and both `record_path` and `real_git` appear in it
    as literals. C-035's allowlist forwards no `__OCX_TESTING_*` variable, so an
    env-var channel would leave the shim writing nowhere inside the very run it
    is meant to observe — which is why the two literal checks are the test, and
    why a source scan for the spelling `os.environ` is **not**: the shim reads
    `os.environ` legitimately, to record it, and a grep for the dotted form
    measures how the import was written rather than where the paths came from.

    `version_override` bakes a fixed `git --version` answer; `None` delegates the
    version query like any other subcommand.
    """
    literals = "\n".join(
        (
            f"RECORD_PATH = Path({str(record_path)!r})",
            f"REAL_GIT = {(None if real_git is None else str(real_git))!r}",
            f"VERSION_OVERRIDE = {version_override!r}",
            f"SNAPSHOT_PATHS = {tuple(SNAPSHOT_PATHS)!r}",
        )
    )
    return f"#!{sys.executable}\n{_SHIM_PREAMBLE}\n{literals}\n\n\n{_SHIM_BODY}"


def install_git_shim(
    tmp_path: Path,
    *,
    variant: GitShimVariant = GitShimVariant.DELEGATE,
    real_git: Path | None = None,
    subdirectory: str = "git_shim_bin",
) -> GitShim:
    """Write a recording `git` shim into a fresh directory under `tmp_path`.

    Placement is checked on **resolved** paths with `Path.is_relative_to`, never
    a string prefix, and `ShimPlacementError` is raised on either failure:

    * the target must be under `tmp_path.resolve()` — a `subdirectory` of
      `../../../bin` defeats a string compare but not this one, and macOS's
      `/tmp` -> `/private/tmp` symlink makes an unresolved compare
      false-negative on a path that *is* inside `tmp_path`;
    * the target must be outside every `DENIED_SHIM_ROOTS` entry — the half that
      still holds when the caller passes the wrong root, which is the structural
      guarantee rather than a restatement of the first check.

    The directory contains **only** the shim (or, for `ABSENT`, nothing at all),
    so prepending it to `PATH` shadows `git` and nothing else.

    `real_git` defaults to `resolve_real_git()`, evaluated here, before the
    caller has had any chance to put the new directory on `PATH`.
    """
    # The PATH directory holds ONLY the shim, so the record file lives one level
    # up: prepending a directory that also carries a JSONL log would still work,
    # but "the directory contains only the shim" is the property that makes
    # `child_path` safe to hand to any child, and it is cheaper to keep than to
    # re-argue at every call site.
    home = tmp_path / subdirectory
    directory = (home / "bin").resolve()

    # Resolved-path checks, never a string prefix: `../../../bin` defeats a
    # prefix compare, and macOS's /tmp -> /private/tmp symlink makes an
    # unresolved compare false-negative on a path that IS inside `tmp_path`.
    if not directory.is_relative_to(tmp_path.resolve()):
        raise ShimPlacementError(
            f"{directory} escapes the test's tmp_path {tmp_path.resolve()}"
        )
    for denied in DENIED_SHIM_ROOTS:
        if directory.is_relative_to(denied.resolve()):
            raise ShimPlacementError(
                f"{directory} is under {denied}, which is on PATH for the whole "
                "tree: a `git` there shadows the real one for every other "
                "acceptance module and for the developer's own shell"
            )

    if real_git is None:
        real_git = resolve_real_git()

    directory.mkdir(parents=True, exist_ok=True)
    record_path = directory.parent / RECORD_NAME
    executable: Path | None = None
    if variant is not GitShimVariant.ABSENT:
        executable = directory / SHIM_NAME
        executable.write_text(
            render_shim_source(
                record_path=record_path,
                real_git=real_git,
                version_override=VERSION_OVERRIDES[variant],
            ),
            encoding="utf-8",
        )
        executable.chmod(0o755)

    return GitShim(
        directory=directory,
        executable=executable,
        record_path=record_path,
        real_git=real_git,
        variant=variant,
    )


__all__ = [
    "DENIED_SHIM_ROOTS",
    "RECORD_NAME",
    "SHIM_NAME",
    "SNAPSHOT_PATHS",
    "VERSION_OVERRIDES",
    "GitInvocation",
    "GitShim",
    "GitShimVariant",
    "ShimPlacementError",
    "install_git_shim",
    "render_shim_source",
    "resolve_real_git",
]
