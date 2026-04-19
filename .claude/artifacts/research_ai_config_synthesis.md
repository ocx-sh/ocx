---
name: research_ai_config_synthesis
description: Cross-repo synthesis of AI config patterns + taxonomy + OCX adoption recommendations
type: research
audited_at: 2026-04-19
inputs:
  - research_ai_config_gstack.md
  - research_ai_config_superpowers.md
  - research_ai_config_get-shit-done.md
  - research_ai_config_sota.md
  - audit_ai_config_ocx.md
---

# AI Config Cross-Repo Synthesis

## Headline Findings (TL;DR)

Ranked priority for OCX, highest impact first:

1. **CSO discipline is the single highest-ROI intervention** — Superpowers has empirical evidence (per `research_ai_config_superpowers.md` Pattern 1) that a skill description containing workflow words ("dispatches", "runs", "iterates") causes Claude to follow the description as a shortcut and skip the full skill body. The OCX audit (per `audit_ai_config_ocx.md`) shows 17 skills with descriptions averaging ~196 chars each, many of which likely violate CSO. A 30-minute description audit fixes every skill invocation across the project at zero token cost. This contradicts official Anthropic guidance ("description = what + when"), but Superpowers has eval evidence the official docs do not.

2. **OCX has 233 lines of undocumented always-loaded context** — `workflow-bugfix.md` (120 ln) and `workflow-refactor.md` (113 ln) are self-labeled "catalog-only" but have no `paths:` frontmatter and load every session (per `audit_ai_config_ocx.md` Staleness Candidates). Combined with `quality-core.md` (167 ln) + `product-tech-strategy.md` (52 ln) + `workflow-intent.md` (39 ln) + `CLAUDE.md` (191 ln) + `rules.md` (142 ln), the always-loaded baseline is 824 lines / ~54 KB. gstack/Superpowers/GSD all keep a tighter baseline via preamble tiering, skill progressive disclosure, or phase-specific manifests.

3. **Cross-session memory is the biggest missing capability** — OCX currently has zero persistent learning store (per `audit_ai_config_ocx.md` Gaps vs SOTA). gstack's JSONL learnings pattern (per `research_ai_config_gstack.md` Pattern 3) and Trail of Bits' `/insights` weekly review (per `research_ai_config_sota.md` OSS §2) both validate this as a convergent gap. Anthropic's auto-memory already provides the storage substrate (`~/.claude/projects/.../memory/MEMORY.md` per `research_ai_config_sota.md` Anthropic §Memory); OCX just needs to write to it systematically.

4. **Convergent gap: agents are context-blind** — GSD's context-monitor bridge file + PostToolUse injection (per `research_ai_config_get-shit-done.md` Pattern 1) is a 150-line hook that gives Claude advisory warnings at 35%/25% context thresholds. OCX already has a statusLine hook — extending it is trivial. Without it, OCX agents discover context exhaustion only when compaction fires, typically mid-task.

5. **OCX's path-scoped rule system IS the SOTA baseline** — Per `research_ai_config_sota.md` Convergent Patterns, every major config converges on path/glob-scoped rules. OCX's `subsystem-*.md` pattern is directly aligned with Anthropic official guidance, Cursor, Continue.dev, and all reference repos. This is a strength to preserve, not a gap to close.

## 1. Cross-Repo Comparison Matrix

Cells: **P**=Present, **A**=Absent, **~**=Partial. Citations in notes.

