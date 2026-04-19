# /// script
# requires-python = ">=3.10"
# ///
"""UserPromptSubmit hook: route natural-language prompts to matching skills.

Scans `.claude/skills/*/SKILL.md` at runtime, parses the `triggers:`
frontmatter field, and emits a single-line suggestion when the prompt
contains a known trigger substring. Triggers live in skill frontmatter —
never encoded in this script — so adding a skill automatically extends
the matcher.

Output is capped at one line (~80 chars) to keep context bloat at zero.
Any runtime error is swallowed: the hook is advisory, not gating.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import hook_utils

MIN_PROMPT_LEN = 10


def parse_frontmatter(text: str) -> dict:
    """Parse the leading YAML frontmatter block from a SKILL.md.

    Supports only the subset needed for routing: `name:` (scalar),
    `user-invocable:` (scalar), `triggers:` (list of scalars). Returns
    a dict with those keys when present. Silently ignores unknown keys
    and malformed input — the routing hook is best-effort.
    """
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}

    end = None
    for i in range(1, len(lines)):
        if lines[i].strip() == "---":
            end = i
            break
    if end is None:
        return {}

    result: dict = {}
    current_list_key: str | None = None
    for raw in lines[1:end]:
        if not raw.strip():
            current_list_key = None
            continue
        if raw.startswith("  - ") or raw.startswith("- "):
            if current_list_key is None:
                continue
            item = raw.split("- ", 1)[1].strip()
            if (item.startswith('"') and item.endswith('"')) or (
                item.startswith("'") and item.endswith("'")
            ):
                item = item[1:-1]
            result.setdefault(current_list_key, []).append(item)
            continue
        if ":" in raw and not raw.startswith(" "):
            key, _, value = raw.partition(":")
            key = key.strip()
            value = value.strip()
            if not value:
                current_list_key = key
                continue
            current_list_key = None
            if (value.startswith('"') and value.endswith('"')) or (
                value.startswith("'") and value.endswith("'")
            ):
                value = value[1:-1]
            result[key] = value
    return result


def build_trigger_map(skills_dir: Path) -> dict[str, str]:
    """Return {lowercased-trigger: skill-name} across all user-invocable skills.

    First-wins on duplicates. Glob order (alphabetical on most FS)
    determines iteration. Structural tests forbid cross-skill duplicates,
    but the hook stays forgiving at runtime.
    """
    mapping: dict[str, str] = {}
    for skill_md in sorted(skills_dir.glob("*/SKILL.md")):
        try:
            text = skill_md.read_text()
        except OSError:
            continue
        fm = parse_frontmatter(text)
        if fm.get("user-invocable") != "true":
            continue
        name = fm.get("name") or skill_md.parent.name
        triggers = fm.get("triggers", [])
        if not isinstance(triggers, list):
            continue
        for trigger in triggers:
            key = trigger.strip().lower()
            if not key:
                continue
            mapping.setdefault(key, name)
    return mapping


def find_match(prompt: str, trigger_map: dict[str, str]) -> str | None:
    """Return the skill name for the first trigger substring found in prompt."""
    lowered = prompt.lower()
    for trigger, skill in trigger_map.items():
        if trigger in lowered:
            return skill
    return None


def main() -> None:
    data = hook_utils.read_input()
    prompt: str = data.get("prompt", "") or ""

    if len(prompt) < MIN_PROMPT_LEN:
        sys.exit(0)
    if prompt.lstrip().startswith("/"):
        sys.exit(0)

    project_dir = hook_utils.get_project_dir()
    if not project_dir:
        sys.exit(0)

    skills_dir = Path(project_dir) / ".claude" / "skills"
    if not skills_dir.is_dir():
        sys.exit(0)

    trigger_map = build_trigger_map(skills_dir)
    if not trigger_map:
        sys.exit(0)

    match = find_match(prompt, trigger_map)
    if match:
        print(f"[route] consider /{match} — see .claude/skills/{match}/SKILL.md")

    sys.exit(0)


if __name__ == "__main__":
    main()
