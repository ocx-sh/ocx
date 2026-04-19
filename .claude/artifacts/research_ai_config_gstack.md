---
name: research_ai_config_gstack
description: Deep study of the gstack AI config repo
type: research
source_repo: https://github.com/garrytan/gstack
audited_at: 2026-04-19
---

# gstack AI Config Research

## TL;DR

gstack is a 39-skill open-source AI engineering workflow framework built by Garry Tan (YC). Its most transferable insight for OCX is the **tiered preamble system**: a `preamble-tier: 1-4` frontmatter field controls how many shared context blocks are injected into a skill's body at compile time, so lightweight skills don't pay the full context cost of heavy ones. The **parallel specialist Review Army** pattern — dispatching 2-7 subagents in parallel with confidence-scored JSON findings, merged by fingerprint in the parent — is a meaningfully more sophisticated version of OCX's current review-fix loop. The **cross-session learnings JSONL store** (typed, confidence-scored, per-project) directly addresses OCX's "every session starts cold" problem. The template compilation pipeline (`.tmpl` + resolvers → `.md`) solves the multi-host problem gstack has, which OCX doesn't; for OCX the pattern's value is narrower: single-sourcing large shared blocks that are currently copy-pasted across skills/rules.

## Purpose & Positioning

gstack is Garry Tan's personal "software factory" — a collection of slash-command workflow skills for Claude Code that model a virtual engineering team (CEO reviewer, QA lead, security officer, release engineer, etc.). It targets YC founders and individual technical builders who want structured processes instead of blank prompts. It is multi-host (10 AI coding agents supported) and includes a persistent headless Chromium browser daemon as its core differentiating tool. The repo is MIT licensed, actively developed, and has shipped 23+ specialists since early 2026.

## Inventory

| Category | Count | Notable files (with size) |
|---|---|---|
| Skill templates (.tmpl) | 39 | `preamble.ts` (856 lines/~35KB), `review/SKILL.md.tmpl` (275 lines), `investigate/SKILL.md.tmpl` (220 lines) |
| Generated SKILL.md | ~39 | One per template; token ceiling 100KB (~25K tokens) enforced at compile time |
| Resolver TypeScript files | 18 | `scripts/resolvers/index.ts`, `preamble.ts`, `review-army.ts`, `learnings.ts`, `confidence.ts`, `composition.ts`, `review.ts` |
| Host configs | 10 | `scripts/host-config.ts` (190 lines), `hosts/*.ts` (claude, codex, factory, kiro, opencode, slate, cursor, openclaw, gbrain, hermes) |
| Generator script | 1 | `scripts/gen-skill-docs.ts` (~600 lines) |
| CI workflows | 5 | `evals.yml` (E2E + LLM judge), `evals-periodic.yml`, `skill-docs.yml`, `actionlint.yml`, `ci-image.yml` |
| Specialist checklists | 7 | `review/specialists/{testing,security,red-team,performance,maintainability,data-migration,api-contract}.md` |
| Support docs | ~40 | `ARCHITECTURE.md`, `ETHOS.md`, `CONTRIBUTING.md`, design docs in `docs/designs/` |
| Bin scripts | ~30 | `bin/gstack-*` shell/compiled scripts (learnings-log, review-log, diff-scope, specialist-stats, etc.) |

Total config size: approximately 800KB across ~170 files (excluding `browse/` browser daemon TypeScript source).

## Architecture

### Dispatch Model

Skills are standalone markdown files (`SKILL.md`) that Claude reads and executes top-to-bottom. There is no shared runtime loader — each skill invocation is an independent Claude session (`claude -p`). Skills compose via the `{{INVOKE_SKILL:name}}` resolver, which instructs Claude to read a child skill's SKILL.md via the Read tool and execute it inline, skipping named shared sections (preamble, telemetry, etc.). This is static composition: the child skill's content is read at runtime, not at compile time.

The `SKILL.md.tmpl` → `SKILL.md` compilation pipeline runs at build time (`bun run gen:skill-docs`). The generator (`scripts/gen-skill-docs.ts`) reads each template, resolves `{{PLACEHOLDER}}` tokens via 40+ named resolver functions from `scripts/resolvers/index.ts`, applies host-specific frontmatter transformation and path/tool rewrites, and writes committed `SKILL.md` files. CI enforces freshness via `gen:skill-docs --dry-run` + `git diff --exit-code`.

