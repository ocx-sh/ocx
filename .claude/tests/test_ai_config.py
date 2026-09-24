"""Structural validation tests for .claude/ AI configuration.

These tests ensure the AI configuration files are internally consistent:
paths resolve, cross-references are valid, frontmatter conventions are met,
and documented counts match reality. No LLM invocation needed — pure
filesystem checks.

Run:
    cd .claude/tests && uv run pytest test_ai_config.py -v
"""

from __future__ import annotations

import ast
import functools
import glob
import os
import re
import shutil
import subprocess
import time
import tomllib
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parents[2]
CLAUDE_DIR = ROOT / ".claude"


@functools.cache
def _tracked_paths() -> tuple[str, ...]:
    """Every tracked path, submodules included, as `git ls-files` spells it.

    Glob liveness is judged against this rather than the disk: a recursive
    `glob.glob` from the root walked `target/` and followed the `bazel-*`
    symlinks into the output base, 9 s for one pattern. A glob that only
    matches build output or ignored files names nothing a rule could load on.
    """
    out = subprocess.run(  # noqa: S603
        ["git", "ls-files", "-z", "--recurse-submodules"],  # noqa: S607
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    paths = tuple(name for name in out.split("\0") if name)
    # Floor the reader: an empty listing would make every glob read as dead,
    # and a truncated one would make the dead ones look like a real finding.
    assert len(paths) >= 1000, f"`git ls-files` returned {len(paths)} paths; the reader stopped early"
    return paths


def _glob_is_live(pattern: str) -> bool:
    """Does `pattern` (a `paths:` glob, relative to the root) match a tracked file?"""
    regex = re.compile(glob.translate(pattern, recursive=True, include_hidden=True))
    return any(regex.fullmatch(path) for path in _tracked_paths())
CLAUDE_MD = ROOT / "CLAUDE.md"
GRIMOIRE_LOCK = ROOT / "grimoire.lock"


# ---------------------------------------------------------------------------
# Vendored vs. project-authored config
# ---------------------------------------------------------------------------
#
# `grim` installs skills and rules into `.claude/` from OCI bundles pinned in
# `grimoire.lock`, and overwrites them wholesale on the next `grim install`.
# A *project-authoring* property — description wording, body-length budget,
# `triggers:` frontmatter, `disable-model-invocation` intent — is therefore
# upstream's contract for those files, not this repo's. Asserting it against a
# vendored file reds this gate on an upstream refresh with nothing here to fix,
# and the only "fix" would be hand-editing a file the next pull clobbers.
#
# The split is read from the lockfile rather than a hand-maintained list, so
# dropping a bundle from `grimoire.toml` puts its artifacts back under this
# repo's authoring rules automatically. A missing lockfile raises rather than
# silently exempting nothing — the strict direction.


def _vendored(kind: str) -> frozenset[str]:
    """Names of `[[skill]]` / `[[rule]]` entries in `grimoire.lock`."""
    lock = tomllib.loads(GRIMOIRE_LOCK.read_text(encoding="utf-8"))
    return frozenset(entry["name"] for entry in lock.get(kind, []))


VENDORED_SKILLS = _vendored("skill")
VENDORED_RULES = _vendored("rule")


def project_skills() -> list[Path]:
    """`SKILL.md` paths this repo authors — vendored skills excluded."""
    return [
        p
        for p in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md"))
        if p.parent.name not in VENDORED_SKILLS
    ]


def is_vendored(md: Path) -> bool:
    """True for a markdown file `grim` installs (vendored skill or rule)."""
    parts = md.relative_to(CLAUDE_DIR).parts
    if len(parts) < 2:
        return False
    if parts[0] == "skills":
        return parts[1] in VENDORED_SKILLS
    if parts[0] == "rules":
        return parts[1].removesuffix(".md") in VENDORED_RULES
    return False


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def claude_md_text() -> str:
    return CLAUDE_MD.read_text(encoding="utf-8")


@pytest.fixture(scope="module")
def claude_md_lines() -> list[str]:
    return CLAUDE_MD.read_text(encoding="utf-8").splitlines()


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
            text = path.read_text(encoding="utf-8")
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
            # Relative to `.claude/`, never absolute — see the same guard below.
            parts = rule_file.relative_to(CLAUDE_DIR).parts
            # Skip historical artifacts + ephemeral state — they preserve old references
            if "artifacts" in parts or "state" in parts:
                continue
            if "worktrees" in parts:
                continue  # another checkout's config, on another branch
            text = rule_file.read_text(encoding="utf-8")
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
            text = skill_md.read_text(encoding="utf-8")
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

    # Owner-unlocked action skills (2026-07-21): deliberately model-invocable
    # despite an action-verb name/hint, so an agent runs the documented
    # workflow instead of improvising the same side effects ad hoc. `/commit`
    # qualifies because its own workflow is the safety rail (stage by name,
    # never push, never `--no-verify`). Reversible by flipping frontmatter back.
    MODEL_INVOCABLE_ACTION_SKILLS = {"commit"}

    def test_action_skills_disable_model_invocation(self) -> None:
        """Skills with side-effectful argument hints must opt out of auto-invocation.

        Any skill whose `argument-hint` contains action verbs
        (deploy|release|sync|create|update|commit|push) must set
        `disable-model-invocation: true` in its frontmatter, unless it is
        listed in `MODEL_INVOCABLE_ACTION_SKILLS`. This prevents Claude from
        triggering destructive or network-touching workflows without explicit
        user intent.
        """
        action_verbs = re.compile(
            r"\b(deploy|release|sync|create|update|commit|push|mirror)\b",
            re.IGNORECASE,
        )
        violations: list[tuple[str, str]] = []
        for skill_md in sorted((CLAUDE_DIR / "skills").glob("*/SKILL.md")):
            text = skill_md.read_text(encoding="utf-8")
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
            if name in self.MODEL_INVOCABLE_ACTION_SKILLS:
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
        text = rule_path.read_text(encoding="utf-8")
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

    @staticmethod
    def _is_shareable(rule: Path) -> bool:
        """Two spellings of "shareable": the `quality-*.md` prefix, and a
        `repository:` frontmatter field naming the upstream the rule is
        vendored from (`rust-*.md`, `bazel-quality.md`).

        Scoped to the frontmatter block, not the first N lines: a rule whose
        *body* happens to open a line with `repository:` would otherwise
        exempt itself from dead-glob detection silently.
        """
        if rule.name.startswith("quality-"):
            return True
        text = rule.read_text(encoding="utf-8")
        frontmatter, closed, _ = text.partition("\n---")
        return (
            text.startswith("---")
            and bool(closed)
            and any(line.startswith("repository:") for line in frontmatter.splitlines())
        )

    @pytest.mark.parametrize(
        "rule", sorted(CLAUDE_DIR.glob("rules/*.md")), ids=lambda rule: rule.name
    )
    def test_all_rule_globs_match_files(self, rule: Path) -> None:
        """Every paths: glob in this rule must match >= 1 file.

        One test id per rule file, so the crate split's directory moves red
        the rule whose glob died by name rather than as one entry in a list
        (plan_crate_split_workspace.md C-024).

        Shareable rules match file types that may exist in *other*
        repositories where the rule gets copied, not just OCX, so a glob with
        no match here is not dead — it names a file type this repo does not
        use. Such a rule skips, and the skip message carries the measured
        ratio; a shareable rule whose globs all match is a plain pass.
        Dead-glob detection binds every OCX-specific rule (subsystem-*.md,
        arch-principles.md, product-context.md, etc.).
        """
        patterns = self._extract_paths(rule)
        dead = [p for p in patterns if not _glob_is_live(p)]
        if dead and self._is_shareable(rule):
            pytest.skip(
                f"shareable rule: {len(patterns) - len(dead)}/{len(patterns)} globs match "
                f"this repository; unmatched here (not dead): {dead}"
            )
        assert not dead, f"{rule.name} has dead glob pattern(s): {dead}"

    def test_package_manager_glob_not_too_broad(self) -> None:
        """subsystem-package-manager.md should not match unrelated root files.

        Bug captured: a `crates/<crate>/src/*.rs` glob matched 23 unrelated files
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


class TestCrateSplitSweep:
    """C-075: the AI-config surface names no path inside the dissolved crate.

    `ocx_lib` was deleted at WP-37. A path literal naming it resolves to
    nothing, and a path that resolves to nothing is the failure this repo keeps
    finding: a glob that fires never, an instruction that sends the next agent
    to a directory that is not there, a rule whose `paths:` matches no file.

    The crate *name* in prose is not swept — "extracted from `ocx_lib` at
    WP-33" is the record of why a seam exists and must survive. What is swept
    is the **path** under `crates/`, which can only be a pointer.

    `DEAD_PATH` is assembled from two halves on purpose: this file is itself
    under a swept root, so spelling the run here would make the sweep report
    its own source and it would be red in every state. Same self-matching
    family as a `pgrep` whose pattern matches the shell running it.
    """

    #: Every tracked file under these roots is read. Directories are walked;
    #: files are taken as they are. `.claude/artifacts/**` is excluded because
    #: those are historical records — a plan or an ADR describes the tree of
    #: the day it was written, and re-pointing one would falsify it.
    SWEPT: tuple[str, ...] = (
        ".claude",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "README.md",
        "AGENTS.md",
        ".agents/memory/hex.md",
        ".claude/rules/product-context.md",
        "website/src/docs/authoring/migration.md",
        "website/src/docs/in-depth/project.md",
    )

    DEAD_PATH = "crates/" + "ocx_lib"

    @staticmethod
    def _tracked_files() -> list[Path]:
        """The swept set, from `git ls-files` so nothing ignored is read.

        `.pytest_cache/` and `tests/.venv/` live under `.claude` and are
        gitignored; reading them would sweep generated bytes and make the
        result depend on whether a test has run.
        """
        out = subprocess.run(  # noqa: S603
            ["git", "ls-files", "-z", "--", *TestCrateSplitSweep.SWEPT],  # noqa: S607
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        return [
            ROOT / name
            for name in out.split("\0")
            if name and "/artifacts/" not in name
        ]

    def test_the_sweep_reads_the_surface(self) -> None:
        """Floor the reader, not only its subject.

        An empty file list and a clean surface produce the same green, and the
        `git ls-files` call is the half that can silently stop returning
        anything — a moved root, a renamed directory, a pathspec that matches
        nothing. This says how much was read before the next test says what was
        in it.
        """
        files = self._tracked_files()
        assert len(files) >= 100, (
            f"the sweep read {len(files)} tracked file(s) across {len(self.SWEPT)} root(s); "
            "it has stopped seeing the surface and the assertion below is vacuous"
        )
        assert any(f.name == "CLAUDE.md" for f in files), (
            "CLAUDE.md is not among the files read, so the root list no longer resolves"
        )

    def test_no_ai_config_file_names_the_dissolved_crate(self) -> None:
        """C-075: no swept file names the dissolved crate's path after WP-38."""
        offenders: list[str] = []
        for path in self._tracked_files():
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            for number, line in enumerate(text.splitlines(), 1):
                if self.DEAD_PATH in line:
                    offenders.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()[:120]}")
        assert not offenders, (
            f"{len(offenders)} AI-config line(s) name `{self.DEAD_PATH}`, which WP-37 deleted — "
            "a path that resolves to nothing sends the next reader nowhere:\n  "
            + "\n  ".join(offenders)
        )


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
        # Find the stated count. Match both "These eight principles distill ..."
        # (canonical form) and "Eight principles distill ..." (caveman-compressed
        # form, with the leading "These" filler dropped).
        match = re.search(r"(?:These\s+)?(\w+) principles distill\b", claude_md_text)
        assert match, "Could not find 'N principles distill' in CLAUDE.md"
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
        text = path.read_text(encoding="utf-8")

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
        text = path.read_text(encoding="utf-8")

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
        text = agent_path.read_text(encoding="utf-8")
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
        body = agent.read_text(encoding="utf-8")

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
        text = agent.read_text(encoding="utf-8")
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
        text = agent.read_text(encoding="utf-8")

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
        text = agent.read_text(encoding="utf-8")

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
        catalog_text = self.CATALOG.read_text(encoding="utf-8")
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
        text = self.CATALOG.read_text(encoding="utf-8")
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
        text = CLAUDE_MD.read_text(encoding="utf-8")
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
        """
        # Candidate directories for resolving a bare filename
        search_dirs = [
            CLAUDE_DIR / "rules",
            CLAUDE_DIR / "agents",
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
            # Runtime state file written by /swarm-plan, deleted by /finalize
            "current_plan.md",
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
            # Relative to `.claude/`, never absolute: an agent runs this gate
            # from `.agents/worktrees/<slug>`, so `md.parts` carries `worktrees`
            # for EVERY file in the tree and the corpus collapsed to `CLAUDE.md`
            # alone — the floor below caught it, which is the whole reason it is
            # there. The same shape was latent in the other three names; a
            # checkout under a directory called `tests` or `state` would have
            # emptied this the same way, and only on the machine it happened on.
            parts = md.relative_to(CLAUDE_DIR).parts
            if "artifacts" in parts or "state" in parts:
                continue  # historical/ephemeral — preserves old references
            if "tests" in parts:
                continue  # test file itself doesn't reference rule files
            if "worktrees" in parts or "node_modules" in parts:
                continue  # nested worktrees + their node_modules are not OCX config
            if is_vendored(md):
                continue  # grim-installed — its internal refs are upstream's
            targets.append(md)

        missing: list[tuple[str, str]] = []
        checked = 0
        for md in targets:
            text = md.read_text(encoding="utf-8")
            for match in ref_pattern.findall(text):
                checked += 1
                if not find_file(match):
                    missing.append((str(md.relative_to(ROOT)), match))

        # Floors on the reader, because `assert not missing` below is produced
        # identically by a clean tree and by a corpus that collapsed to nothing:
        # one more `continue` in the filter chain above, or a tightened class in
        # `ref_pattern`, empties this test in silence.
        #
        # Sized against the collapse, not against a trim. `.claude/rules/` is 37
        # of today's 66 files and 268 of its 337 references, so losing it is what
        # "the corpus collapsed" means here and these floors red on it. Dropping
        # the 13 non-vendored skills instead leaves 53 and 308 and stays green —
        # stated rather than papered over, because a floor tight enough to catch
        # that is a ratchet that reds when someone deletes a skill.
        assert len(targets) >= 40, (
            f"only {len(targets)} markdown target(s) collected — the corpus has "
            "collapsed, and an empty corpus reports the same clean result as a "
            "tree with no broken references"
        )
        assert checked >= 200, (
            f"only {checked} reference(s) checked across {len(targets)} file(s) — "
            "the pattern has stopped matching, and checking nothing reports the "
            "same clean result as checking everything"
        )

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
        claude_text = CLAUDE_MD.read_text(encoding="utf-8")
        catalog_text = self.CATALOG.read_text(encoding="utf-8")

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
        text = rule.read_text(encoding="utf-8")
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
            text = py_file.read_text(encoding="utf-8")
            if "# /// script" not in text:
                missing.append(py_file.name)
        assert not missing, f"Hooks missing PEP 723 header: {missing}"

    def test_post_tool_use_tracker_has_try_except(self) -> None:
        """PostToolUse hook must never exit non-zero — needs try/except."""
        hook = CLAUDE_DIR / "hooks" / "post_tool_use_tracker.py"
        text = hook.read_text(encoding="utf-8")
        assert "try:" in text and "except" in text, (
            "post_tool_use_tracker.py must wrap main logic in try/except "
            "to satisfy the PostToolUse non-blocking contract."
        )

    def test_hooks_use_project_dir_env(self) -> None:
        """Hooks must use CLAUDE_PROJECT_DIR, not os.getcwd()."""
        hooks_dir = CLAUDE_DIR / "hooks"
        violations = []
        for py_file in sorted(hooks_dir.glob("*.py")):
            text = py_file.read_text(encoding="utf-8")
            if "os.getcwd()" in text or "Path.cwd()" in text:
                violations.append(py_file.name)
        assert not violations, (
            f"Hooks using cwd instead of CLAUDE_PROJECT_DIR: {violations}"
        )


# ---------------------------------------------------------------------------
# Taskfile lint: empty file list guard
# ---------------------------------------------------------------------------


class TestTaskfileLint:
    """shell.taskfile.yml must handle empty file lists gracefully."""

    def test_shell_lint_handles_no_scripts(self) -> None:
        """Bug captured: when git ls-files '*.sh' returns nothing, shellcheck
        is called with no arguments and exits non-zero, failing task verify.
        Guard lives on each lint task in shell.taskfile.yml after the
        ocx.taskfile.yml template was retired in favour of direct tool calls
        backed by the project toolchain (ocx.toml → direnv / setup-ocx)."""
        taskfile = ROOT / "taskfiles" / "shell.taskfile.yml"
        text = taskfile.read_text(encoding="utf-8")

        # Every lint task (shellcheck, shfmt:check, format) must guard against
        # empty file lists. Cheapest invariant: at least one precondition
        # checking git ls-files emits something.
        has_guard = bool(re.search(
            r"(preconditions|status|\[ -[nz]|\[\[ -[nz]|test -[nz]|exit 0)",
            text,
        ))
        assert has_guard, (
            "shell.taskfile.yml has no guard for empty file lists. "
            "When no matching files exist, the tool is called with no "
            "arguments and fails."
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
        the authoritative count is 4 and the list appears in meta-ai-config.md
        under `### Current Global Rules`. `rules.md` and `CLAUDE.md` reach
        Claude by a different mechanism (`@`-import / root instructions) and
        are not counted here.

        Vendored rules count. A grim-installed rule with no `paths:` loads
        every session exactly like a hand-written one, so it belongs in the
        always-loaded baseline this test guards — `hex-state.md` is one. What
        this repo controls is the bundle set in `grimoire.toml`, and a bundle
        that adds a new global is a change worth noticing.
        """
        rules_dir = CLAUDE_DIR / "rules"
        globals_found = []
        for rule in sorted(rules_dir.glob("*.md")):
            paths = TestRuleGlobs._extract_paths(rule)
            if not paths:
                globals_found.append(rule.name)
        assert len(globals_found) == 4, (
            f"Expected exactly 4 global rules (no `paths:` frontmatter), "
            f"got {len(globals_found)}: {globals_found}"
        )
        meta_text = (rules_dir / "meta-ai-config.md").read_text(encoding="utf-8")
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
          scope (`crates/**`, `test/**`, `website/**`, `.claude/**`)
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

        catalog = (CLAUDE_DIR / "rules.md").read_text(encoding="utf-8")
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
        # Owner-unlocked (2026-07-21): `/commit` is model-invocable so an agent
        # follows the documented workflow (stage by name, never push, never
        # `--no-verify`) instead of improvising equivalent git calls. Mirrored
        # in `MODEL_INVOCABLE_ACTION_SKILLS` above.
        "commit": False,
        "meta-maintain-config": True,
        "ocx-sync-roadmap": True,
        # Writes `~/.bazelrc` on this machine and can set an `ocx-sh` org
        # secret. Both are side effects outside the repository, and one of them
        # takes a password the user types — so the skill is reachable by name
        # and never by inference from a prompt that merely mentions Bazel.
        "init-bazel-config": True,
        # Pure analysis / advisory — auto-invocation safe
        "builder": False,
        "code-check": False,
        "deps": False,
        "docs": False,
        "meta-validate-context": False,
        "next": True,
        "qa-engineer": False,
        # Writes only a gitignored `out/*.html` and opens it in the local
        # browser — no repo mutation, no network write, nothing published. The
        # page is a reading order over a diff, so auto-invocation is safe and
        # useful the moment a diff is too large to eyeball.
        "review-surface": False,
        "security-auditor": False,
    }

    @staticmethod
    def _parse_frontmatter(skill_md: Path) -> dict[str, str]:
        """Parse single-line key: value pairs from the first `---` block.

        Intentionally simple — CSO descriptions are required to be single-line
        so this parser is sufficient. Multi-line (block-scalar) descriptions
        would produce a truncated value, which the CSO tests flag.
        """
        text = skill_md.read_text(encoding="utf-8")
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

        Scoped to project-authored skills: CSO is this repo's ADR, and a
        vendored skill's description is written upstream.
        """
        violations: list[tuple[str, str]] = []
        for skill_md in project_skills():
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
        """Sum of project-authored skill description chars must stay under
        the 4000-char cap (buffer below Anthropic's 1% context-window
        description budget).

        Pre-Phase-2 baseline was 5004 chars; Phase 2 target is ≤4000, giving
        ≈20% headroom for future skill growth before hitting the cap.

        Vendored descriptions cost context too, and cost more than this cap.
        They are excluded because no edit here can shrink them — the only
        lever over a vendored description is dropping its bundle from
        `grimoire.toml`, which is a decision, not a gate failure.
        """
        total = 0
        per_skill: list[tuple[str, int]] = []
        for skill_md in project_skills():
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

        Scoped to project-authored skills: an accidental flip is an edit made
        here, and a vendored skill's flag arrives set by upstream.
        """
        mismatches: list[tuple[str, object, object]] = []
        unlisted: list[str] = []
        for skill_md in project_skills():
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
        text = gitignore.read_text(encoding="utf-8")
        assert ".claude/state/" in text, (
            "`.gitignore` must contain `.claude/state/` — per-worktree "
            "learnings store / context samples must not be committed. "
            "See `.claude/artifacts/adr_ai_config_cross_session_learnings_store.md`."
        )

    def test_meta_ai_config_has_cross_session_learnings_section(self) -> None:
        """`meta-ai-config.md` must document the Cross-Session Learnings
        Store section and cite the ADR path."""
        meta = CLAUDE_DIR / "rules" / "meta-ai-config.md"
        text = meta.read_text(encoding="utf-8")
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
        `workflow-refactor.md`. Pointer-only files (workflow-feature.md)
        must NOT contain the markers (they link to the canonical) and MUST
        contain a pointer to `workflow-swarm.md#review-fix-loop`.
        """
        # Every carrier must have exactly one BEGIN and one END marker
        carrier_blocks: dict[str, str] = {}
        for carrier in self._CANONICAL_CARRIERS:
            assert carrier.exists(), f"Canonical carrier missing: {carrier}"
            text = carrier.read_text(encoding="utf-8")
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
            f"See `.claude/artifacts/adr_ai_config_review_loop_dedup.md`. "
            f"Quick fix: `task claude:fix:canonical-block` (re-syncs from "
            f"workflow-swarm.md into the other two carriers)."
        )

        # Pointer-only files must NOT contain the markers (no fourth carrier)
        illegal_carriers: list[str] = []
        for pointer in self._POINTER_ONLY_FILES:
            assert pointer.exists(), f"Pointer-only file missing: {pointer}"
            text = pointer.read_text(encoding="utf-8")
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
            text = pointer.read_text(encoding="utf-8")
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

        Scoped to project-authored skills — a vendored SKILL.md is sized by
        its upstream author and cannot be split here.
        """
        violations: list[tuple[str, int]] = []
        for skill_md in project_skills():
            name = skill_md.parent.name
            if name in self._SKILL_BODY_BUDGET_EXCEPTIONS:
                continue
            line_count = len(skill_md.read_text(encoding="utf-8").splitlines())
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

    _ALLOWED_SINGLE_WORD_TRIGGERS = {"deps", "commit"}
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
        """Project-authored user-invocable skills.

        A vendored skill declares (or omits) `triggers:` upstream; adding the
        field here would be undone by the next `grim install`. No vendored
        skill currently ships triggers, so this narrows the subject without
        dropping any trigger from the uniqueness and discrimination checks.
        """
        out: list[tuple[str, dict]] = []
        for skill_md in project_skills():
            fm = cls._parse_frontmatter(skill_md.read_text(encoding="utf-8"))
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
        text = self._HOOK.read_text(encoding="utf-8")
        assert "# /// script" in text, (
            "user_prompt_router.py must start with a PEP 723 inline "
            "script header (`# /// script` … `# ///`)."
        )

    def test_user_prompt_router_uses_project_dir_env(self) -> None:
        text = self._HOOK.read_text(encoding="utf-8")
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

        tree = ast.parse(self._HOOK.read_text(encoding="utf-8"))
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
        settings = json.loads(settings_path.read_text(encoding="utf-8"))
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

        tree = ast.parse(self._HOOK.read_text(encoding="utf-8"))
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


