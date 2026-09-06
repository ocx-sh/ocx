# Tier: high

The **default** review tier for one-way-door-medium diffs: ≤15 files, ≤500
lines changed, 1–2 areas, no one-way-door-high signal. This is the baseline
any caller gets without an explicit tier — the shape most pre-merge reviews
should take. Stage 1 runs spec-compliance and test-coverage in parallel;
Stage 2 runs the full quality/security/performance/docs perspective set
against changed files, each firing only when its trigger matches. The
cross-model pass auto-fires on one-way-door signals, off otherwise.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (inline, no worker)

Read the diff against the resolved baseline. Parse the changed-file list,
map paths to areas using the project's own module/area boundaries (project
context, cached in `hex.md › Pointers`). Read the project's quality rules
for every touched area and the languages the diff uses, directly — no
worker needed for this size.

**Gate** — the diff is fetched; every touched area's rules are loaded.

## Phase 2: Stage 1 — Correctness (parallel, 2 workers)

Launch **in a single batch** so they run concurrently
([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)):

- **1** `reviewer` (focus `spec`, phase `post-implementation`) — reviews
  the full Stub → Specify → Implement trajectory against the design
  record, focused on **Implement**-phase output. Anchor claims in the
  project's own stated patterns (error model, contract conventions,
  dispatch pattern) — never invent one from memory. When the target traces
  to a plan, this slot also runs the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  against its C-/S- IDs — part of the Stage 1 gate below, not a separate
  pass.
- **1** `reviewer` (focus `quality`, test-coverage emphasis) — checks that
  the **Specify** phase produced adequate tests: new code has tests, bug
  fixes have regression tests, edge cases are covered.

Model class per [`models.md`](../hex-core/references/models.md) rows
`reviewer:spec` / `reviewer:quality`, tier `high`. If Stage 1 turns up
actionable findings, surface them prominently — polishing code that
doesn't meet spec or lacks tests wastes downstream effort. Stage 2 still
runs in parallel (not gated on Stage 1), but Stage 1's actionable findings
dominate the verdict.

**Gate** — both reviewers are done; findings, and any convergence gaps,
are classified.

## Phase 3: Stage 2 — Quality / Security / Performance / Docs (parallel)

Launch **in a single batch** so they run concurrently (only applicable
perspectives fire, or when a `perspectives.always` rule matches):

- `reviewer` (focus `quality`) — naming, pattern compliance, duplication,
  the project's own quality rules.