### Context Budget Strategy

**Preamble tiering** is the primary context budget mechanism. The `preamble-tier: 1-4` frontmatter field controls which feature blocks are injected via `{{PREAMBLE}}`, resolved by `scripts/resolvers/preamble.ts` (856 lines):

| Tier | Feature set | Skills using it |
|---|---|---|
| T1 | bash block + update check + telemetry + voice(trimmed) + completion status | browse, benchmark, setup-cookies |
| T2 | T1 + voice(full/persona) + AskUserQuestion format + completeness + context recovery + confusion protocol | investigate, cso, retro, checkpoint, health, document-release |
| T3 | T2 + repo-mode + search-before-building | autoplan, office-hours, plan-ceo/design/eng-review |
| T4 | T3 (TEST_FAILURE_TRIAGE is a separate `{{}}` placeholder, not in preamble) | ship, review, qa, qa-only, design-review, land-and-deploy |

Token ceiling: `TOKEN_CEILING_BYTES = 100_000` (100KB, ~25K tokens) is enforced at compile time with a warning; actual generated SKILL.md files typically run 200–800 lines.

Heavy reference material lives in separate files loaded on demand via the Read tool (e.g., `review/checklist.md`, `review/specialists/*.md`), not inlined.

### Multi-Host Template System

Each host is a typed `HostConfig` object in `hosts/*.ts`. The interface (`scripts/host-config.ts`) specifies:
- Install paths (`globalRoot`, `localSkillRoot`, `hostSubdir`)
- Frontmatter transformation mode: `allowlist` (Codex: keep only `name` + `description`) or `denylist` (Claude: strip only `sensitive:` field)
- Description length limits with configurable behavior (error/truncate/warn)
- Path rewrites (replaceAll, in order): translates `~/.claude/skills/gstack/` to `$GSTACK_ROOT/` for non-Claude hosts
- Tool rewrites: e.g., `AskUserQuestion` → `user_input` for Factory
- `suppressedResolvers`: resolvers that return empty string on this host (e.g., `GBRAIN_CONTEXT_LOAD` suppressed on all non-gbrain hosts)
- Skill include/skip lists, sidecar configs, symlink strategies

Adding a new host requires zero code changes to the generator — only one `hosts/newhost.ts` config file.

## Notable Patterns (ranked by OCX adoption value)

### Pattern 1: Preamble tiering — score 5/5

**What**: A `preamble-tier: 1-4` frontmatter field that controls how many shared context sections are injected into a skill's context window. Lighter skills get a stripped-down preamble; heavy workflow skills get the full treatment.

**Where**: `scripts/resolvers/preamble.ts` (lines 832–855), `{{PREAMBLE}}` placeholder in every `*.tmpl` file.

**How it works**: At compile time, the generator reads `preamble-tier` from frontmatter, passes it to `generatePreamble()`, which concatenates only the tier-appropriate section generators. The preamble itself is injected as-is into the generated `SKILL.md` body — it runs when Claude loads the skill.

**OCX applicability**: Adopt — OCX has some large skill bodies (e.g., `qa-engineer`, `builder`) where preamble sections (commit conventions, branch rules, code quality principles) are copy-pasted. A single `{{PREAMBLE}}` resolver compiled from `arch-principles.md` + `workflow-git.md` + `quality-core.md` shared blocks would eliminate that duplication and give a natural place to tune context cost per skill tier.

---

### Pattern 2: Parallel specialist Review Army — score 4/5

**What**: `{{REVIEW_ARMY}}` dispatches 2-7 parallel Agent subagents, each reading a specialist checklist from `review/specialists/*.md`. Each specialist returns structured JSON findings (`{"severity","confidence","path","line","category","summary","fix","fingerprint","specialist"}`). The parent merges by fingerprint, boosts confidence for multi-specialist confirmation, gates display by score (7+=show, 5-6=caveat, 3-4=appendix, 1-2=suppress).

**Where**: `scripts/resolvers/review-army.ts`, `review/specialists/*.md`, `review/SKILL.md.tmpl` lines 115–120 (`{{REVIEW_ARMY}}`).

