# /// script
# requires-python = ">=3.10"
# ///
"""Stop hook: releases locks, cleans session state, and reminds about uncommitted changes."""

import subprocess
import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import hook_utils


def count_uncommitted_changes(project_dir: str) -> int:
    """Return the number of uncommitted files in the git working tree.

    Returns 0 if the directory is not a git repo or git is unavailable.
    """
    try:
        result = subprocess.run(
            ["git", "-C", project_dir, "status", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode != 0:
            return 0
        lines = [line for line in result.stdout.splitlines() if line.strip()]
        return len(lines)
    except (OSError, subprocess.TimeoutExpired):
        return 0


def build_reminder(uncommitted: int) -> str:
    """Build the uncommitted-changes reminder string."""
    return (
        "\n---\n"
        "[SESSION CLEANUP REMINDER]\n"
        f"- Uncommitted changes detected: {uncommitted} files\n"
        "- Consider: git add && git commit before ending\n"
        "---"
    )


def process(session_id: str, project_dir: str) -> str:
    """Release locks, remove session, and check for uncommitted changes.

    Returns a reminder string if changes exist, otherwise an empty string.
    """
    state = hook_utils.StateManager(project_dir)
    state.release_session_locks(session_id)
    state.remove_session(session_id)

    uncommitted = count_uncommitted_changes(project_dir)
    if uncommitted > 0:
        return build_reminder(uncommitted)
    return ""


def main() -> None:
    input_data = hook_utils.read_input()

    # Prevent infinite loops when the hook itself triggers a Stop event.
    if input_data.get("stop_hook_active") is True:
        sys.exit(0)

    session_id = input_data.get("session_id", "") or ""

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)

    reminder = process(session_id, project_dir)
    if reminder:
        print(reminder)

    sys.exit(0)


if __name__ == "__main__":
    main()
