---
paths:
  - .claude/**
---

# AI Configuration Meta-Rule

Governs how `.claude/` artifacts (skills, rules, agents, hooks) are maintained. Loads when working on any `.claude/` file.

## Core Principle: Context Budget

Every rule, skill description, and CLAUDE.md line competes for the same context window. Bloated config causes Claude to ignore instructions.

| Artifact | Budget | Impact |
|----------|--------|--------|
| CLAUDE.md | <200 lines | Loaded every request — every line costs attention |
| Rules (global) | <200 lines each | Loaded every request — minimize globals |
| Rules (scoped) | <200 lines each | Loaded only on path match — prefer scoping |
| Skill descriptions | 2% of context window total | All descriptions loaded at session start |
| Skill body (SKILL.md) | <500 lines | Loaded only when invoked — safe for detail |
| Hooks | Zero context cost | External scripts, never in context window |
| Subagents | Isolated context | No impact on main session |

**Decision tree — where does this instruction belong?**

```
Must Claude know it every session?
├─ Yes → Is it file/directory-specific?
│  ├─ Yes → .claude/rules/ with paths: scoping
│  └─ No → CLAUDE.md (only if removing it causes mistakes)
└─ No → Is it invoked manually or auto-triggered?
   ├─ Manual with side effects → Skill with disable-model-invocation: true
   ├─ Auto-triggered by context → Skill with good description
   └─ Pure automation, no LLM judgment → Hook (deterministic, zero cost)
```

## Research Protocol

**Every edit to AI configuration must be research-informed.** Spawn agents before writing:

1. **`worker-researcher`** — Search code.claude.com/docs + domain best practices + community patterns
2. **`worker-explorer`** — Check existing `.claude/` artifacts for conventions and cross-references

Never author a skill, rule, or agent from memory alone.

## Artifact Conventions

### Rules (`.claude/rules/*.md`)

- `paths:` frontmatter for scoped rules; omit for global
- <200 lines. If longer, split by domain.
- Structure: types → invariants → gotchas → cross-refs
- Dead glob detection: after renaming directories, verify `paths:` patterns still match files

### Skills (`.claude/skills/{category}/{name}/SKILL.md`)

- `description` is the #1 discovery factor — write as "what it does + when to use it" (max 1024 chars)
- `argument-hint` must be a quoted string
- `allowed-tools` is NOT supported in frontmatter
- `disable-model-invocation: true` for action skills with side effects (commit, deploy, release)
- Progressive disclosure: SKILL.md <500 lines, reference files for details
- `context: fork` to run in isolated subagent (protects main context)

### Agents (`.claude/agents/worker-{name}.md`)

- `model`: haiku (exploration), sonnet (implementation/review), opus (architecture)
- `tools`: minimum needed for the role
- Keep concise — agents inherit project rules automatically

### Hooks (`.claude/hooks/*.py`)

- Zero context cost — the only deterministic enforcement mechanism
- Exit 0 = proceed, exit 2 = block + feed stderr to Claude
- Types: `command` (shell/Python), `prompt` (LLM single-turn), `agent` (LLM multi-turn with tools)
- Written in Python (stdlib only), invoked via `uv run` for cross-platform compatibility
- PEP 723 inline script metadata (`# /// script`) for future dependency declaration
- Shared utilities in `hook_utils.py` — import via `sys.path` insertion
- Use `PreToolUse` for blocking, `PostToolUse` for logging/reminders (never exit non-zero)

### Skill Rules (`skill-rules.json`)

- All paths must point to existing SKILL.md files
- `priority`: high (personas, core), medium (language, audit), low (docs, deps)
- Keep descriptions concise — they consume the 2% skill description budget

## Anti-Patterns

1. **Global rule >200 lines** — same problem as bloated CLAUDE.md
2. **Rules matching `src/**/*`** — too broad, effectively another CLAUDE.md
3. **Duplicate content** across CLAUDE.md, rules, and skills — single source of truth
4. **Verbose SKILL.md** without progressive disclosure — move reference material to supporting files
5. **No `disable-model-invocation`** on action skills — Claude triggers them unpredictably
6. **Too many auto-triggering skills** — description budget fills up, skills get excluded
7. **Dead glob patterns** — rules silently never fire after directory renames
8. **Config drift** — embedded project knowledge goes stale as code evolves

## Consistency Checks

When editing any `.claude/` artifact:

- [ ] Frontmatter follows conventions for the artifact type
- [ ] `skill-rules.json` entry exists and path is valid
- [ ] Cross-references point to existing files
- [ ] New rules reference subsystem context rules where relevant
- [ ] CLAUDE.md stays under 200 lines
- [ ] Global rules total is manageable (currently 5 — monitor growth)
- [ ] AI config structural tests pass: `task lint:ai-config`

## Structural Validation Tests

`.claude/tests/test_ai_config.py` is the automated enforcement layer for the checklist above. These tests live alongside the config they validate (not in `test/`, which is for OCX binary acceptance tests). They run as part of `task verify` (via `lint:ai-config`) and catch:

- **skill-rules.json integrity**: all paths exist, no duplicate names, no orphaned SKILL.md files, no ambiguous keyword overlaps across priority tiers, no overly broad single-word triggers on operations skills
- **Rule glob validity**: every `paths:` glob in scoped rules matches at least one file on disk (dead glob detection)
- **CLAUDE.md consistency**: line budget, stated principle count matches headings, stated worktree count matches table rows
- **Cross-reference accuracy**: workflow filenames in rules exist on disk, artifact paths in skills use correct directories
- **Agent correctness**: tool/body consistency (no commands requiring tools not in frontmatter), completion protocol compliance
- **Hook safety**: no `set -e` in PostToolUse hooks, conditional log trimming
- **Taskfile robustness**: empty file list guards for linting tasks

### When to extend the tests

| Trigger | Test to add |
|---------|------------|
| New skill added | Covered automatically (orphan detection + path existence) |
| New scoped rule added | Covered automatically (glob match validation) |
| New keyword trigger | Covered automatically (overlap + noise detection) |
| New agent with tool restrictions | Add a test verifying body doesn't use commands outside `tools:` |
| New convention in CLAUDE.md | Add a test that counts/validates the stated fact |
| New hook script | Add safety tests (no `set -e` for PostToolUse, exit code discipline) |
| New cross-reference pattern | Add a test that resolves the reference to a real file |

### Running the tests

```sh
task lint:ai-config                    # via task runner (used by task verify)
cd .claude/tests && uv run pytest -v   # directly
```

### Principle: test the contract, not the content

AI config tests validate **structural invariants** (paths exist, counts match, conventions hold). They do NOT validate semantic correctness (is the rule's advice accurate? does the skill produce good output?). Semantic validation is the job of the `/validate-context` skill, invoked on demand.

## Automated Staleness Detection

The PostToolUse hook (`post-tool-use-tracker.sh`) fires on every `Edit|Write` and outputs AI config update reminders when infrastructure files change. Two reminder types:

1. **`context_reminder`** — source subsystem changes (e.g., editing `crates/ocx_lib/src/oci/`) → reminds to update the matching `subsystem-*.md` rule
2. **`config_reminder`** — infrastructure changes (Taskfiles, Vue components, VitePress config, CI workflows, Cargo.toml, deny.toml, mirror configs) → lists the specific AI config files that need updating

**When adding a new infrastructure dependency** (new Taskfile, new component library, new CI tool), add a corresponding `config_reminder` entry to `post-tool-use-tracker.sh` that maps the file pattern to all AI config files that reference it. This ensures future changes trigger update reminders automatically.

## When to Update

| Trigger | Action |
|---------|--------|
| New subsystem created | Add `subsystem-{name}.md` scoped rule |
| Codebase pattern changed | Update affected subsystem rules |
| New tool integrated | Add to relevant skills, `code-quality.md`, and `config_reminder` in hook |
| Taskfile changed | Update CLAUDE.md, code-quality.md, relevant skill Task Runner sections |
| Vue component added/changed | Update `subsystem-website.md`, `documentation.md`, documentation skill |
| CI workflow changed | Update `ci-workflows` skill |
| Claude Code new release | Run `/maintain-ai-config research "Claude Code features"` |
| Directory renamed | Verify `paths:` globs in scoped rules still match |
| Config feels stale | Run `/maintain-ai-config refresh` |
