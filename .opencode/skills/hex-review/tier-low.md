# Tier: low

Inline review for **trivial** diffs — one file, ≤30 lines, no structural
marker ([`classify.md`](classify.md#tier-metric-table)). **Zero spawns:
the orchestrator reads the diff itself** (`adr_0017` C-994). The
adversarial stance still applies; what shrinks is the seat count, to none.
Anything Discover reveals to be larger stops and re-runs as
`/hex-review medium <target>`.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (inline, no worker)

Read the diff against the resolved baseline ([`SKILL.md`](SKILL.md)
step 2) and the one area's quality rules (project context, cached in
`hex.md › Pointers`).

**Gate** — one file, its rules loaded.

## Phase 2: Stage 1 — Correctness (inline)

Review the diff yourself against the composed `spec` + `quality` sections
of [`checklist.md`](../hex-core/references/checklist.md#composition) —
every item answered, verified or not applicable with the reason. Findings
are classified actionable / deferred with `file:line` and a remediation;
severity tags are omitted at this tier
([`severity.md`](../hex-core/references/severity.md#finding-severity)).

**Gate** — every checklist item answered.

## Phase 3: Stage 2 — skipped

## Phase 4: Root-cause analysis — skipped

## Phase 5: Cross-model — skipped

`adversary: off`. An explicit `--adversary` runs the configured skill once
under the [adversary contract](../hex-core/references/adversary.md#adversary-contract),
alone (there is no native seat to batch with), read-only; its findings are
triaged and reported, never fixed. Otherwise log
`Cross-model review skipped: tier=low default`.

## Phase 6: Verdict & Output

Produce the review report using the skeleton from
[`SKILL.md`](SKILL.md#the-review-report):

```markdown
## Code Review: [target]
### Summary
- Verdict: Approve | Needs Work | Request Changes
- Tier: low (inline — 0 spawns)
- Baseline: <base>
- Diff: 1 file, +L / -L lines, 1 area
### Stage 1 — Correctness
[findings with file:line, description, remediation]
### Convergence   <!-- when the target traces to a plan -->
### Fold-Back     <!-- mandatory when the target traces to a plan; full block per SKILL.md § The review report -->
### Deferred Findings
```

**Gate** — the report is presented. No commits.

## Upkeep and handoff

**Fold-Back runs identically at every tier — see
[`SKILL.md`](SKILL.md#the-review-report).** Run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step), then emit
the handoff from [`SKILL.md`](SKILL.md) with:

```
- Scope: trivial (one file)
- Tier: low
- Baseline: <base>
- Overlays: breadth=minimal, rca=off, adversary=off
```

If actionable findings exist and the caller wants them applied:
`/hex-execute low "apply low-tier review findings"`.
