# Tier: xhigh

The full adversarial treatment for **one-way-door-high** diffs: >15 files,
a new package/module, a breaking API, cross-area changes, a security-
sensitive path, or a `breaking-change`/`epic` label. Adds `architect` and
`researcher` to the Stage 2 panel, applies Five Whys to every finding above
Suggest, and runs the cross-model pass as a mandatory final gate before
verdict.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

**Meta-plan preview is mandatory.** At `xhigh`, the gate in
[`SKILL.md`](SKILL.md) step 5 always blocks for explicit approval — this
tier is expensive, and the preview catches a misclassification before
workers launch.

## Phase 1: Discover (inline, no worker)

Read the diff against the resolved baseline. Parse the changed-file list,
map **every** touched area — including adjacent areas cross-cutting changes
might affect — using the project's own module/area boundaries. Read:

- the project's quality rules for every touched area and language;
- any prior ADR, plan, or research artifact in the convention-resolved
  artifact home ([`memory.md`](../hex-core/references/memory.md#location-and-resolution))
  that covers the diff's topic — a diff that contradicts a recorded
  decision is itself a finding.

**Gate** — a full area map is produced; relevant prior decision records are
enumerated.

## Phase 2: Stage 1 — Correctness (parallel, 2 workers)

Same shape as `high`, launched **in a single batch** so they run
concurrently
([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)):

- **1** `reviewer` (focus `spec`, phase `post-implementation`) — reviews
  **Implement**-phase output against the design record and the project's
  stated patterns. When the target traces to a plan, this slot also runs
  the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  against its C-/S- IDs — part of the Stage 1 gate below, not a separate
  pass.
- **1** `reviewer` (focus `quality`, test-coverage emphasis) — checks that
  **Specify**-phase tests cover edge cases, boundary conditions, concurrent
  access, and failure modes.

Model class per [`models.md`](../hex-core/references/models.md) rows
`reviewer:spec` / `reviewer:quality`, tier `xhigh` — both escalate to the
`deep-reasoning` class at this tier. Stub → Specify → Implement traceability
matters extra here: flag any implementation behavior with no corresponding
test or design-record anchor.

**Gate** — both reviewers are done; findings, and any convergence gaps,
are classified.

## Phase 3: Stage 2 — Adversarial panel (parallel, up to 6 workers)

Launch **in a single batch** so they run concurrently (plus any role a
matching `perspectives.always` rule adds):

- `reviewer` (focus `quality`) — when the diff touches a user-facing
  surface (CLI, API, UI), this slot spawns as focus `user-feedback`
  instead (UX/product consistency, grounded in the project's product
  knowledge via `hex.md › Pointers`); plain `quality` otherwise.
- `reviewer` (focus `security`) — always at `xhigh` (treat the diff as
  security-sensitive until proven otherwise).
- `reviewer` (focus `performance`) — always at `xhigh`.
- `doc-reviewer` — always at `xhigh` (doc drift at scale is the default
  failure mode).
- `architect` — boundary respect, dependency direction, trade-off
  honesty; checks the diff against any ADR covering the area.
- `researcher` (focus `competitive-research` when comparing comparable
  tools; grounded in the project's product knowledge via
  `hex.md › Pointers`) — SOTA gap check: how do the field's leading
  tools solve the same problem? Is the algorithm choice current? Any
  known pitfall unaddressed?
- the configured **cross-model adversary**, last in the batch — mandatory
  at this tier; it occupies no worker slot and is triaged in Phase 5
  ([adversary contract](../hex-core/references/adversary.md#adversary-contract),
  `adr_0016` C-987).

Each `reviewer` seat's brief carries the
[`checklist.md`](../hex-core/references/checklist.md#composition) section of
its own focus; the `architect` carries `architecture`, the `researcher`
`pitfalls`. Stage 1 (2, sequential) then Stage 2 (up to 6 concurrent) — peak
concurrency 6, within the shipped 8-worker cap. Under a lower effective cap
(`min(8, max-workers)`) Stage 2 batches per
[`protocol.md`](../hex-core/references/protocol.md#worker-coordination); it
is never trimmed to fit a number. A `perspectives.always` addition on top of
this already-saturated baseline is what merge rule 6's phase ceiling
displaces first
([`config.md` § Merge rules](../hex-core/references/config.md#merge-rules)).
If the diff clearly doesn't need
`researcher` (e.g. a pure refactor with no algorithmic change), skip it —
a diff-content judgement, not a cap accommodation. Model class per
[`models.md`](../hex-core/references/models.md), tier `xhigh` — most rows
escalate to `deep-reasoning` here. Each reviewer classifies findings
actionable or deferred and tags each with a
[severity](../hex-core/references/severity.md#finding-severity); a
Suggest-severity finding is reported but never gates the verdict.

**Gate** — every perspective is done.

## Phase 4: Root-cause analysis (`rca=on`, all findings above [Suggest](../hex-core/references/severity.md#finding-severity))

Apply Five Whys to every Block, High, and Warn finding:

```
**Issue**: <problem>
**Why 1** … **Why 5**: <causal chain>
**Systemic Fix**: <what prevents recurrence>
**Related findings**: <other findings sharing this root, if any>
```

Coverage is deliberately wider than `high` — big cross-area diffs often
share systemic causes. Cluster findings that trace to the same root; note
the pattern (e.g. "three findings all trace to a missing cancellation guard
in the worker pool").

**Gate** — RCA is complete for every finding above Suggest; clusters are
noted.

## Phase 5: Cross-model pass (mandatory)

The configured adversary skill was **launched last in Phase 3's batch**
against the diff (`code-diff` scope) or the artifact (`plan-artifact` scope
for a markdown target) —
[adversary contract](../hex-core/references/adversary.md#adversary-contract);
**this phase triages its return.** One-shot, no looping.

Triage 4-way — actionable (reported in the Cross-Model section, review
stays read-only), deferred (added to Deferred Findings with a reason),
stated-convention (dropped, count mentioned), trivia (dropped, count
mentioned).

No-review path: at this tier the pass is a **gate, not a blocker** —
surface the skip prominently in the verdict summary so the reader knows one
review layer was missed. Log
`Cross-model review skipped: <reason>` and include it in the Summary line.

**Gate** — triage is complete (or the skip is surfaced).

## Phase 6: Verdict & Output

Produce the review report using the skeleton from
[`SKILL.md`](SKILL.md#the-review-report):

```markdown
## Code Review: [target]
### Summary
- Verdict: Approve | Needs Work | Request Changes
- Tier: xhigh
- Baseline: <base>
- Diff: N files, +L / -L lines, S areas
- Cross-model: ran | skipped: <reason>
### Stage 1 — Correctness
#### Spec compliance (post-Implement traceability)
#### Test coverage (Specify-phase adequacy)
### Stage 2 — Adversarial panel
#### Quality
#### Security
#### Performance
#### Documentation
#### Architecture
#### SOTA / technical soundness
### Convergence               <!-- when the target traces to a plan -->
### Fold-Back                 <!-- mandatory when the target traces to a plan; full block per SKILL.md § The review report -->
### Cross-Model Adversarial
### Root-Cause Analysis
[clusters with systemic fixes]
### Deferred Findings
```

**Verdict rules**:

- **Request Changes** — any unresolved Block-tier finding; any security
  vulnerability; any architect-flagged boundary violation; a breaking
  change without a migration plan; tests absent for new behavior; a
  systemic cause affecting ≥3 findings; an unjustified constitution
  violation (a violation with no adequate Constitution Deviations row is
  automatic Request Changes — see the
  [constitution gate](../hex-core/references/protocol.md#constitution-gate)).
- **Needs Work** — **High- or Warn-tier** findings exist, the cross-model
  pass surfaced
  actionable findings not yet addressed, or the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  found unconverged gaps — this caps the verdict at Needs Work, never
  Approve, with `Next: /hex-execute <plan path>`.
- **Approve** — otherwise.

Explicitly surface in the Summary: whether the cross-model pass ran or was
skipped (with reason), any architect-flagged boundary or ADR-compliance
concern, and any SOTA gap the researcher flagged.

**Gate** — the report is presented. No commits.

## Upkeep and handoff

**Fold-Back runs identically at every tier — see
[`SKILL.md`](SKILL.md#the-review-report).**

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
update any `hex.md › Pointers` entry this run revealed as drifted. Then emit
the handoff from [`SKILL.md`](SKILL.md) with:

```
- Scope: large (one-way door, high)
- Tier: xhigh
- Baseline: <base>
- Overlays: breadth=adversarial, rca=on, adversary=on
```

If actionable findings exist:

```
/hex-execute <plan path> "apply xhigh-tier review findings"   <!-- a tracked plan exists -->
/hex-execute "apply xhigh-tier review findings"                <!-- no tracked plan yet -->
```

If a SOTA gap or an architectural concern needs its own decision record,
escalate:

```
/hex-architect "propose an ADR for <concern>"
```
