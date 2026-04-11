# Plan: Remove Deprecated Skill-Rules Suggestion Layer

## Context

Claude Code now discovers skills natively from `.claude/skills/<name>/SKILL.md`
frontmatter and surfaces them (name + description) at session start. The
`skill-rules.json` + `skill_activation_prompt.py` system — built before native
discovery — duplicates this in a second, keyword-matched "SKILL SUGGESTIONS"
block on every prompt. That layer now causes:

- Duplicate skill context per turn
- Noisy matches on common substrings (`test`, `fix`, `plan`…)
- A parallel source of truth (`triggers.keywords`) that must stay in sync with
  `description`
- A structural test suite whose sole purpose is to guard the JSON file

This plan removes the deprecated layer in a single commit. No behavior visible
to the user changes — native discovery continues.

## Out of scope

- Editing `description` fields in individual `SKILL.md` files. They are already
  the discovery signal; improvements to them are a separate pass.
- Removing any other hook (`post_tool_use_tracker`, `pre_commit_verification`,
  etc. — all stay).
- Renaming or restructuring skills.

## Pre-flight sanity check (human, before edits)

Confirm slash-command discovery works via the flat layout alone in the current
Claude Code version. The previous bug (nested category directories silently
broke `/slash` while the suggestion hook masked it) is exactly why this layer
existed — we need to know the net is no longer needed.

- [ ] `/builder` resolves and loads `.claude/skills/builder/SKILL.md`
- [ ] `/swarm-plan` resolves and loads `.claude/skills/swarm-plan/SKILL.md`
- [ ] One medium-priority skill (e.g. `/code-check`) also resolves

If any fails, stop — the layer is still load-bearing and this plan should not
execute.

## File-by-file changes

### 1. Delete files

| File | Reason |
|------|--------|
| `.claude/skills/skill-rules.json` | Sole consumer is the hook being removed |
| `.claude/hooks/skill_activation_prompt.py` | Deprecated suggestion layer |

### 2. `.claude/settings.json`

Remove the entire `UserPromptSubmit` hook block (lines 50-60):

```json
"UserPromptSubmit": [
  {
    "hooks": [
      {
        "type": "command",
        "command": "uv run \"$CLAUDE_PROJECT_DIR/.claude/hooks/skill_activation_prompt.py\"",
        "timeout": 5
      }
    ]
  }
],
```

No other hooks currently use `UserPromptSubmit`, so the key itself goes away
(not just the inner entry). Verify JSON still parses after edit.

### 3. `.claude/tests/test_ai_config.py`

Delete:

- The `skill_rules` fixture (lines 32-39)
- `class TestSkillRules` in its entirety (lines 52-142)
  - `test_all_paths_exist`
  - `test_no_duplicate_names`
  - `test_all_skill_files_have_entry`
  - `test_no_ambiguous_keyword_overlap_across_priority_tiers`
  - `test_no_single_common_word_triggers_on_config_skills`
- The section header comment `# skill-rules.json integrity` (lines 47-49)

Update `class TestSkillsLayout` docstring (line 259-268): drop the sentence
"…while the `skill_activation_prompt.py` suggestion hook continues to work,
making the bug invisible." Replace with a shorter note that Claude Code does
not recurse into nested skill dirs.

**Keep** `TestSkillsLayout` — orphan/flat-layout enforcement is still valuable
and no longer depends on skill-rules.json. It already uses `glob("*/SKILL.md")`
against the filesystem directly.

### 4. `.claude/tests/test_hooks.py`

Delete:

- The import `import skill_activation_prompt` (line 24)
- `class TestSkillActivation` in its entirety (lines 195-295)
  - `_make_skill` helper
  - `test_keyword_match`
  - `test_intent_pattern_match`
  - `test_no_match_returns_empty`
  - `test_dedup_keyword_prevents_intent_match`
  - `test_priority_grouping_order_in_format`
  - `test_missing_skill_file_skipped`
  - `test_invalid_regex_skipped`

