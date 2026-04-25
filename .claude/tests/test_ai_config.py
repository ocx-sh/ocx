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
def claude_md_text() -> str:
    return CLAUDE_MD.read_text()


@pytest.fixture(scope="module")
def claude_md_lines() -> list[str]:
    return CLAUDE_MD.read_text().splitlines()


# ---------------------------------------------------------------------------
# Shareable quality rules: project-independent, path-scoped, no OCX leak
# ---------------------------------------------------------------------------


class TestShareableQualityRules:
    """Quality rules must be project-independent for cross-repo sharing.

    The `quality-core.md` root and `quality-{lang}.md` leaves are intended to
    be copyable into other repositories Michael owns. They must contain zero
    OCX-specific strings — OCX patterns live in `arch-principles.md`
    and `subsystem-*.md`.
    """

    _OCX_FORBIDDEN_STRINGS = [
        "PackageErrorKind",
        "ReferenceManager",
        "PackageManager",
        "ocx_lib",
        "ocx_cli",
        "ocx_mirror",
        "to_relaxed_slug",
        "DIGEST_FILENAME",
        "crates/ocx",
        "DirWalker",
        "Printable",
    ]

    _SHAREABLE_RULES = [
        "quality-core.md",
        "quality-rust.md",
        "quality-python.md",
        "quality-typescript.md",
        "quality-bash.md",
        "quality-vite.md",
    ]

    def test_shareable_rules_no_ocx_leak(self) -> None:
        """Shareable quality rules must not reference OCX-specific names."""
        violations = []
        for name in self._SHAREABLE_RULES:
            path = CLAUDE_DIR / "rules" / name
            if not path.exists():
                continue  # rule not yet created
            text = path.read_text()
            for forbidden in self._OCX_FORBIDDEN_STRINGS:
                if forbidden in text:
                    violations.append((name, forbidden))
        assert not violations, (
            f"Shareable quality rules contain OCX-specific strings: {violations}. "
            f"OCX-specific patterns belong in arch-principles.md or "
            f"subsystem-*.md rules, not in shareable quality rules "
            f"(see meta-ai-config.md Anti-Pattern #10)."
        )

    def test_all_quality_rules_have_paths_frontmatter(self) -> None:
        """Every `quality-{lang}.md` rule must be path-scoped.

        The root `quality-core.md` is global (no paths:) — that's by design,
        it's cross-language. The language leaves must be scoped so they only
        load when editing files of that language.
        """
        missing = []
        for name in self._SHAREABLE_RULES:
            if name == "quality-core.md":
                continue  # root is intentionally global
            path = CLAUDE_DIR / "rules" / name
            if not path.exists():
                continue
            paths_in_frontmatter = TestRuleGlobs._extract_paths(path)
            if not paths_in_frontmatter:
                missing.append(name)
        assert not missing, (
            f"Quality rules missing `paths:` frontmatter: {missing}. "
            f"Language quality rules must be path-scoped so they only load "
            f"when editing that language."
        )

    def test_no_references_to_deleted_language_skills(self) -> None:
        """No files should reference the removed language skills.

        After the reorg, `.claude/skills/{python,rust,typescript,bash,vite}/`
        are gone. Any remaining reference is a broken link.

        Historical artifacts (`.claude/artifacts/`) and ephemeral plan
        scratch (`.claude/state/`) are exempt — they preserve prior-state
        references intentionally with header notes.
        """
        deleted_skills = [
            "skills/python/SKILL.md",
            "skills/rust/SKILL.md",
            "skills/typescript/SKILL.md",
            "skills/bash/SKILL.md",
            "skills/vite/SKILL.md",
        ]
        violations = []
        for rule_file in CLAUDE_DIR.rglob("*.md"):
            # Skip historical artifacts + ephemeral state — they preserve old references
            if "artifacts" in rule_file.parts or "state" in rule_file.parts:
                continue
            text = rule_file.read_text()
            for deleted in deleted_skills:
                if deleted in text:
                    violations.append((str(rule_file.relative_to(ROOT)), deleted))
        assert not violations, (
            f"Files reference deleted language skills: {violations}. "
            f"Update to reference the corresponding quality-*.md rule."
        )


# ---------------------------------------------------------------------------
# Skills layout: flat directory structure (no category subdirectories)
# ---------------------------------------------------------------------------


