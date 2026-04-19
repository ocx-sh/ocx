# ADR: Cross-session learnings store — typed JSONL + hook-integrated capture

## Metadata

**Status:** Proposed (revised Round 2 — relocated canonical store from `~/.claude/projects/<slug>/memory/` to project-local `.claude/state/learnings.jsonl`)
**Date:** 2026-04-19
**Deciders:** AI config overhaul (automated planning session)
**Related Plan:** `plan_ai_config_overhaul.md` (Phase 4)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path: Python stdlib + JSONL; leverages Anthropic auto-memory substrate; no new dependencies
**Domain Tags:** data (schema), infrastructure (hook integration), devops (AI config)
**Supersedes:** Current implicit practice (manual `MEMORY.md` edits when human notices a pattern)
**Superseded By:** N/A

## Context

Per `audit_ai_config_ocx.md` §Gaps vs SOTA, OCX has no cross-session learnings store. Every session starts cold with respect to project-level patterns (recurring `oci-client` quirks, clippy suppressions that need re-justifying, test flakiness patterns, mirror-registry quirks). Manual `MEMORY.md` edits capture user *preferences*, but the project's *technical* recurring patterns are only captured if a human remembers to write them down.

Per `research_ai_config_gstack.md` §Pattern 3, gstack operates a JSONL learnings store at `~/.gstack/projects/{slug}/learnings.jsonl` with a typed schema (fingerprint for dedup, confidence score, category). Per `research_ai_config_sota.md` §Memory and §OSS Reference 2 (Trail of Bits `/insights`), the convergent SOTA approach is to capture project-level technical patterns as distinct from user preferences, and periodically (weekly) promote the stable ones to first-class config (rules, skills, or `MEMORY.md`).

Per synthesis §Headline Finding 3, "OCX already has auto-memory substrate; it just needs write discipline." Anthropic's auto-memory directory `~/.claude/projects/<slug>/memory/` is loaded at every session start (first 200 lines / 25KB of `MEMORY.md`). The substrate is already there; what is missing is a disciplined write pathway.

This is a one-way door at the medium level because (a) the schema, once in production, is inherited by every future session's hook, and (b) TTL and confidence-decay policies, once set, quietly shape which patterns survive. Getting the schema wrong the first time means rewriting entries.

## Decision Drivers

- Exploit existing Anthropic auto-memory substrate — do not invent a parallel store
- Human-authored `MEMORY.md` remains primary per synthesis §3 Divergent Axis 7
- Auto-capture happens via extension of existing hooks — no new hook scripts
- Schema must support dedup, TTL, and confidence decay to prevent bloat
- Policy decisions (TTL length, decay formula) must be explicit, not implicit

## Industry Context & Research

**Research artifact:** `research_ai_config_gstack.md` §Pattern 3; `research_ai_config_sota.md` §Memory + §OSS Reference 2; `research_ai_config_synthesis.md` §Headline Finding 3.

**Trending approach:** Typed JSONL with fingerprint dedup is the emerging standard. Trail of Bits' `/insights` formalizes weekly promotion. Anthropic's auto-memory is the machine substrate.

**Key insight:** Keeping the auto-captured store *separate* from the human-authored `MEMORY.md` is the critical architectural decision. If auto-promotion writes directly to `MEMORY.md`, every noisy pattern pollutes the human surface. Keeping them separate lets `MEMORY.md` stay a curated, high-signal file.

## Revision Note (Round 2 adversarial review)

The original decision — canonical store at `~/.claude/projects/<slug>/memory/learnings.jsonl`, alongside the Anthropic auto-memory `MEMORY.md` — was found to have three under-examined risks (architect Block B3):

1. **Anthropic owns the directory semantics.** Future auto-memory changes could start loading all files in the directory, bleeding machine-written JSONL into context (or orphan the file if the directory is renamed/restructured).
2. **Cross-worktree merge race.** OCX runs 4 worktrees on one machine. All four would write to the shared store on session stop; concurrent `stop_validator.py` runs race on the file.
3. **Thresholds imported from gstack without OCX tuning.** `occurrence_count ≥ 3`, `confidence ≥ 0.7`, decay `-0.02/day` are gstack numbers unmoored from OCX session cadence. At the decay rate, a pattern that surfaces once per sprint decays below the floor before reaching the 3-occurrence threshold.

**Revised decision — project-local store:**

- **Canonical location:** `.claude/state/learnings.jsonl` (per-worktree, gitignored per existing `.state/` convention)
- **Trade-off accepted:** each of OCX's 4 worktrees accumulates its own learnings. Worktrees work on different branches; cross-worktree sharing would be a noise source, not a feature.
- **Isolation from Anthropic substrate:** no file is written under `~/.claude/projects/<slug>/memory/` by this ADR. Anthropic's directory remains human-authored only. When Anthropic changes substrate semantics, OCX is unaffected.
- **Concurrency:** writes guarded by `hook_utils.py` StateManager file lock (already in use for other `.state/` artifacts).
- **Thresholds:** ship Phase 4 in **logging-only mode** for the first 30 days. No auto-promotion to any prominent surface. At day 30, review the corpus and tune thresholds from real data. Document this staging explicitly.
- **Schema migration:** add explicit rule to this ADR — field additions only; renames or removals require a one-shot migration script committed with the schema change. Include a `schema_version` field (starts at `1`); a parity test asserts the schema module's declared version matches the version in each JSONL record.