| Pattern | gstack | superpowers | gsd | SOTA mainstream | OCX current |
|---|---|---|---|---|---|
| Path-scoped rules (glob frontmatter) | A (uses preamble tiering instead) | A (no rules, skill-only) | A (config-key based) | P (Anthropic `paths:`, Cursor `globs:`, Continue `globs:`) | P (27 scoped rules) |
| Skill progressive disclosure (description → body → supporting) | P (templates + separate reference files) | P (references/ dirs per skill) | P (workflows file refs, thin shells) | P (Anthropic spec) | ~ (swarm skills tiered; `ocx-create-mirror` 332-line monolith) |
| CSO discipline (description = trigger only) | A (descriptions summarize workflow) | P (empirically tested) | A (descriptions summarize) | ~ (Anthropic says "what+when"; Superpowers contradicts with evidence) | Unknown — likely violated (17 skills, audit not done) |
| Multi-host templating / canonical-source sync | P (39 `.tmpl` → `.md`; 10 hosts) | ~ (no templates; INSTALL.md per host + symlinks) | P (npm install + runtime adaptation for 10 runtimes) | P (agentskills.io cross-platform standard) | A (single-host Claude Code) |
| Session-start context injection via hook | A (none) | A (uses skill description matching) | P (SessionStart + statusLine) | P (SessionStart hook supported) | P (`session_start_loader.py` 67 ln) |
| Context monitor infrastructure (bridge file + PostToolUse injection) | A | A | **P** (193-ln hook; 35%/25% thresholds; /tmp bridge) | Partial — docs mention context rot; no standard mechanism | A |
| Role-based model profiles (quality/balanced/budget) | ~ (fixed preamble tiers; no role axis) | A (no model routing) | **P** (config-level profile × agent role → model) | ~ (everything-claude-code has NanoClaw v2) | ~ (tier overlays in `overlays.md`; models hardcoded in agent files) |
| Tier dispatch (complexity-based) | A | A | A | A | **P** (swarm skills low/auto/high/max) |
| Preamble tiering (shared blocks injected by tier) | **P** (T1-T4; 856-ln resolver) | A | A | A | A |
| Parallel specialist review (JSON findings + confidence gating) | **P** (Review Army; 2-7 specialists; JSON with fingerprint dedup + confidence score) | ~ (two-stage review, spec then quality; not JSON) | P (gsd-plan-checker 8-dim verification; read-only) | P (Anthropic agent teams; Trail of Bits layered checkers) | ~ (swarm-review has perspectives; single reviewer; no JSON schema; no confidence) |
| Cross-session learnings store (JSONL + schema) | **P** (`~/.gstack/projects/{slug}/learnings.jsonl`; typed schema; confidence) | A | ~ (`features.global_learnings` opt-in; wave SUMMARY.md) | P (Anthropic auto-memory at `~/.claude/projects/.../memory/`; everything-claude-code instinct v2) | A (audit confirms: "every session starts cold") |
| Rationalization tables / Red Flags / Iron Law framing | ~ (some red-flag lists in specialist checklists) | **P** (every discipline skill; Meincke 2025 evidence) | A | A | A (`quality-core.md` has anti-pattern tiers but no rationalization table; `workflow-bugfix.md` has phases but no Red Flags section) |
| Subagent context isolation (4-status-code reports) | ~ (specialist JSON findings) | **P** (DONE / DONE_WITH_CONCERNS / NEEDS_CONTEXT / BLOCKED) | P (wave-based; fresh 200K per worker) | P (Anthropic "bag of agents" anti-pattern research) | ~ (workers use `context: fork`; no standardized 4-code report) |
| "Absence = enabled" config philosophy | A | A | **P** (`workflow.*` absent-enabled; `features.*` absent-disabled) | A | A |
| Defense-in-depth checker agents (read-only) | ~ (specialists are advisory) | A | **P** (gsd-plan-checker / gsd-integration-checker / gsd-verifier / gsd-security-auditor; no Write/Edit perms) | P (Trail of Bits layered security) | ~ (worker-reviewer/worker-doc-reviewer are advisory; tools include Bash) |
| Prompt injection guards (write + read-time) | A | A | **P** (gsd-prompt-guard 97 ln + gsd-read-injection-scanner 130 ln with summarization-survival patterns) | P (Trail of Bits) | ~ (`pre_tool_use_validator.py` covers secrets; no injection scanning; no read-time scanner) |
| UserPromptSubmit skill routing hook | A | A | A | P (ChrisWiles claude-code-showcase) | A |
| Eval store / LLM-judge for skill quality | **P** (Tier 3 LLM-as-judge scoring; `evals.yml` + `evals-periodic.yml`; ~$0.15 per run) | ~ (skill TDD via subagent pressure testing, not scored) | A | ~ (emerging; no standard) | A (1512 lines of structural tests; zero semantic tests) |
| Wave-based execution with fresh context per wave | A | ~ (subagent-per-task) | **P** (dependency DAG → parallel Task() spawns; fresh 200K each; `.lock` file coordination) | ~ (Anthropic agent teams experimental) | A (workers share session; no wave boundary) |
| Diff-based test tier selection | **P** (`test/helpers/touchfiles.ts`; gate vs periodic tiers) | A | A | A | A (`task verify` runs full suite) |
| Skill TDD (pressure test before deploy) | ~ (has evals) | **P** (RED-GREEN-REFACTOR; subagent pressure scenarios; Meincke-2025 psychology) | A | A | A |
| Token ceiling at compile time | **P** (`TOKEN_CEILING_BYTES = 100_000`) | A (skill body size is guidance only) | P (agent size budget test: XL=1600, LARGE=1000, DEFAULT=500) | P (Anthropic guidance: <200 CLAUDE.md, <500 skill body) | ~ (`meta-ai-config.md` documents budgets; no enforcement test) |
| No-placeholder plan discipline | ~ (specialist checklists) | **P** (explicit "plan failures" list; self-review) | P (thin-orchestrator pattern enforces file manifests) | A | ~ (templates exist; no placeholder scan) |
| Agent skill injection at spawn | A | A | **P** (`agent_skills` config map; XML block prepended to Task() prompt) | A | A (workers inherit project-level skills via `.claude/` discovery only) |
| INVOKE_SKILL composition (on-demand child loading) | **P** (`{{INVOKE_SKILL:name}}` resolver) | P (`superpowers:skill-name` cross-refs) | P (workflow `@file` refs) | P (Anthropic `@path` imports + skill invocation) | ~ (`@.claude/rules/*` imports; no in-skill composition) |