class TestSkillsLayout:
    """Enforce the canonical flat `.claude/skills/<name>/SKILL.md` layout.

    Claude Code discovers skills at `.claude/skills/<name>/SKILL.md` exactly —
    it does not recurse into nested skill directories for in-project skills.
    These tests lock in the flat layout.
    """

    _CATEGORY_DIRS = {
        "personas",
        "operations",
        "languages",
        "core-engineering",
        "product",
    }

    def test_all_skills_at_flat_layout(self) -> None:
        """SKILL.md files must live at `.claude/skills/<name>/SKILL.md`."""
        flat = list((CLAUDE_DIR / "skills").glob("*/SKILL.md"))
        nested = list((CLAUDE_DIR / "skills").glob("*/*/SKILL.md"))
        assert flat, "No SKILL.md files found at .claude/skills/<name>/SKILL.md"
        assert not nested, (
            f"Skills must live at `.claude/skills/<name>/SKILL.md` — no "
            f"category subdirectories. Claude Code does not discover nested "
            f"skill paths. Found: {[str(p.relative_to(ROOT)) for p in nested]}"
        )

    def test_skill_dir_matches_frontmatter_name(self) -> None:
        """Each skill's directory name must equal its frontmatter `name:` field."""
        mismatches = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            text = skill_md.read_text()
            if not text.startswith("---"):
                continue
            _, front, _ = text.split("---", 2)
            frontmatter_name = None
            for line in front.splitlines():
                if line.strip().startswith("name:"):
                    frontmatter_name = line.split(":", 1)[1].strip().strip('"').strip("'")
                    break
            if frontmatter_name is None:
                continue
            dir_name = skill_md.parent.name
            if frontmatter_name != dir_name:
                mismatches.append((dir_name, frontmatter_name))
        assert not mismatches, (
            f"Skill directory name must match frontmatter `name:` field — "
            f"Claude Code uses the directory name as the slash command "
            f"identifier. Mismatches (dir, name): {mismatches}"
        )

    def test_no_category_directories_under_skills(self) -> None:
        """No `personas/`, `operations/`, `languages/`, etc. under `.claude/skills/`."""
        skills_dir = CLAUDE_DIR / "skills"
        violating = [
            name
            for name in self._CATEGORY_DIRS
            if (skills_dir / name).is_dir()
        ]
        assert not violating, (
            f"Category subdirectories are forbidden under `.claude/skills/` "
            f"— they break slash command discovery. Found: {violating}"
        )

    def test_action_skills_disable_model_invocation(self) -> None:
        """Skills with side-effectful argument hints must opt out of auto-invocation.

        Any skill whose `argument-hint` contains action verbs
        (deploy|release|sync|create|update|commit|push) must set
        `disable-model-invocation: true` in its frontmatter. This prevents
        Claude from triggering destructive or network-touching workflows
        without explicit user intent.
        """
        action_verbs = re.compile(
            r"\b(deploy|release|sync|create|update|commit|push|mirror)\b",
            re.IGNORECASE,
        )
        violations: list[tuple[str, str]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            text = skill_md.read_text()
            if not text.startswith("---"):
                continue
            _, front, _ = text.split("---", 2)
            frontmatter: dict[str, str] = {}
            for line in front.splitlines():
                if ":" in line:
                    key, _, value = line.partition(":")
                    frontmatter[key.strip()] = value.strip().strip('"').strip("'")
            arg_hint = frontmatter.get("argument-hint", "")
            name = frontmatter.get("name", skill_md.parent.name)
            if not arg_hint:
                continue
            if not action_verbs.search(arg_hint) and not action_verbs.search(name):
                continue
            if frontmatter.get("disable-model-invocation", "").lower() != "true":
                violations.append((name, arg_hint))
        assert not violations, (
            f"Skills with action-verb argument hints must set "
            f"`disable-model-invocation: true`. Violations (name, hint): "
            f"{violations}"
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
        """Every paths: glob in .claude/rules/*.md must match >= 1 file.

        Shareable `quality-*.md` rules are exempt: they are designed to match
        file types that may exist in *other* repositories where this rule gets
        copied, not just OCX. A missing match in OCX doesn't mean the glob is
        dead — it means that file type isn't used here. Dead-glob detection
        still applies to OCX-specific rules (subsystem-*.md, architecture-
        principles.md, product-context.md, etc.).
        """
        shareable_prefixes = ("quality-",)
        dead_globs = []
        for rule in sorted(CLAUDE_DIR.glob("rules/*.md")):
            if rule.name.startswith(shareable_prefixes):
                continue
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
    """workflow-feature.md must have sequential step numbers."""

    def test_swarm_workflow_step_numbers_are_sequential(self) -> None:
        """Bug captured: Steps go 1, 2, 3, 3, 4, 5, 6, 7 (duplicate 3)."""
        path = CLAUDE_DIR / "rules" / "workflow-feature.md"
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
        path = CLAUDE_DIR / "skills" / "security-auditor" / "SKILL.md"
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
        required by workflow-swarm.md coordination protocol."""
        agent = CLAUDE_DIR / "agents" / "worker-tester.md"
        text = agent.read_text()
        assert "task verify" in text, (
            "worker-tester.md must mention 'task verify' per the "
            "workflow-swarm.md coordination protocol"
        )

    def test_worker_reviewer_inlines_quality_rules(self) -> None:
        """worker-reviewer.md must inline a minimal tagged preamble of
        block-tier quality anchors — not the full checklist.

        Updated for rule catalog refactor: the agent no longer inlines the
        full 20-item checklist. Instead it cites a short "Always Apply"
        preamble (≤5 anchors) plus a pointer to `.claude/rules.md`. Each
        block-tier anchor must cite its source rule file so drift is
        visible at review.
        """
        agent = CLAUDE_DIR / "agents" / "worker-reviewer.md"
        text = agent.read_text()

        # Must point at the rule catalog
        assert "rules.md" in text, (
            "worker-reviewer.md must point at `.claude/rules.md` so the "
            "reviewer can discover rules that don't auto-load."
        )

        # Must have an "Always Apply" block-tier preamble
        assert "Always Apply" in text, (
            "worker-reviewer.md must have an 'Always Apply' section with "
            "block-tier anchors that fire at attention."
        )

        # Must cite source rule files in the preamble (visible drift)
        assert "quality-rust.md" in text, (
            "worker-reviewer.md preamble must cite `quality-rust.md` as "
            "the source of Rust block-tier anchors."
        )

        # Minimum anchor set — must cover the highest-severity Rust rules
        minimum_anchors = [".unwrap()", "MutexGuard", "blocking I/O"]
        missing = [a for a in minimum_anchors if a not in text]
        assert not missing, (
            f"worker-reviewer.md preamble must include block-tier anchors. "
            f"Missing: {missing}"
        )

    def test_worker_builder_reads_quality_rules_before_writes(self) -> None:
        """worker-builder.md must point at `.claude/rules.md` and ship a
        minimal tagged "Always Apply" preamble BEFORE the on-completion
        section. The catalog replaces the old "read the rule file first"
        step since path-scoped rules auto-load while the agent writes.
        """
        agent = CLAUDE_DIR / "agents" / "worker-builder.md"
        text = agent.read_text()

        assert "rules.md" in text, (
            "worker-builder.md must point at `.claude/rules.md` so the "
            "builder can discover rules that don't auto-load."
        )
        assert "Always Apply" in text, (
            "worker-builder.md must have an 'Always Apply' preamble that "
            "fires at attention even when path-scoped rules don't load."
        )

        catalog_pointer = text.find("rules.md")
        completion_header = text.find("On Completion")
        assert catalog_pointer >= 0 and completion_header >= 0
        assert catalog_pointer < completion_header, (
            "worker-builder.md must reference the catalog before the "
            "On Completion section (rules come before reporting)."
        )


# ---------------------------------------------------------------------------
# Rule catalog: `.claude/rules.md` must mirror `.claude/rules/`
# ---------------------------------------------------------------------------


class TestRuleCatalog:
    """`.claude/rules.md` is the discoverability entry point for rules.

    It must stay in sync with the contents of `.claude/rules/` so that
    plan-phase and research-phase readers get an accurate map of what
    rules exist before any file is open.
    """

    CATALOG = CLAUDE_DIR / "rules.md"

    def test_catalog_exists(self) -> None:
        assert self.CATALOG.exists(), (
            "`.claude/rules.md` must exist — it is the authoritative "
            "rule catalog pointed to from CLAUDE.md."
        )

    def test_catalog_covers_all_rules(self) -> None:
        """Every rule file in `.claude/rules/*.md` must be referenced
        somewhere in `.claude/rules.md`."""
        catalog_text = self.CATALOG.read_text()
        missing = []
        for rule in sorted(CLAUDE_DIR.glob("rules/*.md")):
            if rule.name not in catalog_text:
                missing.append(rule.name)
        assert not missing, (
            f"Rules missing from `.claude/rules.md`: {missing}. "
            f"Add each new rule to the relevant table in the catalog."
        )

    def test_catalog_references_resolve(self) -> None:
        """Every `*.md` reference in the catalog must resolve to a real
        file under `.claude/rules/` (or be a clearly non-rule link)."""
        text = self.CATALOG.read_text()
        # Match backticked rule filenames like `quality-rust.md`
        refs = set(re.findall(r"`([a-z][a-z0-9-]*\.md)`", text))
        missing = []
        for ref in refs:
            candidate = CLAUDE_DIR / "rules" / ref
            if not candidate.exists():
                missing.append(ref)
        assert not missing, (
            f"Catalog references non-existent rule files: {missing}"
        )

    def test_claude_md_points_to_catalog(self) -> None:
        """CLAUDE.md must link to `.claude/rules.md` so the catalog stays
        discoverable for every session."""
        text = CLAUDE_MD.read_text()
        assert ".claude/rules.md" in text, (
            "CLAUDE.md must contain a link to `.claude/rules.md` — the "
            "catalog is only valuable if every session sees the pointer."
        )

    def test_all_markdown_refs_resolve(self) -> None:
        """Every backticked `*.md` reference in `.claude/**/*.md` and
        `CLAUDE.md` must resolve to a real file on disk.

        Catches drift after renames: if a rule is renamed but a worker,
        skill, or catalog still points at the old name, that reference
        is effectively dead — the rule will never be discovered from
        that path. Skips historical artifacts (they preserve old state).

        **Provisional**: this is a regex-based fallback. It only catches
        backticked bare filenames, not real markdown links or anchor
        fragments. Replace with `task claude:lint:links` (lychee) once lychee
        is mirrored via OCX, installed across dev + CI, and wired into
        the `claude:tests` task. At that point delete this test.
        Tracking: the `mirrors/lychee/` config exists but has not yet
        been sync'd to a registry.
        """
        # Candidate directories for resolving a bare filename
        search_dirs = [
            CLAUDE_DIR / "rules",
            CLAUDE_DIR / "agents",
            CLAUDE_DIR / "references",
            CLAUDE_DIR / "hooks",
            CLAUDE_DIR / "templates",
            CLAUDE_DIR,
            ROOT,
        ]

        # Allowlist: filenames that are external, third-party, or
        # documentation-style references we don't need to resolve.
        allowlist = {
            "file.md",
            "somefile.md",
            "N.md",
            "README.md",
            "CHANGELOG.md",
            # Generated at build time, not in repo
            "dependencies.md",
        }

        # Prefixes for artifact/plan/memory example patterns — these show
        # up in tables and prose as naming examples, not real references.
        example_prefixes = (
            "adr_",
            "plan_",
            "system_design_",
            "design_spec_",
            "security_audit_",
            "research_",
            "feedback_",
            "project_",
            "user_",
        )

        def find_file(name: str) -> bool:
            if name in allowlist:
                return True
            if name.startswith(example_prefixes):
                return True
            for d in search_dirs:
                if (d / name).exists():
                    return True
            # Also allow anywhere under .claude or root (catch templates/ etc.)
            matches = list(CLAUDE_DIR.rglob(name))
            if matches:
                return True
            matches = list((ROOT / "website").rglob(name)) if (ROOT / "website").exists() else []
            if matches:
                return True
            return False

        ref_pattern = re.compile(r"`([a-z][a-z0-9_-]*\.md)`")

        targets: list[Path] = [CLAUDE_MD]
        for md in CLAUDE_DIR.rglob("*.md"):
            if "artifacts" in md.parts or "state" in md.parts:
                continue  # historical/ephemeral — preserves old references
            if "tests" in md.parts:
                continue  # test file itself doesn't reference rule files
            targets.append(md)

        missing: list[tuple[str, str]] = []
        for md in targets:
            text = md.read_text()
            for match in ref_pattern.findall(text):
                if not find_file(match):
                    missing.append((str(md.relative_to(ROOT)), match))

        assert not missing, (
            f"Markdown files reference non-existent `.md` files. Each tuple "
            f"is (file with broken ref, missing target):\n" +
            "\n".join(f"  {src} → {tgt}" for src, tgt in missing)
        )

    def test_catalog_subsystem_coverage(self) -> None:
        """Every subsystem listed in CLAUDE.md's subsystem table must also
        appear in the catalog's `By subsystem` section. The catalog is
        allowed to list more subsystems than CLAUDE.md (it's the fuller
        reference), but it must never list fewer."""
        claude_text = CLAUDE_MD.read_text()
        catalog_text = self.CATALOG.read_text()

        # Extract rule names from CLAUDE.md subsystem table rows
        # Lines look like: "| OCI registry/index | `subsystem-oci.md` | ... |"
        claude_rules = set(re.findall(r"`(subsystem-[a-z-]+\.md)`", claude_text))
        catalog_rules = set(re.findall(r"`(subsystem-[a-z-]+\.md)`", catalog_text))

        missing = claude_rules - catalog_rules
        assert not missing, (
            f"Subsystems listed in CLAUDE.md but missing from catalog "
            f"`By subsystem` section: {missing}"
        )


# ---------------------------------------------------------------------------
# Release implementation references
# ---------------------------------------------------------------------------


class TestReleaseImplementation:
    """workflow-release.md must reference actual workflow filenames."""

    def test_workflow_filenames_match_disk(self) -> None:
        """Bug captured: body references 'publish-to-registry.yml' but actual
        file is 'post-release-oci-publish.yml'."""
        rule = CLAUDE_DIR / "rules" / "workflow-release.md"
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
            f"workflow-release.md references non-existent workflows: "
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
    """ocx.taskfile.yml must handle empty file lists gracefully."""

    def test_shell_lint_handles_no_scripts(self) -> None:
        """Bug captured: when git ls-files '*.sh' returns nothing, shellcheck
        is called with no arguments and exits non-zero, failing task verify.
        The guard now lives in the reusable ocx-exec template."""
        taskfile = ROOT / "taskfiles" / "ocx.taskfile.yml"
        text = taskfile.read_text()

        # There should be a precondition or guard for empty file lists
        has_guard = bool(re.search(
            r"(preconditions|status|\[ -[nz]|\[\[ -[nz]|test -[nz]|exit 0)",
            text,
        ))
        assert has_guard, (
            "ocx.taskfile.yml has no guard for empty file lists. "
            "When no matching files exist, the tool is called with no "
            "arguments and fails."
        )


# ---------------------------------------------------------------------------
# Swarm tier dispatch: progressive disclosure invariants
# Parametrized across all tiered swarm skills.
# ---------------------------------------------------------------------------


# Skills that use the progressive-disclosure tier dispatch pattern.
# Each entry: (skill-dir name, flag grammar that must appear verbatim in
# both SKILL.md and overlays.md). The flag grammar is the subset unique
# to that skill — `--codex` is shared and always required.
_TIERED_SWARM_SKILLS = [
    (
        "swarm-plan",
        (
            "--architect=inline|sonnet|opus",
            "--research=skip|1|3",
            "--codex",
        ),
    ),
    (
        "swarm-execute",
        (
            "--builder=sonnet|opus",
            "--loop-rounds=1|2|3",
            "--review=minimal|full|adversarial",
            "--codex",
        ),
    ),
    (
        "swarm-review",
        (
            "--breadth=minimal|full|adversarial",
            "--rca=on|off",
            "--codex",
        ),
    ),
]


class TestSwarmTiers:
    """Tiered swarm skills use progressive disclosure — SKILL.md is a
    thin dispatch layer that reads one of `tier-{low,high,max}.md`. The
    classifier and overlay axis definitions live in their own files.
    These tests lock in the structural invariants of that layout across
    every tiered swarm skill."""

    _TIER_FILES = ("tier-low.md", "tier-high.md", "tier-max.md")
    _SUPPORT_FILES = ("classify.md", "overlays.md")

    @pytest.mark.parametrize(
        "skill_name", [name for name, _ in _TIERED_SWARM_SKILLS]
    )
    def test_skill_md_under_200_lines(self, skill_name: str) -> None:
        """SKILL.md must stay under 200 lines — dispatch-only.

        Per meta-ai-config.md, SKILL.md bodies should stay ≤500, but
        tiered swarm skills adopt a tighter ceiling since phase content
        has been extracted into tier files. 200 is the hard ceiling.
        """
        skill = CLAUDE_DIR / "skills" / skill_name / "SKILL.md"
        line_count = len(skill.read_text().splitlines())
        assert line_count < 200, (
            f"{skill_name}/SKILL.md is {line_count} lines (ceiling: 199). "
            f"Phase plans belong in tier-*.md; shared content here."
        )

    @pytest.mark.parametrize(
        "skill_name", [name for name, _ in _TIERED_SWARM_SKILLS]
    )
    def test_tier_files_exist(self, skill_name: str) -> None:
        """Each tier file referenced by SKILL.md must exist on disk."""
        skill_dir = CLAUDE_DIR / "skills" / skill_name
        skill_text = (skill_dir / "SKILL.md").read_text()
        missing = [t for t in self._TIER_FILES if not (skill_dir / t).exists()]
        # SKILL.md must reference them by name (dispatch contract)
        unreferenced = [
            t for t in self._TIER_FILES
            if t not in skill_text and t.replace("-", "") not in skill_text
        ]
        assert not missing, f"{skill_name} tier files missing from disk: {missing}"
        assert not unreferenced, (
            f"{skill_name}/SKILL.md must reference each tier file by name: "
            f"{unreferenced}"
        )

    @pytest.mark.parametrize(
        "skill_name", [name for name, _ in _TIERED_SWARM_SKILLS]
    )
    def test_support_files_exist(self, skill_name: str) -> None:
        """classify.md and overlays.md must exist and be referenced
        from SKILL.md."""
        skill_dir = CLAUDE_DIR / "skills" / skill_name
        skill_text = (skill_dir / "SKILL.md").read_text()
        for name in self._SUPPORT_FILES:
            path = skill_dir / name
            assert path.exists(), f"Missing {skill_name} support file: {name}"
            assert name in skill_text, (
                f"{skill_name}/SKILL.md must reference {name} so the "
                f"dispatch knows to Read it."
            )

    @pytest.mark.parametrize(
        "skill_name", [name for name, _ in _TIERED_SWARM_SKILLS]
    )
    def test_classify_has_example_per_tier(self, skill_name: str) -> None:
        """classify.md must show at least one example per tier (low /
        high / max) so the classifier has concrete anchors."""
        text = (CLAUDE_DIR / "skills" / skill_name / "classify.md").read_text()
        missing = [
            t for t in ("**low**", "**high**", "**max**")
            if t not in text
        ]
        assert not missing, (
            f"{skill_name}/classify.md missing examples for: {missing}. "
            f"Each tier needs at least one example signal."
        )

    @pytest.mark.parametrize(
        ("skill_name", "required_forms"), _TIERED_SWARM_SKILLS
    )
    def test_overlays_match_skill_flag_grammar(
        self, skill_name: str, required_forms: tuple[str, ...]
    ) -> None:
        """overlays.md axis values must match the flag grammar declared
        in SKILL.md. Drift between the two produces parser bugs."""
        skill_dir = CLAUDE_DIR / "skills" / skill_name
        skill_text = (skill_dir / "SKILL.md").read_text()
        overlays_text = (skill_dir / "overlays.md").read_text()

        drift = []
        for form in required_forms:
            if form not in skill_text:
                drift.append((skill_name, "SKILL.md", form))
            if form not in overlays_text:
                drift.append((skill_name, "overlays.md", form))
        assert not drift, (
            f"Flag grammar drift between SKILL.md and overlays.md: "
            f"{drift}. Keep the two in lockstep."
        )

    @pytest.mark.parametrize(
        "skill_name", [name for name, _ in _TIERED_SWARM_SKILLS]
    )
    def test_tier_files_preserve_contract_first_tdd(
        self, skill_name: str
    ) -> None:
        """Every tier must preserve the contract-first TDD skeleton
        (Stub → Specify → Implement → Review). Dropping it at any tier
        breaks the plan→execute handoff contract."""
        skill_dir = CLAUDE_DIR / "skills" / skill_name
        skeleton_anchors = ["Stub", "Specify", "Implement", "Review"]
        violations = []
        for tier in self._TIER_FILES:
            text = (skill_dir / tier).read_text()
            missing = [a for a in skeleton_anchors if a not in text]
            if missing:
                violations.append((skill_name, tier, missing))
        assert not violations, (
            f"Tier files must keep the contract-first TDD skeleton. "
            f"Missing anchors: {violations}"
        )


# ---------------------------------------------------------------------------
# AI config overhaul — Phase 1 invariants
# ---------------------------------------------------------------------------


class TestAiConfigOverhaulPhase1:
    """Post-Phase-1 invariants for the AI config overhaul.

    Locks in the path-scope correction (workflow rules scoped, not global),
    the 3-global enumeration in meta-ai-config.md, and the declared overlap
    table in rules.md. See .claude/artifacts/plan_ai_config_overhaul.md.
    """

    def test_workflow_rules_have_paths(self) -> None:
        """workflow-bugfix.md and workflow-refactor.md must be path-scoped.

        They self-label as catalog-only; without `paths:` frontmatter they load
        every session and contribute ~230 undocumented lines to the always-loaded
        baseline. Enforced post-Phase-1 of the AI config overhaul.
        """
        missing = []
        for name in ("workflow-bugfix.md", "workflow-refactor.md"):
            path = CLAUDE_DIR / "rules" / name
            assert path.exists(), f"{name} missing"
            paths = TestRuleGlobs._extract_paths(path)
            if not paths:
                missing.append(name)
        assert not missing, (
            f"Rules missing `paths:` frontmatter: {missing}. "
            f"See adr_ai_config_path_scope_correction.md."
        )

    def test_global_rule_count_matches(self) -> None:
        """Stated global-rule count in meta-ai-config.md must match actual.

        A global rule is any `.claude/rules/*.md` file without a non-empty
        `paths:` frontmatter entry. Post Phase 1 of the AI config overhaul,
        the authoritative count is 3 and the list appears in meta-ai-config.md
        under `### Current Global Rules`. `rules.md` and `CLAUDE.md` reach
        Claude by a different mechanism (`@`-import / root instructions) and
        are not counted here.
        """
        rules_dir = CLAUDE_DIR / "rules"
        globals_found = []
        for rule in sorted(rules_dir.glob("*.md")):
            paths = TestRuleGlobs._extract_paths(rule)
            if not paths:
                globals_found.append(rule.name)
        assert len(globals_found) == 3, (
            f"Expected exactly 3 global rules (no `paths:` frontmatter), "
            f"got {len(globals_found)}: {globals_found}"
        )
        meta_text = (rules_dir / "meta-ai-config.md").read_text()
        assert "### Current Global Rules" in meta_text, (
            "meta-ai-config.md must contain `### Current Global Rules` "
            "enumeration (Phase 1 T3)"
        )
        # Every global must be explicitly named in the enumeration
        for name in globals_found:
            assert name in meta_text, (
                f"Global rule {name} is not enumerated in meta-ai-config.md "
                f"`### Current Global Rules` — stated/actual drift"
            )

    def test_path_overlaps_declared_or_absent(self) -> None:
        """Any two rules sharing a `paths:` pattern must be declared in rules.md.

        Exempt rules (intended broad coupling):
        - `quality-*.md` — language quality rules co-fire with subsystem rules
          on `**/*.rs`, `**/*.py`, etc. (e.g., quality-rust.md + subsystem-oci.md
          on `**/*.rs` is intended).
        - `workflow-bugfix.md`, `workflow-refactor.md` — source-work-surface
          scope (`crates/**`, `test/**`, `website/**`, `mirrors/**`, `.claude/**`)
          per adr_ai_config_path_scope_correction.md. Co-firing with subsystem
          rules on their respective scopes is the intended coupling.
        """
        _exempt_prefixes = ("quality-",)
        _exempt_names = {"workflow-bugfix.md", "workflow-refactor.md"}
        rules_dir = CLAUDE_DIR / "rules"
        pattern_owners: dict[str, list[str]] = {}
        for rule in sorted(rules_dir.glob("*.md")):
            if rule.name.startswith(_exempt_prefixes):
                continue
            if rule.name in _exempt_names:
                continue
            for p in TestRuleGlobs._extract_paths(rule):
                pattern_owners.setdefault(p, []).append(rule.name)

        catalog = (CLAUDE_DIR / "rules.md").read_text()
        # Extract declared pairs from the overlap table: lines like
        # `| \`file-a.md\` + \`file-b.md\` | ...`
        declared_pairs: set[frozenset[str]] = set()
        for line in catalog.splitlines():
            if "+" not in line or "`" not in line or not line.startswith("|"):
                continue
            files = re.findall(r"`([^`]+\.md)`", line)
            if len(files) >= 2:
                # Treat all filenames in the left cell as one declared group
                declared_pairs.add(frozenset(files))

        undeclared: list[tuple[str, list[str]]] = []
        for pattern, owners in pattern_owners.items():
            if len(owners) < 2:
                continue
            # Any pair drawn from owners must appear as a subset of a declared group
            for i in range(len(owners)):
                for j in range(i + 1, len(owners)):
                    pair = frozenset({owners[i], owners[j]})
                    if not any(pair <= group for group in declared_pairs):
                        undeclared.append((pattern, [owners[i], owners[j]]))
                        break
        assert not undeclared, (
            f"Undeclared path-scope overlaps: {undeclared}. "
            f"Declare in rules.md `## Declared Path-Scope Overlaps` table or "
            f"narrow one of the rule's `paths:` patterns."
        )


# ---------------------------------------------------------------------------
# AI config overhaul — Phase 2 invariants (CSO description audit)
# ---------------------------------------------------------------------------


class TestAiConfigOverhaulPhase2:
    """Post-Phase-2 invariants for the AI config overhaul.

    Locks in the Contextual Signal Only (CSO) policy for skill descriptions:
    descriptions describe trigger conditions (what the user says / what the
    task looks like), never the workflow itself. See
    `.claude/artifacts/adr_ai_config_skill_description_csopolicy.md`.
    """

    # Hyphen-aware word boundary: require a whitespace / punctuation boundary
    # on both sides so hyphen-joined fragments like `dry-runs` or `re-runs`
    # do not falsely trigger the CSO filter.
    _FORBIDDEN_VERB_RE = re.compile(
        r"(?<![\w-])(dispatches|runs|iterates|orchestrates|performs|executes|handles)(?![\w-])",
        re.IGNORECASE,
    )

    # Per-skill `disable-model-invocation` intent table. Prevents accidental
    # flips (action skill losing the flag, or pure-advisory skill gaining it).
    _EXPECTED_DISABLE_MODEL_INVOCATION = {
        # Action skills with side effects — must disable auto-invocation
        "commit": True,
        "finalize": True,
        "codex-adversary": True,
        "meta-maintain-config": True,
        "ocx-create-mirror": True,
        "ocx-sync-roadmap": True,
        "swarm-plan": True,
        "swarm-execute": True,
        # Pure analysis / advisory — auto-invocation safe
        "architect": False,
        "builder": False,
        "code-check": False,
        "deps": False,
        "docs": False,
        "meta-validate-context": False,
        "next": True,
        "qa-engineer": False,
        "security-auditor": False,
        "swarm-review": False,
    }

    @staticmethod
    def _parse_frontmatter(skill_md: Path) -> dict[str, str]:
        """Parse single-line key: value pairs from the first `---` block.

        Intentionally simple — CSO descriptions are required to be single-line
        so this parser is sufficient. Multi-line (block-scalar) descriptions
        would produce a truncated value, which the CSO tests flag.
        """
        text = skill_md.read_text()
        if not text.startswith("---"):
            return {}
        _, front, _ = text.split("---", 2)
        fm: dict[str, str] = {}
        for line in front.splitlines():
            if ":" not in line:
                continue
            key, _, value = line.partition(":")
            fm[key.strip()] = value.strip().strip('"').strip("'")
        return fm

    def test_skill_descriptions_are_cso_compliant(self) -> None:
        """Every skill description must describe trigger conditions, not the
        workflow itself. Forbidden literal verbs: dispatches, runs, iterates,
        orchestrates, performs, executes, handles (case-insensitive).

        Per meta-ai-config.md budget: each description ≤1024 chars. CSO
        descriptions are single-line; multi-line (block-scalar) is disallowed
        because the simple parser above would truncate and the real context
        loader would concatenate — both paths degrade discoverability.
        """
        violations: list[tuple[str, str]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            name = skill_md.parent.name
            fm = self._parse_frontmatter(skill_md)
            desc = fm.get("description", "")
            if not desc:
                violations.append((name, "missing or empty description"))
                continue
            if len(desc) > 1024:
                violations.append(
                    (name, f"description too long: {len(desc)} > 1024 chars")
                )
            match = self._FORBIDDEN_VERB_RE.search(desc)
            if match:
                violations.append(
                    (name, f"contains forbidden verb {match.group(0)!r}")
                )
        assert not violations, (
            f"Skill descriptions violate CSO policy: {violations}. "
            f"See `.claude/artifacts/adr_ai_config_skill_description_csopolicy.md`."
        )

    def test_skill_description_budget_under_cap(self) -> None:
        """Sum of all skill description chars must stay under the 4000-char
        cap (buffer below Anthropic's 1% context-window description budget).

        Pre-Phase-2 baseline was 5004 chars; Phase 2 target is ≤4000, giving
        ≈20% headroom for future skill growth before hitting the cap.
        """
        total = 0
        per_skill: list[tuple[str, int]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            fm = self._parse_frontmatter(skill_md)
            desc = fm.get("description", "")
            total += len(desc)
            per_skill.append((skill_md.parent.name, len(desc)))
        assert total <= 4000, (
            f"Total skill description budget {total} chars exceeds 4000-char "
            f"cap. Per-skill lengths: {per_skill}"
        )

    def test_skill_disable_model_invocation_intent(self) -> None:
        """Fixture-based stability test: every skill's
        `disable-model-invocation` flag must match its declared intent.

        Prevents accidental flips — e.g. an action skill losing the flag
        (silently auto-invoked by Claude), or a pure-advisory skill gaining
        it (needlessly removed from auto-invocation).
        """
        mismatches: list[tuple[str, object, object]] = []
        unlisted: list[str] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            name = skill_md.parent.name
            if name not in self._EXPECTED_DISABLE_MODEL_INVOCATION:
                unlisted.append(name)
                continue
            fm = self._parse_frontmatter(skill_md)
            actual = fm.get("disable-model-invocation", "false").lower() == "true"
            expected = self._EXPECTED_DISABLE_MODEL_INVOCATION[name]
            if actual != expected:
                mismatches.append((name, expected, actual))
        assert not unlisted, (
            f"Skills not in `_EXPECTED_DISABLE_MODEL_INVOCATION` intent table: "
            f"{unlisted}. Add each to the table with the correct expected "
            f"flag value before it will pass the stability check."
        )
        assert not mismatches, (
            f"Skill `disable-model-invocation` intent mismatches "
            f"(name, expected, actual): {mismatches}. "
            f"Update the skill frontmatter or the intent table in this test "
            f"if the policy has genuinely changed."
        )


# ---------------------------------------------------------------------------
# AI config overhaul — Phase 4 invariants (cross-session learnings store)
# ---------------------------------------------------------------------------


class TestAiConfigOverhaulPhase4:
    """Post-Phase-4 invariants for the AI config overhaul.

    Locks in the project-local learnings store location and the
    `meta-ai-config.md` Cross-Session Learnings section. See
    `.claude/artifacts/adr_ai_config_cross_session_learnings_store.md`.
    """

    def test_gitignore_contains_state_dir(self) -> None:
        """`.gitignore` must ignore `.claude/state/` so the learnings store
        (and other per-worktree ephemera) is never accidentally committed.

        Phase 3 added the entry; this test locks it in so a future
        gitignore edit cannot silently drop it.
        """
        gitignore = ROOT / ".gitignore"
        assert gitignore.exists(), "`.gitignore` missing at repo root"
        text = gitignore.read_text()
        assert ".claude/state/" in text, (
            "`.gitignore` must contain `.claude/state/` — per-worktree "
            "learnings store / context samples must not be committed. "
            "See `.claude/artifacts/adr_ai_config_cross_session_learnings_store.md`."
        )

    def test_meta_ai_config_has_cross_session_learnings_section(self) -> None:
        """`meta-ai-config.md` must document the Cross-Session Learnings
        Store section and cite the ADR path."""
        meta = CLAUDE_DIR / "rules" / "meta-ai-config.md"
        text = meta.read_text()
        assert "## Cross-Session Learnings Store" in text, (
            "meta-ai-config.md must contain `## Cross-Session Learnings Store` "
            "header (Phase 4 T4)"
        )
        assert "adr_ai_config_cross_session_learnings_store.md" in text, (
            "meta-ai-config.md Cross-Session Learnings section must cite "
            "the ADR path so readers can find the decision record."
        )


# ---------------------------------------------------------------------------
# AI config overhaul — Phase 5 invariants (Review-Fix Loop parity + skill body budget)
# ---------------------------------------------------------------------------


class TestAiConfigOverhaulPhase5:
    """Post-Phase-5 invariants for the AI config overhaul.

    Locks in the three-carrier byte-identical Review-Fix Loop parity
    (`workflow-swarm.md`, `workflow-bugfix.md`, `workflow-refactor.md`)
    and the 200-line ceiling for every `SKILL.md`. See
    `.claude/artifacts/adr_ai_config_review_loop_dedup.md` and
    `.claude/artifacts/plan_ai_config_overhaul.md` (Phase 5).
    """

    _CANONICAL_CARRIERS = (
        CLAUDE_DIR / "rules" / "workflow-swarm.md",
        CLAUDE_DIR / "rules" / "workflow-bugfix.md",
        CLAUDE_DIR / "rules" / "workflow-refactor.md",
    )

    # Files that must point at the canonical Review-Fix Loop but NOT contain
    # the canonical markers themselves. Prevents accidental fourth carrier.
    _POINTER_ONLY_FILES = (
        CLAUDE_DIR / "rules" / "workflow-feature.md",
        CLAUDE_DIR / "skills" / "swarm-execute" / "SKILL.md",
        CLAUDE_DIR / "skills" / "swarm-execute" / "tier-low.md",
        CLAUDE_DIR / "skills" / "swarm-execute" / "tier-high.md",
        CLAUDE_DIR / "skills" / "swarm-execute" / "tier-max.md",
    )

    _BEGIN_MARKER = "<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->"
    _END_MARKER = "<!-- REVIEW_FIX_LOOP_CANONICAL_END -->"

    # Explicit exception list for `test_skill_body_budget`. Every entry needs
    # a docstring comment (in this class) explaining why it is exempt.
    # Current state: empty. Every SKILL.md is ≤200 lines after Phase 5.
    _SKILL_BODY_BUDGET_EXCEPTIONS: tuple[str, ...] = ()

    def test_review_fix_loop_parity(self) -> None:
        """The three canonical carriers must contain byte-identical
        Review-Fix Loop blocks between the HTML comment markers.

        Carriers: `workflow-swarm.md`, `workflow-bugfix.md`,
        `workflow-refactor.md`. Pointer-only files (workflow-feature.md,
        swarm-execute SKILL + tier files) must NOT contain the markers
        (they link to the canonical) and MUST contain a pointer to
        `workflow-swarm.md#review-fix-loop`.
        """
        # Every carrier must have exactly one BEGIN and one END marker
        carrier_blocks: dict[str, str] = {}
        for carrier in self._CANONICAL_CARRIERS:
            assert carrier.exists(), f"Canonical carrier missing: {carrier}"
            text = carrier.read_text()
            begin_count = text.count(self._BEGIN_MARKER)
            end_count = text.count(self._END_MARKER)
            assert begin_count == 1, (
                f"{carrier.name} must contain exactly one "
                f"{self._BEGIN_MARKER} marker (got {begin_count})."
            )
            assert end_count == 1, (
                f"{carrier.name} must contain exactly one "
                f"{self._END_MARKER} marker (got {end_count})."
            )
            begin_idx = text.index(self._BEGIN_MARKER)
            end_idx = text.index(self._END_MARKER) + len(self._END_MARKER)
            assert begin_idx < end_idx, (
                f"{carrier.name}: BEGIN marker must precede END marker."
            )
            carrier_blocks[carrier.name] = text[begin_idx:end_idx]

        # Byte-identity across all three carriers
        reference_name, reference_block = next(iter(carrier_blocks.items()))
        divergent: list[tuple[str, str]] = []
        for name, block in carrier_blocks.items():
            if block != reference_block:
                divergent.append((name, reference_name))
        assert not divergent, (
            f"Canonical Review-Fix Loop blocks diverged across carriers. "
            f"Every carrier must contain byte-identical prose between the "
            f"markers. Divergent carriers: {divergent}. "
            f"See `.claude/artifacts/adr_ai_config_review_loop_dedup.md`."
        )

        # Pointer-only files must NOT contain the markers (no fourth carrier)
        illegal_carriers: list[str] = []
        for pointer in self._POINTER_ONLY_FILES:
            assert pointer.exists(), f"Pointer-only file missing: {pointer}"
            text = pointer.read_text()
            if self._BEGIN_MARKER in text or self._END_MARKER in text:
                illegal_carriers.append(str(pointer.relative_to(ROOT)))
        assert not illegal_carriers, (
            f"Pointer-only files contain canonical Review-Fix Loop markers "
            f"(would create a fourth carrier): {illegal_carriers}. "
            f"Replace the marker block with a pointer to "
            f"`workflow-swarm.md#review-fix-loop`."
        )

        # Pointer-only files must link to the canonical anchor (or equivalent)
        missing_pointer: list[str] = []
        for pointer in self._POINTER_ONLY_FILES:
            text = pointer.read_text()
            # Accept any reference to the canonical carrier's Review-Fix Loop
            # section — anchor slug `#review-fix-loop` or direct filename
            # pointer is sufficient.
            if "workflow-swarm.md#review-fix-loop" not in text:
                missing_pointer.append(str(pointer.relative_to(ROOT)))
        assert not missing_pointer, (
            f"Pointer-only files missing a link to "
            f"`workflow-swarm.md#review-fix-loop`: {missing_pointer}."
        )

    def test_skill_body_budget(self) -> None:
        """Every `.claude/skills/*/SKILL.md` must be ≤200 lines.

        SKILL.md is loaded only when invoked — per meta-ai-config.md the
        budget is <500 lines — but post-Phase-5 every SKILL.md in OCX
        stays ≤200 via progressive disclosure (`references/` subdir for
        detail material). Exceptions live in
        `_SKILL_BODY_BUDGET_EXCEPTIONS` with a docstring justification.
        """
        violations: list[tuple[str, int]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            name = skill_md.parent.name
            if name in self._SKILL_BODY_BUDGET_EXCEPTIONS:
                continue
            line_count = len(skill_md.read_text().splitlines())
            if line_count > 200:
                violations.append((name, line_count))
        assert not violations, (
            f"SKILL.md files exceed the 200-line progressive-disclosure "
            f"budget: {violations}. Extract detail sections into "
            f"`<skill-dir>/references/*.md` and replace with pointers, or "
            f"add the skill to `_SKILL_BODY_BUDGET_EXCEPTIONS` with a "
            f"justification."
        )


# ---------------------------------------------------------------------------
# UserPromptSubmit routing hook: triggers contract + hook sanity
# ---------------------------------------------------------------------------


class TestPromptRoutingTriggers:
    """Enforce the `triggers:` contract for user-invocable skills.

    The `user_prompt_router.py` UserPromptSubmit hook reads the
    `triggers:` frontmatter field from each skill at runtime. Any
    user-invocable skill without triggers silently drops out of the
    matcher, so natural-language prompts never route to it.
    """

    _ALLOWED_SINGLE_WORD_TRIGGERS = {"deps", "commit", "finalize"}
    _MIN_TRIGGERS = 3
    _MAX_TRIGGERS = 7

    @staticmethod
    def _parse_frontmatter(text: str) -> dict:
        if not text.startswith("---"):
            return {}
        lines = text.splitlines()
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

    @classmethod
    def _user_invocable_skills(cls) -> list[tuple[str, dict]]:
        out: list[tuple[str, dict]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            fm = cls._parse_frontmatter(skill_md.read_text())
            if fm.get("user-invocable") == "true":
                out.append((skill_md.parent.name, fm))
        return out

    def test_user_invocable_skills_have_triggers(self) -> None:
        """Every user-invocable skill must declare 3–7 triggers."""
        violations: list[tuple[str, str]] = []
        for name, fm in self._user_invocable_skills():
            triggers = fm.get("triggers")
            if not isinstance(triggers, list) or not triggers:
                violations.append((name, "missing or empty triggers: list"))
                continue
            if not (self._MIN_TRIGGERS <= len(triggers) <= self._MAX_TRIGGERS):
                violations.append(
                    (name, f"has {len(triggers)} triggers (want 3–7)")
                )
        assert not violations, (
            f"User-invocable skills missing or malformed `triggers:` "
            f"frontmatter: {violations}. The UserPromptSubmit routing hook "
            f"reads this list at runtime — without it, natural-language "
            f"prompts never route to the skill. See "
            f"`.claude/rules/meta-ai-config.md` Anti-Pattern #12."
        )

    def test_triggers_unique_across_skills(self) -> None:
        """No trigger phrase may appear in two skills' `triggers:` lists.

        The routing hook uses first-match-wins at runtime, but
        cross-skill duplicates are ambiguous by design — they silently
        bias routing on glob order. The structural gate fails loud.
        """
        seen: dict[str, str] = {}
        collisions: list[tuple[str, str, str]] = []
        for name, fm in self._user_invocable_skills():
            for trigger in fm.get("triggers") or []:
                key = trigger.strip().lower()
                if not key:
                    continue
                if key in seen and seen[key] != name:
                    collisions.append((key, seen[key], name))
                else:
                    seen[key] = name
        assert not collisions, (
            f"Duplicate triggers across skills "
            f"(trigger, first-skill, second-skill): {collisions}"
        )

    def test_triggers_are_discriminating(self) -> None:
        """Each trigger must be ≥2 words OR an allowed single-word domain token."""
        violations: list[tuple[str, str]] = []
        for name, fm in self._user_invocable_skills():
            for trigger in fm.get("triggers") or []:
                key = trigger.strip()
                words = key.split()
                if len(words) >= 2:
                    continue
                if key.lower() in self._ALLOWED_SINGLE_WORD_TRIGGERS:
                    continue
                violations.append((name, trigger))
        assert not violations, (
            f"Single-word triggers are only allowed from the domain-token "
            f"set {sorted(self._ALLOWED_SINGLE_WORD_TRIGGERS)}. Violations "
            f"(skill, trigger): {violations}. Use a 2+ word phrase to reduce "
            f"false positives."
        )


class TestUserPromptRouter:
    """Sanity checks on the user_prompt_router.py hook script."""

    _HOOK = CLAUDE_DIR / "hooks" / "user_prompt_router.py"

    def test_user_prompt_router_has_pep723_header(self) -> None:
        text = self._HOOK.read_text()
        assert "# /// script" in text, (
            "user_prompt_router.py must start with a PEP 723 inline "
            "script header (`# /// script` … `# ///`)."
        )

    def test_user_prompt_router_uses_project_dir_env(self) -> None:
        text = self._HOOK.read_text()
        assert "get_project_dir" in text, (
            "user_prompt_router.py must resolve the project directory via "
            "`hook_utils.get_project_dir()` — not `os.getcwd()` / `Path.cwd()`."
        )
        assert "os.getcwd()" not in text and "Path.cwd()" not in text, (
            "user_prompt_router.py must not use `os.getcwd()` or `Path.cwd()`."
        )

    def test_user_prompt_router_exits_zero(self) -> None:
        """Every `sys.exit(...)` in the router must pass `0`.

        The hook is advisory; a non-zero exit would make Claude Code
        treat the prompt as blocked. AST scan to catch future drift.
        """
        import ast

        tree = ast.parse(self._HOOK.read_text())
        bad: list[tuple[int, str]] = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            func = node.func
            name = None
            if isinstance(func, ast.Attribute) and func.attr == "exit":
                if isinstance(func.value, ast.Name) and func.value.id == "sys":
                    name = "sys.exit"
            elif isinstance(func, ast.Name) and func.id == "exit":
                name = "exit"
            if name is None:
                continue
            if not node.args:
                continue
            arg = node.args[0]
            if isinstance(arg, ast.Constant) and arg.value == 0:
                continue
            bad.append((node.lineno, ast.unparse(node)))
        assert not bad, (
            f"user_prompt_router.py must only ever `sys.exit(0)` — the hook "
            f"is advisory, not gating. Non-zero exits found: {bad}"
        )

    def test_user_prompt_router_registered_in_settings(self) -> None:
        import json

        settings_path = CLAUDE_DIR / "settings.json"
        settings = json.loads(settings_path.read_text())
        hooks = settings.get("hooks", {})
        ups = hooks.get("UserPromptSubmit") or []
        commands = [
            h.get("command", "")
            for entry in ups
            for h in entry.get("hooks", [])
        ]
        assert any(
            "user_prompt_router.py" in cmd for cmd in commands
        ), (
            "user_prompt_router.py is not registered under "
            "`hooks.UserPromptSubmit` in `.claude/settings.json`."
        )

    def test_user_prompt_router_output_is_single_line(self) -> None:
        """Every `print(...)` call in the router emits a single line.

        AST scan: the printed expression must be a constant or f-string
        whose literal parts contain no newline. Guards the "zero context
        bloat" invariant from the item 3 design.
        """
        import ast

        tree = ast.parse(self._HOOK.read_text())
        bad: list[tuple[int, str]] = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            func = node.func
            if not (isinstance(func, ast.Name) and func.id == "print"):
                continue
            if not node.args:
                continue
            arg = node.args[0]
            if isinstance(arg, ast.Constant):
                if isinstance(arg.value, str) and "\n" in arg.value:
                    bad.append((node.lineno, repr(arg.value)))
                continue
            if isinstance(arg, ast.JoinedStr):
                for part in arg.values:
                    if isinstance(part, ast.Constant) and isinstance(
                        part.value, str
                    ) and "\n" in part.value:
                        bad.append((node.lineno, ast.unparse(arg)))
                        break
                continue
            bad.append((node.lineno, ast.unparse(node)))
        assert not bad, (
            f"user_prompt_router.py `print(...)` calls must emit a single "
            f"line with no newlines — zero context bloat is a load-bearing "
            f"invariant. Violations: {bad}"
        )
