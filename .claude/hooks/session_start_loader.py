# /// script
# requires-python = ">=3.10"
# ///
"""SessionStart hook: loads project context and swarm state."""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import hook_utils


def build_context(active_sessions: int, handoff_message: str | None) -> str:
    """Build the context string from swarm state and handoff message.

    Returns an empty string when there is nothing to report.
    """
    parts: list[str] = []

    if active_sessions > 1:
        parts.append(
            f"[SWARM STATUS]\n"
            f"- Active agents in project: {active_sessions}\n"
            f"- Check file locks before major edits"
        )

    if handoff_message:
        parts.append(f"[HANDOFF FROM PREVIOUS SESSION]\n{handoff_message}")

    return "\n\n".join(parts)


def process(input_data: dict, project_dir: str) -> str:
    """Run session start logic given parsed input and project_dir.

    Returns the context string to emit (may be empty).
    """
    source = input_data.get("source", "startup") or "startup"
    session_id = input_data.get("session_id", "") or ""

    state = hook_utils.StateManager(project_dir)
    state.clean_old_sessions()
    state.write_session(session_id, source)

    active_sessions = state.count_active_sessions()
    handoff_message = state.read_and_clear_handoff()

    return build_context(active_sessions, handoff_message)


def main() -> None:
    input_data = hook_utils.read_input()

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)

    context = process(input_data, project_dir)
    if context:
        print(context)

    sys.exit(0)


if __name__ == "__main__":
    main()
