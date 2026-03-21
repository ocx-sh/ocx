"""Structural validation tests for .claude/ AI configuration.

These tests ensure the AI configuration files are internally consistent:
paths resolve, cross-references are valid, frontmatter conventions are met,
and documented counts match reality. No LLM invocation needed — pure
filesystem checks.

Run:
    cd .claude/tests && uv run pytest test_ai_config.py -v
"""

from __future__ import annotations

import glob
import json
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
CLAUDE_DIR = ROOT / ".claude"
CLAUDE_MD = ROOT / "CLAUDE.md"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def skill_rules() -> dict:
    path = CLAUDE_DIR / "skills" / "skill-rules.json"
    return json.loads(path.read_text())


@pytest.fixture(scope="module")
def claude_md_text() -> str:
    return CLAUDE_MD.read_text()


@pytest.fixture(scope="module")
def claude_md_lines() -> list[str]:
    return CLAUDE_MD.read_text().splitlines()


# ---------------------------------------------------------------------------
# skill-rules.json integrity
# ---------------------------------------------------------------------------


class TestSkillRules:
    """Validate skill-rules.json paths and uniqueness."""

    def test_all_paths_exist(self, skill_rules: dict) -> None:
        """Every path in skill-rules.json must resolve to an existing file."""
        missing = []
        for skill in skill_rules["skills"]:
            path = ROOT / skill["path"]
            if not path.exists():
                missing.append(skill["path"])
        assert not missing, f"skill-rules.json references missing files: {missing}"

    def test_no_duplicate_names(self, skill_rules: dict) -> None:
        """No two skills may share the same name."""
        names = [s["name"] for s in skill_rules["skills"]]
        dupes = [n for n in names if names.count(n) > 1]
        assert not dupes, f"Duplicate skill names: {set(dupes)}"

    def test_all_skill_files_have_entry(self, skill_rules: dict) -> None:
        """Every SKILL.md on disk must have an entry in skill-rules.json."""
        registered = {s["path"] for s in skill_rules["skills"]}
        on_disk = set()
        for skill_md in CLAUDE_DIR.rglob("SKILL.md"):
            rel = skill_md.relative_to(ROOT)
            on_disk.add(str(rel))
        orphans = on_disk - registered
        assert not orphans, f"SKILL.md files without skill-rules.json entry: {orphans}"

    def test_no_ambiguous_keyword_overlap_across_priority_tiers(
        self, skill_rules: dict
    ) -> None:
        """Skills at different priority tiers should not share keywords.

        Bug captured: code-check (medium) and swarm-review (high) both had
        'code review', making code-check unreachable for that trigger.

        Overlaps within the same priority tier or between related skills
        (e.g., language + dependency-management) are acceptable since the hook
        suggests both.
        """
        priority_map = {s["name"]: s["priority"] for s in skill_rules["skills"]}
        keyword_owners: dict[str, list[str]] = {}
        for skill in skill_rules["skills"]:
            for kw in skill["triggers"].get("keywords", []):
                keyword_owners.setdefault(kw, []).append(skill["name"])

        # Known intentional overlaps between related skills
        allowed_pairs = {
            frozenset({"dependency-management", "python"}),
            frozenset({"dependency-management", "rust"}),
        }

        cross_priority_conflicts = {}
        for kw, owners in keyword_owners.items():
            if len(owners) <= 1:
                continue
            priorities = {priority_map[o] for o in owners}
            if len(priorities) > 1:
                pair = frozenset(owners)
                if pair in allowed_pairs:
                    continue
                cross_priority_conflicts[kw] = [
                    f"{o} ({priority_map[o]})" for o in owners
                ]
        assert not cross_priority_conflicts, (
            f"Keywords shared across priority tiers (lower-priority skill is "
            f"unreachable): {cross_priority_conflicts}"
        )

    def test_no_single_common_word_triggers_on_config_skills(
        self, skill_rules: dict
    ) -> None:
        """Operations skills should not use single common words as triggers.

        Bug captured: 'skill', 'rule', 'agent', 'hook' as standalone keywords
        for maintain-ai-config fire on unrelated sentences. High-priority
        persona/language skills (builder, typescript) may use broader triggers
        intentionally.
        """
        noisy_words = {"skill", "rule", "agent", "hook"}
        # Only check operations and medium/low priority skills
        violations = []
        for skill in skill_rules["skills"]:
            category = skill["path"].split("/")[2] if len(skill["path"].split("/")) > 2 else ""
            if category == "operations" or skill["priority"] in ("medium", "low"):
                for kw in skill["triggers"].get("keywords", []):
                    if kw in noisy_words:
                        violations.append((skill["name"], kw))
        assert not violations, (
            f"Operations/low-priority skills with overly broad triggers: {violations}"
        )