## Considered Options

### Option 1: JSONL at `~/.claude/projects/<slug>/memory/learnings.jsonl` (Anthropic substrate, parallel file)

**Description:** Auto-captured learnings written to `learnings.jsonl` alongside the human-authored `MEMORY.md`, both under Anthropic's auto-memory directory. Anthropic loads `MEMORY.md` at session start; `learnings.jsonl` is not auto-loaded but can be referenced by hooks or skills.

| Pros | Cons |
|---|---|
| Reuses Anthropic substrate — shared across worktrees automatically | Not auto-loaded into context; requires explicit read by `session_start_loader.py` if wanted at session start |
| Clean separation from human-authored file | Platform-specific directory path (Linux vs macOS vs WSL) |

### Option 2: JSONL at `.claude/state/learnings.jsonl` (project-local)

**Description:** Project-local store committed via `.state/` conventions.

| Pros | Cons |
|---|---|
| Simple; in-repo | Loses cross-worktree sharing |
| | Every contributor accumulates distinct entries; dedup becomes team-level concern |

### Option 3: Emit learnings as comments inside `MEMORY.md` (single-file approach)

**Description:** Append `<!-- LEARNING: ... -->` HTML comments (stripped by Anthropic per SOTA §Memory) to the human-authored file.

| Pros | Cons |
|---|---|
| No new file; zero context cost (comments stripped) | Pollutes the human file with machine-written content |
| | Loses schema — parsing comments back is fragile |

### Option 4: Skip structured store — rely on human noticing patterns

**Description:** Status quo.

| Pros | Cons |
|---|---|
| Zero cost | Every session starts cold — convergent SOTA gap per synthesis |

## Decision Outcome

**Chosen Option (revised Round 2):** Option 2 — JSONL at `.claude/state/learnings.jsonl` (project-local, per-worktree).

**Rationale:** The original Option 1 (Anthropic substrate) was rejected in Round 2 adversarial review for three compounding reasons (see Revision Note above): (a) Anthropic owns the directory semantics, (b) cross-worktree merge races on a shared file, (c) threshold numbers imported from gstack were unmoored from OCX session cadence. Option 2 — project-local `.claude/state/learnings.jsonl` — trades cross-worktree sharing (which was a noise source anyway, since OCX worktrees work on different branches) for isolation from Anthropic's directory semantics and deterministic single-writer concurrency within a worktree. Option 3 would pollute the human-curated `MEMORY.md`. Option 4 leaves the gap open. Cross-worktree learnings can later be consolidated manually if a pattern emerges across worktrees.

### Schema (v1)

```json
{
  "schema_version": 1,
  "id": "uuid-v4",
  "created_at": "2026-04-19T10:00:00Z",
  "source_agent": "worker-reviewer",
  "source_session": "abc123",
  "category": "rust|python|ts|oci|test|clippy|mirror|build|other",
  "fingerprint": "sha256(category|normalized_summary)",
  "summary": "short human-readable description, <=160 chars",
  "evidence_ref": ".claude/artifacts/... or commit sha or file:line",
  "confidence": 0.0,
  "confidence_updated_at": "2026-04-19T10:00:00Z",
  "ttl_days": 90,
  "occurrence_count": 1
}
```

**Schema migration rules:**
- Field **additions** are permitted without a migration; old records remain valid, new records carry the extra field.
- Field **renames or removals** require a one-shot migration script committed in the same change, plus a `schema_version` bump.
- `schema_version` starts at `1`. A parity test asserts the `LearningsStore` code's declared version equals the `schema_version` in every record at read time; mismatched records are quarantined to `.claude/state/learnings-orphan.jsonl` for manual review, never silently dropped.

### Cleanup Policy

Run in `stop_validator.py` on every session end:

1. Drop entries older than `created_at + ttl_days`
2. Drop entries with `confidence < 0.3`
3. Merge entries sharing `fingerprint` — increment `occurrence_count`, update `confidence_updated_at`
4. Apply confidence decay: `confidence -= 0.02` per day since `confidence_updated_at` (floor 0.0)
5. Confidence replenishment: a new emission matching an existing fingerprint sets `confidence = min(1.0, confidence + 0.15)` and updates timestamps

### Promotion Policy — staged rollout

**Stage 1 (first 30 days after Phase 4 lands):** logging-only mode. The store accumulates records; `stop_validator.py` writes a quiet one-line summary ("N learnings captured, M unique fingerprints") but does **not** emit promotion candidates. No auto-promotion to any prominent surface. At day 30, the human reviews `.claude/state/learnings.jsonl` and tunes the thresholds below based on observed distribution.

