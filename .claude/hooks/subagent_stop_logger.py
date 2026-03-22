# /// script
# requires-python = ">=3.10"
# ///
"""SubagentStop hook: logs subagent completion and trims the log."""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import hook_utils


def log_and_trim(project_dir: str) -> None:
    """Append a completion entry to the subagent log and trim it."""
    state = hook_utils.StateManager(project_dir)
    state.log_subagent_completion()
    state.trim_subagent_log()


def main() -> None:
    input_data = hook_utils.read_input()

    # Prevent infinite loops when the hook itself triggers a SubagentStop event.
    if input_data.get("stop_hook_active") is True:
        sys.exit(0)

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)

    try:
        log_and_trim(project_dir)
    except Exception:
        # Non-blocking: subagent logging must never prevent the agent from stopping.
        pass

    sys.exit(0)


if __name__ == "__main__":
    main()