# ---------------------------------------------------------------------------
# Rule frontmatter: paths: globs must match at least one file
# ---------------------------------------------------------------------------


class TestRuleGlobs:
    """Verify that scoped rules' paths: globs match actual files."""

    @staticmethod
    def _extract_paths(rule_path: Path) -> list[str]:
        """Parse YAML frontmatter paths: list from a rule file."""
        text = rule_path.read_text()
        if not text.startswith("---"):
            return []
        _, front, _ = text.split("---", 2)
        paths = []
        in_paths = False
        for line in front.splitlines():
            if line.strip().startswith("paths:"):
                in_paths = True
                continue
            if in_paths:
                stripped = line.strip()
                if stripped.startswith("- "):
                    paths.append(stripped[2:].strip().strip('"').strip("'"))
                elif stripped and not stripped.startswith("#"):
                    break
        return paths

    def test_all_rule_globs_match_files(self) -> None:
        """Every paths: glob in .claude/rules/*.md must match >= 1 file."""
        dead_globs = []
        for rule in sorted(CLAUDE_DIR.glob("rules/*.md")):
            for pattern in self._extract_paths(rule):
                matches = glob.glob(str(ROOT / pattern), recursive=True)
                if not matches:
                    dead_globs.append((rule.name, pattern))
        assert not dead_globs, f"Rules with dead glob patterns: {dead_globs}"

    def test_package_manager_glob_not_too_broad(self) -> None:
        """subsystem-package-manager.md should not match unrelated root files.

        Bug captured: crates/ocx_lib/src/*.rs matches 23 unrelated files
        (archive.rs, auth.rs, env.rs, etc.) causing the package manager rule
        to load when editing unrelated code.
        """
        rule = CLAUDE_DIR / "rules" / "subsystem-package-manager.md"
        paths = self._extract_paths(rule)
        # The rule should NOT have a catch-all *.rs glob at the crate root
        broad = [p for p in paths if p.endswith("/*.rs") and "package_manager" not in p]
        assert not broad, (
            f"subsystem-package-manager.md has overly broad glob(s): {broad}. "
            f"This loads the package manager context for unrelated files."
        )


# ---------------------------------------------------------------------------
# CLAUDE.md consistency
# ---------------------------------------------------------------------------


class TestClaudeMd:
    """CLAUDE.md must be internally consistent."""

    def test_line_budget(self, claude_md_lines: list[str]) -> None:
        """CLAUDE.md must stay under 200 lines (context budget rule)."""
        assert len(claude_md_lines) <= 200, (
            f"CLAUDE.md is {len(claude_md_lines)} lines (budget: 200)"
        )

    def test_principle_count_matches_headings(self, claude_md_text: str) -> None:
        """The stated number of principles must match the actual count.

        Bug captured: Says 'seven principles' but there are eight headings
        (### 1 through ### 8).
        """
        # Find the stated count
        match = re.search(r"These (\w+) principles", claude_md_text)
        assert match, "Could not find 'These N principles' in CLAUDE.md"
        stated = match.group(1)

        # Count actual principle headings (### N.)
        headings = re.findall(r"^### \d+\.", claude_md_text, re.MULTILINE)
        actual = len(headings)

        word_to_num = {
            "one": 1, "two": 2, "three": 3, "four": 4, "five": 5,
            "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10,
        }
        stated_num = word_to_num.get(stated.lower(), int(stated) if stated.isdigit() else None)
        assert stated_num == actual, (
            f"CLAUDE.md says '{stated}' principles but has {actual} headings"
        )

    def test_worktree_count_matches_table(self, claude_md_text: str) -> None:
        """The stated worktree count must match the table rows.

        Bug captured: Says 'Three git worktrees' but table has four entries.
        """
        match = re.search(r"\*\*Worktrees\*\*: (\w+) git worktrees", claude_md_text)
        assert match, "Could not find worktree count in CLAUDE.md"
        stated = match.group(1)

        word_to_num = {
            "one": 1, "two": 2, "three": 3, "four": 4, "five": 5,
        }
        stated_num = word_to_num.get(stated.lower(), int(stated) if stated.isdigit() else None)

        # Count table rows (lines with | that aren't headers or separators)
        in_worktree = False
        rows = 0
        for line in claude_md_text.splitlines():
            if "Worktrees" in line:
                in_worktree = True
                continue
            if in_worktree:
                if line.strip().startswith("| `"):
                    rows += 1
                elif line.strip() and not line.strip().startswith("|"):
                    break
        assert stated_num == rows, (
            f"CLAUDE.md says '{stated}' worktrees but table has {rows} entries"
        )