## 2. Convergent Patterns (≥2 repos + SOTA alignment)

Ranked by confidence × OCX payoff.

| Rank | Pattern | Repos | SOTA alignment | OCX payoff |
|---|---|---|---|---|
| 1 | **Path-scoped rules** | superpowers uses paths implicitly via skill descriptions; gstack via preamble tiers; gsd via file manifests | Universal (Anthropic, Cursor, Continue) | **Already adopted** — validate and preserve |
| 2 | **Skill progressive disclosure** | All three | Anthropic spec | High — `ocx-create-mirror` (332 ln) is the last monolith; extract reference sections |
| 3 | **CSO description discipline** | Superpowers (with evidence) | Contradicts Anthropic official but has empirical backing; no opposing evidence | **Highest ROI** — 30-min audit, affects every skill invocation |
| 4 | **Cross-session learnings store** | gstack (JSONL), gsd (partial), Trail of Bits (`/insights`), Anthropic (auto-memory) | Anthropic auto-memory standard | High — OCX already has auto-memory substrate; just needs write discipline |
| 5 | **Rationalization tables / Red Flags** | Superpowers (evidence), gstack (partial) | Meincke et al. 2025 (33%→72% compliance improvement) | High — low token cost, affects discipline-enforcing rules |
| 6 | **No-placeholder plan discipline** | Superpowers (strict), gstack (checklists), gsd (file manifests) | Aligns with "bag of agents" research (focused briefs not full history) | Medium — directly applies to `plan.template.md` |
| 7 | **Parallel specialist review with structured findings** | gstack (Review Army + JSON + confidence), gsd (4 read-only checkers) | Anthropic agent teams; Trail of Bits layered checkers | Medium — OCX swarm-review already has perspectives; add JSON schema + confidence |
| 8 | **Subagent context isolation protocol** | Superpowers (4-status codes), gstack (JSON findings), gsd (fresh context per wave) | "Bag of agents" research | Medium — OCX has `context: fork`; formalize the 4-status-code report |
| 9 | **Context budget discipline (compile-time ceiling)** | gstack (100KB ceiling), gsd (agent size budget test), Anthropic (<200 CLAUDE.md) | Universal | Medium — add structural test enforcing per-file line budgets |
| 10 | **Defense-in-depth read-only checkers** | gsd (4 agents with no Write/Edit), Trail of Bits (hooks+skills+deny rules) | Universal for security-serious configs | Medium — restrict `worker-reviewer`/`worker-doc-reviewer` tools |