# ---------------------------------------------------------------------------
# Plan Status block — top-of-plan progress signal for /next
# ---------------------------------------------------------------------------


class TestPlanStatusBlock:
    """Every plan file in `.claude/state/plans/plan_*.md` must carry a `## Status`
    block at the top with the four mandatory fields. Schema and protocol live in
    `.claude/rules/meta-ai-config.md` "Plan Status Protocol".

    The `.claude/state/` directory is gitignored — plans are local per-worktree.
    Tests skip silently when no plan files exist (fresh checkout).
    Excludes `meta-plan_*.md` files (skill-internal scratch artifacts).
    """

    _MANDATORY_FIELDS = (
        "**Plan:**",
        "**Active phase:**",
        "**Step:**",
        "**Last update:**",
    )

    def _plan_files(self) -> list[Path]:
        plans_dir = CLAUDE_DIR / "state" / "plans"
        if not plans_dir.exists():
            return []
        candidates = [
            p
            for p in sorted(plans_dir.glob("plan_*.md"))
            if not p.name.startswith("meta-plan_")
        ]
        # `.claude/state/` is gitignored — plans are per-worktree scratch. Only
        # enforce the Status-block invariant on plans tracked by git; local
        # untracked plans are the developer's own scratch (legacy plans from
        # before the protocol existed, in-progress drafts, etc.) and should not
        # block commits on this worktree.
        #
        # Use plain `git ls-files <paths>` (NOT `--error-unmatch`) so a mixed
        # set of tracked + untracked candidates is handled correctly:
        # `--error-unmatch` aborts on the first untracked path with exit 128 +
        # empty stdout, which would silently exempt every tracked plan in the
        # same call.
        try:
            result = subprocess.run(
                ["git", "ls-files", "--", *[str(p) for p in candidates]],
                cwd=ROOT,
                capture_output=True,
                encoding="utf-8",
                check=False,
            )
        except (OSError, FileNotFoundError):
            # No git available — fall back to enforcing on everything.
            return candidates
        tracked = {(ROOT / line).resolve() for line in result.stdout.splitlines() if line}
        return [p for p in candidates if p.resolve() in tracked]

    def _extract_status_block(self, text: str) -> str | None:
        """Return content between `## Status` and the next `## ` heading, or None."""
        lines = text.splitlines()
        start = None
        for i, line in enumerate(lines):
            if line.strip() == "## Status":
                start = i + 1
                break
        if start is None:
            return None
        end = len(lines)
        for j in range(start, len(lines)):
            if lines[j].startswith("## ") and lines[j].strip() != "## Status":
                end = j
                break
        return "\n".join(lines[start:end])

    def test_every_plan_has_status_block(self) -> None:
        """Each plan_*.md must contain a `## Status` heading."""
        plans = self._plan_files()
        if not plans:
            pytest.skip("No plan files in .claude/state/plans/ (fresh checkout)")
        missing = [
            p.relative_to(ROOT) for p in plans if "## Status" not in p.read_text(encoding="utf-8")
        ]
        assert not missing, (
            f"Plans missing `## Status` block: {missing}. "
            f"Add one per `.claude/templates/artifacts/plan.template.md` schema. "
            f"Protocol: `.claude/rules/meta-ai-config.md` `Plan Status Protocol`."
        )

    def test_status_block_has_all_mandatory_fields(self) -> None:
        """Status block must contain Plan / Active phase / Step / Last update."""
        plans = self._plan_files()
        if not plans:
            pytest.skip("No plan files in .claude/state/plans/ (fresh checkout)")
        violations: list[tuple[Path, list[str]]] = []
        for plan in plans:
            block = self._extract_status_block(plan.read_text(encoding="utf-8"))
            if block is None:
                # covered by previous test
                continue
            missing_fields = [f for f in self._MANDATORY_FIELDS if f not in block]
            if missing_fields:
                violations.append((plan.relative_to(ROOT), missing_fields))
        assert not violations, (
            f"Plans with incomplete Status block: {violations}. "
            f"Required fields: {list(self._MANDATORY_FIELDS)}."
        )

    def test_status_block_in_first_30_lines(self) -> None:
        """Status block must be near the top — /next reads first 30 lines only."""
        plans = self._plan_files()
        if not plans:
            pytest.skip("No plan files in .claude/state/plans/ (fresh checkout)")
        too_late: list[tuple[Path, int]] = []
        for plan in plans:
            lines = plan.read_text(encoding="utf-8").splitlines()
            for i, line in enumerate(lines[:30], start=1):
                if line.strip() == "## Status":
                    break
            else:
                # not found in first 30 lines
                for j, line in enumerate(lines, start=1):
                    if line.strip() == "## Status":
                        too_late.append((plan.relative_to(ROOT), j))
                        break
        assert not too_late, (
            f"`## Status` block must appear within first 30 lines for /next "
            f"to read it cheaply. Late blocks: {too_late}."
        )

    def test_template_has_status_block(self) -> None:
        """Plan templates must seed the Status block so new plans get one for free."""
        templates = [
            CLAUDE_DIR / "templates" / "artifacts" / "plan.template.md",
            CLAUDE_DIR / "templates" / "artifacts" / "bugfix_plan.template.md",
        ]
        for tmpl in templates:
            assert tmpl.exists(), f"Template missing: {tmpl}"
            text = tmpl.read_text(encoding="utf-8")
            assert "## Status" in text, (
                f"{tmpl.relative_to(ROOT)} missing `## Status` block — "
                f"new plans created from this template would fail "
                f"TestPlanStatusBlock invariants."
            )
            block = self._extract_status_block(text)
            assert block is not None, f"Status heading present but block parse failed: {tmpl}"
            for field in self._MANDATORY_FIELDS:
                assert field in block, (
                    f"{tmpl.relative_to(ROOT)} Status block missing field {field}"
                )


