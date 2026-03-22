# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook (Bash): validate conventional commit message format.

Fires only when a Bash tool call contains a git commit command with -m.
Blocks commits whose message does not match the conventional commits pattern.
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import re

import hook_utils

_GIT_COMMIT_RE = re.compile(r"\bgit\s+commit\b")

# Matches conventional commit types used in this project (see CLAUDE.md)
_CONVENTIONAL_RE = re.compile(
    r"^(feat|fix|refactor|ci|chore|docs|test|perf|build|style)(\(.+\))?(!)?\s*: .+"
)

# Extract the message from: git commit -m "message" or git commit -m 'message'
# Also handles heredoc: git commit -m "$(cat <<'EOF'\nmessage\nEOF\n)"
_MSG_SINGLE_QUOTE_RE = re.compile(r"""-m\s+'([^']+)'""")
_MSG_DOUBLE_QUOTE_RE = re.compile(r'''-m\s+"([^"]+)"''')
_MSG_HEREDOC_RE = re.compile(r"-m\s+\"\$\(cat\s+<<'?EOF'?\n(.+?)(?:\n\s*EOF)", re.DOTALL)


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def is_git_commit(command: str) -> bool:
    """Return True if the shell command contains a git commit invocation."""
    return bool(_GIT_COMMIT_RE.search(command))


def extract_commit_message(command: str) -> str | None:
    """Extract the commit message from a git commit -m command.

    Returns the first line of the message, or None if no -m flag found.
    """
    # Try heredoc first (most complex pattern)
    match = _MSG_HEREDOC_RE.search(command)
    if match:
        # Return first non-empty line
        for line in match.group(1).splitlines():
            stripped = line.strip()
            if stripped:
                return stripped
        return None

    # Try single-quoted message
    match = _MSG_SINGLE_QUOTE_RE.search(command)
    if match:
        return match.group(1).splitlines()[0].strip()

    # Try double-quoted message
    match = _MSG_DOUBLE_QUOTE_RE.search(command)
    if match:
        return match.group(1).splitlines()[0].strip()

    return None


def is_conventional_commit(message: str) -> bool:
    """Return True if the message follows conventional commit format."""
    return bool(_CONVENTIONAL_RE.match(message.strip()))


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    data = hook_utils.read_input()

    tool_name: str = data.get("tool_name", "")
    if tool_name != "Bash":
        sys.exit(0)

    command: str = data.get("tool_input", {}).get("command", "")
    if not is_git_commit(command):
        sys.exit(0)

    # Only validate if there's a -m flag (interactive commits can't be validated)
    message = extract_commit_message(command)
    if message is None:
        sys.exit(0)

    if not is_conventional_commit(message):
        hook_utils.output_json(
            hook_utils.deny(
                f"BLOCKED: Commit message does not follow conventional commits format.\n\n"
                f"Got: \"{message}\"\n\n"
                f"Expected: <type>(<optional scope>): <description>\n"
                f"Types: feat, fix, refactor, ci, chore, docs, test, perf, build, style\n"
                f"Examples:\n"
                f"  feat: add package search command\n"
                f"  fix(oci): handle missing manifest gracefully\n"
                f"  chore: update AI configuration"
            )
        )

    sys.exit(0)


if __name__ == "__main__":
    main()