## 3. Divergent Patterns (OCX Must Pick a Side)

| Axis | Side A | Side B | Evidence | OCX should pick | Rationale |
|---|---|---|---|---|---|
| **Description philosophy** | "what it does + when to use" (Anthropic official) | "trigger conditions only, never summarize workflow" (Superpowers) | Superpowers has eval evidence of skill bypass; Anthropic docs do not address this failure mode | **Side B (CSO)** | Empirical evidence beats documentation when they conflict; affects every skill invocation; reversible if wrong |
| **Hook enforcement posture** | Zero hooks, pure context engineering (Superpowers) | Heavy hook stack (gsd: 7 JS + 3 shell; gstack: compile-time validation) | Anthropic: hooks are the only deterministic enforcement layer | **Middle — deterministic hooks only** | Keep current 8 OCX hooks as deterministic gates; do NOT migrate advisory rules to hooks (Superpowers is right that behavioral enforcement via hooks is maintenance debt); add GSD's context monitor as the one exception |
| **Multi-host abstraction** | Full templating pipeline (gstack Bun/TS, gsd npm install-time adaptation) | Single-host only (Superpowers symlinks, OCX current) | OCX is Claude Code-only per architectural constraint | **Side B (single-host)** | Multi-host is pure overhead for OCX; patterns transfer via manual copy if a second host is ever added |
| **Session-start context mechanism** | Hook-based (gsd SessionStart + statusLine) | Skill-description-matching (Superpowers `using-superpowers`) | Both work; hooks are deterministic, skills are cheaper | **Hybrid** | Keep `session_start_loader.py` for deterministic swarm handoff; add description-triggered skill for broad priming if needed |
| **Skill invocation default** | Manual-only (`disable-model-invocation: true` on everything per Trail of Bits) | LLM-auto for knowledge skills (most configs) | Description budget pressure above 50 skills | **Hybrid (current OCX pattern)** | Action skills (commit, deploy, mirror) = manual; knowledge skills (architect, builder, qa-engineer) = auto; matches current OCX state |
| **Canonical source + sync vs. single repo** | Canonical source + deploy (gstack templates, gsd npm) | Single committed directory (Superpowers, OCX) | OCX has no distribution requirement | **Side B (single committed)** | OCX's `.claude/` is per-project; no consumers; canonical-source overhead unjustified |
| **Tiering axis: complexity vs. role** | Complexity tier (OCX low/auto/high/max) | Role tier (gsd planner=opus, executor=sonnet, mapper=haiku) | Orthogonal dimensions; can be combined | **Both (combine)** | OCX already has complexity tier; add role dimension to formalize existing hardcoded model assignments (worker-architect=opus, worker-explorer=haiku, etc.) |
| **Memory primary** | Auto-generated (Windsurf Memories) | Human-authored first (Anthropic CLAUDE.md) | Anthropic is the platform OCX runs on | **Human-authored primary** | OCX already follows Anthropic model; auto-memory is additive (MEMORY.md enhancement, not replacement) |

## 4. Pattern Adoption Matrix

Each pattern: source → OCX current state → payoff → cost → reversibility → phase.