class TestVerifyDeepBuildMatrix:
    """Structural invariant: `verify-deep.yml::build` matrix must keep an
    enabled (non-commented-out) `macos-latest` entry on `main`.

    Background: ADR `adr_file_lock_unification.md` §CI Integration permits an
    iteration tactic of temporarily commenting out the `macos-latest` matrix
    entry to save ≈ $0.50/run while debugging the new Windows leg. The tactic
    is correct during iteration but produces a permanent macOS coverage gap
    if the restore is forgotten. The architect review (P7) flagged the
    human-checklist safeguard as historically failure-prone and asked for a
    structural test that physically blocks merge to `main` when macOS is
    missing.

    This test reads `verify-deep.yml` as text and asserts a non-comment line
    matching `os: macos-latest` appears in the `build` job's matrix block.
    Pure structural — does not validate the rest of the matrix, just that the
    macOS leg is enabled when the branch lands.
    """

    _WORKFLOW = ROOT / ".github" / "workflows" / "verify-deep.yml"

    def test_macos_latest_is_enabled_in_verify_deep_build(self) -> None:
        assert self._WORKFLOW.exists(), (
            f"workflow missing: {self._WORKFLOW.relative_to(ROOT)}"
        )
        text = self._WORKFLOW.read_text(encoding="utf-8")
        # Locate the `build:` job block. It starts at `^  build:` (2-space
        # indent under `jobs:`) and ends at the next sibling-level job
        # (`^  \w[\w-]*:`) or EOF.
        build_match = re.search(r"(?m)^  build:\n", text)
        assert build_match is not None, (
            "`build:` job not found in verify-deep.yml — workflow shape changed?"
        )
        # Find the start of the next 2-space-indented job (sibling level).
        next_job = re.search(r"(?m)^  [A-Za-z_][A-Za-z0-9_-]*:\n", text[build_match.end():])
        block = text[build_match.start() : build_match.end() + next_job.start()] if next_job else text[build_match.start():]
        # Within the build block, look for an UNCOMMENTED line matching `os: macos-latest`.
        # A commented-out matrix entry begins with `#` (possibly preceded by whitespace).
        for raw_line in block.splitlines():
            stripped = raw_line.lstrip()
            if stripped.startswith("#"):
                continue
            if "os: macos-latest" in stripped:
                return  # invariant holds
        # Fallthrough: macos entry missing or only present as a comment.
        raise AssertionError(
            "`os: macos-latest` matrix entry missing from `verify-deep.yml::build` "
            "(or only present as a commented-out line). Restore it before merging "
            "to `main` — see ADR adr_file_lock_unification.md §CI Integration "
            "'Final state diff (must land before main)'. The iteration tactic of "
            "temporarily commenting out macOS is permitted DURING Windows debug "
            "cycles only; the leg MUST be re-enabled before the branch lands."
        )


