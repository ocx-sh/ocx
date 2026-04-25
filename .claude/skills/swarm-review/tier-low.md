# Tier: low — /swarm-review

Min-effort review for small diffs: flag/option changes, doc edits, test fixtures, single-subsystem tweaks ≤3 files. Adversarial anchors still fire (protocol part of skill identity) but parallel perspective panel shrink to one reviewer — no RCA, no Codex. Match what fast branch check against `--base=HEAD~1` or close sibling branch feel like.

Load this file via `Read` from `SKILL.md` after config announced.

## Phase 1: Discover

Read diff against resolved baseline (already computed by SKILL.md step 2). Identify single subsystem touched by matching changed paths against `.claude/rules.md` "By subsystem". Read that subsystem's `subsystem-*.md` rule inline. For pure Python tests, read `subsystem-tests.md`.

**Gate**: Diff fetched, single subsystem identified, context rule read.

## Phase 2: Stage 1 — Correctness (single reviewer)

> **Reviewer model**: `worker-reviewer` launch this tier use resolved `--reviewer` overlay value (tier=low default `haiku`; escalate to `sonnet` when structural markers from `classify.md:48-61` present). See `overlays.md` reviewer axis.

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) to review diff. Phase `post-implementation` because review run against **Implement-phase output** — code already exist (not Stub-phase scaffolding, not Specify-phase tests). Reviewer apply:

- OCX pattern compliance (error model, symlink safety, CLI/API contract)
- Quality (naming, style, tests present, duplication)

Spec-compliance phase anchors cover Stub / Specify / Implement lifecycle positions without reviewer needing look at stubs or specification tests separately — this tier whole review collapse to one pass.

**Gate**: Reviewer complete; findings classified as actionable or deferred.

## Phase 3: Stage 2 — skipped

Two-Way Door scope. No security, performance, documentation, or architect perspectives. If discover phase surprise with signals classifier should have caught (e.g., dependency change slip into "doc" diff), stop and re-run `/swarm-review high <target>` — no silent upgrade mid-pipeline.

**Gate**: Skip logged in output; proceed to verdict.

## Phase 4: Root-cause analysis — skipped

`rca: off`. Reviewer report findings with proximate cause and remediation only. If finding smell systemic, reviewer still flag as deferred with reason — human can escalate to `/swarm-review high` or `/architect` for Five Whys.

## Phase 5: Cross-model — skipped

`codex: off`. If user explicit pass `--codex`, run pass anyway (user override). Else log `Cross-model gate skipped: tier=low default` and continue.

## Phase 6: Verdict & Output

Produce review report using shared skeleton from `SKILL.md`:

```markdown
## Code Review: [target]
### Summary
- Verdict: [Approve / Needs Work / Request Changes]
- Tier: low
- Baseline: <base>
- Diff: N files, +L / -L lines, 1 subsystem
### Stage 1 — Correctness
[Findings with file:line, description, remediation]
### Deferred Findings
[Each with: what it is, why human judgment is needed]
```

Omit Stage 2, Cross-Model, and Root-Cause sections — absence = tier contract, not bug.

**Gate**: Report printed. No commits (review read-only).

## Handoff

Standard handoff from `SKILL.md`. Classification line:

```
- Scope: Small (Two-Way Door)
- Tier: low
- Baseline: <base>
- Overlays: breadth=minimal, rca=off, codex=off
```

If actionable findings exist and caller want fixes:

    /swarm-execute --target=diff "apply low-tier review findings"