| Pattern | Source | OCX current | Payoff | Cost | Reversibility | Phase |
|---|---|---|---|---|---|---|
| CSO description audit (all skills) | superpowers | 17 skills, not audited | **High** — fixes skill-bypass on every invocation; zero token cost | Trivial (30 min) | Two-Way Door | Quick win |
| Fix `workflow-bugfix.md` + `workflow-refactor.md` paths | audit | No `paths:` → loads every session | **High** — removes 233 lines from always-loaded baseline (~28% reduction) | Trivial (5 min) | Two-Way Door | Quick win |
| Remove dead `skill-rules.json` reference in CLAUDE.md L186 | audit | Dead reference | Low — fixes broken doc | Trivial | Two-Way Door | Quick win |
| Rationalization tables on discipline rules | superpowers | None | **High** — Meincke 33%→72% compliance | Low (5 tables × 5 rows) | Two-Way Door | Quick win |
| Iron Law framing on non-negotiable gates | superpowers | Inconsistent imperatives | Medium — strengthens commonly-skipped gates | Low (2-3 lines per rule) | Two-Way Door | Quick win |
| No-placeholder mandate in plan templates | superpowers | Templates exist; no scan | Medium — directly applies to swarm plans | Low | Two-Way Door | Quick win |
| Context monitor (bridge + PostToolUse) | gsd | None — agents context-blind | **High** — 150-line hook; statusLine already exists | Low-Medium | Two-Way Door | Quick win |
| Progressive disclosure for `ocx-create-mirror` (332 ln) | gstack/Anthropic | Monolithic | Medium — aligns with swarm-skill pattern | Medium | Two-Way Door | Structural |
| Cross-session learnings JSONL store | gstack + Anthropic auto-memory | None (zero session memory) | **High** — recurring `oci-client` quirks, clippy suppressions compound | Medium (hook epilog + schema) | One-Way Door Medium | Structural |
| Structural test for per-file line budgets | gstack (100KB), gsd (size test) | Documented, not enforced | Medium — prevents future bloat | Low (add to `test_ai_config.py`) | Two-Way Door | Structural |
| Structural test for path-scope overlaps | audit | workflow-git/release overlap; quality-security/subsystem-ci overlap | Medium — prevents double-loading regressions | Low | Two-Way Door | Structural |
| PostToolUse Markdown tracking (close `.md` skip gap) | audit | `.md` files excluded | Low — config changes invisible to tracker | Low | Two-Way Door | Structural |
| Four-status-code subagent report protocol | superpowers | Workers report free-form | Medium — orchestration clarity | Low (document + template) | Two-Way Door | Structural |
| Role-based model profile formalization | gsd | Tier overlays + hardcoded agent models | Medium — enables per-role overrides without editing 9 agent files | Medium | One-Way Door Medium | Structural |
| Read-only tool restriction on reviewer agents | gsd | `worker-reviewer` has Bash | Medium — prevents reviewer masking problems by fixing them | Low | Two-Way Door | Structural |
| JSON findings schema + confidence gating for review | gstack | Free-form findings | Medium — structured review output; dedup via fingerprint | Medium | Two-Way Door | Structural |
| Prompt injection guards (write + read-time) | gsd | `pre_tool_use_validator` covers secrets only | Medium — protects `.claude/artifacts/` from injection | Medium (2 hooks, ~250 lines total) | Two-Way Door | Structural |
| UserPromptSubmit skill routing | ChrisWiles/SOTA | None | Medium — complements description-based matching; frees description budget | Medium | Two-Way Door | Structural |
| "Absence = enabled" doc convention | gsd | Not documented | Low — clarity only; no code change | Trivial | Two-Way Door | Quick win |
| Wave-based parallel execution with fresh context | gsd | Workers share session | Medium-High — eliminates context rot architecturally | High (DAG analysis; lock file; Task() spawn redesign) | One-Way Door High | Aspirational |
| Diff-based test tier selection | gstack | `task verify` runs full | Medium — CI cost reduction | High (touchfile mapping for pytest tests) | Two-Way Door | Aspirational |
| LLM-as-judge for doc/skill quality | gstack | 1512 lines structural; zero semantic | Medium — catches semantic regressions | High (eval harness + cost budget) | One-Way Door Medium | Aspirational |
| Skill TDD (pressure test before deploy) | superpowers | Ad-hoc | Low immediate / High long-term | High (per-skill test scaffolding) | Two-Way Door | Aspirational |
| Two-stage review (spec then quality) | superpowers | Single reviewer pass | Low-Medium — tier=max only | Medium | Two-Way Door | Aspirational |
| Preamble tiering (T1-T4 shared blocks) | gstack | Copy-paste across skills | Medium — if OCX ever builds a compilation step | Very High (resolver pipeline) | One-Way Door High | **Skip** (wrong tool for single-host) |
| Multi-host templating | gstack/gsd | Single-host | Zero | Very High | — | **Skip** |
| Persona/brand voice in preamble | gstack | Neutral senior-engineer | Zero | — | — | **Skip** |
| Full GSD pipeline (discuss→plan→execute→verify→UAT) | gsd | Swarm workflow exists | Zero | Very High | — | **Skip** — OCX has equivalent |
| Read-before-edit advisory hook | gsd | Claude Code native enforcement | Zero | — | — | **Skip** |
| Graphviz DOT flowcharts | superpowers | Markdown tables | Zero — Claude Code doesn't render DOT | — | — | **Skip** (adopt principle, not format) |