# ---------------------------------------------------------------------------
# CLI command table coverage in subsystem-cli-commands.md
# ---------------------------------------------------------------------------


class TestSubsystemCliCommandsTableCoverage:
    """Every clap command file must appear in the Command Summary table.

    Regression guard against the B2 finding on PR #87 — the `package sign`
    and `package verify` commands shipped without rows in the Command Summary
    table of `subsystem-cli-commands.md`. When a new CLI command is added,
    this test fails until the doc table catches up.
    """

    _CLI_COMMANDS_RULE = CLAUDE_DIR / "rules" / "subsystem-cli-commands.md"
    _COMMAND_DIR = ROOT / "crates" / "ocx_cli" / "src" / "command"

    # Filename stems whose first row in the table maps to a multi-token command
    # ("package_sign.rs" → "package sign"). The table cell uses the spaced
    # form, so we normalize the stem to match.
    _STEM_REWRITES = {
        # OCI-tier package subcommands (file stem -> "package <verb>").
        "install": "package install",
        "uninstall": "package uninstall",
        "select": "package select",
        "deselect": "package deselect",
        "exec": "package exec",
        "env": "package env",
        "deps": "package deps",
        "which": "package which",
        "package_copy": "package copy",
        "package_create": "package create",
        "package_inspect": "package inspect",
        "package_attest": "package attest",
        "package_pull": "package pull",
        "package_receipt": "package receipt",
        "package_push": "package push",
        "package_sbom": "package sbom",
        "package_announce": "package announce",
        "package_claim": "package claim",
        # `cascade check` / `cascade repair` are two levels below `package`; the
        # table cell parser keeps the first two tokens, so both map to the same
        # documented head.
        # `description push` / `description pull` nest the same way as cascade.
        "package_description_push": "package description",
        "package_description_pull": "package description",
        "package_cascade_check": "package cascade",
        "package_cascade_repair": "package cascade",
        "package_sign": "package sign",
        "package_verify": "package verify",
        "package_test": "package test",
        # Patch-group subcommands (file stem -> "patch <verb>").
        "patch_freeze": "patch freeze",
        "patch_publish": "patch publish",
        "patch_sync": "patch sync",
        "patch_test": "patch test",
        "patch_why": "patch why",
        # Toolchain-tier `ocx env` / `ocx exec` live in toolchain_{env,exec}.rs.
        "toolchain_env": "env",
        "toolchain_exec": "exec",
        # Grouped subcommands (file stem -> "<group> <verb>").
        "index_catalog": "index catalog",
        "index_list": "index list",
        "index_update": "index update",
        "index_sync": "index sync",
        "index_regenerate": "index regenerate",
        # Managed-config group subcommands (file stem -> "config <verb>").
        "config_setup": "config setup",
        "config_update": "config update",
        "config_push": "config push",
        "config_test": "config test",
        "direnv_init": "direnv init",
        "direnv_export": "direnv export",
        "shell_allow": "shell allow",
        "shell_completion": "shell completion",
        "shell_revoke": "shell revoke",
        "shell_state": "shell state",
    }

    # Commands intentionally absent from the public table (internal /
    # umbrella subcommands; `verify` is documented as `package verify`).
    _EXEMPT = {
        # Parent group dispatchers whose subcommands are documented individually
        # (the leaf subcommands live under nested directories, not globbed here):
        "package",
        "patch",
        "shell",
        "index",
        "config",
        "launcher",
        "direnv",
        "self_group",
        # `patch_common.rs` = shared helpers for the patch group (not a command);
        # `script_runner.rs` = internal Starlark host runner, not a user command.
        "patch_common",
        "script_runner",
        # `index_common.rs` = shared helpers for the index group (not a command);
        # `package_cascade.rs` = the nested `package cascade` dispatcher, whose
        # two leaves are documented individually.
        "index_common",
        "package_cascade",
        # `package_description.rs` = the nested `package description`
        # dispatcher, whose two leaves are documented individually;
        # `deprecated.rs` = the 0.6 deprecated-spelling warnings (deleted
        # whole in 0.7), not a command.
        "package_description",
        "deprecated",
        # `package_sign_common.rs` = shared OIDC/signing helpers for
        # `package sign` and `package attest` (not a command).
        "package_sign_common",
        # `app.rs` / `command.rs` rooted dispatchers (not commands themselves).
    }

    def _table_cells(self) -> set[str]:
        """Extract the leading-cell tokens from the Command Summary table."""
        text = self._CLI_COMMANDS_RULE.read_text(encoding="utf-8")
        # Find the "Command Summary" section and walk its `|`-delimited rows.
        section_start = text.find("## Command Summary")
        assert section_start != -1, "Command Summary section missing"
        section = text[section_start:]
        next_section = section.find("\n## ", 1)
        if next_section != -1:
            section = section[:next_section]
        cells: set[str] = set()
        for line in section.splitlines():
            if not line.startswith("| `"):
                continue
            # Row shape: `| \`name [ARGS]\` | ... | ... |`
            first_cell = line.split("|", 2)[1].strip()
            if first_cell.startswith("`"):
                first_cell = first_cell.strip("`")
            # Strip leading "ocx " if present, then take first word(s) up to ARGS.
            head = first_cell.split(" ")
            # Multi-token command (e.g. "package sign IDENTIFIER") — keep first
            # two tokens if first is a group; else single token.
            group_heads = {"package", "patch", "shell", "index", "config", "ci", "launcher", "generate", "direnv", "self"}
            if head and head[0] in group_heads and len(head) >= 2:
                cells.add(f"{head[0]} {head[1]}")
            elif head:
                cells.add(head[0])
        return cells

    def test_subsystem_cli_commands_table_covers_all_commands(self) -> None:
        if not self._COMMAND_DIR.is_dir():
            pytest.skip("CLI command directory missing — pre-Rust checkout")
        documented = self._table_cells()
        missing: list[str] = []
        for cmd_file in sorted(self._COMMAND_DIR.glob("*.rs")):
            stem = cmd_file.stem
            if stem in self._EXEMPT:
                continue
            expected = self._STEM_REWRITES.get(stem, stem)
            if expected not in documented:
                missing.append(f"{cmd_file.relative_to(ROOT)} → expected `{expected}`")
        assert not missing, (
            f"CLI command files without a row in `subsystem-cli-commands.md` "
            f"Command Summary: {missing}. Add a row (one per command) so new "
            f"commands are discoverable from the AI-config catalog."
        )


# ---------------------------------------------------------------------------
# The retired verify stamp (plan_crate_split_workspace.md WP-40 H8)
# ---------------------------------------------------------------------------


class TestRetiredVerifyStamp:
    """No tracked `.md` / `.py` / `.yml` instructs the bare-epoch stamp.

    Since the JSON mark (WP-04) a bare integer at
    `.claude/hooks/.state/<mark file>` reads as *not verified* and overwrites
    the mark `task verify` wrote — so the old one-liner is a trap, not a
    shortcut. `task verify` writes the mark; a hand mark is only ever
    `task verify:mark`.

    The needle is any shell write into the mark file — a redirect or a `tee`
    followed by its path — assembled from parts, so this file never matches
    itself; what produced the bytes (`echo`, `date`, `printf`, a wrapped
    line) is not part of it. The files listed below still name the spelling
    — each to say it is retired — and are the only ones allowed to: set
    equality keeps the list honest in both directions (a new carrier reds; a
    carrier that drops the spelling reds until it is removed here).
    """

    _MARK_FILE = "commit-" + "verified"
    _NEEDLE = re.compile(r"(>|\btee\b)\s*\S*" + re.escape(_MARK_FILE))
    _RETIRED_SPELLING_CARRIERS = frozenset(
        {
            # Name the spelling to say it reads as *not verified*.
            ".claude/rules/workflow-git.md",
            ".claude/skills/commit/SKILL.md",
            # The H8 / DX-15 finding text and its execution log.
            ".claude/artifacts/plan_crate_split_workspace.md",
            # Pre-JSON archaeology: quotes what workflow-git.md said at v0.6.2.
            ".claude/artifacts/research_crate_split_archaeology.md",
            # The design note that retired the stamp (its hint-text finding).
            ".claude/artifacts/research_crate_split_verification_tiers.md",
        }
    )

    def _carriers(self) -> set[str]:
        """Tracked `.md` / `.py` / `.yml` files whose working-tree text has the needle."""
        listed = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z", "--", "*.md", "*.py", "*.yml"],
            capture_output=True,
            encoding="utf-8",
            check=True,
        ).stdout
        return {
            rel
            for rel in listed.split("\0")
            if rel and self._NEEDLE.search((ROOT / rel).read_text(encoding="utf-8", errors="replace"))
        }

    @pytest.mark.parametrize(
        "stamp",
        [
            pytest.param("task verify && echo $(date +%s) > .claude/hooks/.state/", id="echo"),
            pytest.param("date +%s > .claude/hooks/.state/", id="date-alone"),
            pytest.param("printf '%s' \"$(date +%s)\" > …/", id="printf"),
            pytest.param("echo $(date +%s) | tee .claude/hooks/.state/", id="tee"),
            pytest.param("echo $(date\n+%s) > …/", id="line-wrapped"),
        ],
    )
    def test_needle_matches_the_retired_spelling(self, stamp: str) -> None:
        """The needle's own red state: a drifted regex must not pass the sweep vacuously."""
        assert self._NEEDLE.search(stamp + self._MARK_FILE)
        assert not self._NEEDLE.search("`task verify:mark` writes the " + self._MARK_FILE + " mark")
        assert not self._NEEDLE.search("reads `.claude/hooks/.state/" + self._MARK_FILE + "` as JSON")

    def test_only_the_listed_files_name_the_retired_stamp(self) -> None:
        found = self._carriers()
        assert found == self._RETIRED_SPELLING_CARRIERS, (
            f"files instructing the retired bare-epoch stamp: "
            f"{sorted(found - self._RETIRED_SPELLING_CARRIERS)} (say `task verify` writes the mark and "
            f"a hand mark is `task verify:mark`); listed carriers that no longer name it: "
            f"{sorted(self._RETIRED_SPELLING_CARRIERS - found)} (remove them from the list)"
        )


# ---------------------------------------------------------------------------
# The `verify` summary names what `.verify:lint` / `.verify:build-test` run
# ---------------------------------------------------------------------------


