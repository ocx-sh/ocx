# /// script
# requires-python = ">=3.10"
# ///
"""PostToolUse hook (Edit|MultiEdit|Write): track file modifications and emit reminders.

CRITICAL: This is a PostToolUse hook. It MUST never exit non-zero.
All logic is wrapped in a top-level try/except to guarantee silent failure.

Tracks edited files in .file-tracker.log, checks for context staleness
(subsystem rules that may need updating), and emits config update reminders
for infrastructure files.
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import fnmatch
from pathlib import Path

import hook_utils

# ---------------------------------------------------------------------------
# Reminder tables — copied verbatim from post-tool-use-tracker.sh
# ---------------------------------------------------------------------------

# (glob_pattern, rule_filename, subsystem_label)
CONTEXT_REMINDERS: list[tuple[str, str, str]] = [
    ("crates/ocx_lib/src/oci/**", "subsystem-oci.md", "OCI"),
    ("crates/ocx_lib/src/file_structure/**", "subsystem-file-structure.md", "File Structure"),
    ("crates/ocx_lib/src/package/**", "subsystem-package.md", "Package"),
    ("crates/ocx_lib/src/package_manager/**", "subsystem-package-manager.md", "Package Manager"),
    ("crates/ocx_cli/src/**", "subsystem-cli.md", "CLI"),
    ("crates/ocx_mirror/**", "subsystem-mirror.md", "Mirror"),
    ("website/**", "subsystem-website.md", "Website"),
]

# (glob_pattern, targets_text, what_description)
CONFIG_REMINDERS: list[tuple[str, str, str]] = [
    (
        "taskfile.yml",
        (
            "- CLAUDE.md (Build & Development Commands section)\n"
            "- .claude/rules/code-quality.md (task verify pipeline)\n"
            "- .claude/skills/personas/builder/SKILL.md (Task Runner section)\n"
            "- .claude/skills/personas/qa-engineer/SKILL.md (Task Runner section)"
        ),
        "the root Taskfile",
    ),
    (
        "taskfiles/*",
        (
            "- CLAUDE.md (Build & Development Commands section)\n"
            "- .claude/rules/code-quality.md (task verify pipeline)"
        ),
        "an included Taskfile",
    ),
    (
        "test/taskfile.yml",
        (
            "- .claude/rules/subsystem-tests.md (Running Tests section)\n"
            "- .claude/skills/personas/qa-engineer/SKILL.md (Task Runner section)"
        ),
        "the test Taskfile",
    ),
    (
        "website/taskfile.yml",
        (
            "- .claude/rules/subsystem-website.md (Task Commands section)\n"
            "- .claude/skills/product/documentation/SKILL.md (Website Build section)"
        ),
        "the website Taskfile",
    ),
    (
        "website/.vitepress/theme/components/*",
        (
            "- .claude/rules/subsystem-website.md (Custom Vue Components section)\n"
            "- .claude/rules/documentation.md (Vue Components Available section)\n"
            "- .claude/skills/product/documentation/SKILL.md (Vue Components section)"
        ),
        "a Vue component — update props/slots in AI config if the component API changed",
    ),
    (
        "website/.vitepress/config.mts",
        "- .claude/rules/subsystem-website.md (VitePress Configuration section)",
        "the VitePress config",
    ),
    (
        "website/.vitepress/theme/index.mts",
        "- .claude/rules/subsystem-website.md (check component registration list)",
        "the theme registration file — if you added/removed components, update AI config",
    ),
    (
        ".github/workflows/*",
        "- .claude/skills/operations/ci-workflows/SKILL.md",
        "a CI workflow",
    ),
    (
        "Cargo.toml",
        (
            "- .claude/skills/core-engineering/dependency-management/SKILL.md"
            " (if dependencies changed)\n"
            "- CLAUDE.md (Architecture section, if new crates added)"
        ),
        "Cargo.toml",
    ),
    (
        "deny.toml",
        "- .claude/skills/core-engineering/dependency-management/SKILL.md (License Policy section)",
        "the cargo-deny config",
    ),
    (
        "mirrors/mirror-*.yml",
        "- .claude/rules/subsystem-mirror.md (Mirror Configs section)",
        "a mirror configuration",
    ),
    (
        ".claude/rules/subsystem-*.md",
        "- .claude/rules/architecture-principles.md (Design Principles table, Cross-Cutting Modules)",
        "a subsystem context rule — verify architecture principles are still accurate",
    ),
    (
        ".claude/artifacts/adr_*.md",
        "- .claude/rules/architecture-principles.md (ADR Index table)",
        "an ADR — update the ADR Index in architecture-principles.md",
    ),
]


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def glob_match(rel_path: str, pattern: str) -> bool:
    """Return True if rel_path matches the given glob pattern.

    Uses fnmatch.fnmatch() which supports *, ?, and [...].
    The ** pattern is expanded to match any path prefix via a prefix check,
    mirroring the bash glob behaviour for path wildcards.
    """
    if "**" in pattern:
        # Split on ** and check that the prefix matches and suffix matches
        prefix, _, suffix = pattern.partition("**")
        if not rel_path.startswith(prefix):
            return False
        remainder = rel_path[len(prefix):]
        if suffix:
            # suffix may start with '/', strip it for fnmatch
            return fnmatch.fnmatch(remainder, suffix.lstrip("/"))
        return True
    return fnmatch.fnmatch(rel_path, pattern)


def check_context_staleness(rel_path: str) -> str | None:
    """Return a reminder string if rel_path matches a subsystem pattern, else None."""
    for pattern, rule, subsystem in CONTEXT_REMINDERS:
        if glob_match(rel_path, pattern):
            return (
                "---\n"
                "CONTEXT REMINDER\n"
                "---\n"
                "\n"
                f"You edited files in the {subsystem} subsystem.\n"
                f"If you changed public types, error variants, or module structure, "
                f"update .claude/rules/{rule}"
            )
    return None


def check_config_reminder(rel_path: str) -> str | None:
    """Return a reminder string if rel_path matches an infrastructure config pattern."""
    for pattern, targets, what in CONFIG_REMINDERS:
        if glob_match(rel_path, pattern):
            return (
                "---\n"
                "AI CONFIG UPDATE NEEDED\n"
                "---\n"
                "\n"
                f"You changed {what}.\n"
                "Update the following AI configuration to reflect these changes:\n"
                f"{targets}\n"
                "\n"
                "Run task --list to verify available tasks match what is documented."
            )
    return None


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    try:
        data = hook_utils.read_input()

        tool_name: str = data.get("tool_name", "")
        tool_input: dict = data.get("tool_input", {})
        file_path: str = tool_input.get("file_path") or tool_input.get("path") or ""

        if not file_path:
            sys.exit(0)

        if file_path.endswith(".md"):
            sys.exit(0)

        project_dir = hook_utils.get_project_dir()
        if not project_dir:
            sys.exit(0)

        rel_path = hook_utils.relative_path(file_path, project_dir)

        state = hook_utils.StateManager(project_dir)

        # Log the modification
        state.log_modification(tool_name, rel_path)

        # Collect any reminders
        reminders: list[str] = []

        context_reminder = check_context_staleness(rel_path)
        if context_reminder:
            reminders.append(context_reminder)

        config_reminder = check_config_reminder(rel_path)
        if config_reminder:
            reminders.append(config_reminder)

        # Trim tracker log
        state.trim_tracker()

        if reminders:
            print("\n".join(reminders))

    except Exception:
        # PostToolUse must never fail — swallow all exceptions silently
        pass

    sys.exit(0)


if __name__ == "__main__":
    main()