**How it works**: The parent first runs `gstack-diff-scope` to detect stack/scope signals (SCOPE_AUTH, SCOPE_BACKEND, SCOPE_FRONTEND, SCOPE_MIGRATIONS, SCOPE_API). Based on signals and diff size, it selects 2-7 specialists. Always-on: testing + maintainability (50+ line diffs). Conditional: security (if SCOPE_AUTH), performance (backend or frontend), data-migration (migrations), api-contract (API). Adaptive gating: `gstack-specialist-stats` tracks per-specialist findings rate; specialists with 0 findings in 10+ dispatches are auto-skipped. Red team fires only if DIFF_LINES > 200 or any specialist found a CRITICAL finding.

**OCX applicability**: Adapt — OCX's swarm-review does parallel adversarial review but with a single reviewer, not domain-specialized subagents. For OCX's Rust codebase, relevant specialists would be: correctness (ownership/lifetime), security (input validation, unsafe blocks), API contract (public trait/function signature stability), and test coverage. The JSON-structured findings with fingerprint dedup and confidence gating is directly useful for the OCX review-fix loop.

---

### Pattern 3: Cross-session learnings JSONL store — score 4/5

**What**: Every skill can write typed, confidence-scored learnings to `~/.gstack/projects/{slug}/learnings.jsonl` via `gstack-learnings-log`. The preamble auto-loads the 3 most recent learnings at session start (when count > 5). Four-layer persistence: learnings (what you know), timeline (what happened), checkpoints (where you are), health (how good the code is).

**Where**: `scripts/resolvers/learnings.ts`, `{{LEARNINGS_SEARCH}}` and `{{LEARNINGS_LOG}}` in `investigate/SKILL.md.tmpl` and `review/SKILL.md.tmpl`, `docs/designs/SELF_LEARNING_V0.md`.

**How it works**: Each JSONL entry: `{ts, skill, type, key, insight, confidence, source, branch, commit, files[]}`. Storage is append-only; duplicates resolved at read time by `gstack-learnings-search` (latest winner per key+type). Cross-project discovery is opt-in via config. The `{{LEARNINGS_SEARCH}}` resolver auto-promotes cross-project learnings visibility with a one-time AskUserQuestion.

**OCX applicability**: Adopt — OCX currently has no cross-session memory. The JSONL format is trivial to implement in a hook or skill epilog. High payoff: recurring bug patterns in the same subsystem, `oci-client` quirks, accepted clippy suppressions, and test fixture conventions could compound across sessions. The typed schema (type enum: operational, pitfall, preference, investigation) is well-designed for OCX's use cases.

---

### Pattern 4: Confidence calibration with display gating — score 4/5

**What**: Every review finding carries a 1-10 confidence score. Display gates: 7+=show normally, 5-6=show with caveat "verify this is an issue", 3-4=suppress to appendix, 1-2=suppress entirely. Calibration learning: if user confirms a <7-confidence finding, log the corrected pattern as a learning.

**Where**: `scripts/resolvers/confidence.ts` (37 lines), `{{CONFIDENCE_CALIBRATION}}` in `review/SKILL.md.tmpl`.

**How it works**: The resolver injects the rubric into the skill body as a prompt instruction. The agent self-reports confidence scores on each finding. No external model or tool call — purely prompt-driven calibration that feeds back into the learnings store.

**OCX applicability**: Adopt — OCX's review-fix loop classifies findings as actionable/deferred but has no confidence scores. Adding confidence scores to finding classification would make the "actionable vs deferred" decision more principled and would make the calibration learning loop possible.

---

### Pattern 5: INVOKE_SKILL composition — score 3/5

**What**: `{{INVOKE_SKILL:skill-name}}` renders prose instructing Claude to read a child skill's SKILL.md via the Read tool and follow its instructions from top-to-bottom, skipping a defined list of shared sections (preamble, telemetry, AskUserQuestion format, completeness principle). Supports a `skip=` parameter for additional sections.

**Where**: `scripts/resolvers/composition.ts` (48 lines), used in `autoplan/SKILL.md.tmpl` to chain CEO+design+eng+DX reviews.

**How it works**: The parent skill passes the child's file path to Claude's Read tool. Claude reads the child's SKILL.md and executes it inline. The parent's preamble has already run, so the child skips its own preamble (deduplication by name match). This is cheaper than spawning a subagent since it shares the same context window.