class TestVerifySummary:
    """`task verify --summary` lists every task the two phases call (WP-40 W22).

    The summary is what a reader consults instead of the two internal tasks,
    so a task added to `.verify:lint` or `.verify:build-test` and not to the
    summary is a gate nobody knows runs.
    """

    _TASKFILE = ROOT / "taskfile.yml"
    _PHASES = (".verify:lint", ".verify:build-test")

    @classmethod
    def _tasks(cls) -> dict:
        """`taskfile.yml`'s `tasks:` mapping, parsed — not read line by line.

        A hand-rolled indentation reader only ever fails *partially*: a step
        respelled `- cmd: task X` drops out of the list while every other
        entry keeps it non-empty, so a "did the reader find anything?" guard
        stays green over a task the summary no longer has to name.
        """
        return yaml.safe_load(cls._TASKFILE.read_text(encoding="utf-8"))["tasks"]

    def _summary_and_ran(self) -> tuple[str, list[str]]:
        tasks = self._tasks()
        ran: list[str] = []
        for phase in self._PHASES:
            body = tasks[phase]
            entries = [*(body.get("deps") or []), *(body.get("cmds") or [])]
            assert entries, f"taskfile.yml `{phase}` has no `deps:`/`cmds:` entries"
            # Per entry, not "some entry survived": every step of a phase is a
            # `task:` dispatch, so anything else is a gate this reader cannot
            # name — and the summary is the only place a reader learns it runs.
            opaque = [entry for entry in entries if not (isinstance(entry, dict) and "task" in entry)]
            assert not opaque, (
                f"taskfile.yml `{phase}` entries that are not a `task:` dispatch: {opaque} — "
                f"the `verify` summary is generated from `task:` entries alone, so a step "
                f"spelled any other way runs without the summary having to name it"
            )
            ran += [entry["task"] for entry in entries]
        return tasks["verify"]["summary"], ran

    def test_summary_names_every_task_the_phases_run(self) -> None:
        summary, ran = self._summary_and_ran()
        missing = [task for task in ran if task not in summary]
        assert not missing, (
            f"taskfile.yml `verify` summary does not name {missing}, which "
            f".verify:lint / .verify:build-test run — add them to the summary"
        )

    def test_summary_names_no_task_the_phases_do_not_run(self) -> None:
        """The other containment: a task dropped from a phase must leave the summary too."""
        summary, ran = self._summary_and_ran()
        allowed = set(ran) | set(self._PHASES) | {".verify:mark"}
        named = set(re.findall(r"(?<![\w:])\.?[a-z]+(?::[a-z-]+)+(?![\w:])", summary))
        stale = sorted(named - allowed)
        assert not stale, (
            f"taskfile.yml `verify` summary names {stale}, which neither .verify:lint nor "
            f".verify:build-test runs — drop them from the summary or add the task"
        )

    def test_scoped_summary_names_the_escalating_table_rows(self) -> None:
        """`verify:scoped`'s summary lists TABLE_ESCALATES as scoped_gate.py has it (WP-40 W17).

        The set shrinks when WP-37 moves the verbs out of the CLI crate; the
        summary must shrink with it.
        """
        gate = (ROOT / "scripts" / "scoped_gate.py").read_text(encoding="utf-8")
        declared = re.search(r"^TABLE_ESCALATES = frozenset\((\{[^}]*\})\)", gate, re.M)
        assert declared, "scripts/scoped_gate.py no longer declares `TABLE_ESCALATES = frozenset({...})`"
        expected = set(ast.literal_eval(declared.group(1)))
        summary = " ".join(self._tasks()["verify:scoped"]["summary"].split())
        named = re.search(r"TABLE_ESCALATES: ([\w, ]+?) —", summary)
        assert named, "taskfile.yml `verify:scoped` summary has no `TABLE_ESCALATES: a, b, c —` clause"
        assert set(named.group(1).split(", ")) == expected, (
            f"`verify:scoped` summary names {named.group(1)!r}; scoped_gate.py TABLE_ESCALATES is "
            f"{sorted(expected)}"
        )


# ---------------------------------------------------------------------------
# WP-42 R16: every `*self-test` task is reachable from `task verify`
# ---------------------------------------------------------------------------


class TestSelfTestsRunOnAGate:
    """A self-test hanging off a task no gate calls is a check nobody runs.

    `TestVerifySummary` above catches a phase member the summary forgets.
    Nothing caught the other direction: `rust:test:ceiling:self-test` was
    called only by `rust:verify`, which `taskfile.yml`, `.github/workflows/**`
    and `scripts/scoped_gate.py` never invoke — so the fixtures that prove the
    skip-ceiling parser ran nowhere, and a review finding (D24) was closed on
    them. Fourth recurrence of the class; this is what makes a fifth red.

    The walk follows `task:` dispatches only, which is what a self-test is
    wired with. A task reached by a bare shell line (`- task rust:x` in a
    `cmd:`) reads as unreachable here — the strict direction, and `task:` is
    how every phase member of `verify` is spelled today.
    """

    _ROOT_TASKFILE = ROOT / "taskfile.yml"
    # Listed, not discovered-and-waved-through: a new self-test is a task that
    # must be wired, so it reds this list until someone puts it on a gate and
    # names it here.
    _SELF_TESTS = frozenset(
        {
            "scripts:self-test",
        }
    )

    @classmethod
    def _graph(cls) -> tuple[dict[str, list[str]], list[str]]:
        """`{full task name: [full names it dispatches]}` over the root taskfile and its includes.

        Namespacing follows go-task: a name inside an included file resolves
        against that file first and against the root namespace otherwise, and
        a leading `:` is always the root namespace.
        """
        root = yaml.safe_load(cls._ROOT_TASKFILE.read_text(encoding="utf-8"))
        files = {"": cls._ROOT_TASKFILE}
        for prefix, entry in (root.get("includes") or {}).items():
            path = entry if isinstance(entry, str) else entry.get("taskfile")
            files[prefix] = ROOT / str(path).lstrip("./")

        bodies: dict[str, dict] = {}
        owners: dict[str, str] = {}
        for prefix, path in files.items():
            if not path.exists():
                continue
            for name, body in (yaml.safe_load(path.read_text(encoding="utf-8")).get("tasks") or {}).items():
                full = f"{prefix}:{name}" if prefix else name
                bodies[full] = body if isinstance(body, dict) else {}
                owners[full] = prefix

        def resolve(name: str, prefix: str) -> str:
            if name.startswith(":"):
                return name[1:]
            local = f"{prefix}:{name}" if prefix else name
            return local if local in bodies else name

        graph = {
            full: [
                resolve(entry["task"], owners[full])
                for entry in [*(body.get("deps") or []), *(body.get("cmds") or [])]
                if isinstance(entry, dict) and "task" in entry
            ]
            for full, body in bodies.items()
        }
        return graph, sorted(bodies)

    @classmethod
    def _reachable_from(cls, entry: str) -> set[str]:
        graph, _ = cls._graph()
        seen, queue = set(), [entry]
        while queue:
            task = queue.pop()
            if task in seen:
                continue
            seen.add(task)
            queue += graph.get(task, [])
        return seen

    def test_the_walk_reads_the_taskfiles(self) -> None:
        """The reader's own control: the self-tests this class names all exist.

        A typo in `_SELF_TESTS` would otherwise make the reachability test
        below assert nothing about a task that is really there.
        """
        _, defined = self._graph()
        missing = sorted(self._SELF_TESTS - set(defined))
        assert not missing, (
            f"{missing} are not tasks of taskfile.yml or its includes — the names in "
            f"_SELF_TESTS are stale, or the walk stopped reading a file"
        )
        found = {task for task in defined if task.rsplit(":", 1)[-1].endswith("self-test")}
        assert found == self._SELF_TESTS, (
            f"self-test tasks in the tree are {sorted(found)}, this class names "
            f"{sorted(self._SELF_TESTS)} — a new self-test must be wired onto a gate and "
            f"listed here, a retired one dropped from both"
        )

    def test_every_self_test_is_reachable_from_verify(self) -> None:
        reachable = self._reachable_from("verify")
        orphans = sorted(self._SELF_TESTS - reachable)
        assert not orphans, (
            f"{orphans} are not reachable from `task verify` by any `task:` dispatch — a "
            f"self-test outside every gate is a check nobody runs (WP-42 R16)"
        )

    def test_rust_verify_is_not_a_gate(self) -> None:
        """The walk's red half, and the standing decision about `rust:verify`.

        `rust:verify` is the Rust leg a developer runs during a review-fix
        loop (four subsystem rules and `workflow-feature.md` send them to it,
        and the split full gate uses it as one leg) — it is not reached by
        `task verify`, and nothing it runs may be left to it alone. If this
        reds because `verify` now calls it, the decision changed and the
        `desc` of `rust:verify` has to say so; if it reds because the walk
        returns everything, the test above was never asserting anything.
        """
        assert "rust:verify" not in self._reachable_from("verify"), (
            "`task verify` now reaches `rust:verify` — re-read its `desc`, which states "
            "that no gate calls it and that its members are wired individually"
        )

    def test_the_parked_bazel_test_engine_arm_keeps_its_entry_point(self) -> None:
        """`OCX_TEST_ENGINE=bazel` is PARKED, and a parked arm is the kind that rots.

        WP-30 returned NO-GO, so the CI execution swap does not land and
        `rust:test:unit:bazel` runs in no lane. The plan keeps it — the reopen
        condition is a later R2 re-measurement with the cache realm deployed, and
        the arm's floor (`selected >= CRATES_TEST_TARGETS`) is the measured work
        that would otherwise have to be redone.

        What this asserts is the one thing that can break while nothing runs it:
        the dispatch still resolves. `test:unit` selects `test:unit:{ENGINE}` by
        string, so renaming the arm turns `OCX_TEST_ENGINE=bazel` into "task not
        found" and no gate in this repository would notice. It deliberately does
        **not** assert reachability from `verify` — being unreachable is what
        parked means, and asserting otherwise would demand the lane WP-30
        refused.
        """
        _, defined = self._graph()
        arm = "rust:test:unit:bazel"
        assert arm in defined, (
            f"`{arm}` is gone, but `taskfiles/rust.taskfile.yml`'s `test:unit` still "
            f"dispatches `test:unit:{{{{.ENGINE}}}}` — `OCX_TEST_ENGINE=bazel` now resolves to "
            f"no task. Retire the dispatch in the same change, or restore the arm"
        )
        body = yaml.safe_load(
            (ROOT / "taskfiles" / "rust.taskfile.yml").read_text(encoding="utf-8")
        )["tasks"]
        dispatch = [
            entry["task"]
            for entry in (body["test:unit"].get("cmds") or [])
            if isinstance(entry, dict) and "task" in entry
        ]
        assert dispatch == ["test:unit:{{.ENGINE}}"], (
            f"`rust:test:unit` dispatches {dispatch} — the engine selection is what makes "
            f"`{arm}` reachable at all, and the arm is parked, so nothing else would red"
        )


# ---------------------------------------------------------------------------
# The Bazel gate set has a reader: prose against the taskfile (R-2)
# ---------------------------------------------------------------------------


