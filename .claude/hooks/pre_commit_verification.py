# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook (Bash): enforce pre-commit verification gate.

Fires only when a Bash tool call contains a git commit command.
Blocks the commit unless `task verify` was run recently (5-minute TTL).
Uses deny (hard block), not additionalContext (ignorable hint).
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import re
from pathlib import Path

import hook_utils

_GIT_COMMIT_RE = re.compile(r"\bgit\s+commit\b")


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def is_git_commit(command: str) -> bool:
    """Return True if the shell command contains a git commit invocation."""
    return bool(_GIT_COMMIT_RE.search(command))


def detect_project_tools(project_dir: str) -> list[str]:
    """Detect available project tools by inspecting well-known files.

    Returns a list of tool-name strings such as "cargo-test", "ruff", "go-vet".
    """
    root = Path(project_dir)
    tools: list[str] = []

    # --- Rust ---
    if (root / "Cargo.toml").exists():
        tools.extend(["cargo-test", "cargo-clippy", "cargo-fmt"])

    # --- TypeScript / JavaScript ---
    pkg_json = root / "package.json"
    if pkg_json.exists():
        try:
            pkg_text = pkg_json.read_text()
        except OSError:
            pkg_text = ""

        # Determine package manager
        if (root / "pnpm-lock.yaml").exists():
            pkg_mgr = "pnpm"
        elif (root / "package-lock.json").exists():
            pkg_mgr = "npm"
        else:
            pkg_mgr = "pnpm"

        if '"lint"' in pkg_text:
            tools.append(f"lint({pkg_mgr})")
        if '"test"' in pkg_text:
            tools.append(f"test({pkg_mgr})")
        # Match any of the three typecheck script name variants
        if re.search(r'"typecheck|"tsc|"type-check"', pkg_text):
            tools.append(f"typecheck({pkg_mgr})")

        if (root / "biome.json").exists() or (root / "biome.jsonc").exists():
            tools.append("biome")

    # --- Python ---
    pyproject = root / "pyproject.toml"
    if pyproject.exists():
        try:
            py_text = pyproject.read_text()
        except OSError:
            py_text = ""

        if "ruff" in py_text:
            tools.append("ruff")
        if "pytest" in py_text:
            tools.append("pytest")
        if "mypy" in py_text:
            tools.append("mypy")

    # --- Go ---
    if (root / "go.mod").exists():
        tools.extend(["go-test", "go-vet"])
        # golangci-lint: available in PATH or configured locally
        import shutil

        if shutil.which("golangci-lint") or (root / ".golangci.yml").exists():
            tools.append("golangci-lint")

    return tools


def build_deny_reason(detected_tools: list[str], state_dir: str) -> str:
    """Return the deny reason explaining how to pass the verification gate."""
    tools_str = (
        " ".join(detected_tools) if detected_tools else "none detected - check manually"
    )
    return (
        "BLOCKED: Cannot commit without passing verification.\n\n"
        "Run `task verify` first (format, clippy, lint, license, build, tests).\n"
        f"Detected tools: {tools_str}\n\n"
        "After verification passes, mark it complete:\n"
        f"  echo $(date +%s) > {state_dir}/commit-verified\n\n"
        "Then retry the commit."
    )


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

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)
    state = hook_utils.StateManager(project_dir)

    if state.is_recently_verified():
        sys.exit(0)

    tools = detect_project_tools(project_dir)
    state_dir_str = str(state.state_dir)
    reason = build_deny_reason(tools, state_dir_str)

    hook_utils.output_json(hook_utils.deny(reason))
    sys.exit(0)


if __name__ == "__main__":
    main()
