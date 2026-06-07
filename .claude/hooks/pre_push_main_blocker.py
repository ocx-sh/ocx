# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse (Bash) hook: blocks direct pushes to main or master.

Scoped to this project's git repo: pushes from a sibling repo
(via `cd /other/repo && git push …`) bypass the block — that repo
owns its own branching policy.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import hook_utils

_DENY_REASON = (
    "BLOCKED: Cannot push directly to main branch. "
    "Trunk-based development requires:\n\n"
    "1. Create a feature branch: git checkout -b feature/your-change\n"
    "2. Commit your changes on the branch\n"
    "3. Push the branch: git push -u origin feature/your-change\n"
    "4. Create a PR for review\n\n"
    "Current branch: {branch}"
)

_CD_PREFIX_RE = re.compile(r"^\s*cd\s+(?:'([^']+)'|\"([^\"]+)\"|(\S+))\s*&&")


def is_git_push(command: str) -> bool:
    """Return True when the command contains a 'git push' invocation."""
    return bool(re.search(r"\bgit\s+push\b", command))


def _has_explicit_branch_pair(command: str) -> bool:
    """Return True when the push command specifies an explicit remote/branch pair."""
    return bool(re.search(r"\bgit\s+push\b.*\s+[a-zA-Z0-9_-]+\s+[a-zA-Z0-9_/.-]+", command))


def _is_plain_push(command: str) -> bool:
    """Return True for 'git push', 'git push origin', 'git push --flag origin' (no branch)."""
    return bool(
        re.search(r"\bgit\s+push\s*$", command)
        or re.search(r"\bgit\s+push\s+(--[\w-]+\s+)*[a-zA-Z0-9_-]+\s*$", command)
    )


def parse_command_cwd(command: str) -> str | None:
    """Extract the working directory from a leading `cd X && ...` prefix.

    Three quoting forms supported: `cd '…'`, `cd "…"`, `cd unquoted`.
    """
    m = _CD_PREFIX_RE.match(command)
    if not m:
        return None
    return m.group(1) or m.group(2) or m.group(3)


def resolve_git_root(start_dir: str) -> str | None:
    """Return the absolute git toplevel containing ``start_dir`` or None."""
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=start_dir,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    out = result.stdout.strip()
    return out or None


def push_targets_this_project(command: str, project_dir: str) -> bool:
    """Return True when the push runs in this project's git repo.

    Heuristic — if we can't pin down the push's cwd (no `cd …` prefix,
    no callable ``git``), assume the push IS for this project so the
    block still applies. False-positives on cross-repo pushes are
    acceptable; false-negatives (skipping the gate on an in-repo push
    to main) are not.
    """
    cwd = parse_command_cwd(command)
    if cwd is None:
        return True
    git_root = resolve_git_root(cwd)
    if git_root is None:
        return True
    try:
        return os.path.realpath(git_root) == os.path.realpath(project_dir)
    except OSError:
        return True


def is_push_to_main(command: str, current_branch: str) -> bool:
    """Return True when the push targets main or master."""
    if re.search(r"\bgit\s+push\b.*\b(main|master)\b", command):
        return True

    if current_branch in ("main", "master"):
        if not _has_explicit_branch_pair(command):
            if _is_plain_push(command):
                return True

    return False


def get_current_branch(project_dir: str) -> str:
    """Return the current git branch name, or 'unknown' on failure."""
    try:
        result = subprocess.run(
            ["git", "-C", project_dir, "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        pass
    return "unknown"


def main() -> None:
    input_data = hook_utils.read_input()

    tool_name = input_data.get("tool_name", "") or ""
    if tool_name != "Bash":
        sys.exit(0)

    command = (input_data.get("tool_input") or {}).get("command", "") or ""
    if not is_git_push(command):
        sys.exit(0)

    project_dir = hook_utils.get_project_dir() or ""

    # NEW: scope the gate to pushes from *this* project's git repo. A push
    # routed into a sibling repo via `cd /other/repo && git push …` is not
    # our branching-policy concern — that repo decides. Without this
    # scoping, every cross-repo push hit by the same Claude session is
    # measured against THIS project's current branch, producing nonsense
    # block reasons (e.g. "Current branch: feat/some-ocx-branch" on a
    # cmake-candidate push).
    if not push_targets_this_project(command, project_dir):
        sys.exit(0)

    current_branch = get_current_branch(project_dir) if project_dir else "unknown"

    if is_push_to_main(command, current_branch):
        hook_utils.output_json(hook_utils.deny(_DENY_REASON.format(branch=current_branch)))

    sys.exit(0)


if __name__ == "__main__":
    main()
