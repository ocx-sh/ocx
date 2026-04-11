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

        Historical artifacts (`.claude/artifacts/`) are exempt — they preserve
        prior-state references intentionally with header notes.
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
            # Skip historical artifacts — they preserve old references
            if "artifacts" in rule_file.parts:
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
            if "artifacts" in md.parts:
                continue  # historical — preserves old references
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