**OCX applicability**: Adapt — OCX's `/swarm-execute` chains multiple workers via explicit subagents. A lighter-weight in-session composition (like INVOKE_SKILL) could be useful for the quality gate chain (clippy → tests → security scan) within a single skill invocation without spawning subagents. However, OCX's existing subsystem verify pattern (`task rust:verify`) achieves this at the shell level, so the payoff is low unless OCX develops multi-step meta-skills.

---

### Pattern 6: Error-message-as-guidance philosophy — score 3/5

**What**: Every error from the browse CLI is rewritten via `wrapError()` to include recovery instructions written for an AI agent, not a human. Playwright's internal stack traces are stripped; actionable next-step guidance is added.

**Where**: `browse/src/error-handling.ts`. Architecture documented in `ARCHITECTURE.md` "Error philosophy" section.

**Example**: "Element not found or not interactable. Run `snapshot -i` to see available elements." vs Playwright's raw error.

**OCX applicability**: Adopt in spirit — OCX's Rust error types already use `anyhow` with rich context chains. The gap is in skill bodies: when `ocx` CLI exits non-zero or a task fails, skill instructions should explicitly tell the agent what to try next rather than just "if the command fails, investigate." This costs nothing to add to existing SKILL.md bodies.

---

### Pattern 7: Diff-based E2E test tier selection — score 3/5

**What**: Each E2E test declares its file dependencies in `test/helpers/touchfiles.ts`. The `test:evals` command auto-selects tests touching changed files. Two tiers: `gate` (runs on every PR) and `periodic` (weekly/manual). LLM-as-judge (Tier 3, ~$0.15, ~30s) scores generated docs for clarity/completeness/actionability.

**Where**: `CLAUDE.md` (commands), `.github/workflows/evals.yml`, `ARCHITECTURE.md` "Template test tiers" section.

**How it works**: `bun run eval:select` shows which tests would run. Tier 1 (free, <5s): static validation of `$B` command references. Tier 2 (~$3.85, ~20min): spawn real `claude -p` session, run each skill, check for errors. Tier 3 (~$0.15, ~30s): Sonnet scores docs.

**OCX applicability**: Adapt — OCX has pytest acceptance tests but no diff-based test selection. OCX's `task verify` runs the full suite on every change. A diff-gated acceptance test runner that maps test files to the OCX commands they exercise would reduce CI cost. The LLM-as-judge pattern for documentation quality scoring is novel — OCX could apply it to verify that `user-guide.md` accurately describes new CLI commands after each feature.

---

### Pattern 8: Typed multi-host HostConfig — score 1/5 (skip for OCX)

**What**: Each host is a single typed `HostConfig` TypeScript object in `hosts/*.ts`. Zero code changes to add a new host — only one config file. The type is validated on every `bun test`. Supports allowlist/denylist frontmatter modes, path rewrites, tool rewrites, suppressed resolvers, description length limits, and conditional frontmatter fields.

**Where**: `scripts/host-config.ts` (190 lines), `hosts/*.ts`, `docs/ADDING_A_HOST.md`.

**OCX applicability**: Skip — OCX is Claude Code only. The underlying design pattern (typed config object over code switches) is excellent, but OCX has no multi-host requirement. If OCX ever adds Cursor or Codex support, this is the right model.

## Token/Context Discipline

Evidence of tight context budget management throughout the codebase:

1. **Preamble tiers** are the primary mechanism — T1 browse skill pays ~100 lines of shared context; T4 ship/review pays ~400 lines. For OCX's 30K-line config problem, this pattern is directly applicable: separate OCX's shared principle blocks into tiers and only inject the tier matching a skill's complexity.

2. **Token ceiling enforcement at compile time**: `TOKEN_CEILING_BYTES = 100_000` in `scripts/gen-skill-docs.ts` (line 559). If any generated SKILL.md exceeds 100KB (~25K tokens), the compiler warns. OCX has no equivalent ceiling — some combined subsystem rules are likely already exceeding practical per-skill context budgets.

3. **Separate reference files, loaded on-demand**: `review/checklist.md` and `review/specialists/*.md` are not inlined. The skill body instructs Claude to `Read` them when needed. For OCX, `arch-principles.md` is always-loaded but could be split: the glossary and ADR index are reference material, while the 8 core design principles are the actual behavioral instructions.

4. **Progressive disclosure via `{{INVOKE_SKILL}}`**: Child skill content is loaded only when the parent's workflow reaches that step. For OCX, this could reduce initial context cost of complex meta-skills.