class TestBazelGateProse:
    """Every Bazel gate `task verify` runs is named where a reader looks.

    Nothing bound this prose to the taskfile. `TestVerifySummary` above holds
    only `taskfile.yml`'s *own* summary against its phase bodies, which is
    exactly why the taskfile stayed right while `CLAUDE.md`,
    `subsystem-taskfiles.md` and the contributing page all still said "four
    gates" after the fifth and sixth landed. Fourth-plus recurrence of the
    "prose paraphrases an authority with no reader" class; this is the reader.
    """

    _BAZEL_TASKFILE = ROOT / "taskfiles" / "bazel.taskfile.yml"
    _PHASES = (".verify:lint", ".verify:build-test")
    _PROSE = (
        "CLAUDE.md",
        ".claude/rules/subsystem-taskfiles.md",
        "website/src/docs/contributing/bazel.md",
    )
    #: The files that state the gate *count* in words. A count is the one part
    #: of this prose a containment check cannot catch: adding a seventh gate to
    #: a sentence that still says "six" leaves every name present.
    _COUNTING = (*_PROSE, ".claude/rules/subsystem-ci.md")
    _NUMBER_WORDS = {3: "three", 4: "four", 5: "five", 6: "six", 7: "seven", 8: "eight"}
    _COUNT_PHRASE = re.compile(r"\b(three|four|five|six|seven|eight)\s+(?:\w+\s+)?gates\b", re.I)

    #: Measured with `Cargo.bazel.lock.json` moved aside, not assumed:
    #: `bazel:pin:check` (`bazel --version`) and `bazel:lint`
    #: (`bazel run //:buildifier.check`) both still exit 0, while
    #: `bazel:build:nobuild` exits 201 and `bazel mod deps` exits 2 —
    #: every `crates/*/BUILD.bazel` does `load("@crates//:defs.bzl", …)` at
    #: loading phase, so anything reaching the graph needs the lockfile.
    _NO_BOOTSTRAP = frozenset({"bazel:pin:check", "bazel:lint"})

    @classmethod
    def _gates(cls) -> list[str]:
        """The `bazel:*` tasks the two `verify` phases dispatch directly."""
        tasks = yaml.safe_load((ROOT / "taskfile.yml").read_text(encoding="utf-8"))["tasks"]
        return [
            entry["task"]
            for phase in cls._PHASES
            for entry in [*(tasks[phase].get("deps") or []), *(tasks[phase].get("cmds") or [])]
            if isinstance(entry, dict) and str(entry.get("task", "")).startswith("bazel:")
        ]

    def test_the_reader_finds_the_gates(self) -> None:
        """This class's own control: a walk that returned nothing would pass every
        containment assertion below over any prose at all."""
        gates = self._gates()
        assert len(gates) >= 4, (
            f"`.verify:lint` and `.verify:build-test` dispatch {gates} — fewer than four "
            f"`bazel:` tasks means the reader stopped, not that the gate set shrank"
        )
        assert len(set(gates)) == len(gates), f"a gate is dispatched twice: {gates}"

    @pytest.mark.parametrize("relative", _PROSE)
    def test_every_gate_is_named_in_the_prose(self, relative: str) -> None:
        text = (ROOT / relative).read_text(encoding="utf-8")
        # The full `bazel:` spelling, never a bare suffix: `lint` and `test`
        # occur in any of these files as ordinary prose, so a suffix-tolerant
        # containment check is green over a page that names no gate at all.
        missing = [gate for gate in self._gates() if gate not in text]
        assert not missing, (
            f"{relative} does not name {missing}, which `task verify` runs off "
            f"`taskfiles/bazel.taskfile.yml` — a gate the prose omits is a gate a reader of "
            f"that file does not know exists"
        )

    @pytest.mark.parametrize("relative", _COUNTING)
    def test_the_stated_gate_count_is_the_gate_count(self, relative: str) -> None:
        expected = self._NUMBER_WORDS[len(self._gates())]
        text = (ROOT / relative).read_text(encoding="utf-8")
        # Only a count on a line that is about Bazel. `subsystem-taskfiles.md`
        # also says "Three gates" of the *verify tiers*, which is a different
        # subject and a correct sentence.
        stated = {
            word.lower()
            for line in text.splitlines()
            if "bazel" in line.lower()
            for word in self._COUNT_PHRASE.findall(line)
        }
        assert stated, (
            f"{relative} states no `<word> gates` count — every file in this list carried one, "
            f"and dropping it removes the thing this test reads rather than fixing the drift"
        )
        assert stated == {expected}, (
            f"{relative} says {sorted(stated)} gates; `task verify` runs {len(self._gates())} "
            f"({expected}): {self._gates()}"
        )

    def test_every_graph_reaching_bazel_task_bootstraps(self) -> None:
        """`Cargo.bazel.lock.json` is gitignored, so CI arrives without it.

        `bazel:build:nobuild` and `bazel:test:scoped` each lost their
        `- task: bootstrap` to one commit that never mentioned the removal.
        Locally that is invisible — phase 1's `scripts:verify` reaches
        `bazel:bootstrap` before phase 2 runs — and red on every clean CI run.
        """
        bodies = yaml.safe_load(self._BAZEL_TASKFILE.read_text(encoding="utf-8"))["tasks"]
        invokes = re.compile(r"\bbazel\s+(?!--version\b)\w")
        orphans: list[str] = []
        for name, body in bodies.items():
            full = f"bazel:{name}"
            if full == "bazel:bootstrap" or full in self._NO_BOOTSTRAP:
                continue
            items = [*(body.get("cmds") or []), *([body["cmd"]] if "cmd" in body else [])]
            shell = " ".join(item for item in items if isinstance(item, str)) + " ".join(
                str(item.get("cmd", "")) for item in items if isinstance(item, dict)
            )
            if invokes.search(shell) and "bazel:bootstrap" not in (
                TestSelfTestsRunOnAGate._reachable_from(full)
            ):
                orphans.append(full)
        assert not orphans, (
            f"{orphans} invoke `bazel` against the graph and never reach `bazel:bootstrap`. "
            f"`Cargo.bazel.lock.json` is gitignored (DX-23), so `actions/checkout` never "
            f"delivers it and every `crates/*/BUILD.bazel` loads `@crates//:defs.bzl` at "
            f"loading phase — measured, the task exits 201 on `Unable to read lockfile`. "
            f"Add `- task: bootstrap`, or add the task to `_NO_BOOTSTRAP` with the "
            f"lockfile-absent exit code you measured"
        )

    def test_the_no_bootstrap_exemptions_still_exist(self) -> None:
        """A stale exemption silently widens the test above into a no-op."""
        bodies = yaml.safe_load(self._BAZEL_TASKFILE.read_text(encoding="utf-8"))["tasks"]
        missing = sorted(name for name in self._NO_BOOTSTRAP if name.split(":", 1)[1] not in bodies)
        assert not missing, (
            f"{missing} are exempted from the bootstrap requirement and are not tasks of "
            f"{self._BAZEL_TASKFILE.name} — the exemption list is stale"
        )


# ---------------------------------------------------------------------------
# The catalog's "By auto-load path" table is checked, not merely written (B5R-12)
# ---------------------------------------------------------------------------


class TestCatalogAutoLoadPaths:
    """`.claude/rules.md`'s auto-load table against the rules' own frontmatter.

    Nothing read this table before. Reverting one of its rows left the whole
    structural suite passing and byte-identical, while reverting the same
    change in the rule's frontmatter reddened three tests — so the rows were
    correct by diligence alone, and the next directory move had nothing
    catching them.

    Two properties, both derived: the table names exactly the rules that
    auto-load, and the globs it spells are live. It is deliberately not
    asserted to *equal* the frontmatter — a row groups several rules under one
    edit path, which is what makes it readable — so a glob narrower than the
    rule's own (`test/**/*.py` under `**/*.py`) is correct and stays.
    """

    _SECTION = "## By auto-load path"

    @staticmethod
    def _rows(section_heading: str = "## By auto-load path") -> list[tuple[str, str]]:
        text = (CLAUDE_DIR / "rules.md").read_text(encoding="utf-8")
        assert section_heading in text, f"`.claude/rules.md` has no `{section_heading}` section"
        section = text.split(section_heading, 1)[1].split("\n## ", 1)[0]
        rows = []
        for line in section.splitlines():
            if not line.startswith("| ") or "---" in line or line.startswith("| Edit path"):
                continue
            cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
            if len(cells) >= 2:
                rows.append((cells[0], cells[-1]))
        assert len(rows) > 10, f"the auto-load table parsed as {len(rows)} row(s) — the reader stopped matching"
        return rows

    @staticmethod
    def _scoped_rules() -> set[str]:
        return {
            rule.name
            for rule in sorted(CLAUDE_DIR.glob("rules/*.md"))
            if TestRuleGlobs._extract_paths(rule)
        }

    def test_every_scoped_rule_is_named_in_the_table(self) -> None:
        """A rule that fires on a path and is absent from the table is a rule
        nobody knows fires — the catalog exists to answer exactly that."""
        named = {rule for _, rules in self._rows() for rule in re.findall(r"\[([a-z0-9._-]+\.md)\]", rules)}
        missing = sorted(self._scoped_rules() - named)
        assert not missing, (
            f"`.claude/rules.md` § By auto-load path names no row for {missing}, which declare "
            f"`paths:` and therefore auto-load — add the row"
        )

    def test_the_table_claims_auto_load_for_no_unscoped_rule(self) -> None:
        """The other containment: a rule that lost its `paths:` stops firing,
        and a row still promising it is the unmatched-glob class in prose."""
        named = {rule for _, rules in self._rows() for rule in re.findall(r"\[([a-z0-9._-]+\.md)\]", rules)}
        stale = sorted(named - self._scoped_rules())
        assert not stale, (
            f"`.claude/rules.md` § By auto-load path lists {stale} as auto-loading, and they "
            f"declare no `paths:` — they load globally or not at all"
        )

    @pytest.mark.parametrize("row", _rows(), ids=lambda row: row[0][:48])
    def test_every_glob_the_table_spells_is_live(self, row: tuple[str, str]) -> None:
        """A directory move kills the row's glob as surely as the rule's own.

        The same carve-out `test_all_rule_globs_match_files` makes: a row
        naming only shareable rules describes file types other repositories
        have, so an unmatched glob there is not dead.
        """
        edit_paths, rules = row
        named = re.findall(r"\[([a-z0-9._-]+\.md)\]", rules)
        patterns = re.findall(r"`([^`]+)`", edit_paths)
        assert patterns, f"auto-load row `{edit_paths}` spells no glob at all"
        dead = [p for p in patterns if not _glob_is_live(p)]
        if dead and named and all(TestRuleGlobs._is_shareable(CLAUDE_DIR / "rules" / n) for n in named):
            pytest.skip(f"row names only shareable rules; unmatched here (not dead): {dead}")
        assert not dead, (
            f"`.claude/rules.md` § By auto-load path row `{edit_paths}` spells dead glob(s) "
            f"{dead} — the rule it describes no longer fires on them"
        )