## 5. Taxonomy of AI Config Concerns

Eleven concerns every mature AI config must address. For each: canonical mechanism + OCX state (gap / partial / strong).

| # | Concern | Canonical mechanism | OCX mechanism | State |
|---|---|---|---|---|
| 1 | **Discovery** — how does the agent find the right rule/skill? | Path-scoped auto-load (rules) + description-matching (skills) + catalog (planning) | `paths:` frontmatter + skill descriptions + `rules.md` catalog | **Strong** — SOTA-aligned |
| 2 | **Loading** — when does content enter context? | Progressive disclosure: metadata always / body on invocation / supporting on demand | Skill descriptions always; SKILL.md on invocation; subsystem rules on path match | **Strong** — swarm skills use tier files; `ocx-create-mirror` is the last monolithic gap |
| 3 | **Enforcement** — advisory vs deterministic | Rules = advisory; Hooks = deterministic; Skills = guided workflow | 8 hooks (4 blocking, 4 non-blocking); rules; skills | **Strong** — hooks cover the deterministic layer |
| 4 | **Composition** — how do skills/rules reference each other? | `@file` imports (force-load) vs skill-name refs (on-demand) | `@` imports in CLAUDE.md; `[[link]]` in rules; swarm skill → tier file | **Partial** — no in-skill INVOKE pattern; cross-refs work but aren't lazy where they should be |
| 5 | **Memory** — cross-session state | Anthropic auto-memory `~/.claude/projects/.../memory/MEMORY.md` + first-party write discipline | MEMORY.md exists (user preferences persisted) | **Partial** — substrate exists; no systematic write pattern for project-level learnings; no schema |
| 6 | **Review** — quality feedback loop | Review-Fix Loop with perspectives + optional cross-model adversarial | `/swarm-review` with perspectives; optional Codex adversarial; no JSON schema; no confidence score | **Partial** — functional but lacks structured findings and confidence gating |
| 7 | **Context budget** — per-session byte discipline | <200 lines CLAUDE.md; <200 per global rule; <500 per skill body; compile-time ceiling | Documented in `meta-ai-config.md`; not enforced; `workflow-bugfix/refactor` violate global budget | **Partial** — documentation without enforcement; 824-line always-loaded baseline |
| 8 | **Multi-host** — one-config-many-tools | Template compilation (gstack) or install-time adaptation (gsd) or agentskills.io standard | Claude Code only | **N/A for OCX** — skip by design |
| 9 | **Security** — prompt injection, secrets | `pre_tool_use` hooks + read-time scanners + deny rules + sandbox | `pre_tool_use_validator.py` (secrets only); no read-time scanner; no injection guard | **Partial** — secrets covered; injection vectors uncovered |
| 10 | **Eval** — quantitative skill quality | LLM-as-judge + pressure testing + structural tests | 1512 lines of structural tests (`test_ai_config.py` 1014 + `test_hooks.py` 498); zero semantic tests | **Gap** — structural strong, semantic absent |
| 11 | **Cost** — model tier routing | Role-based profile (gsd) × task-complexity tier (OCX) × explicit overrides | Task-complexity tier (low/auto/high/max); model names hardcoded in 9 agent files | **Partial** — complexity dimension is strong; role dimension is implicit and scattered |