5. **Suppressed resolvers per host**: GBrain-specific context blocks (`{{GBRAIN_CONTEXT_LOAD}}`, `{{GBRAIN_SAVE_RESULTS}}`) return empty string on non-gbrain hosts. For OCX, if multiple "host variants" are ever needed (e.g., a lighter config for Cursor), the resolver suppression pattern would work.

6. **GCOMPACTION.md (tabled, 2026-04-17)**: gstack investigated context compaction for built-in tool outputs but found it architecturally blocked — `PostToolUse` hooks can only replace MCP tool output (`updatedMCPToolOutput`), not Claude Code's native tools (Bash, Read, Grep, Glob). This is consistent with OCX's RTK finding that RTK is Bash-PreToolUse-only and cannot cover Read/Grep/Glob. Neither gstack nor OCX has found a solution for native tool output compaction.

## Anti-Patterns for OCX

1. **Bun+TypeScript compilation pipeline** (`scripts/gen-skill-docs.ts`, 600+ lines): OCX is single-host Claude Code. The template compilation system solves the multi-host problem gstack has but OCX does not. For OCX, if shared-block extraction is needed, a simple Python script or even manual `@include` convention would achieve the same result with far less toolchain overhead. Do not adopt the full pipeline.

2. **Garry Tan persona/brand injection in preamble** (preamble tier 2+ `generateVoiceDirective()`): The "GStack voice" section (~100 lines in T2-T4) injects a founder persona, YC ethos, and writing style tailored to Garry's audience. OCX's audience is AI agents automating CI/CD pipelines — the persona content would be noise. OCX's neutral senior-engineer tone in `quality-core.md` is correct.

3. **Session tracking + telemetry infrastructure** (Supabase, `~/.gstack/analytics/`, `gstack-telemetry-log`): gstack tracks skill usage for product analytics. OCX has no equivalent requirement. The telemetry code adds ~30 lines to every generated preamble. Skip entirely.

4. **40-placeholder resolver registry** for a single-host project: gstack's 40+ named resolvers (`SLUG_EVAL`, `DEPLOY_BOOTSTRAP`, `CHANGELOG_WORKFLOW`, etc.) exist because many must vary per host. For OCX's single-host context, if shared-block extraction is adopted, 5-10 resolvers covering the highest-value shared blocks (preamble, review checklist, commit conventions, learnings injection) would deliver 80% of the value at 10% of the complexity.

5. **AskUserQuestion format enforcement in preamble**: gstack's AskUserQuestion protocol (re-ground, simplify, recommend, lettered options) is optimized for interactive workflows with non-technical users. OCX's CLAUDE.md is for AI agents executing automation pipelines. The AskUserQuestion format section adds ~40 lines to T2+ preambles — not applicable to OCX's machine-oriented context.

## Direct Applicability Summary

| Pattern | OCX Fit | Estimated Payoff |
|---|---|---|
| Preamble tiering (T1-T4 shared blocks) | Adopt | High — directly addresses OCX's 30K-line bloat; subsystem rules currently copy-paste 3-4 shared blocks |
| Parallel specialist Review Army + JSON findings + confidence gating | Adopt (adapt JSON schema to Rust finding types) | High — OCX review-fix loop would benefit from domain-specialized Rust reviewers (correctness, API contract, unsafe) |
| Cross-session learnings JSONL store | Adopt | High — OCX has zero cross-session memory; recurring patterns in oci-client, file-structure, test fixtures would compound |
| Confidence calibration on review findings | Adopt | Medium — small incremental improvement to existing OCX review-fix loop; low implementation cost |
| Error-message-as-guidance in skill bodies | Adopt in spirit | Medium — OCX skill bodies already describe error recovery but inconsistently |
| INVOKE_SKILL composition | Adapt if needed | Low — OCX subsystem verify pattern (shell-level chaining) covers the same use case more simply |
| Diff-based eval test selection | Adapt | Low-Medium — OCX acceptance tests are slow; diff-gated selection could reduce CI cost; LLM-as-judge for doc quality is novel |
| Typed HostConfig | Skip | Zero — OCX is single-host |
| Bun+TypeScript compilation pipeline | Skip | Zero — single-host OCX doesn't need multi-host renderer |
| Persona/brand voice in preamble | Skip | Zero — wrong audience; OCX is automation-first |