**Stage 2 (after threshold tuning):** when a fingerprint's `occurrence_count ≥ N` AND `confidence ≥ C` (N and C are tuned in Stage 1; defaults are the gstack values `N=3`, `C=0.7` pending evidence), `stop_validator.py` emits a prominent `[LEARNING PROMOTION CANDIDATE]` block at session end with the summary and evidence_ref. The human decides whether to copy it to `MEMORY.md`. **No auto-promotion ever** — promotion to `MEMORY.md` is always a human action.

### Capture Pathway

1. Subagent produces a structured finding → includes a `[LEARNING]` JSON block in its output
2. `subagent_stop_logger.py` parses the block and appends to `.state/learnings-pending.jsonl`
3. At session end, `stop_validator.py` merges `.state/learnings-pending.jsonl` into **`.claude/state/learnings.jsonl`** (project-local), applying dedup + cleanup under file-lock protection via `hook_utils.py` StateManager

### Consequences

**Positive:**
- Recurring patterns captured without blocking on human noticing
- Schema + TTL + decay prevents bloat
- Cross-worktree sharing via Anthropic substrate
- Human-authored `MEMORY.md` preserved as curated surface

**Negative:**
- Subagents must include `[LEARNING]` blocks — requires updating worker prompt templates
- Schema migrations have cost; first version must be stable

**Risks:**
- **Store bloat.** Mitigation: TTL + confidence decay + fingerprint dedup (schema-enforced).
- **Low-signal noise.** Mitigation: minimum confidence threshold (0.3) before entries are kept; promotion requires 0.7.
- **Schema breakage on revision.** Mitigation: `version` field reserved for future additive changes; never remove fields.
- **Privacy/secrets in summary field.** Mitigation: `pre_tool_use_validator.py` already scans Write/Edit for secrets; extend `LearningsStore.append` to re-run the same secret regex before writing.

## Implementation Plan

1. [ ] Extend `hook_utils.py` with `LearningsStore` dataclass (`append`, `prune`, `promote`, `fingerprint`) including `SCHEMA_VERSION = 1` constant and `quarantine_if_mismatched(record)` helper
2. [ ] Extend `subagent_stop_logger.py` to scan subagent output for `[LEARNING]` JSON blocks and append to `.state/learnings-pending.jsonl`
3. [ ] Extend `stop_validator.py` to merge `.state/learnings-pending.jsonl` into `.claude/state/learnings.jsonl` under StateManager file-lock, apply cleanup policy (Stage 1 = logging-only summary; Stage 2 = promotion candidates after threshold tuning)
4. [ ] Document the schema + migration rules + staged-rollout policy in `meta-ai-config.md` under a new "Cross-Session Learnings" section
5. [ ] Update worker prompt templates (in `.claude/agents/worker-*.md`) with an optional final step: "If you observed a recurring pattern during this task, emit a `[LEARNING]` JSON block at the end of your output."
6. [ ] Unit tests in `test_hooks.py`: schema_version validation, quarantine on mismatch, TTL pruning, fingerprint dedup, confidence decay, secret-redaction on append. **No promotion-threshold test in Stage 1** (promotion is off during logging-only period).
7. [ ] Integration test: fake `[LEARNING]` marker → verify deterministic path (record persisted, dedup applied, schema_version=1 present on disk). Acceptance for M10 is this integration test, not a heuristic session count.
8. [ ] Create `.claude/state/.day30-review-reminder` sentinel file dated 30 days after Phase 4 merge. A `SessionStart` hook or manual calendar prompts the human to review `.claude/state/learnings.jsonl` and tune N/C thresholds for Stage 2.

## Validation

- [ ] `test_hooks.py` passes including new `LearningsStore` tests
- [ ] **Deterministic integration test**: a fake `[LEARNING]` JSON marker flows subagent_stop_logger → stop_validator → `.claude/state/learnings.jsonl`, exactly one record is persisted with `schema_version=1`, duplicate markers are deduped by fingerprint. This replaces the prior heuristic "≥1 learning per session" check which depended on reviewer judgment.
- [ ] `MEMORY.md` remains untouched by hook-driven writes (only human-written changes) — asserted by a file-mtime check in the integration test
- [ ] `.claude/state/learnings.jsonl` path is gitignored (asserted by a `test_gitignore_contains_learnings_jsonl` structural test)
- [ ] Schema-mismatch quarantine test: a record with `schema_version=999` is moved to `learnings-orphan.jsonl`, not silently dropped, not merged into the canonical store
- [ ] Secret-redaction test: a learning containing a fake API key is not written to the store
- [ ] Stage 2 promotion test is **deferred** until threshold tuning is complete (after the 30-day logging-only window)

## Links

- `plan_ai_config_overhaul.md` Phase 4
- `research_ai_config_gstack.md` §Pattern 3
- `research_ai_config_sota.md` §Memory + §OSS Reference 2 (Trail of Bits `/insights`)
- `research_ai_config_synthesis.md` §Headline Finding 3 + §3 Divergent Axis 7

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-19 | AI config overhaul planning | Initial draft |
