# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook (Write|Edit|MultiEdit): validate file operations for swarm consistency.

Checks file locks, protected paths, and potential secrets before allowing writes.
Acquires a lock on success so other agents can detect concurrent edits.
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import re
from pathlib import Path

import hook_utils

# ---------------------------------------------------------------------------
# Secret detection patterns — compiled once at module load
# ---------------------------------------------------------------------------

_SECRET_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    (
        "generic secret",
        re.compile(
            r"(api[_\-]?key|secret|password|token|credential).*[=:]\s*[\"']?[a-zA-Z0-9+/]{20,}",
            re.IGNORECASE,
        ),
    ),
    (
        "AWS access key",
        re.compile(r"AKIA[0-9A-Z]{16}"),
    ),
    (
        "JWT token",
        re.compile(r"eyJ[a-zA-Z0-9_\-]+\.eyJ[a-zA-Z0-9_\-]+\.[a-zA-Z0-9_\-]+"),
    ),
    (
        "exported secret",
        re.compile(
            r"export\s+(API_KEY|SECRET|PASSWORD|TOKEN|CREDENTIAL|AWS_|PRIVATE_KEY)=[\"']?[a-zA-Z0-9+/]{20,}",
            re.IGNORECASE,
        ),
    ),
    (
        "GitHub personal access token",
        re.compile(r"ghp_[a-zA-Z0-9]{36}"),
    ),
    (
        "private key",
        re.compile(r"-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----"),
    ),
]

_TEST_FILE_SUFFIXES = (
    ".test.ts",
    ".spec.ts",
    ".test.tsx",
    ".spec.tsx",
    ".test.js",
    ".spec.js",
)

_PROTECTED_PATTERNS = (".git/", ".env", ".mcp.json")


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def is_test_file(rel_path: str) -> bool:
    """Return True if the path looks like a test/spec file."""
    return any(rel_path.endswith(suffix) for suffix in _TEST_FILE_SUFFIXES)


def detect_secrets(content: str) -> str | None:
    """Scan content for common secret patterns.

    Returns the secret type string on first match, or None if clean.
    """
    for secret_type, pattern in _SECRET_PATTERNS:
        if pattern.search(content):
            return secret_type
    return None


def is_protected(rel_path: str) -> bool:
    """Return True if rel_path matches any protected pattern."""
    return any(pattern in rel_path for pattern in _PROTECTED_PATTERNS)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    data = hook_utils.read_input()

    tool_name: str = data.get("tool_name", "")
    tool_input: dict = data.get("tool_input", {})
    session_id: str = data.get("session_id", "")

    file_path: str = tool_input.get("file_path") or tool_input.get("path") or ""

    # Not a file operation — nothing to do
    if not file_path:
        sys.exit(0)

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)
    rel_path = hook_utils.relative_path(file_path, project_dir)

    state = hook_utils.StateManager(project_dir)

    # --- Lock check ---
    blocking = state.check_lock(rel_path, session_id)
    if blocking:
        hook_utils.output_json(
            hook_utils.deny(
                f"File '{rel_path}' is being edited by another agent. "
                "Wait for completion or coordinate via git."
            )
        )
        sys.exit(0)

    # --- Protected files ---
    if is_protected(rel_path):
        hook_utils.output_json(
            hook_utils.deny(
                f"Cannot modify protected file: {rel_path}. "
                "Use appropriate commands or escalate."
            )
        )
        sys.exit(0)

    # --- Secret detection (Write/Edit only, skip test files) ---
    if tool_name in ("Write", "Edit") and not is_test_file(rel_path):
        content: str = (
            tool_input.get("content") or tool_input.get("new_string") or ""
        )
        secret_type = detect_secrets(content)
        if secret_type:
            hook_utils.output_json(
                hook_utils.ask(
                    f"Potential {secret_type} detected in content. "
                    "Please verify this is not sensitive data."
                )
            )
            sys.exit(0)

    # --- Acquire lock ---
    acquired = state.acquire_lock(rel_path, session_id, tool_name)
    if not acquired:
        hook_utils.output_json(
            hook_utils.deny(
                "Failed to acquire file lock — another agent may be editing this file. "
                "Wait and retry."
            )
        )
        sys.exit(0)

    sys.exit(0)


if __name__ == "__main__":
    main()