## Citation Summary

All claims cite primary artifacts. Key recurring references:

- gstack preamble tiering: per `research_ai_config_gstack.md` §Pattern 1
- gstack learnings JSONL: per `research_ai_config_gstack.md` §Pattern 3
- gstack Review Army: per `research_ai_config_gstack.md` §Pattern 2
- Superpowers CSO: per `research_ai_config_superpowers.md` §Pattern 1
- Superpowers rationalization tables: per `research_ai_config_superpowers.md` §Pattern 2
- Superpowers 4-status-code subagent protocol: per `research_ai_config_superpowers.md` §Pattern 4
- GSD context monitor: per `research_ai_config_get-shit-done.md` §Pattern 1
- GSD model profiles: per `research_ai_config_get-shit-done.md` §Pattern 2
- GSD absence=enabled: per `research_ai_config_get-shit-done.md` §"Absence = Enabled"
- GSD prompt injection guards: per `research_ai_config_get-shit-done.md` §Pattern 5
- Anthropic auto-memory: per `research_ai_config_sota.md` §Anthropic Memory
- Anthropic skill description budget (1%): per `research_ai_config_sota.md` §Skill Description Budget
- Anthropic cache TTL regression: per `research_ai_config_sota.md` §Prompt Caching
- Trail of Bits `/insights`: per `research_ai_config_sota.md` §OSS Reference 2
- ChrisWiles UserPromptSubmit: per `research_ai_config_sota.md` §OSS Reference 3
- OCX 824-line baseline: per `audit_ai_config_ocx.md` §Headline Numbers + §Always-Loaded Baseline
- OCX mislabeled catalog-only rules: per `audit_ai_config_ocx.md` §Staleness Candidates
- OCX dead `skill-rules.json` reference: per `audit_ai_config_ocx.md` §Staleness Candidates
- OCX duplication cluster #1 (Review-Fix Loop 7+ locations): per `audit_ai_config_ocx.md` §Duplication Scan
- OCX gaps (wave execution, context monitor, eval store): per `audit_ai_config_ocx.md` §Gaps vs SOTA

---

**Completion summary**: The top three convergent patterns OCX should adopt are (1) **CSO description audit** across all 17 skills — trivial cost, highest per-skill-invocation ROI, backed by Superpowers' empirical evidence; (2) **Cross-session learnings store** using Anthropic's existing auto-memory substrate — OCX has zero session memory today, and both gstack and Trail of Bits validate this as a convergent gap; (3) **Context monitor hook** (bridge file + PostToolUse injection) — GSD's 150-line pattern gives OCX's currently context-blind agents advisory warnings at 35%/25% thresholds, and OCX's existing statusLine hook is the perfect substrate. The top divergent axis OCX must pick a side on is **description philosophy** — Anthropic official guidance ("description = what + when") vs Superpowers' empirically-evidenced CSO discipline ("description = trigger conditions only"). OCX should pick CSO because Superpowers has eval evidence of skill bypass that Anthropic docs do not address; this decision affects every skill invocation and is reversible in minutes if wrong.
