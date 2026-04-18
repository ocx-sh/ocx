# Tier: max — /swarm-review

Full adversarial kitchen sink for large diffs (>15 files, cross-
subsystem, new crate, breaking API, protocol change, security-sensitive
paths, or `breaking-change` / `epic` label). Adds `worker-architect`
(SOLID, boundary, dependency direction) and `worker-researcher` (SOTA
gap check) to the Stage 2 panel, applies Five Whys RCA to every
finding above Suggest, and runs the Codex cross-model pass as a
mandatory final gate before verdict.

Load this file via `Read` from `SKILL.md` after the config is announced.

**Auto meta-plan preview**: when tier resolves to `max`, SKILL.md step 5
auto-fires the meta-plan gate (users can opt out with `--no-dry-run`).
Cost transparency — max-tier runs are expensive and this catches
misclassifications before tokens burn.

## Phase 1: Discover

Read the diff against the resolved baseline. Parse the file list and
map paths to subsystems — **all** of them, including adjacent
subsystems that might be affected by cross-cutting changes. Read:

- `subsystem-*.md` rules for every touched subsystem
- The relevant ADR (`.claude/artifacts/adr_*.md`) if one covers the
  diff's topic
- `.claude/rules/product-context.md` — the diff may imply positioning
  shifts the review must catch
- Language quality rules matching diff languages

**Gate**: Diff fetched, full context loaded, product-context read.

## Phase 2: Stage 1 — Correctness (parallel, 2 workers)

> **Reviewer model**: every `worker-reviewer` launch in this tier uses the resolved `--reviewer` overlay value (tier=max default `sonnet`; escalated to `opus` when `--breadth=adversarial` fires). See `overlays.md` reviewer axis.

Same as tier-high — launch in parallel:

- **1** `worker-reviewer` (focus: `spec-compliance`, phase:
  `post-implementation`) — reviews the **Implement**-phase output
  against OCX anchors
- **1** `worker-reviewer` (focus: `quality`, lens: test-coverage) —
  checks the **Specify**-phase tests cover edge cases, boundary
  conditions, concurrent access, failure modes

At max tier, the **Stub** / **Specify** / **Implement** lifecycle
traceability is particularly important — the reviewer notes any
behavior in the implementation that has no corresponding test or
design-record anchor.

**Gate**: Both reviewers complete.

## Phase 3: Stage 2 — Adversarial breadth (parallel, up to 6 workers)

Launch in parallel:

- `worker-reviewer` (focus: `quality`) — including CLI-UX lens when
  the diff touches command surface
- `worker-reviewer` (focus: `security`) — always at max (assume
  security-sensitive until proven otherwise)
- `worker-reviewer` (focus: `performance`) — always at max
- `worker-doc-reviewer` — always at max (doc drift at scale is the
  default failure mode); model per resolved `--doc-reviewer` overlay
  (`sonnet` default; `haiku` when narrow-scope doc trigger fires — see
  `overlays.md` doc-reviewer axis)
- `worker-architect` — SOLID, subsystem boundary respect, dependency
  direction, trade-off honesty; checks the diff against any ADR that
  covers the area
- `worker-researcher` — SOTA gap: how do leading tools (Cargo, npm,
  pip/uv, Go modules, Helm) solve the same problem? Is the algorithm
  choice current? Known pitfalls unaddressed?

Stage 1 (2) + Stage 2 (6) = 8 total workers. At the 8 concurrent
worker ceiling — do not add more without dropping one. If the diff
clearly doesn't need `worker-researcher` (e.g., pure refactor with no
algorithmic change), skip it and stay at 7.

Each reviewer classifies findings as actionable / deferred / suggest.

**Gate**: All perspectives complete.

## Phase 4: Root-cause analysis (rca=on, all findings above Suggest)

Apply Five Whys to every Block, High, and Warn finding. Max-tier
coverage is deliberately wider than high-tier because large cross-
subsystem diffs often share systemic causes — clustering findings by
root reveals patterns (e.g., "three findings all trace back to a
missing async cancellation guard in the worker pool").

```
**Issue**: [problem]
**Why 1** … **Why 5**: [causal chain]
**Systemic Fix**: [what prevents recurrence]
**Related findings**: [list of other findings sharing this root]
```

**Gate**: RCA complete for all findings above Suggest. Clusters noted.

## Phase 5: Cross-model — Codex (mandatory)

Invoke `codex-adversary` with scope `code-diff --base <base>` against
the branch diff. One-shot, no looping.

Triage per `overlays.md`:

- **Actionable** → reported in the Cross-Model section of the output.
  Review is read-only — no builder fix pass.
- **Deferred** → added to Deferred Findings with reason
- **Stated-convention** → dropped, count mentioned
- **Trivia** → dropped, count mentioned

Unavailable path: at max-tier this is a **gate, not a blocker** — but
surface the skip prominently in the verdict so the reader knows one
layer of review was missed. Log `Cross-model gate skipped: <reason>`
and include it in the Summary line.

**Gate**: Codex triage complete (or skip surfaced).

## Phase 6: Verdict & Output

Produce the review report using the shared skeleton from `SKILL.md`:

```markdown
## Code Review: [target]
### Summary
- Verdict: [Approve / Needs Work / Request Changes]
- Tier: max
- Baseline: <base>
- Diff: N files, +L / -L lines, S subsystems
- Cross-model: [ran | skipped: <reason>]
### Stage 1 — Correctness
#### Spec-compliance (post-Implement traceability)
#### Test Coverage (Specify-phase adequacy)
### Stage 2 — Adversarial panel
#### Quality
#### Security
#### Performance
#### Documentation
#### Architecture
#### SOTA / Technical Soundness
### Cross-Model Adversarial (Codex)
### Root-Cause Analysis
[Clusters with systemic fixes]
### Deferred Findings
```

**Verdict rules**:
- **Request Changes** if any Block-tier finding remains unresolved;
  any security vulnerability exists; any architect-flagged boundary
  violation; breaking changes lack migration plan; tests absent for
  new behavior; systemic cause affecting ≥3 findings
- **Needs Work** if Warn-tier findings exist or Cross-model pass
  surfaced actionable findings not yet addressed
- **Approve** otherwise

At max-tier, explicitly surface in the Summary:
- Whether the Codex gate ran or was skipped (with reason)
- Architect-flagged boundary or ADR-compliance concerns
- SOTA gaps the researcher flagged

**Gate**: Report printed. No commits.

## Handoff

Standard handoff from `SKILL.md`. Classification line:

```
- Scope: Large (One-Way Door High)
- Tier: max
- Baseline: <base>
- Overlays: breadth=adversarial, rca=on, codex=on
```

If actionable findings exist:

    /swarm-execute max .claude/artifacts/plan_[feature].md
    /swarm-execute max "apply max-tier review findings"   # no plan yet

If SOTA gaps or architectural concerns need their own ADR, escalate:

    /architect "propose ADR for [concern]"
