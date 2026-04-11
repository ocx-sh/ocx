---
paths:
  - .claude/**
---

# AI Configuration Meta-Rule

Governs how `.claude/` artifacts (skills, rules, agents, hooks) are maintained. Loads when working on any `.claude/` file.

## Three Activation Layers

Claude Code has three distinct rule-activation mechanisms. Each serves a different purpose — conflating them produces either dead rules or context bloat.

| Layer | Activation | Use for | Example |
|---|---|---|---|
| **Rule** (`.claude/rules/*.md`) | `paths:` glob — fires when editing matching file | Standards/context needed *while writing* the file | `quality-rust.md` on `**/*.rs` |
| **Skill** (`.claude/skills/<name>/SKILL.md`) | `description` matched by LLM against the current task | Workflow + criteria for a task topic | `deps` for "add a crate" |
| **Catalog** (`.claude/rules.md`) | Read-on-demand during planning, pointed at from CLAUDE.md | Discovering what rules exist *before* any file is open | — |

Path-scoped rules don't fire during planning, research, or architecture work — no file is open yet. Skills require the LLM to already know the skill exists. The catalog (`.claude/rules.md`) closes this gap: it's the authoritative map browsed during plan/research phases. Any change to `.claude/rules/` must be reflected in the catalog in the same commit; structural tests enforce parity.

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

**Every edit to AI configuration must be research-informed.** AI config work uses the **canonical multi-agent research pattern** defined in `/swarm-plan` (Phases 1-2: Discover + Research). Do not reinvent it — delegate.

Spawn workers in parallel before writing:

- **`worker-explorer`** (1-2 agents) — Check existing `.claude/` artifacts for conventions, cross-references, and prior decisions. Map the neighborhood of the artifact being created or changed.
- **`worker-researcher`** (1-3 agents, split by axis when non-trivial):
  - *Claude Code / tooling axis* — `code.claude.com/docs`, frontmatter conventions, new hook/skill/agent features
  - *Domain axis* — best practices for the artifact's subject (Rust patterns, OCI spec, cargo-deny, testing, etc.)
  - *Community axis* — how other projects structure similar artifacts

Persist substantial findings as `.claude/artifacts/research_[topic].md` so future AI config sessions can reuse them. Never author a skill, rule, or agent from memory alone.

See `/swarm-plan` "Research as a Reusable Primitive" for the full pattern contract.

## Artifact Conventions

### Rules (`.claude/rules/*.md`)

- `paths:` frontmatter for scoped rules; omit for global
- <200 lines. If longer, split by domain.
- Structure: types → invariants → gotchas → cross-refs
- Dead glob detection: after renaming directories, verify `paths:` patterns still match files
- **Shareable quality rules** use the `quality-*.md` naming convention (e.g., `quality-rust.md`, `quality-python.md`). These must be **project-independent** — no references to OCX types, modules, or conventions. OCX-specific patterns live in `arch-principles.md` and `subsystem-*.md`. Shareable rules use broad `paths:` globs (e.g., `**/*.rs`) so they activate regardless of project layout.

### Skills (`.claude/skills/<name>/SKILL.md` — canonical flat layout)

- `description` is the #1 discovery factor — write as "what it does + when to use it" (max 1024 chars)
- `argument-hint` must be a quoted string
- `allowed-tools` is NOT supported in frontmatter
- `disable-model-invocation: true` for action skills with side effects (commit, deploy, release)
- Progressive disclosure: SKILL.md <500 lines, reference files for details
- `context: fork` to run in isolated subagent (protects main context)
- **No category subdirectories.** Claude Code discovers skills at `.claude/skills/<name>/SKILL.md` exactly and does not recurse deeper for in-project skills. Nesting for grouping (e.g., `personas/`, `operations/`) silently breaks `/slash-command` discovery. Enforce at the test layer.

### Agents (`.claude/agents/worker-{name}.md`)

- `model`: haiku (exploration), sonnet (implementation/review), opus (architecture)
- `tools`: minimum needed for the role
- Keep concise — agents inherit project rules automatically
- **Minimal anchored preamble + catalog pointer.** Agents point at `.claude/rules.md` for the full rule catalog, then inline a short "Always Apply" preamble (≤5 block-tier anchors, each tagged with its source rule file). The preamble fires at attention even when path-scoped auto-loading hasn't triggered yet. Anchors must cite their source file so drift is visible at review time. This replaces the earlier "deliberate redundancy" pattern where entire rule checklists were duplicated into agent bodies — that approach caused drift and heavy maintenance cost.

### Hooks (`.claude/hooks/*.py`)

- Zero context cost — the only deterministic enforcement mechanism
- Exit 0 = proceed, exit 2 = block + feed stderr to Claude
- Types: `command` (shell/Python), `prompt` (LLM single-turn), `agent` (LLM multi-turn with tools)
- Written in Python (stdlib only), invoked via `uv run` for cross-platform compatibility
- PEP 723 inline script metadata (`# /// script`) for future dependency declaration
- Shared utilities in `hook_utils.py` — import via `sys.path` insertion
- Use `PreToolUse` for blocking, `PostToolUse` for logging/reminders (never exit non-zero)

## Anti-Patterns

1. **Global rule >200 lines** — same problem as bloated CLAUDE.md
2. **Rules matching `src/**/*`** — too broad, effectively another CLAUDE.md
3. **Duplicate content** across CLAUDE.md, rules, and skills — single source of truth
4. **Verbose SKILL.md** without progressive disclosure — move reference material to supporting files
5. **No `disable-model-invocation`** on action skills — Claude triggers them unpredictably
6. **Too many auto-triggering skills** — description budget fills up, skills get excluded
7. **Dead glob patterns** — rules silently never fire after directory renames
8. **Config drift** — embedded project knowledge goes stale as code evolves
9. **Category subdirectories under `.claude/skills/`** — breaks slash command discovery. Caught structurally by `test_all_skills_at_flat_layout`.
10. **Language rules with project-specific content** — `quality-{lang}.md` files are shareable across repos. Any reference to OCX types (`PackageErrorKind`, `ReferenceManager`) or modules (`ocx_lib/`, `crates/ocx_mirror/`) belongs in `arch-principles.md` or `subsystem-*.md`, not in a language rule. Enforced structurally by `test_shareable_rules_no_ocx_leak`.
11. **Catalog drift** — adding, removing, or renaming a rule in `.claude/rules/` without updating `.claude/rules.md` in the same commit. Enforced by `test_catalog_covers_all_rules` and `test_catalog_references_resolve`.

## Consistency Checks

When editing any `.claude/` artifact:

- [ ] Frontmatter follows conventions for the artifact type
- [ ] Cross-references point to existing files
- [ ] New rules reference subsystem context rules where relevant
- [ ] CLAUDE.md stays under 200 lines
- [ ] Global rules total is manageable (currently 5 — monitor growth)
- [ ] AI config structural tests pass: `task claude:tests`

## Structural Validation Tests

`.claude/tests/test_ai_config.py` is the automated enforcement layer for the checklist above. These tests live alongside the config they validate (not in `test/`, which is for OCX binary acceptance tests). They run as part of `task verify` (via `claude:tests`) and catch:

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
task claude:tests                      # via task runner (used by task verify)
cd .claude/tests && uv run pytest -v   # directly
```

### Principle: test the contract, not the content

AI config tests validate **structural invariants** (paths exist, counts match, conventions hold). They do NOT validate semantic correctness (is the rule's advice accurate? does the skill produce good output?). Semantic validation is the job of the `/meta-validate-context` skill, invoked on demand.

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
| New tool integrated | Add to relevant skills, `quality-core.md`, and `config_reminder` in hook |
| Taskfile changed | Update CLAUDE.md, quality-core.md, relevant skill Task Runner sections |
| Vue component added/changed | Update `subsystem-website.md`, `docs-style.md`, documentation skill |
| CI workflow changed | Update `subsystem-ci.md` rule |
| Claude Code new release | Run `/meta-maintain-config research "Claude Code features"` |
| Directory renamed | Verify `paths:` globs in scoped rules still match |
| Config feels stale | Run `/meta-maintain-config refresh` |
