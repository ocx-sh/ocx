# /// script
# requires-python = ">=3.10"
# ///
"""SubagentStop hook: logs subagent completion, trims the log, and captures
learnings emitted by the subagent via `[LEARNING]` JSON markers.

Per .claude/artifacts/adr_ai_config_cross_session_learnings_store.md, captured
learnings go to `.state/learnings-pending.jsonl` and are merged into the
project-local canonical store (`.claude/state/learnings.jsonl`) at session end
by stop_validator.py.
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import hook_utils
import pre_tool_use_validator


def _iter_string_payloads(obj: object) -> list[str]:
    """Yield every string value nested inside the input dict."""
    out: list[str] = []
    if isinstance(obj, str):
        out.append(obj)
    elif isinstance(obj, dict):
        for v in obj.values():
            out.extend(_iter_string_payloads(v))
    elif isinstance(obj, list):
        for v in obj:
            out.extend(_iter_string_payloads(v))
    return out


def capture_learnings(input_data: dict, project_dir: str) -> int:
    """Scan input for [LEARNING] markers; append each to the pending queue.

    Redacts records whose summary or evidence_ref fields contain secrets
    (reusing the existing `pre_tool_use_validator.detect_secrets` regex
    set). Redacted records are skipped, not stored.

    Returns the number of records appended.

    Known Stage-1 limitation: scans every nested string in the hook payload,
    so quoted ``[LEARNING] {...}`` examples in test fixtures, docs, or diffs
    may be captured as false positives. Stage 1 is logging-only (no
    user-facing side effects) so this is acceptable noise; Stage 2 will
    narrow extraction to an intentional-emission envelope once the Claude
    Code SubagentStop payload schema is documented enough to distinguish
    intentional subagent output from incidental strings. Tracked by Codex
    adversarial finding 3 (2026-04-19).
    """
    store = hook_utils.LearningsStore(project_dir)
    session_id = input_data.get("session_id", "")
    source_agent = input_data.get("tool_name", "subagent")
    count = 0
    for payload in _iter_string_payloads(input_data):
        for raw in hook_utils.parse_learning_markers(payload):
            raw.setdefault("source_session", session_id)
            raw.setdefault("source_agent", source_agent)
            record = store.normalize_record(raw, source_session=session_id)
            # Secret redaction: drop records whose summary/evidence_ref
            # match known secret patterns. See ADR §Privacy/secrets.
            combined = f"{record.get('summary', '')}\n{record.get('evidence_ref', '')}"
            if pre_tool_use_validator.detect_secrets(combined):
                continue
            store.append_pending(record)
            count += 1
    return count


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
        capture_learnings(input_data, project_dir)
    except Exception:
        # Non-blocking: subagent logging + learning capture must never prevent
        # the agent from stopping.
        pass

    sys.exit(0)


if __name__ == "__main__":
    main()