# ---------------------------------------------------------------------------
# Feature workflow step numbering
# ---------------------------------------------------------------------------


class TestFeatureWorkflow:
    """feature-workflow.md must have sequential step numbers."""

    def test_swarm_workflow_step_numbers_are_sequential(self) -> None:
        """Bug captured: Steps go 1, 2, 3, 3, 4, 5, 6, 7 (duplicate 3)."""
        path = CLAUDE_DIR / "rules" / "feature-workflow.md"
        text = path.read_text()

        # Extract numbered list items (N. **Label**)
        steps = re.findall(r"^(\d+)\.\s+\*\*", text, re.MULTILINE)
        numbers = [int(s) for s in steps]

        # Check first workflow section only (before "## Agent Team")
        agent_team_idx = text.index("## Agent Team")
        first_section = text[:agent_team_idx]
        first_steps = re.findall(r"^(\d+)\.\s+\*\*", first_section, re.MULTILINE)
        first_numbers = [int(s) for s in first_steps]

        expected = list(range(1, len(first_numbers) + 1))
        assert first_numbers == expected, (
            f"Swarm workflow steps are not sequential: {first_numbers} "
            f"(expected {expected})"
        )


# ---------------------------------------------------------------------------
# Artifact paths in skills
# ---------------------------------------------------------------------------


class TestArtifactPaths:
    """Skills must reference correct artifact directories."""

    def test_security_auditor_artifact_path(self) -> None:
        """Bug captured: security-auditor says './artifacts/' instead of
        '.claude/artifacts/'."""
        path = CLAUDE_DIR / "skills" / "personas" / "security-auditor" / "SKILL.md"
        text = path.read_text()

        # Must not reference ./artifacts/ (wrong path)
        wrong_refs = re.findall(r"\./artifacts/", text)
        correct_refs = re.findall(r"\.claude/artifacts/", text)

        assert not wrong_refs, (
            f"security-auditor references wrong artifact path './artifacts/' "
            f"(should be '.claude/artifacts/')"
        )
        assert correct_refs, "security-auditor should reference .claude/artifacts/"


# ---------------------------------------------------------------------------
# Agent tool consistency
# ---------------------------------------------------------------------------


class TestAgentDefinitions:
    """Agent frontmatter must be consistent with body content."""

    @staticmethod
    def _parse_agent_tools(agent_path: Path) -> set[str]:
        """Extract tools from agent frontmatter."""
        text = agent_path.read_text()
        if not text.startswith("---"):
            return set()
        _, front, _ = text.split("---", 2)
        for line in front.splitlines():
            if line.strip().startswith("tools:"):
                tools_str = line.split(":", 1)[1].strip()
                return {t.strip() for t in tools_str.split(",")}
        return set()

    def test_architecture_explorer_no_bash_in_body_without_tool(self) -> None:
        """Bug captured: worker-architecture-explorer has ls commands in body
        but no Bash tool in frontmatter."""
        agent = CLAUDE_DIR / "agents" / "worker-architecture-explorer.md"
        tools = self._parse_agent_tools(agent)
        body = agent.read_text()

        has_bash_tool = "Bash" in tools
        # Check for shell commands in code blocks
        has_shell_commands = bool(re.search(r"```bash\n.*\bls\b", body, re.DOTALL))

        if has_shell_commands:
            assert has_bash_tool, (
                "worker-architecture-explorer.md has bash commands in body "
                "but 'Bash' is not in its tools frontmatter"
            )

    def test_worker_tester_mentions_verify(self) -> None:
        """Bug captured: worker-tester.md doesn't mention 'task verify' as
        required by swarm-workers.md coordination protocol."""
        agent = CLAUDE_DIR / "agents" / "worker-tester.md"
        text = agent.read_text()
        assert "task verify" in text, (
            "worker-tester.md must mention 'task verify' per the "
            "swarm-workers.md coordination protocol"
        )


# ---------------------------------------------------------------------------
# Release implementation references
# ---------------------------------------------------------------------------