### 5. `.claude/rules/meta-ai-config.md`

Four edits:

- **Line 71** (Skills section bullet): remove the "while the
  `skill_activation_prompt.py` suggestion hook continues to work, making the
  bug invisible" clause. Replace with: "Nesting for grouping (e.g.,
  `personas/`, `operations/`) silently breaks `/slash-command` discovery.
  Enforce at the test layer."
- **Lines 88-93** (the entire `### Skill Rules (skill-rules.json)` subsection):
  delete.
- **Line 114** (Consistency Checks checklist): delete the
  `` - [ ] `skill-rules.json` entry exists and path is valid `` line.
- **Line 125** (Structural Validation Tests bullet list): delete the
  `**skill-rules.json integrity**` bullet. Update the lead-in sentence if the
  bullet count in prose breaks.

### 6. `.claude/skills/maintain-ai-config/SKILL.md`

Five edits (lines 31, 44, 48, 119, 139). All reference `skill-rules.json` as
part of the AI-config maintenance workflow:

- **Line 31**: "Update `skill-rules.json`, CLAUDE.md tables, meta-rule
  inventory" → "Update CLAUDE.md tables, meta-rule inventory"
- **Line 44**: delete "Every `skill-rules.json` path → existing file?"
- **Line 48**: delete "No orphan skills (on disk but missing from
  `skill-rules.json`)?"
- **Line 119**: delete the table row for `skill-rules.json`
- **Line 139**: delete "ALWAYS validate `skill-rules.json` after changes"

Re-read the surrounding prose for each edit and tighten wording so the
paragraph still flows.

### 7. `.claude/artifacts/plan_quality_rules_reorg.md`

This is a historical artifact. **Leave untouched** — artifacts preserve
prior-state references by design (there is already precedent: see
`test_no_references_to_deleted_language_skills` which explicitly exempts
`.claude/artifacts/`). If we later add an equivalent test for the skill-rules
removal, apply the same exemption.

## Verification

After all edits:

```sh
task lint:ai-config     # runs .claude/tests/test_ai_config.py
task verify             # full project gate (fmt, clippy, tests, acceptance)
```

Specifically:

- `test_ai_config.py` must pass with `TestSkillRules` removed
- `test_hooks.py` must pass with `TestSkillActivation` removed
- `settings.json` must parse (Claude Code reloads it on next session)
- Grep `skill-rules\|skill_activation` across the repo — only permitted hits
  should be inside `.claude/artifacts/`

```sh
rg "skill-rules|skill_activation" .claude
# expect: only .claude/artifacts/... matches
```

## Commit

Single `chore:` commit (per `feedback_chore_for_ai_settings.md` — AI config
changes use `chore:`, not `feat`/`refactor`):

```
chore(claude): remove deprecated skill-rules suggestion layer

Claude Code now surfaces skills natively from SKILL.md frontmatter,
making skill-rules.json and skill_activation_prompt.py redundant.
Removes the JSON, the hook, the UserPromptSubmit wiring, and their
structural tests. Native flat-layout discovery + TestSkillsLayout
remain as the enforcement mechanism.
```

Do **not** push — per project rule, the human decides when to push.

## Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Some environment still relies on keyword suggestions to nudge skill use | Pre-flight sanity check covers `/slash` discovery; native description-based discovery is the intended replacement |
| A future contributor re-adds skill-rules.json without realising it's deprecated | The `meta-ai-config.md` edits explicitly remove the convention; the test file no longer has a `skill_rules` fixture to copy from |
| Orphan detection lost | `TestSkillsLayout.test_all_skills_at_flat_layout` + `test_skill_dir_matches_frontmatter_name` already enforce the structural invariants that matter; orphaned JSON entries are impossible once the JSON is gone |

## Rollback

`git revert` on the single chore commit restores every file. No data migration,
no state change outside `.claude/`.
