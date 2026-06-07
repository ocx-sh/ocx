# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook (Bash): enforce pre-commit verification gate.

Fires only when a Bash tool call contains a git commit command **and**
the commit's git repo root matches this project's root. Commits in
unrelated sibling repos (e.g. a mirror candidate at /home/me/dev/other)
bypass the gate — their verify gate isn't ours.

Blocks the commit unless `task verify` was run recently (5-minute TTL).
Uses deny (hard block), not additionalContext (ignorable hint).
"""

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import hook_utils

_GIT_COMMIT_RE = re.compile(r"\bgit\s+commit\b")
_CD_PREFIX_RE = re.compile(r"^\s*cd\s+(?:'([^']+)'|\"([^\"]+)\"|(\S+))\s*&&")


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def is_git_commit(command: str) -> bool:
    """Return True if the shell command contains a git commit invocation."""
    return bool(_GIT_COMMIT_RE.search(command))


def parse_command_cwd(command: str) -> str | None:
    """Extract the working directory from a leading `cd X && ...` prefix.

    Returns the path string (unquoted) or None when no recognised prefix.
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


def commit_targets_this_project(command: str, project_dir: str) -> bool:
    """Return True when the git repo the command will commit into is this project.

    Heuristic — if we can't pin down the commit's cwd (no `cd … && …`
    prefix, no callable ``git``), assume the commit IS for this project so
    the gate still applies. False-positives on a cross-repo commit are
    acceptable; false-negatives (skipping the gate for an in-repo commit)
    are not.
    """
    cwd = parse_command_cwd(command)
    if cwd is None:
        # No explicit cd prefix → safest assumption is the commit is in
        # the project dir. Keep the gate.
        return True
    git_root = resolve_git_root(cwd)
    if git_root is None:
        # `cd …` to a non-repo? Then `git commit` will fail anyway; let
        # the gate apply so we surface a clearer signal.
        return True
    try:
        return os.path.realpath(git_root) == os.path.realpath(project_dir)
    except OSError:
        return True


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

    # NEW: scope the gate to commits in *this* project's git repo. Commits
    # routed into a sibling repo via `cd /other/repo && git commit …` are
    # not our verify concern — that repo has its own pipeline. Without this
    # scoping, every cross-repo commit hits the OCX gate even though no
    # OCX code is touched.
    if not commit_targets_this_project(command, project_dir):
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
