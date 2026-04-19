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
    """Release locks, remove session, merge learnings, and check for
    uncommitted changes.

    Returns a reminder string composed of the Stage 1 learnings summary
    (when present) and the uncommitted-changes reminder (when present).
    Either may be empty; the combined result is joined with a newline.
    """
    state = hook_utils.StateManager(project_dir)
    state.release_session_locks(session_id)
    state.remove_session(session_id)

    # Phase 4 — merge pending learnings under StateManager file-lock
    # semantics. Failure here must not block the stop.
    learnings_summary = ""
    try:
        store = hook_utils.LearningsStore(project_dir)
        if store.pending_path.exists() or store.canonical_path.exists():
            stats = store.merge_pending()
            learnings_summary = store.stage1_summary(stats)
    except Exception:
        learnings_summary = ""

    uncommitted = count_uncommitted_changes(project_dir)
    lines: list[str] = []
    if learnings_summary:
        lines.append(learnings_summary)
    if uncommitted > 0:
        lines.append(build_reminder(uncommitted))
    return "\n".join(lines)


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
