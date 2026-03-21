# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse (Bash) hook: blocks direct pushes to main or master."""

import re
import subprocess
import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

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


def is_git_push(command: str) -> bool:
    """Return True when the command contains a 'git push' invocation."""
    return bool(re.search(r"\bgit\s+push\b", command))


def _has_explicit_branch_pair(command: str) -> bool:
    """Return True when the push command specifies an explicit remote/branch pair."""
    # Matches patterns like: git push origin feature-branch
    # or: git push --set-upstream origin feature-branch
    return bool(re.search(r"\bgit\s+push\b.*\s+[a-zA-Z0-9_-]+\s+[a-zA-Z0-9_/.-]+", command))


def _is_plain_push(command: str) -> bool:
    """Return True for 'git push', 'git push origin', 'git push --flag origin' (no branch)."""
    # Matches: git push [options] [single-remote]  — no trailing branch argument
    return bool(
        re.search(r"\bgit\s+push\s*$", command)
        or re.search(r"\bgit\s+push\s+(--[\w-]+\s+)*[a-zA-Z0-9_-]+\s*$", command)
    )


def is_push_to_main(command: str, current_branch: str) -> bool:
    """Return True when the push targets main or master.

    Two cases:
    - Explicit: the command names 'main' or 'master' after 'git push'.
    - Implicit: the current branch is main/master and no explicit branch pair
      is given (the push will therefore target the tracking branch, which is
      also main/master).
    """
    # Explicit branch in command
    if re.search(r"\bgit\s+push\b.*\b(main|master)\b", command):
        return True

    # Implicit: on main/master with no explicit remote/branch pair
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
    current_branch = get_current_branch(project_dir) if project_dir else "unknown"

    if is_push_to_main(command, current_branch):
        hook_utils.output_json(hook_utils.deny(_DENY_REASON.format(branch=current_branch)))

    sys.exit(0)


if __name__ == "__main__":
    main()