- `reviewer` (focus `security`) — **fires when** the diff touches an
  auth/crypto/signing path, input handling, or archive extraction (see
  [`classify.md`](classify.md#structural-marker-signals)); otherwise
  skipped.
- `reviewer` (focus `performance`) — **fires when** the diff touches a hot
  path or async code; otherwise skipped.
- `reviewer` (focus `user-feedback`) — **fires when** the diff touches a
  user-facing surface (CLI flags/messages, public API names, docs UX);
  grounded in the project's product knowledge (via `hex.md › Pointers`) when
  present; otherwise skipped.
- `doc-reviewer` — **fires when** the documentation-trigger matrix
  matches changed files (CLI/flags, config/env, schema, install docs,
  changelog — from project context per
  [`doc-reviewer`](../hex-core/references/workers/doc-reviewer.md)).
- the configured **cross-model adversary**, last in the batch, **when
  `adversary=on`** — it occupies no worker slot and is triaged in Phase 5
  ([adversary contract](../hex-core/references/adversary.md#adversary-contract),
  `adr_0016` C-987).

Each `reviewer` seat's brief carries the
[`checklist.md`](../hex-core/references/checklist.md#composition) section of
its own focus. Each reviewer classifies findings actionable or deferred and tags each with a
[severity](../hex-core/references/severity.md#finding-severity); a
Suggest-severity finding is reported but never gates the verdict. Model class
per
[`models.md`](../hex-core/references/models.md), tier `high`. Peak
concurrency: up to 4 Stage 2 workers (Stage 1's 2 already done) — within the
effective cap `min(8, max-workers)`; a lower cap batches Stage 2 per
[`protocol.md`](../hex-core/references/protocol.md#worker-coordination)
rather than dropping a perspective.

**Gate** — every applicable Stage 2 perspective is done.

## Phase 4: Root-cause analysis (`rca=on`, [Block/High](../hex-core/references/severity.md#finding-severity) findings)

For every finding classified Block or High, apply Five Whys:

```
**Issue**: <problem>
**Why 1**: <first-level cause>
**Why 2**: <deeper cause>
**Why 3**: <deeper cause>
**Why 4**: <deeper cause>
**Why 5**: <root cause>
**Systemic Fix**: <what prevents recurrence across the codebase>
```

Stop early if the causal chain terminates before five levels — quality
matters more than depth. Note when a finding shares a root cause with
another. Warn-tier findings skip RCA at this tier (`xhigh` applies it to
those too).

**Gate** — RCA is complete for every Block/High finding.

## Phase 5: Cross-model pass (`adversary`, when it fires)

When `adversary=on` (user flag, or classifier-inferred from a one-way-door
or security signal), the configured adversary skill was **launched last in
Phase 3's batch** — `code-diff` scope for a diff target, `plan-artifact`
scope for a markdown target
([`overlays.md`](overlays.md), [adversary contract](../hex-core/references/adversary.md#adversary-contract));
**this phase triages its return.** One-shot, no looping.

Triage 4-way:

- **actionable** — reported in the Cross-Model section. Review is
  read-only — no fix pass here; hand off to `/hex-execute` if the caller
  wants it applied.
- **deferred** — added to Deferred Findings with a reason.
- **stated-convention** — dropped, count mentioned.
- **trivia** — dropped, count mentioned.

When the adversary produces no review — the skill is unavailable, or it ran and
did not complete one — log `Cross-model review skipped: <reason>`
and continue.

**Gate** — triage is complete (or the skip is logged).

## Phase 6: Verdict & Output

Produce the review report using the skeleton from
[`SKILL.md`](SKILL.md#the-review-report):

```markdown
## Code Review: [target]
### Summary
- Verdict: Approve | Needs Work | Request Changes
- Tier: high
- Baseline: <base>
- Diff: N files, +L / -L lines, S areas
### Stage 1 — Correctness
#### Spec compliance
#### Test coverage
### Stage 2 — Quality / Security / Performance / Docs
#### Quality
#### Security             <!-- if fired -->
#### Performance           <!-- if fired -->
#### Documentation         <!-- if fired -->
### Convergence               <!-- when the target traces to a plan -->
### Fold-Back                 <!-- mandatory when the target traces to a plan; full block per SKILL.md § The review report -->
### Cross-Model Adversarial   <!-- if adversary fired -->
### Root-Cause Analysis
### Deferred Findings
```

**Verdict rules**:

- **Request Changes** — any unresolved Block-tier finding; a security
  vulnerability; a breaking change without a migration note; tests absent
  for new code; an unjustified constitution violation (a violation with no
  adequate Constitution Deviations row is automatic Request Changes — see
  the
  [constitution gate](../hex-core/references/protocol.md#constitution-gate)).
- **Needs Work** — **High- or Warn-tier** findings exist but no Block-tier
  finding, or
  the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  found unconverged gaps — this caps the verdict at Needs Work, never
  Approve, with `Next: /hex-execute <plan path>`.
- **Approve** — otherwise.

Don't nitpick style the project's own formatter or linter already
enforces.

**Gate** — the report is presented. No commits.

## Upkeep and handoff

**Fold-Back runs identically at every tier — see
[`SKILL.md`](SKILL.md#the-review-report).**

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
update any `hex.md › Pointers` entry this run revealed as drifted. Then emit
the handoff from [`SKILL.md`](SKILL.md) with:

```
- Scope: medium (one-way door, where signals fire)
- Tier: high
- Baseline: <base>
- Overlays: breadth=full, rca=on, adversary=<off|on>
```

If actionable findings exist:

```
/hex-execute <plan path> "apply review findings"   <!-- a tracked plan exists -->
/hex-execute "apply high-tier review findings"   <!-- no tracked plan -->
```