class TestTelemetryGateAttributes:
    """C-018: `task telemetry:push` tags the span with the gate that ran.

    Runs the task FOR REAL against a copy of the taskfile in a fresh git repo,
    with a stub `junit2otlp` first on PATH that records its argv. `task --dry`
    would print the unexpanded `$vars`, so only a real run can see the value
    that reaches `--additional-attributes` — and the red on today's taskfile
    (no `ocx.gate`) is reachable only this way.
    """

    TASKFILE = ROOT / "taskfiles" / "telemetry.taskfile.yml"
    # The fixture's run start, local time; the escape marker is judged against it.
    RUN_START = "2026-01-01T00:00:00"
    RUN_START_EPOCH = time.mktime((2026, 1, 1, 0, 0, 0, 0, 0, -1))

    def _push(
        self, tmp_path: Path, *, gate: str | None = None, marker_mtime: float | None = None
    ) -> tuple[dict[str, str], Path]:
        task = shutil.which("task")
        if task is None:
            pytest.fail("`task` (go-task) is not on PATH — run under `ocx exec --` or `task claude:tests`")
        repo = tmp_path / "repo"
        repo.mkdir()
        shutil.copyfile(self.TASKFILE, repo / "Taskfile.yml")
        for args in (
            ("init", "-q", "-b", "work"),
            ("add", "-A"),
            ("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "seed"),
        ):
            subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, encoding="utf-8")
        junit = tmp_path / "junit.xml"
        junit.write_text(
            f'<testsuites><testsuite name="s" timestamp="{self.RUN_START}" tests="1">'
            '<testcase classname="c" name="n" time="0.1"/></testsuite></testsuites>\n',
            encoding="utf-8",
        )
        if marker_mtime is not None:
            marker = repo / "target" / "bazel" / "accept" / "gate_escape"
            marker.parent.mkdir(parents=True)
            marker.write_text("", encoding="utf-8")
            os.utime(marker, (marker_mtime, marker_mtime))
        stub_dir = tmp_path / "stub"
        stub_dir.mkdir()
        argv_file = tmp_path / "junit2otlp.argv"
        stub = stub_dir / "junit2otlp"
        stub.write_text(f"#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > '{argv_file}'\ncat > /dev/null\n", encoding="utf-8")
        stub.chmod(0o755)
        home = tmp_path / "home"
        home.mkdir()
        env = {k: v for k, v in os.environ.items() if k != "OCX_GATE" and not k.startswith("GIT_")}
        env.update(
            PATH=f"{stub_dir}{os.pathsep}{os.environ['PATH']}",
            OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:9",
            HOME=str(home),
        )
        if gate is not None:
            env["OCX_GATE"] = gate
        run = subprocess.run(
            [task, "-d", str(repo), "push", f"JUNIT={junit}", "SUITE=acceptance"],
            capture_output=True, encoding="utf-8", env=env, check=False,
        )
        assert run.returncode == 0, f"`task push` failed: {run.stdout}{run.stderr}"
        assert argv_file.is_file(), f"the stub junit2otlp never ran: {run.stdout}{run.stderr}"
        argv = argv_file.read_text(encoding="utf-8").splitlines()
        assert "--additional-attributes" in argv, f"no --additional-attributes in {argv}"
        value = argv[argv.index("--additional-attributes") + 1]
        attrs = dict(pair.partition("=")[::2] for pair in value.split(","))
        return attrs, repo

    def test_ocx_gate_comes_from_the_environment(self, tmp_path: Path) -> None:
        attrs, _ = self._push(tmp_path, gate="inner")
        assert attrs.get("ocx.gate") == "inner", attrs

    def test_an_unset_ocx_gate_omits_the_attribute(self, tmp_path: Path) -> None:
        attrs, _ = self._push(tmp_path)
        assert "ocx.gate" not in attrs, attrs

    def test_the_git_tree_is_heads_tree(self, tmp_path: Path) -> None:
        attrs, repo = self._push(tmp_path, gate="inner")
        tree = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD^{tree}"],
            check=True, capture_output=True, encoding="utf-8",
        ).stdout.strip()
        assert attrs.get("ocx.git.tree") == tree, attrs

    def test_a_marker_newer_than_the_run_start_is_an_escape(self, tmp_path: Path) -> None:
        attrs, _ = self._push(tmp_path, gate="full", marker_mtime=self.RUN_START_EPOCH + 3600)
        assert attrs.get("ocx.gate.escape") == "true", attrs

    def test_a_marker_older_than_the_run_start_is_stale(self, tmp_path: Path) -> None:
        attrs, _ = self._push(tmp_path, gate="full", marker_mtime=self.RUN_START_EPOCH - 3600)
        assert "ocx.gate.escape" not in attrs, attrs

    def test_no_marker_is_no_escape(self, tmp_path: Path) -> None:
        attrs, _ = self._push(tmp_path, gate="full")
        assert "ocx.gate.escape" not in attrs, attrs


class TestTelemetryAcceptPush:
    """C-018 / S-018: T2's own push — `task telemetry:accept`, which `bazel:test:accept` calls.

    `task verify` reaches no other `telemetry:push`, so without this `ocx.gate=full`
    and `ocx.gate.escape` never leave the host. Run FOR REAL, as the class above:
    a copy of the taskfile in a fresh repo, a stub `junit2otlp` first on PATH that
    records its argv and the XML it was fed.
    """

    TASKFILE = ROOT / "taskfiles" / "telemetry.taskfile.yml"
    SINCE = 1_800_000_000  # the run start `bazel:test:accept` passes

    def _accept(
        self, tmp_path: Path, *, fresh: bool = True, endpoint: bool = True, marker: bool = True
    ) -> tuple[subprocess.CompletedProcess[str], Path, Path]:
        task = shutil.which("task")
        if task is None:
            pytest.fail("`task` (go-task) is not on PATH — run under `ocx exec --` or `task claude:tests`")
        repo = tmp_path / "repo"
        repo.mkdir()
        shutil.copyfile(self.TASKFILE, repo / "Taskfile.yml")
        for args in (
            ("init", "-q", "-b", "work"),
            ("add", "-A"),
            ("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "seed"),
        ):
            subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, encoding="utf-8")
        reports = repo / "target" / "bazel" / "accept"
        start = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(self.SINCE + 5))
        for module, mtime in (("test_fresh", self.SINCE + 60 if fresh else self.SINCE - 60), ("test_cached", self.SINCE - 3600)):
            report = reports / module / "junit.xml"
            report.parent.mkdir(parents=True)
            report.write_text(
                f'<testsuites><testsuite name="pytest" timestamp="{start}" tests="1">'
                f'<testcase classname="c" name="{module}_case" time="0.1"/></testsuite></testsuites>\n',
                encoding="utf-8",
            )
            os.utime(report, (mtime, mtime))
        if marker:
            (reports / "gate_escape").write_text("test_fresh\n", encoding="utf-8")
            os.utime(reports / "gate_escape", (self.SINCE + 120, self.SINCE + 120))
        stub_dir = tmp_path / "stub"
        stub_dir.mkdir()
        argv_file, stdin_file = tmp_path / "junit2otlp.argv", tmp_path / "junit2otlp.stdin"
        stub = stub_dir / "junit2otlp"
        stub.write_text(
            f"#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > '{argv_file}'\ncat > '{stdin_file}'\n",
            encoding="utf-8",
        )
        stub.chmod(0o755)
        home = tmp_path / "home"
        home.mkdir()
        env = {k: v for k, v in os.environ.items() if k not in ("OCX_GATE", "OTEL_EXPORTER_OTLP_ENDPOINT") and not k.startswith("GIT_")}
        env.update(PATH=f"{stub_dir}{os.pathsep}{os.environ['PATH']}", HOME=str(home))
        if endpoint:
            env["OTEL_EXPORTER_OTLP_ENDPOINT"] = "http://127.0.0.1:9"
        run = subprocess.run(
            [task, "-d", str(repo), "accept", f"REPORTS={reports}", f"SINCE={self.SINCE}"],
            capture_output=True, encoding="utf-8", env=env, check=False,
        )
        assert run.returncode == 0, f"`task accept` failed: {run.stdout}{run.stderr}"
        return run, argv_file, stdin_file

    def test_t2_pushes_this_runs_reports_as_gate_full_with_the_escape(self, tmp_path: Path) -> None:
        run, argv_file, stdin_file = self._accept(tmp_path)
        assert argv_file.is_file(), f"T2 pushed nothing: {run.stdout}{run.stderr}"
        argv = argv_file.read_text(encoding="utf-8").splitlines()
        value = argv[argv.index("--additional-attributes") + 1]
        attrs = dict(pair.partition("=")[::2] for pair in value.split(","))
        assert attrs.get("ocx.gate") == "full", attrs
        assert attrs.get("ocx.suite") == "acceptance", attrs
        assert attrs.get("ocx.gate.escape") == "true", attrs
        pushed = stdin_file.read_text(encoding="utf-8")
        assert "test_fresh_case" in pushed, pushed
        assert "test_cached_case" not in pushed, "a cached verdict from an earlier run must not be re-pushed"

    def test_a_run_that_executed_nothing_pushes_nothing(self, tmp_path: Path) -> None:
        run, argv_file, _ = self._accept(tmp_path, fresh=False)
        assert not argv_file.exists(), f"an all-cached run must push nothing: {run.stdout}{run.stderr}"

    def test_no_endpoint_no_push(self, tmp_path: Path) -> None:
        run, argv_file, _ = self._accept(tmp_path, endpoint=False)
        assert not argv_file.exists(), f"with no OTLP endpoint nothing may be pushed: {run.stdout}{run.stderr}"


# ---------------------------------------------------------------------------
# One build of the acceptance binary (plan_test_speed_tiers.md C-021, P-12)
# ---------------------------------------------------------------------------


def _acceptance_build_drift(test_taskfile: str, rust_taskfile: str) -> list[str]:
    """What keeps `rust:build` and `.build-binaries` from building `ocx` twice.

    `task verify` runs both, and the Bazel acceptance targets run
    `//crates/ocx_cli:ocx` itself. So `rust:build` must only delegate, and
    `.build-binaries` must copy the Bazel output rather than compile a second
    binary with cargo — a second producer is a second compile per verify, and a
    copy from any other directory hands the pytest entry points a binary the
    Bazel lane never tested.
    """

    def cmds(task: dict) -> list:
        return task.get("cmds", []) + ([task["cmd"]] if "cmd" in task else [])

    def text(cmd: object) -> str:
        return cmd if isinstance(cmd, str) else (cmd.get("cmd", "") if isinstance(cmd, dict) else "")

    binaries = cmds(yaml.safe_load(test_taskfile)["tasks"][".build-binaries"])
    build = cmds(yaml.safe_load(rust_taskfile)["tasks"]["build"])
    problems = []
    delegated = [c for c in build if isinstance(c, dict) and c.get("task") == ":test:.build-binaries"]
    if len(delegated) != 1 or len(build) != 1:
        problems.append(f"rust:build must only delegate to :test:.build-binaries, got {build}")
    shell = [text(c) for c in binaries]
    problems += [f".build-binaries compiles with cargo: {c}" for c in shell if re.search(r"\bcargo build\b", c)]
    if not any(re.search(r"\bbazel\b.* build .*//crates/ocx_cli:ocx\b", c) for c in shell):
        problems.append(".build-binaries builds no //crates/ocx_cli:ocx with bazel")
    body = "\n".join(shell)
    if "cquery --output=files" not in body:
        problems.append(".build-binaries does not ask Bazel for its output paths")
    if not re.search(r"^\s*publish //crates/ocx_cli:ocx ", body, re.MULTILINE):
        problems.append(".build-binaries publishes no //crates/ocx_cli:ocx")
    problems += [f"copy reads cargo's target/: {line}" for line in body.splitlines() if re.search(r"\bcp\s+\S*target/", line)]
    return problems


