#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The commit and push gate git itself runs.

Two entry points, because the two hooks are installed by different owners:
`commit-msg` is a prek hook declared in `.pre-commit-config.yaml`, and
`pre-push` is `scripts/pre-push.sh`, copied into git's hooks directory —
prek hands a declared hook no stdin and skips the pre-push run for a delete
or a ref-creating `--all`, which the push gate must see. Both armed by
`task git:hooks`.

    scripts/commit_gate.py --check-message <msg-file>   # prek appends it
    scripts/commit_gate.py --check-push <repo-root>          # git's pre-push stdin
    scripts/commit_gate.py --require-full-mark <repo-root>   # release:prepare's guard
    scripts/commit_gate.py --self-test

Why git runs this and not a Claude PreToolUse hook: deciding "is this shell
command a git commit" means parsing an arbitrary shell string, and every round
of patching that tokenizer produced either a new bypass (a global option it did
not know took a value, a command substitution, an interpreter's `-e` script) or
a new outage (a heredoc it could not lex, a comment that merely said the word
"git"). git has no such problem. It invokes `commit-msg` for every commit and
`pre-push` for every push, however either was spelled, and hands the hook the
finished message and the already-resolved refspecs. There is nothing left to
parse, so the whole family of bypasses and over-blocks is unreachable rather
than fixed.

`--check-message` rules, in order, each stating its own reason on stderr:

  1. Git state bends rules 3 to 5, and how far depends on which state (D39).
     A REBASE is fully exempt: git rewrites HEAD through intermediate states
     no mark can certify, and a `reword` hands this hook a message git has
     already consumed, so there is nothing left to compare a subject against.
     A merge, cherry-pick or revert handed back to the caller exempts the
     SUBJECT only, and only the subject git itself prepared in `MERGE_MSG` —
     this repository's merge subjects (`Merge WP-41 DX-52: ...`) are not
     conventional and were never judged, because `git merge` contains no
     `git commit` for the old hook to find. The mark rules still apply there:
     HEAD is the pre-operation commit and the working tree is the one about
     to be committed, which is exactly what a mark certifies. Keying on the
     state file alone was D39: `git revert -n` and `git merge --no-commit`
     reach that state without git ever stopping, and bought an arbitrary
     subject over arbitrary staged content with no mark, on any branch. The
     test is still never the subject text alone — a `Merge ...` *prefix*
     carve-out would let any commit skip the gate by renaming itself. The
     accepted cost is that `git commit --amend` on a merge commit OUTSIDE a
     rebase is refused as non-conventional; do it inside the rebase that
     reworded it.
  2. The subject `Checkpoint` is ALLOWED with no mark. Deliberate, and the
     ruling on deferred finding D26: `taskfile.yml` `checkpoint:` writes
     `git commit --allow-empty -m "Checkpoint"` and then amends it, a
     checkpoint is the working phase's amendable commit, and the gate that
     matters applies at finalize. Without this carve-out the gate breaks the
     documented dev cycle.
  3. A subject that is neither Conventional Commits nor `release:` is refused.
     `release:` is admitted here — it is not one of the conventional types, and
     rule 4 exists to gate it, so refusing it at rule 3 would make rule 4's
     release arm unreachable and refuse `task release:prepare`'s own commit.
  4. `release:` or a commit on main/master needs a *full* mark; anything else
     a *scoped* one (plan_crate_split_workspace.md C-021).
  5. The mark must be fresh (5-minute TTL), written at the current HEAD, of at
     least the required scope, and written BY THIS WORKING TREE. A mark
     carrying no `toplevel` reads as unverified.

Rule 5's last clause is R17: `scripts/scoped_gate.py` writes one mark per
*project dir*, so sibling worktrees branched from one HEAD all read and write
the same file. Discriminating on HEAD alone they certify each other's
unverified trees — fail-OPEN, inside one 5-minute window. The writer's
realpath'd working tree is recorded in the mark and compared here.

Stdlib only, and `--self-test` drives real git: it builds throwaway
repositories under `.tmp/`, arms each one the way `task git:hooks` arms a
clone — `prek install` against the real `.pre-commit-config.yaml`, plus the
real `scripts/pre-push.sh` — and runs actual `git commit` / `git push`
through a shell, asserting the exit code and the reason text — including with
the hooks disarmed, so a denial is attributable to the gate and not to a
broken fixture.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from scoped_gate import mark_file, read_mark, worktree_id, write_mark

REPO_ROOT = Path(__file__).resolve().parents[1]
PREK_CONFIG = REPO_ROOT / ".pre-commit-config.yaml"
PRE_PUSH_SCRIPT = REPO_ROOT / "scripts" / "pre-push.sh"

TTL_SECONDS = 300
TRUNK_BRANCHES = frozenset({"main", "master"})
CHECKPOINT_SUBJECT = "Checkpoint"
RELEASE_PREFIX = "release:"
# Subjects git itself generates for `rebase --autosquash` to consume.
_AUTOSQUASH_RE = re.compile(r"^(fixup|squash|amend)! ")


def _admitted_subject_form(subject: str) -> bool:
    """True for the subject forms rule 3 admits without being conventional."""
    return subject.startswith(RELEASE_PREFIX) or bool(_AUTOSQUASH_RE.match(subject))

# Verbatim from the retired conventional_commit_validator.py: the accepted
# types are CLAUDE.md's, and a commit refused here must be refused for the
# same reason it was before.
_CONVENTIONAL_RE = re.compile(
    r"^(feat|fix|refactor|ci|chore|docs|test|perf|build|style)(\(.+\))?(!)?\s*: .+"
)

# The per-worktree git dir holds one of these while an operation that replays
# or combines existing history is in flight. `--absolute-git-dir` is the
# per-worktree dir, not the common one, so a linked worktree's rebase is seen
# here and a sibling's is not. They are two groups because they earn two
# different carve-outs (D39).
#
# A REPLAY drives the commits itself. git's sequencer commits its own replays
# without running this hook at all (measured: a clean `cherry-pick`, a clean
# `revert` and a `rebase --continue` reach no `commit-msg`), HEAD walks
# through intermediate states no mark can certify, and during a `reword`
# `rebase-merge/message` is already empty — git consumed it — so a
# subject-versus-git comparison has nothing to read. Fully exempt; the
# accepted cost is that a hand-written commit made at a `rebase -i` stop is
# exempt too.
_REPLAY_MARKERS = (
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase"),
)

# A HANDBACK stops and leaves the commit to the caller. git wrote the subject
# it wants into `MERGE_MSG` (a merge's `-m` text or its generated `Merge
# branch ...`, a revert's `Revert "..."`, a pick's original subject) and left
# HEAD at the pre-operation commit — so the mark rules mean here exactly what
# they mean anywhere: the working tree about to be committed is the one a
# verify run certifies. Only the subject is exempt, and only git's own.
_HANDBACK_MARKERS = (
    ("MERGE_HEAD", "a merge"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
)

_PUSH_DENY = (
    "BLOCKED: Cannot push directly to {ref}. "
    "Trunk-based development requires:\n\n"
    "1. Create a feature branch: git checkout -b feature/your-change\n"
    "2. Commit your changes on the branch\n"
    "3. Push the branch: git push -u origin feature/your-change\n"
    "4. Create a PR for review\n\n"
    "Current branch: {branch}"
)

# Appended when {operation} is in flight: the subject rule bends for git's own
# subject, and this says how to reach it instead of guessing.
_CONCLUDING = (
    "\n\nConcluding {operation}: the subject git prepared is accepted as it stands"
    " (`git commit --no-edit`), a hand-written one must be conventional. Pick the"
    " merge subject at `git merge -m <subject>`, which is what git puts there."
)

_NOT_CONVENTIONAL = (
    "BLOCKED: Commit message does not follow conventional commits format.\n\n"
    'Got: "{subject}"\n\n'
    "Expected: <type>(<optional scope>): <description>\n"
    "Types: feat, fix, refactor, ci, chore, docs, test, perf, build, style\n"
    "Examples:\n"
    "  feat: add package search command\n"
    "  fix(oci): handle missing manifest gracefully\n"
    "  chore: update AI configuration"
)


# ---------------------------------------------------------------------------
# Asking git
# ---------------------------------------------------------------------------


def git_out(repo: str, *args: str) -> str | None:
    """Stripped stdout of `git <args>` in ``repo``, or None when git cannot answer.

    "Cannot answer" is any of: no `git` on PATH, ``repo`` missing, a 5-second
    hang, a non-zero exit, empty output. Every caller treats None as "no
    information" and fails closed on it — never as a value.
    """
    try:
        result = subprocess.run(
            ["git", "-C", repo, *args],
            capture_output=True,
            encoding="utf-8",
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def operation_in_progress(git_dir: str | None, markers: tuple[tuple[str, str], ...]) -> str | None:
    """Which of ``markers`` git left in ``git_dir``, described, or None."""
    if git_dir is None:
        return None
    for name, description in markers:
        if os.path.exists(os.path.join(git_dir, name)):
            return description
    return None


def prepared_subject(git_dir: str) -> str | None:
    """The subject git prepared for the operation it handed back, or None.

    `MERGE_MSG` is git's file, not the committer's: git writes it when the
    operation starts and `git commit --no-edit` is how the conclusion picks it
    up. Comment lines are skipped because git appends its own (`# Conflicts:`)
    — the opposite of `subject_from_file`, whose input is a human's message
    where a leading `#` is part of the subject.

    A missing or unreadable file is None, never a match: the subject rule then
    applies as it does to any other commit.
    """
    try:
        text = Path(git_dir, "MERGE_MSG").read_text(encoding="utf-8")
    except OSError:
        return None
    return next(
        (line.strip() for line in text.splitlines() if line.strip() and not line.startswith("#")),
        None,
    )


def subject_from_file(msg_path: str) -> str | None:
    """The first non-blank line of git's commit-message file, or None if empty.

    Deliberately NOT comment-aware. git strips comment lines only when the
    message is edited (`--cleanup` resolves to `strip`); under `-m` and `-F` it
    resolves to `whitespace` and a leading `#` is part of the subject. Since
    commit-msg cannot tell those apart, skipping comment lines made
    `git commit -m '#41 fix the thing'` — an ordinary issue reference — read as
    an empty message and skip every rule below it.

    So the first non-blank line wins whatever it looks like. The cost is that an
    editor commit abandoned with an empty message is refused here quoting git's
    own template line, one step before git would have aborted for the same
    reason; the commit does not happen either way.
    """
    try:
        text = Path(msg_path).read_text(encoding="utf-8")
    except OSError as problem:
        raise SystemExit(f"commit_gate: cannot read the message file: {problem}") from problem
    return next((line.strip() for line in text.splitlines() if line.strip()), None)


# ---------------------------------------------------------------------------
# The verify mark
# ---------------------------------------------------------------------------


def is_recently_verified(
    mark: dict,
    *,
    head: str | None,
    toplevel: str | None,
    require_full: bool,
    ttl_seconds: int = TTL_SECONDS,
) -> bool:
    """True when ``mark`` certifies THIS tree, at THIS head, at the needed scope.

    Fail-closed on every unknown: a missing, unparseable, bare-integer,
    scope-less, head-less, toplevel-less or timestamp-less mark is "not
    verified", and a `scoped` mark never satisfies ``require_full``.
    """
    if mark.get("scope") not in ("full", "scoped"):
        return False
    if require_full and mark["scope"] != "full":
        return False
    # A mark certifies the tree it was written on: same HEAD, same working
    # tree. Either unknown is a refusal, so one mark never covers a second
    # commit (re-mark with `task verify:mark`) and never covers a sibling
    # worktree sitting at the same HEAD (R17).
    if head is None or mark.get("head") != head:
        return False
    if toplevel is None or mark.get("toplevel") != toplevel:
        return False
    try:
        verified_time = int(mark["timestamp"])
    except (KeyError, TypeError, ValueError):
        return False
    # A timestamp in the future is not "fresh forever": clock skew or a
    # hand-edited mark reads as unverified.
    return 0 <= time.time() - verified_time < ttl_seconds


def build_deny_reason(mark: dict, state_path: str, why_full: str | None) -> str:
    """The refusal text, carrying the remediation a blocked commit needs."""
    found = f"scope: {mark.get('scope', 'none')}" if mark else "no JSON mark"
    if mark and mark.get("head"):
        found += f" for HEAD {str(mark['head'])[:10]} (a mark certifies the HEAD it was written on)"
    if mark and mark.get("toplevel"):
        found += f", written by {mark['toplevel']} (and the working tree that earned it)"
    if why_full:
        how = (
            f"This commit needs a FULL verify mark ({why_full}); found {found}.\n"
            "Run `task verify` (format, clippy, lint, license, build, tests) — it writes the"
            " full mark. A scoped run or `task verify:mark` does not count here.\n"
        )
    else:
        how = (
            f"Found {found} at {state_path}.\n"
            "Run `task verify` (format, clippy, lint, license, build, tests) or"
            " `task verify:scoped`; both write the mark. After a passing verify where"
            " only merge context changed: `task verify:mark`.\n"
        )
    return "BLOCKED: Cannot commit without passing verification.\n\n" + how + "\nThen retry."


# ---------------------------------------------------------------------------
# The three modes
# ---------------------------------------------------------------------------


def requires_full_mark(subject: str, repo_root: str) -> str | None:
    """Why this commit needs a *full* mark, or None when a scoped one will do."""
    if subject.startswith(RELEASE_PREFIX):
        return f"subject {subject!r} is a release"
    branch = git_out(repo_root, "rev-parse", "--abbrev-ref", "HEAD")
    if branch in TRUNK_BRANCHES:
        return f"the branch is {branch}"
    return None


def check_message(msg_path: str, repo_root: str) -> str | None:
    """The refusal reason for this commit, or None to let it through."""
    subject = subject_from_file(msg_path)
    # An empty message is a REFUSAL, never an allow: `--allow-empty-message`
    # means git will not refuse it, and letting it through here skipped every
    # rule below.
    if subject is None:
        return "BLOCKED: empty commit message — the gate cannot judge a commit with no subject."
    git_dir = git_out(repo_root, "rev-parse", "--absolute-git-dir")
    if operation_in_progress(git_dir, _REPLAY_MARKERS):
        return None
    if subject == CHECKPOINT_SUBJECT:
        return None
    # The handback carve-out, narrowed to the commit that concludes the
    # operation *on git's own subject* (D39). `git revert -n` and `git merge
    # --no-commit` leave the same state file behind with no conflict and no
    # stop, so the state file alone admitted any subject over any staged
    # content; the subject git prepared is what the caller cannot choose after
    # the fact. Rules 4 and 5 below are NOT skipped here.
    handback = operation_in_progress(git_dir, _HANDBACK_MARKERS)
    concludes = bool(handback) and git_dir is not None and subject == prepared_subject(git_dir)
    # Two subject forms that are not Conventional Commits and must still pass
    # rule 3, or the rule below them becomes unreachable:
    #   `release:`  — rule 4 exists to gate it, and it is the subject
    #                 `task release:prepare` prints. (The retired hook pair had
    #                 this backwards: the conventional validator denied every
    #                 `release:` subject before the full-mark rule ran at all.)
    #   `fixup!` /  — git generates these, `rebase --autosquash` consumes them,
    #   `squash!` /   and they never reach a changelog. The retired gate named
    #   `amend!`      `--fixup` as let-through for the same reason.
    # Neither is an escape hatch: rules 4 and 5 still apply to both.
    if not concludes and not _admitted_subject_form(subject) and not _CONVENTIONAL_RE.match(subject):
        refusal = _NOT_CONVENTIONAL.format(subject=subject)
        if handback:
            refusal += _CONCLUDING.format(operation=handback)
        return refusal
    why_full = requires_full_mark(subject, repo_root)
    path = mark_file()
    mark = read_mark(path)
    if is_recently_verified(
        mark,
        head=git_out(repo_root, "rev-parse", "HEAD"),
        toplevel=worktree_id(Path(repo_root)),
        require_full=why_full is not None,
    ):
        return None
    return build_deny_reason(mark, str(path), why_full)


def check_push(stdin_text: str, repo_root: str) -> str | None:
    """The refusal reason for this push, or None to let it through.

    git's pre-push stdin is one `<local-ref> <local-sha> <remote-ref>
    <remote-sha>` line per ref it resolved, so a bare `git push`, an alias,
    `HEAD:refs/heads/main` and a redirected invocation all arrive identically.
    An all-zero local sha is a DELETE of the remote ref; deleting the trunk is
    still a change to the trunk, so it gets no carve-out and the remote ref's
    own name decides.
    """
    for line in stdin_text.splitlines():
        fields = line.split()
        if len(fields) < 4:
            continue
        remote_ref = fields[2]
        if remote_ref.rsplit("/", 1)[-1] in TRUNK_BRANCHES:
            branch = git_out(repo_root, "rev-parse", "--abbrev-ref", "HEAD") or "unknown"
            return _PUSH_DENY.format(ref=remote_ref, branch=branch)
    return None


def require_full_mark_cli(repo_root: str) -> int:
    """`task release:prepare`'s guard: 0 on a fresh full mark, 1 otherwise."""
    path = mark_file()
    mark = read_mark(path)
    if is_recently_verified(
        mark,
        head=git_out(repo_root, "rev-parse", "HEAD"),
        toplevel=worktree_id(Path(repo_root)),
        require_full=True,
    ):
        print("verify mark: fresh full mark")
        return 0
    print(
        "BLOCKED: a fresh full verify mark (scope: full, 5-minute TTL) is required;"
        f" found scope: {mark.get('scope', 'none')}. Run `task verify` — `task verify:mark`"
        " writes a scoped mark and does not count.",
        file=sys.stderr,
    )
    return 1


# ---------------------------------------------------------------------------
# --self-test
#
# Every case drives real git through a real shell against a throwaway
# repository armed with the REAL config and the REAL push script. Nothing
# here calls `check_message` or `check_push` directly: the defect class this
# replaces lived in "does the gate ever get invoked", which a direct call
# cannot see — and under prek the invocation is a generated shim, so it is
# even less inspectable and even more worth exercising.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> str | None:
    return None if condition else problem


def _git(repo: Path, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        encoding="utf-8",
        env=env,
        check=False,
    )


def _env_for(project_dir: Path) -> dict[str, str]:
    """git's environment with the mark home pinned at ``project_dir``.

    `mark_file()` honours `CLAUDE_PROJECT_DIR` when it holds a `.claude/`, so
    this is how a scratch repo gets its own mark instead of the real one.
    """
    return {**os.environ, "CLAUDE_PROJECT_DIR": str(project_dir)}


def _arm(repo: Path) -> None:
    """Install both hooks into ``repo``, the way `task git:hooks` does.

    The config and the push script are the repository's own, so a case that
    goes green went green against what a developer actually runs. `prek
    install` writes its shim into this repo's own `.git/hooks`, and the push
    script is copied there.

    The config is copied VERBATIM, which means its `entry` stays
    repository-relative — so `scripts/` is symlinked in rather than rewritten,
    and a typo in the real `entry` fails these cases instead of hiding behind a
    fixture that spelled the path its own way. git is told to ignore the
    symlink so `git add -A` in a case cannot stage it.

    `.claude/` is ignored for a sharper reason, and it is the real
    `.gitignore:32` rule, not a fixture convenience: prek reverts every
    tracked-but-unstaged file to its committed content before running a hook
    and restores it after. A TRACKED mark is therefore read at its committed
    value, so a scratch repo that let `git add -A` stage the mark judged every
    commit against a mark two steps old (measured). The real repository
    ignores the mark, and so must these.
    """
    shutil.copyfile(PREK_CONFIG, repo / PREK_CONFIG.name)
    (repo / "scripts").symlink_to(REPO_ROOT / "scripts")
    git_dir = Path(_git(repo, "rev-parse", "--absolute-git-dir").stdout.strip())
    (git_dir / "info").mkdir(exist_ok=True)
    with (git_dir / "info" / "exclude").open("a", encoding="utf-8") as fh:
        fh.write("/scripts\n/.pre-commit-config.yaml\n/.claude/\n")
    subprocess.run(
        # The same flags `task git:hooks` uses — a fixture that armed the shim
        # differently would not be exercising what a clone gets.
        ["prek", "install", "--allow-missing-config"],
        cwd=str(repo), capture_output=True, encoding="utf-8", check=True,
    )
    hooks = Path(_git(repo, "rev-parse", "--absolute-git-dir").stdout.strip()) / "hooks"
    hooks.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(PRE_PUSH_SCRIPT, hooks / "pre-push")
    (hooks / "pre-push").chmod(0o755)


def _make_repo(path: Path, branch: str = "work") -> Path:
    """A repository seeded with one commit, then armed with the real hooks.

    Armed *after* the seed commit so the seed does not need a mark.
    """
    (path / ".claude").mkdir(parents=True)
    _git(path.parent, "init", "-q", "-b", branch, str(path))
    _git(path, "config", "user.name", "gate")
    _git(path, "config", "user.email", "gate@example.invalid")
    (path / "seed").write_text("seed\n", encoding="utf-8")
    _git(path, "add", "-A")
    _git(path, "commit", "-q", "-m", "seed")
    _arm(path)
    return path


def _mark(project_dir: Path, repo: Path, scope: str, *, age: int = 0, toplevel: Path | None = None) -> None:
    """Write a verify mark for ``repo`` into ``project_dir``'s mark home."""
    path = project_dir / ".claude" / "hooks" / ".state" / "commit-verified"
    head = _git(repo, "rev-parse", "HEAD").stdout.strip()
    write_mark(path, scope, [], head, str((toplevel or repo).resolve()))
    if age:
        mark = json.loads(path.read_text(encoding="utf-8"))
        mark["timestamp"] -= age
        path.write_text(json.dumps(mark, sort_keys=True) + "\n", encoding="utf-8")


def _shell(repo: Path, script: str, project_dir: Path) -> subprocess.CompletedProcess[str]:
    """Run ``script`` with bash, in ``repo``, as a developer's shell would."""
    return subprocess.run(
        ["bash", "-c", script],
        cwd=str(repo),
        capture_output=True,
        encoding="utf-8",
        env=_env_for(project_dir),
        check=False,
    )


def _dirty(repo: Path, name: str = "work") -> None:
    (repo / name).write_text(f"{time.time()}\n", encoding="utf-8")
    _git(repo, "add", "-A")


def _self_test_cases(tmp: Path) -> list[tuple[str, object]]:
    no_hooks = tmp / "no-hooks"
    no_hooks.mkdir()

    def the_load_bearing_red_and_green() -> str | None:
        """The same commit, denied with no mark and allowed with one."""
        repo = _make_repo(tmp / "redgreen")
        _dirty(repo)
        denied = _shell(repo, "git commit -m 'chore: x'", repo)
        if denied.returncode == 0:
            return "a commit with no mark must be refused"
        if "Cannot commit without passing verification" not in denied.stderr:
            return f"the refusal must say why: {denied.stderr!r}"
        # The same command, with the hook disarmed: proves the denial above is
        # the gate's and not a broken fixture.
        disarmed = _shell(repo, f"git -c core.hooksPath={no_hooks} commit -m 'chore: x'", repo)
        if disarmed.returncode != 0:
            return f"the disarmed control must commit: {disarmed.stderr!r}"
        _git(repo, "reset", "-q", "--hard", "HEAD~1")
        _dirty(repo)
        _mark(repo, repo, "scoped")
        allowed = _shell(repo, "git commit -m 'chore: x'", repo)
        if allowed.returncode != 0:
            return f"a fresh scoped mark must admit the commit: {allowed.stderr!r}"
        # One mark, one commit: the same mark is still fresh in time and was
        # written by this very tree, but HEAD has moved underneath it. Nothing
        # else distinguishes this from the commit that just passed.
        _dirty(repo)
        again = _shell(repo, "git commit -m 'chore: x again'", repo)
        return expect(
            again.returncode != 0, "a mark must not cover a second commit at a new HEAD"
        )

    def the_refusal_survives_every_spelling() -> str | None:
        """The spellings the retired tokenizer missed or over-blocked."""
        repo = _make_repo(tmp / "spellings")
        (repo / "shallow").write_text("", encoding="utf-8")
        spellings = {
            "one line": "git commit -m x",
            "newline separated": "git add -A ;\ngit commit -m x",
            "line continuation": "git \\\ncommit -m x",
            "nested shell": "bash -c 'git commit -m x'",
            # The old enumeration failed OPEN on an interpreter's source
            # string: `git commit` there was "a literal, not a command".
            "an interpreter's -e script": "perl -e 'system(\"git\", \"commit\", \"-m\", \"x\")'",
            # R21: an unlisted global option that takes a value made the next
            # token look like the subcommand, so the gate never fired.
            "an unknown global option": "git --shallow-file shallow commit -m x",
            # R22: a command substitution the token walk read as an argument.
            "a command substitution": 'echo "$(git commit -m x)"',
        }
        for name, script in spellings.items():
            _dirty(repo)
            before = _git(repo, "rev-parse", "HEAD").stdout.strip()
            run = _shell(repo, script, repo)
            # HEAD, not the exit code: `perl -e 'system(...)'` exits 0 whatever
            # the child did, so the exit code is a proxy and the tree is the
            # fact. The reason text is what proves the gate — not the shell —
            # produced it.
            if _git(repo, "rev-parse", "HEAD").stdout.strip() != before:
                return f"{name}: the commit landed ({script!r})"
            output = run.stderr + run.stdout
            if "does not follow conventional commits" not in output:
                return f"{name}: no gate reason in the output: {output!r}"
        return None

    def a_command_that_only_mentions_git_is_not_a_commit() -> str | None:
        """R15's outage is structurally gone: nothing parses the command."""
        repo = _make_repo(tmp / "mentions")
        script = repo / "sync.sh"
        script.write_text("#!/usr/bin/env bash\necho synced\n", encoding="utf-8")
        script.chmod(0o755)
        run = _shell(repo, "./sync.sh  # don't forget the git submodules", repo)
        if run.returncode != 0 or "synced" not in run.stdout:
            return f"a command that merely mentions git must just run: {run.stderr!r}"
        # The control: the same shell, actually committing, is still refused.
        _dirty(repo)
        control = _shell(repo, "./sync.sh && git commit -m x", repo)
        return expect(control.returncode != 0, "the control must still be refused")

    def the_commit_message_rules() -> str | None:
        repo = _make_repo(tmp / "rules")
        _dirty(repo)
        checkpoint = _shell(repo, "git commit -m Checkpoint", repo)
        if checkpoint.returncode != 0:
            return f"`Checkpoint` must commit with no mark (D26): {checkpoint.stderr!r}"
        _dirty(repo)
        bad = _shell(repo, "git commit -m 'made some changes'", repo)
        if bad.returncode == 0 or "does not follow conventional commits" not in bad.stderr:
            return f"a non-conventional subject must be refused: {bad.stderr!r}"
        _mark(repo, repo, "scoped")
        release = _shell(repo, "git commit -m 'release: v9.9.9'", repo)
        if release.returncode == 0 or "needs a FULL verify mark" not in release.stderr:
            return f"`release:` must refuse a scoped mark: {release.stderr!r}"
        _mark(repo, repo, "full")
        released = _shell(repo, "git commit -m 'release: v9.9.9'", repo)
        if released.returncode != 0:
            return f"`release:` must accept a full mark: {released.stderr!r}"
        # A commit on main needs the full mark too, on the same tree.
        _git(repo, "switch", "-q", "-c", "main")
        _dirty(repo)
        _mark(repo, repo, "scoped")
        on_main = _shell(repo, "git commit -m 'chore: x'", repo)
        if on_main.returncode == 0 or "the branch is main" not in on_main.stderr:
            return f"a commit on main must need the full mark: {on_main.stderr!r}"
        return None

    def a_subject_cannot_hide_from_rule_3() -> str | None:
        """Three ways a subject skipped every rule, all found by review.

        The first two were live bypasses: a `#`-leading subject read as a
        comment and then as an empty message, and `--allow-empty-message`.
        Both landed unverified commits on `main`, which is the strictest state
        this gate has. The third is the other polarity — `fixup!` is git's own
        spelling and must still commit on a valid mark.
        """
        repo = _make_repo(tmp / "subjects", branch="main")
        for name, script in {
            # git strips comment lines only when the message is EDITED; under
            # `-m` a leading `#` is the subject, and an issue reference is the
            # most ordinary way to write one.
            "an issue reference": "git commit -m '#41 drop the trust anchor check'",
            "an empty message": "git commit --allow-empty-message -m ''",
        }.items():
            _dirty(repo)
            before = _git(repo, "rev-parse", "HEAD").stdout.strip()
            run = _shell(repo, script, repo)
            if _git(repo, "rev-parse", "HEAD").stdout.strip() != before:
                return f"{name}: an unverified commit landed on main ({script!r})"
            if "BLOCKED" not in run.stderr:
                return f"{name}: refused without a gate reason: {run.stderr!r}"
        # The other polarity: an autosquash subject on a valid mark commits.
        _git(repo, "switch", "-q", "-c", "work")
        _dirty(repo)
        _mark(repo, repo, "scoped")
        fixup = _shell(repo, "git commit --fixup=HEAD", repo)
        return expect(
            fixup.returncode == 0,
            f"`--fixup` is git's own subject and must commit on a mark: {fixup.stderr!r}",
        )

    def a_replay_commits_on_gits_own_subject() -> str | None:
        """A merge carries a subject this gate never authored — and still needs a mark.

        Both halves of rule 1 as D39 left it: `Merge WP-41: side` is not
        conventional and must commit, while rules 4 and 5 keep applying,
        because the merge's working tree is the one a verify run certifies and
        HEAD has not moved yet.
        """
        repo = _make_repo(tmp / "replay")
        _git(repo, "switch", "-q", "-c", "side")
        (repo / "side").write_text("side\n", encoding="utf-8")
        _git(repo, "add", "-A")
        _mark(repo, repo, "scoped")
        made = _shell(repo, "git commit -m 'feat: side'", repo)
        if made.returncode != 0:
            return f"the side commit needs to land first: {made.stderr!r}"
        _git(repo, "switch", "-q", "work")
        (repo / "trunk").write_text("trunk\n", encoding="utf-8")
        _git(repo, "add", "-A")
        _mark(repo, repo, "scoped")
        if _shell(repo, "git commit -m 'feat: trunk'", repo).returncode != 0:
            return "the trunk commit needs to land first"
        # Red: that mark certified the commit it was spent on, and HEAD has
        # moved. A merge is a commit like any other to rules 4 and 5.
        refused = _shell(repo, "git merge --no-ff -m 'Merge WP-41: side' side", repo)
        if refused.returncode == 0:
            return "a merge with no fresh mark must be refused"
        if "Cannot commit without passing verification" not in refused.stderr + refused.stdout:
            return f"the merge must be refused by the mark rule: {refused.stderr!r}"
        # Green: the same merge, mid-flight, concluded on git's own subject.
        _mark(repo, repo, "scoped")
        concluded = _shell(repo, "git commit --no-edit", repo)
        if concluded.returncode != 0:
            return f"a marked merge must conclude: {concluded.stderr!r}{concluded.stdout!r}"
        subject = _git(repo, "log", "-1", "--format=%s").stdout.strip()
        if subject != "Merge WP-41: side":
            return f"the merge subject drifted: {subject!r}"
        return expect(
            len(_git(repo, "log", "-1", "--format=%p").stdout.split()) == 2,
            "the concluded commit must be the merge itself",
        )

    def a_paused_replay_is_not_a_free_commit() -> str | None:
        """D39: `git revert -n` and `git merge --no-commit` set the state file too.

        Neither conflicts, neither stops, and both leave the marker the
        carve-out keyed on — so one command bought an arbitrary subject over
        arbitrary staged content with no mark, on `main`, which is the
        strictest state this gate has. What is exempt now is the subject git
        itself prepared, so the same pause concluded honestly still commits.
        """
        repo = _make_repo(tmp / "paused", branch="main")
        # The mark home is OUTSIDE the repo here: this case stages with
        # `git add -A` while a mark exists, and a mark inside the tree would
        # ride into the commits and then collide with `rebase`'s checkout.
        home = tmp / "paused-home"
        (home / ".claude").mkdir(parents=True)
        _git(repo, "switch", "-q", "-c", "side")
        (repo / "side").write_text("side\n", encoding="utf-8")
        _git(repo, "add", "-A")
        _mark(home, repo, "full")
        if _shell(repo, "git commit -m 'feat: side'", home).returncode != 0:
            return "the side commit needs to land first"
        _git(repo, "switch", "-q", "main")
        _dirty(repo, "second")
        _mark(home, repo, "full")
        if _shell(repo, "git commit -m 'feat: second'", home).returncode != 0:
            return "the commit to revert needs to land first"
        pauses = {
            "a revert left uncommitted": ("git revert -n HEAD", "git revert --quit"),
            "a merge left uncommitted": (
                "git merge --no-ff --no-commit side",
                "git merge --abort",
            ),
        }
        for name, (pause, release) in pauses.items():
            if _shell(repo, pause, home).returncode != 0:
                return f"{name}: the fixture's own pause failed ({pause!r})"
            before = _git(repo, "rev-parse", "HEAD").stdout.strip()
            (repo / "poison").write_text("poison\n", encoding="utf-8")
            _git(repo, "add", "-A")
            # A full mark is on disk and fresh: what refuses this is rule 3,
            # so the case cannot pass for the mark's reason.
            _mark(home, repo, "full")
            run = _shell(repo, "git commit -m 'unverified on main, zero rules applied'", home)
            if _git(repo, "rev-parse", "HEAD").stdout.strip() != before:
                return f"{name}: an arbitrary commit landed on main during {pause!r}"
            if "does not follow conventional commits" not in run.stderr:
                return f"{name}: the subject rule must refuse it: {run.stderr!r}"
            if "Concluding a" not in run.stderr:
                return f"{name}: the refusal must say how to conclude instead: {run.stderr!r}"
            _shell(repo, release, home)
            _git(repo, "reset", "-q", "--hard", "HEAD")
            (repo / "poison").unlink(missing_ok=True)
        # The other polarity, on the same pause: git's own subject concludes
        # it — refused without a mark, admitted with one.
        if _shell(repo, "git revert -n HEAD", home).returncode != 0:
            return "the fixture's revert failed"
        mark = home / ".claude" / "hooks" / ".state" / "commit-verified"
        mark.unlink(missing_ok=True)
        unmarked = _shell(repo, "git commit --no-edit", home)
        if unmarked.returncode == 0:
            return "concluding a revert with no mark must be refused"
        if "Cannot commit without passing verification" not in unmarked.stderr:
            return f"the refusal must be the mark rule: {unmarked.stderr!r}"
        _mark(home, repo, "full")
        concluded = _shell(repo, "git commit --no-edit", home)
        if concluded.returncode != 0:
            return f"a marked revert must conclude: {concluded.stderr!r}{concluded.stdout!r}"
        subject = _git(repo, "log", "-1", "--format=%s").stdout.strip()
        if subject != 'Revert "feat: second"':
            return f"the concluding commit must carry git's subject: {subject!r}"
        # The carve-out that stays whole: a rebase rewrites HEAD itself, so a
        # commit made at a `rebase -i` stop is exempt from subject and mark
        # both. Deliberate — `reword` hands the hook a message git has already
        # consumed, and no mark can certify an intermediate replay state.
        paused_rebase = _shell(
            repo, "GIT_SEQUENCE_EDITOR=\"sed -i '1s/^pick/edit/'\" git rebase -i HEAD~1", home
        )
        if paused_rebase.returncode != 0:
            return f"the fixture's rebase failed: {paused_rebase.stderr!r}"
        mark.unlink(missing_ok=True)
        _dirty(repo, "split")
        split = _shell(repo, "git commit -m 'a subject no rule admits'", home)
        _shell(repo, "git rebase --abort", home)
        return expect(
            split.returncode == 0,
            f"a commit at a rebase stop stays exempt: {split.stderr!r}",
        )

    def the_push_gate() -> str | None:
        repo = _make_repo(tmp / "push", branch="main")
        remote = tmp / "push-remote.git"
        _git(tmp, "init", "-q", "--bare", str(remote))
        _git(repo, "remote", "add", "origin", str(remote))
        log = tmp / "push.log"
        cases = {
            "an explicit main": ("git push origin main", True),
            # R18: the redirection is the shell's business and never reaches
            # the hook — git resolved the refspec before running it — so the
            # reason lands in the log instead of on stderr, and the push is
            # refused exactly the same.
            "a redirected push": (f"git push origin main > {log} 2>&1", True),
            "a fully qualified refspec": ("git push origin HEAD:refs/heads/main", True),
            "a bare push on main": ("git push -u origin", True),
            "a branch merely spelled like main": ("git push origin main:feature/main-fix", False),
        }
        for name, (script, refused) in cases.items():
            run = _shell(repo, script, repo)
            output = run.stderr + run.stdout + (log.read_text(encoding="utf-8") if log.exists() else "")
            if refused and run.returncode == 0:
                return f"{name}: the push was not refused ({script!r})"
            if refused and "Cannot push directly to" not in output:
                return f"{name}: no gate reason: {output!r}"
            if not refused and run.returncode != 0:
                return f"{name}: the push must be allowed: {run.stderr!r}"
            log.unlink(missing_ok=True)
        # The remote is the fact: nothing named main reached it, and the
        # branch merely spelled like main did.
        refs = _git(remote, "for-each-ref", "--format=%(refname)").stdout.split()
        if "refs/heads/main" in refs:
            return f"a refused push still reached the remote: {refs}"
        if "refs/heads/feature/main-fix" not in refs:
            return f"the allowed push did not reach the remote: {refs}"
        disarmed = _shell(repo, f"git -c core.hooksPath={no_hooks} push origin main", repo)
        if disarmed.returncode != 0:
            return f"the disarmed control must push: {disarmed.stderr!r}"
        return expect(
            "refs/heads/main" in _git(remote, "for-each-ref", "--format=%(refname)").stdout,
            "the disarmed push must actually land on the remote",
        )

    def the_push_hook_still_feeds_git_lfs() -> str | None:
        """`scripts/pre-push.sh` owns the hook `git lfs install` would own.

        This repository tracks its images in LFS, so the one hook the gate
        takes over owes LFS the delegation its generated hook would have done.
        stdin can be read once and two consumers need it, so the script
        replays it — and a REFUSED push must not upload objects for a push
        that will not happen. A stub `git-lfs` earlier on PATH records what it
        was handed.
        """
        repo = _make_repo(tmp / "lfs", branch="main")
        remote = tmp / "lfs-remote.git"
        _git(tmp, "init", "-q", "--bare", str(remote))
        _git(repo, "remote", "add", "origin", str(remote))
        stub_dir = tmp / "lfs-stub"
        stub_dir.mkdir()
        record = tmp / "lfs-stdin.txt"
        stub = stub_dir / "git-lfs"
        stub.write_text(
            f'#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> {record}\ncat >> {record}\n',
            encoding="utf-8",
        )
        stub.chmod(0o755)
        env = {**_env_for(repo), "PATH": f"{stub_dir}{os.pathsep}{os.environ['PATH']}"}

        def push(script: str) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                ["bash", "-c", script], cwd=str(repo), capture_output=True,
                encoding="utf-8", env=env, check=False,
            )

        refused = push("git push origin main")
        if refused.returncode == 0:
            return "the gate must still refuse with the LFS delegation in the shim"
        if record.exists():
            return f"a refused push must not reach git-lfs: {record.read_text(encoding='utf-8')!r}"
        allowed = push("git push origin main:feature/ok")
        if allowed.returncode != 0:
            return f"an allowed push must succeed: {allowed.stderr!r}"
        if not record.exists():
            return "an allowed push must reach git-lfs — LFS objects would silently stop uploading"
        handed = record.read_text(encoding="utf-8")
        if "pre-push origin" not in handed:
            return f"git-lfs must get the pre-push verb and the remote: {handed!r}"
        if "refs/heads/feature/ok" not in handed:
            return f"git-lfs must get the same refs stdin the gate saw: {handed!r}"
        return None

    def arming_leaves_git_lfs_its_own_three_hooks() -> str | None:
        """`post-checkout`, `post-commit` and `post-merge` are LFS's again.

        They were only ever ours because `core.hooksPath` took them from `git
        lfs install`; nothing points away from git's hooks directory now, so
        LFS installs them itself. The regression this guards is silent — the
        working tree checks out pointer files and nobody sees an error — so it
        is checked the way `task git:hooks` leaves a clone: LFS installs, then
        prek, and all four hooks must still be there afterwards.
        """
        if shutil.which("git-lfs") is None:
            return None  # not installed here; `pre-push` covers the delegation
        repo = tmp / "lfs-own-hooks"
        repo.mkdir()
        _git(repo.parent, "init", "-q", "-b", "work", str(repo))
        for step in (["lfs", "install", "--local", "--force"],):
            run = _git(repo, *step)
            if run.returncode != 0:
                return f"`git {' '.join(step)}` failed: {run.stderr!r}"
        _arm(repo)
        hooks = Path(_git(repo, "rev-parse", "--absolute-git-dir").stdout.strip()) / "hooks"
        for name in ("post-checkout", "post-commit", "post-merge"):
            hook = hooks / name
            if not hook.is_file():
                return f"{name}: arming removed the hook `git lfs install` wrote"
            if "git lfs" not in hook.read_text(encoding="utf-8"):
                return f"{name}: no longer delegates to git-lfs: {hook.read_text(encoding='utf-8')!r}"
        # The gate's own hook must have won this one: it delegates to LFS
        # itself, and LFS's generated version does not run the gate.
        pre_push = (hooks / "pre-push").read_text(encoding="utf-8")
        return expect(
            "commit_gate.py" in pre_push,
            f"pre-push must be the gate's, not LFS's generated one: {pre_push!r}",
        )

    def a_branch_without_the_config_still_commits() -> str | None:
        """The hooks directory is shared and absolute; most branches predate it.

        `.githooks` was a RELATIVE hooksPath, so a worktree on a branch without
        it ran no hook and nobody noticed. The hooks directory git uses now is
        the common one, so the shim runs in every worktree whatever it has
        checked out — and prek exits 1 with "No `prek.toml` or
        `.pre-commit-config.yaml` found" when the tree has neither, which would
        block every commit on every branch that has not merged this config yet.
        `--allow-missing-config` is what keeps that a degradation instead of an
        outage, and this is the case that reds when the flag is dropped.
        """
        repo = _make_repo(tmp / "no-config")
        (repo / PREK_CONFIG.name).unlink()
        _dirty(repo)
        _mark(repo, repo, "scoped")
        commit = _shell(repo, "git commit -m 'chore: x'", repo)
        if commit.returncode != 0:
            return f"a tree with no config must commit, not error: {commit.stderr!r}"
        # And the push gate is a copy in the hooks directory, so it is NOT
        # branch-dependent: it must still refuse the trunk here.
        remote = tmp / "no-config-remote.git"
        _git(tmp, "init", "-q", "--bare", str(remote))
        _git(repo, "remote", "add", "origin", str(remote))
        refused = _shell(repo, "git push origin work:main", repo)
        return expect(
            refused.returncode != 0,
            "the push gate is a copy, not read from the tree — it must refuse regardless",
        )

    def preks_stash_does_not_hide_the_mark() -> str | None:
        """prek reverts unstaged tracked files around a hook; the mark survives.

        Before running a hook prek writes every tracked-but-unstaged change to
        a patch, checks the tree out clean, and restores the patch afterwards.
        Anything the gate reads out of the working tree is therefore read at
        its COMMITTED value for the duration of the hook. The mark is
        gitignored (`.gitignore:32`) and so unaffected — this pins that: track
        the mark and this case reds, which is the whole reason the ignore rule
        is load-bearing rather than tidy.
        """
        repo = _make_repo(tmp / "stash")
        # An unstaged change to a TRACKED file is what triggers the stash.
        (repo / "seed").write_text("unstaged\n", encoding="utf-8")
        _dirty(repo, "staged")
        _mark(repo, repo, "scoped")
        allowed = _shell(repo, "git commit -m 'chore: x'", repo)
        if allowed.returncode != 0:
            return f"the mark must survive prek's stash: {allowed.stderr!r}"
        return expect(
            (repo / "seed").read_text(encoding="utf-8") == "unstaged\n",
            "prek must restore the unstaged change it saved",
        )

    def a_mark_from_a_sibling_worktree_certifies_nothing() -> str | None:
        """R17, with every control the review asked for.

        Worktree B branches from A's HEAD into A's mark home, which is what a
        batch of parallel work packages does. Before the `toplevel` field, A's
        mark admitted B's unverified tree.
        """
        a = _make_repo(tmp / "r17")
        b = tmp / "r17-sibling"
        _git(a, "worktree", "add", "-q", str(b), "-b", "sibling")
        _arm(b)
        head = _git(a, "rev-parse", "HEAD").stdout.strip()
        if _git(b, "rev-parse", "HEAD").stdout.strip() != head:
            return "the sibling worktree must start at the same HEAD"
        mark = a / ".claude" / "hooks" / ".state" / "commit-verified"

        def commit_in(repo: Path) -> subprocess.CompletedProcess[str]:
            _dirty(repo)
            # CLAUDE_PROJECT_DIR is `a` for BOTH: one shared mark home is
            # exactly the shape that made HEAD stop discriminating.
            return _shell(repo, "git commit -m 'chore: x'", a)

        if mark.exists():
            mark.unlink()
        if commit_in(b).returncode == 0:
            return "no mark: the commit must be refused"
        _mark(a, a, "scoped")
        _git(a, "commit", "-q", "-m", "chore: move a's head")
        if commit_in(b).returncode == 0:
            return "a mark from a different HEAD must be refused"
        _mark(a, b, "scoped", age=TTL_SECONDS + 60)
        if commit_in(b).returncode == 0:
            return "a stale mark must be refused"
        # The one R17 adds: A's tree earned it, B's did not, same HEAD.
        _mark(a, b, "scoped", toplevel=a)
        refused = commit_in(b)
        if refused.returncode == 0:
            return "a mark written by a SIBLING worktree must be refused"
        if "written by" not in refused.stderr:
            return f"the refusal must name the writing tree: {refused.stderr!r}"
        # Own HEAD, own toplevel: allowed.
        _mark(a, b, "scoped")
        allowed = commit_in(b)
        return expect(
            allowed.returncode == 0, f"b's own mark must admit b's commit: {allowed.stderr!r}"
        )

    def a_mark_with_no_writer_reads_as_unverified() -> str | None:
        """The pre-R17 mark shape, which is the one already on every disk."""
        repo = _make_repo(tmp / "legacy")
        _mark(repo, repo, "scoped")
        path = repo / ".claude" / "hooks" / ".state" / "commit-verified"
        mark = json.loads(path.read_text(encoding="utf-8"))
        del mark["toplevel"]
        path.write_text(json.dumps(mark, sort_keys=True) + "\n", encoding="utf-8")
        _dirty(repo)
        run = _shell(repo, "git commit -m 'chore: x'", repo)
        return expect(run.returncode != 0, "a mark with no `toplevel` must read as unverified")

    def the_release_prepare_guard() -> str | None:
        """`--require-full-mark`'s exit contract, which release:prepare scripts."""
        repo = _make_repo(tmp / "guard")

        def run(project_dir: Path) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                [sys.executable, str(Path(__file__).resolve()), "--require-full-mark", str(repo)],
                capture_output=True,
                encoding="utf-8",
                env=_env_for(project_dir),
                check=False,
            )

        _mark(repo, repo, "scoped")
        scoped = run(repo)
        if scoped.returncode != 1 or "task verify" not in scoped.stderr:
            return f"a scoped mark must exit 1 with the remedy: {scoped.returncode} {scoped.stderr!r}"
        _mark(repo, repo, "full")
        full = run(repo)
        if full.returncode != 0:
            return f"a full mark must exit 0: {full.stderr!r}"
        path = repo / ".claude" / "hooks" / ".state" / "commit-verified"
        path.write_text(str(int(time.time())), encoding="utf-8")
        return expect(run(repo).returncode == 1, "a bare-integer stamp must exit 1")

    return [
        ("a commit is refused with no mark and allowed with one", the_load_bearing_red_and_green),
        ("the refusal survives every spelling", the_refusal_survives_every_spelling),
        ("a command that only mentions git is not a commit", a_command_that_only_mentions_git_is_not_a_commit),
        ("the commit-message rules", the_commit_message_rules),
        ("no subject form hides from rule 3", a_subject_cannot_hide_from_rule_3),
        ("a merge commits on git's subject, never without a mark", a_replay_commits_on_gits_own_subject),
        ("a paused replay is not a free commit", a_paused_replay_is_not_a_free_commit),
        ("the push gate", the_push_gate),
        ("the push hook still feeds git lfs", the_push_hook_still_feeds_git_lfs),
        ("arming leaves git lfs its own three hooks", arming_leaves_git_lfs_its_own_three_hooks),
        ("a branch without the config still commits", a_branch_without_the_config_still_commits),
        ("prek's stash does not hide the mark", preks_stash_does_not_hide_the_mark),
        ("a sibling worktree's mark certifies nothing", a_mark_from_a_sibling_worktree_certifies_nothing),
        ("a mark with no writer reads as unverified", a_mark_with_no_writer_reads_as_unverified),
        ("the release:prepare guard", the_release_prepare_guard),
    ]


def self_test() -> int:
    scratch_root = REPO_ROOT / ".tmp"
    scratch_root.mkdir(exist_ok=True)
    failures = 0
    with tempfile.TemporaryDirectory(prefix="commit-gate-", dir=scratch_root) as tmp:
        cases = _self_test_cases(Path(tmp))
        for name, thunk in cases:
            try:
                problem = thunk()
            except (SystemExit, NotImplementedError) as stop:
                problem = f"{type(stop).__name__}: {stop}"
            status = "ok  " if problem is None else "FAIL"
            failures += problem is not None
            print(f"{status} {name}")
            if problem is not None:
                print(f"       {problem}")
    print(f"self-test: {len(cases) - failures}/{len(cases)} rules hold ({failures} wrong)")
    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    # prek appends the message file as the one filename and runs the hook from
    # the repository root, so there is no second argument to pass and none to
    # trust: the root is where git says it is.
    mode.add_argument(
        "--check-message", nargs=1, metavar="MSG_FILE", help="the commit-msg hook"
    )
    mode.add_argument("--check-push", nargs=1, metavar="REPO_ROOT", help="the pre-push hook")
    mode.add_argument(
        "--require-full-mark", nargs=1, metavar="REPO_ROOT", help="release:prepare's guard"
    )
    mode.add_argument("--self-test", action="store_true", help="show every rule red/green")
    ns = parser.parse_args(argv)
    if ns.self_test:
        return self_test()
    if ns.require_full_mark:
        return require_full_mark_cli(ns.require_full_mark[0])
    reason = (
        check_message(ns.check_message[0], git_out(".", "rev-parse", "--show-toplevel") or ".")
        if ns.check_message
        else check_push(sys.stdin.read(), ns.check_push[0])
    )
    if reason is None:
        return 0
    print(reason, file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
