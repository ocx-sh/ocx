# /// script
# requires-python = ">=3.10"
# ///
"""UserPromptSubmit hook: suggest relevant skills based on the user's prompt.

Loads .claude/skills/skill-rules.json and matches keywords and intent patterns
against the submitted prompt. Groups matches by priority and prints suggestions.
"""

import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))

import json
import re
from pathlib import Path

import hook_utils


# ---------------------------------------------------------------------------
# Pure logic functions (testable)
# ---------------------------------------------------------------------------


def load_skill_rules(project_dir: str) -> dict | None:
    """Load skill-rules.json from <project_dir>/.claude/skills/. Returns None on error."""
    rules_path = Path(project_dir) / ".claude" / "skills" / "skill-rules.json"
    try:
        return json.loads(rules_path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def match_skills(prompt: str, skills: list[dict], project_dir: str) -> list[dict]:
    """Match skills against a prompt.

    Returns a list of match dicts with keys: skill, match_type, matched_term.
    Deduplicates: if a skill already matched via keyword, intent patterns are skipped.
    Skill files that do not exist on disk are skipped entirely.
    """
    prompt_lower = prompt.lower()
    matches: list[dict] = []
    matched_names: set[str] = set()

    for skill in skills:
        skill_path = Path(project_dir) / skill.get("path", "")
        if not skill_path.exists():
            continue

        triggers = skill.get("triggers", {})
        name = skill.get("name", "")

        # Keyword match (case-insensitive substring)
        for keyword in triggers.get("keywords", []):
            if keyword.lower() in prompt_lower:
                matches.append(
                    {"skill": skill, "match_type": "keyword", "matched_term": keyword}
                )
                matched_names.add(name)
                break

        # Intent pattern match (regex, IGNORECASE) — skip if already matched by keyword
        if name not in matched_names:
            for pattern in triggers.get("intent_patterns", []):
                try:
                    if re.search(pattern, prompt, re.IGNORECASE):
                        matches.append(
                            {
                                "skill": skill,
                                "match_type": "intent",
                                "matched_term": pattern,
                            }
                        )
                        matched_names.add(name)
                        break
                except re.error:
                    # Invalid regex — skip
                    pass

    return matches


def format_suggestions(matches: list[dict]) -> str:
    """Format matched skills into a human-readable suggestion block."""
    priority_order = ["critical", "high", "medium", "low"]
    icons: dict[str, str] = {
        "critical": "[!]",
        "high": "[*]",
        "medium": "[+]",
        "low": "[i]",
    }

    grouped: dict[str, list[dict]] = {p: [] for p in priority_order}
    for match in matches:
        priority = match["skill"].get("priority", "low")
        if priority in grouped:
            grouped[priority].append(match)

    lines: list[str] = ["", "---", "SKILL SUGGESTIONS", "---"]

    for priority in priority_order:
        group = grouped[priority]
        if group:
            lines.append("")
            lines.append(f"{icons[priority]} {priority.upper()} PRIORITY:")
            for match in group:
                skill = match["skill"]
                lines.append(f"   - {skill['name']}: {skill['description']}")
                lines.append(f"     Path: {skill['path']}")
                lines.append(
                    f"     Matched: {match['match_type']} \"{match['matched_term']}\""
                )

    lines.append("")
    lines.append(
        "To use a suggested skill, read its SKILL.md file at the provided path with the Read tool, then follow its instructions."
    )
    lines.append("---")

    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    try:
        data = hook_utils.read_input()
        if not data:
            sys.exit(0)

        prompt: str = data.get("prompt", "")
        if not prompt:
            sys.exit(0)

        project_dir = hook_utils.get_project_dir() or data.get("cwd", "")
        if not project_dir:
            sys.exit(0)

        rules = load_skill_rules(project_dir)
        if not rules:
            sys.exit(0)

        skills = rules.get("skills", [])
        if not skills:
            sys.exit(0)

        matches = match_skills(prompt, skills, project_dir)
        if not matches:
            sys.exit(0)

        print(format_suggestions(matches))
    except Exception:
        # Silently exit on any error — never disrupt workflow
        sys.exit(0)


if __name__ == "__main__":
    main()