class TestAcceptanceBinaryOneBuild:
    """C-021: one producer of `ocx` — `.build-binaries` copies Bazel's, `rust:build` delegates."""

    TEST_TASKFILE = ROOT / "test" / "taskfile.yml"
    RUST_TASKFILE = ROOT / "taskfiles" / "rust.taskfile.yml"

    def _texts(self) -> tuple[str, str]:
        return self.TEST_TASKFILE.read_text(encoding="utf-8"), self.RUST_TASKFILE.read_text(encoding="utf-8")

    def test_tree_builds_the_acceptance_binary_once(self) -> None:
        assert _acceptance_build_drift(*self._texts()) == []

    def test_a_second_cargo_producer_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        cargo = rust_tf.replace(
            "      - task: :test:.build-binaries\n",
            "      - cargo build --profile test-bin -p ocx --features ocx/__testing --locked\n"
            "      - task: :test:.build-binaries\n",
        )
        assert cargo != rust_tf, "the synthetic cargo producer did not land"
        problems = _acceptance_build_drift(test_tf, cargo)
        assert any(p.startswith("rust:build must only delegate") for p in problems), problems

    def test_copy_from_cargo_target_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        stale = test_tf.replace('cp "$src" "$2.new"', 'cp {{.ROOT_DIR}}/target/release/ocx "$2.new"')
        assert stale != test_tf, "the synthetic stale copy did not land"
        problems = _acceptance_build_drift(stale, rust_tf)
        assert len([p for p in problems if p.startswith("copy reads cargo's target/")]) == 1, problems

    def test_a_spelled_output_path_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        spelled = test_tf.replace("cquery --output=files", "info bazel-bin")
        assert spelled != test_tf, "the synthetic output-path spelling did not land"
        problems = _acceptance_build_drift(spelled, rust_tf)
        assert ".build-binaries does not ask Bazel for its output paths" in problems, problems

    def test_not_publishing_ocx_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        dropped = test_tf.replace("publish //crates/ocx_cli:ocx ", "publish //crates/ocx_cli:elsewhere ")
        assert dropped != test_tf, "the synthetic publish drop did not land"
        problems = _acceptance_build_drift(dropped, rust_tf)
        assert ".build-binaries publishes no //crates/ocx_cli:ocx" in problems, problems

    def test_cargo_build_in_build_binaries_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        cargo = re.sub(
            r"ocx exec bazel -- bazel \{\{\.CACHE_RC\}\} build ",
            "cargo build -p ocx && ocx exec bazel -- bazel {{.CACHE_RC}} build ",
            test_tf,
        )
        assert cargo != test_tf, "the synthetic cargo build did not land"
        problems = _acceptance_build_drift(cargo, rust_tf)
        assert any(p.startswith(".build-binaries compiles with cargo") for p in problems), problems

    def test_dropping_the_ocx_target_reds(self) -> None:
        test_tf, rust_tf = self._texts()
        dropped = test_tf.replace(" //crates/ocx_cli:ocx ", " ")
        assert dropped != test_tf, "the synthetic label drop did not land"
        problems = _acceptance_build_drift(dropped, rust_tf)
        assert ".build-binaries builds no //crates/ocx_cli:ocx with bazel" in problems, problems


# ---------------------------------------------------------------------------
# C-022 (plan_test_speed_tiers.md; ADR adr_test_speed_tiers.md § C-AGENT):
# agent-gate wording sweep
# ---------------------------------------------------------------------------


class TestAgentGateWording:
    """Every project-authored file under `.claude/rules/**`, `.claude/agents/**`
    and `.claude/skills/**` that prescribes a gate must state the canonical
    sentence (ADR `adr_test_speed_tiers.md` § C-AGENT):

        Per task / review-fix iteration: `task verify:scoped --force`. Full
        `task verify` runs at WP merge (enforced by the commit gate), at
        finalize, and whenever `verify:scoped` escalates (it then runs
        `task verify` itself).

    The line-scoped regex below is the ADR's own enforcement design: a line
    naming bare `` `task verify` `` (never `` `task verify:<subcommand>` ``)
    together with a per-task/final-gate trigger phrase is the old, false
    "full verify per task" prescription this sweep forbids — UNLESS the same
    line also names `merge`, `finalize` or `escalat`, which is what the
    correct sentence always does, or the file:line sits on the explicit
    allow-list below with a reason.
    """

    _TRIGGER = re.compile(
        r"per task|each task|every iteration|final gate before commit|review-fix",
        re.IGNORECASE,
    )
    _BARE_VERIFY = re.compile(r"task verify(?!:)")
    _EXEMPT = re.compile(r"merge|finalize|escalat", re.IGNORECASE)

    # (repo-relative path, 1-indexed line number): reason. Empty today —
    # every known gate-prescribing line either avoids the trigger phrases or
    # names merge/finalize/escalat on the same line. Add an entry here only
    # with a comment explaining why the qualifier cannot live on that line.
    _ALLOWLIST: frozenset[tuple[str, int]] = frozenset()

    @classmethod
    def _offending_lines(cls, text: str) -> list[int]:
        """1-indexed line numbers violating the C-AGENT sentence."""
        return [
            i
            for i, line in enumerate(text.splitlines(), start=1)
            if cls._BARE_VERIFY.search(line)
            and cls._TRIGGER.search(line)
            and not cls._EXEMPT.search(line)
        ]

    @staticmethod
    def _sweep_targets() -> list[Path]:
        targets = []
        for sub in ("rules", "agents", "skills"):
            for md in sorted((CLAUDE_DIR / sub).rglob("*.md")):
                if is_vendored(md):
                    continue
                targets.append(md)
        return targets

    def test_synthetic_offending_line_is_red(self) -> None:
        """A line combining bare `task verify` with a per-task trigger and no
        merge/finalize/escalat qualifier must be flagged."""
        text = "Run `task verify` per task, before every commit.\n"
        hits = self._offending_lines(text)
        assert hits == [1], f"expected line 1 flagged, got {hits}"

    def test_synthetic_allowed_line_is_green(self) -> None:
        """The same shape, qualified with `merge` on the same line, must not
        be flagged — this is what the canonical sentence itself does."""
        text = "Run `task verify` per task at WP merge time.\n"
        assert self._offending_lines(text) == []

    def test_one_site_reverted_is_red(self) -> None:
        """Reverting `subsystem-cli.md`'s Quality Gate section to its
        pre-C-022 text (`task rust:verify` per review-fix loop, `task
        verify` = "final gate before commit") must red the sweep."""
        target = CLAUDE_DIR / "rules" / "subsystem-cli.md"
        original = target.read_text(encoding="utf-8")
        canonical = (
            "Per task / review-fix iteration: `task verify:scoped --force`. "
            "Full `task verify` runs at WP merge (enforced by the commit "
            "gate), at finalize, and whenever `verify:scoped` escalates (it "
            "then runs `task verify` itself)."
        )
        assert canonical in original, (
            "fixture text not found in subsystem-cli.md — update this test's "
            "`canonical` string to match the current C-AGENT sentence"
        )
        reverted_text = (
            "During review-fix loop, run `task rust:verify` — not full "
            "`task verify`.\nFull `task verify` = final gate before commit."
        )
        reverted = original.replace(canonical, reverted_text)
        assert reverted != original
        hits = self._offending_lines(reverted)
        assert hits, "reverting subsystem-cli.md's Quality Gate section must red the sweep"

    def test_tree_is_green(self) -> None:
        """Every project-authored gate-prescribing file states the C-AGENT
        sentence (or an exempt/allow-listed variant) today."""
        violations: dict[str, list[int]] = {}
        for md in self._sweep_targets():
            rel = str(md.relative_to(ROOT))
            hits = [
                ln
                for ln in self._offending_lines(md.read_text(encoding="utf-8"))
                if (rel, ln) not in self._ALLOWLIST
            ]
            if hits:
                violations[rel] = hits
        assert not violations, (
            f"C-AGENT sentence violations (bare `task verify` + a per-task/"
            f"final-gate trigger phrase, no merge/finalize/escalat qualifier "
            f"on the same line): {violations}. State the canonical sentence "
            f"(ADR adr_test_speed_tiers.md § C-AGENT) or add a reasoned "
            f"entry to `_ALLOWLIST`."
        )

    # Sites whose *old* wording carried no per-task/final-gate trigger phrase
    # (e.g. "task verify before mark complete"), so `_offending_lines` is
    # blind to them by construction — the line-scoped regex can only catch a
    # line naming the false claim, not a line that merely fails to name the
    # true one. These five are asserted positively instead: each must contain
    # the literal canonical fragment `` `task verify:scoped --force` ``.
    _POSITIVE_SITES: tuple[Path, ...] = (
        CLAUDE_DIR / "agents" / "worker-builder.md",
        CLAUDE_DIR / "agents" / "worker-tester.md",
        CLAUDE_DIR / "skills" / "builder" / "SKILL.md",
        CLAUDE_DIR / "skills" / "deps" / "SKILL.md",
        CLAUDE_DIR / "rules" / "workflow-swarm.md",
    )

    _CANONICAL_FRAGMENT = "task verify:scoped --force"

    def test_positive_sites_are_red_when_reverted(self) -> None:
        """`worker-builder.md`'s pre-C-022 text ("task verify before mark
        complete") carries no trigger phrase, so the regex sweep cannot see
        it revert — the positive assertion must."""
        target = CLAUDE_DIR / "agents" / "worker-builder.md"
        original = target.read_text(encoding="utf-8")
        assert self._CANONICAL_FRAGMENT in original
        reverted = original.replace(
            "Use `task` commands for standard workflows: `task verify:scoped "
            "--force` per task/review-fix iteration; full `task verify` runs "
            "at WP merge (enforced by the commit gate), at finalize, and "
            "whenever `verify:scoped` escalates (it then runs `task verify` "
            "itself). `task test:quick` (acceptance). Run `task --list` to "
            "discover commands.",
            "Use `task` commands for standard workflows: `task verify` "
            "(full gate), `task test:quick` (acceptance). Run `task --list` "
            "to discover commands.",
        )
        assert reverted != original, (
            "fixture text not found in worker-builder.md — update this "
            "test's replacement strings to match the current wording"
        )
        assert self._CANONICAL_FRAGMENT not in reverted

    def test_positive_sites_are_green_on_the_tree(self) -> None:
        """Every site whose old wording the regex sweep is blind to states
        the canonical `task verify:scoped --force` fragment today."""
        missing = [
            str(p.relative_to(ROOT))
            for p in self._POSITIVE_SITES
            if self._CANONICAL_FRAGMENT not in p.read_text(encoding="utf-8")
        ]
        assert not missing, (
            f"these gate-prescribing sites are missing the canonical "
            f"`{self._CANONICAL_FRAGMENT}` fragment, and the ADR regex "
            f"cannot see their old wording revert (no trigger phrase): "
            f"{missing}"
        )