class TestReleaseImplementation:
    """release-implementation.md must reference actual workflow filenames."""

    def test_workflow_filenames_match_disk(self) -> None:
        """Bug captured: body references 'publish-to-registry.yml' but actual
        file is 'post-release-oci-publish.yml'."""
        rule = CLAUDE_DIR / "rules" / "release-implementation.md"
        text = rule.read_text()
        workflows_dir = ROOT / ".github" / "workflows"

        # Find workflow filenames referenced in context of .github/workflows/
        _, _, body = text.split("---", 2)
        # Match explicit workflow references (e.g., "workflows/foo.yml" or
        # names that appear in workflow-related context). Exclude non-workflow
        # .yml files like dependabot.yml by only checking names that appear
        # near workflow-related text or are in the frontmatter paths list.
        referenced = set(re.findall(r"workflows?/(\w[\w-]+\.yml)", body))
        # Also capture standalone .yml refs that look like workflow names
        # (contain "release", "verify", "publish", "test")
        for match in re.findall(r"`(\w[\w-]+\.yml)`", body):
            if any(w in match for w in ("release", "verify", "publish", "test", "install")):
                referenced.add(match)

        # Check each referenced workflow exists
        missing = []
        for wf in referenced:
            if not (workflows_dir / wf).exists():
                missing.append(wf)

        assert not missing, (
            f"release-implementation.md references non-existent workflows: "
            f"{missing}"
        )


# ---------------------------------------------------------------------------
# Hook script safety
# ---------------------------------------------------------------------------


class TestHookScript:
    """Hook scripts must follow safety rules and conventions."""

    def test_all_hooks_are_python(self) -> None:
        """All hooks should be Python files, not bash scripts."""
        hooks_dir = CLAUDE_DIR / "hooks"
        sh_files = list(hooks_dir.glob("*.sh"))
        ts_files = list(hooks_dir.glob("*.ts"))
        assert not sh_files, f"Bash hooks still exist: {[f.name for f in sh_files]}"
        assert not ts_files, f"TypeScript hooks still exist: {[f.name for f in ts_files]}"

    def test_all_hooks_have_pep723_header(self) -> None:
        """Every Python hook must have a PEP 723 inline script header."""
        hooks_dir = CLAUDE_DIR / "hooks"
        missing = []
        for py_file in sorted(hooks_dir.glob("*.py")):
            if py_file.name == "hook_utils.py":
                continue  # utils module, not a standalone script
            text = py_file.read_text()
            if "# /// script" not in text:
                missing.append(py_file.name)
        assert not missing, f"Hooks missing PEP 723 header: {missing}"

    def test_post_tool_use_tracker_has_try_except(self) -> None:
        """PostToolUse hook must never exit non-zero — needs try/except."""
        hook = CLAUDE_DIR / "hooks" / "post_tool_use_tracker.py"
        text = hook.read_text()
        assert "try:" in text and "except" in text, (
            "post_tool_use_tracker.py must wrap main logic in try/except "
            "to satisfy the PostToolUse non-blocking contract."
        )

    def test_hooks_use_project_dir_env(self) -> None:
        """Hooks must use CLAUDE_PROJECT_DIR, not os.getcwd()."""
        hooks_dir = CLAUDE_DIR / "hooks"
        violations = []
        for py_file in sorted(hooks_dir.glob("*.py")):
            text = py_file.read_text()
            if "os.getcwd()" in text or "Path.cwd()" in text:
                violations.append(py_file.name)
        assert not violations, (
            f"Hooks using cwd instead of CLAUDE_PROJECT_DIR: {violations}"
        )


# ---------------------------------------------------------------------------
# Taskfile lint: empty file list guard
# ---------------------------------------------------------------------------


class TestTaskfileLint:
    """lint.taskfile.yml must handle empty file lists gracefully."""

    def test_shell_lint_handles_no_scripts(self) -> None:
        """Bug captured: when git ls-files '*.sh' returns nothing, shellcheck
        is called with no arguments and exits non-zero, failing task verify."""
        taskfile = ROOT / "taskfiles" / "lint.taskfile.yml"
        text = taskfile.read_text()

        # There should be a precondition or guard for empty SCRIPTS
        has_guard = bool(re.search(
            r"(preconditions|status|\[ -[nz]|\[\[ -[nz]|test -[nz]|exit 0)",
            text,
        ))
        assert has_guard, (
            "lint.taskfile.yml has no guard for empty SCRIPTS variable. "
            "When no .sh files exist, shellcheck/shfmt are called with no "
            "arguments and fail."
        )
