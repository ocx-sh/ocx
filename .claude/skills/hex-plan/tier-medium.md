# Tier: medium

Minimal plan for **two-way-door** changes — a flag or option, a doc edit, a
fixture, a single-area tweak of ≤3 files. Scale the worker count and
research depth down; the plan stays contract-first.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (single worker)

Launch **1** `explorer` scoped to the single area the target touches — no
`architecture-explorer`, the scope is too small to earn one. In parallel,
read directly the project rules for the area (from project context, cached
in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)) and the
specific code region the prompt names. This read also picks up any
`Federation:` bullets under `hex.md › Pointers`, alongside every other
pointer (C-314) — absent them, nothing changes.

GitHub: if the target resolved to a PR, its file list is the explicit scope —
skip any broad issue scan.

**Gate** — the code region is mapped and reusable utilities are identified.

## Phase 2: Research (skip)

No `researcher`. The orchestrator may make a brief inline check against
project context if the change touches positioning-sensitive behavior.

**Gate** — the skip is logged in the plan header
(`Research: skipped — two-way door`). If the inline check surfaces a surprise
that makes this not a two-way door, **stop and re-run** at a higher tier
rather than silently upgrading mid-flow.

## Phase 3: Classify (inline)

Confirm the two-way-door scope inline in the plan header. If Discover revealed
the change is *not* two-way (it touches a public surface, a stored format, a
protocol), **stop and re-run** as `/hex-plan high "…"` — never silently
upgrade mid-pipeline.

## Phase 4: Design (inline)

Draft the design inline in the plan artifact; no `architect` worker. It must
still carry:

- **Component contracts** — the public function/type signature(s) touched,
  with expected behavior.
- **User experience** — at least one action → expected outcome scenario.
- **Error taxonomy** — the failure modes this change adds or alters.
- **Edge cases** — the boundary conditions for the new behavior.

Trade-off analysis is optional here — a single sentence naming the chosen
approach is enough when the change is genuinely small.

## Phase 5: Decompose (inline)

One pipeline: a few ordered steps (for ≤3 files, possibly one), each a
Stub → Specify → Implement brief, no contract wave, no marks
([`SKILL.md`](SKILL.md#the-plan-artifact)). The Parallelization section is
still required, even for one pipeline with no dependencies
([`worktree.md`](../hex-core/references/worktree.md#pipeline-worktree-mechanics))
— the tier's ≤3-file scope is itself the justification; the "Shippable after
wave" line is exempt, since the sole pipeline is the shippable unit.

**Federation.** When `hex.md › Pointers` carries `Federation:` bullets and
the target's scope lies in a satellite, Decompose offers that satellite's
key in the pipeline's `Repo` column and adds the mandatory integration
pipeline row (C-311) — the plan then carries more than the single pipeline
this tier otherwise collapses to. Once more than one pipeline exists,
disjointness is keyed on `(Repo, path)`, not bare paths (C-316).
`/hex-plan` never runs the C-303 pre-flight and never writes into a
satellite (C-314). Absent `Federation:` bullets, unchanged.

## Phase 6: Review (single reviewer, single pass)

Launch **1** `reviewer` (focus `spec`, phase `post-stub`) on the draft plan —
no adversarial panel, no adversary pass (`adversary: off` at this tier). Run
the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
capped at **one round**: the orchestrator edits the plan directly on
actionable findings; only a Block-tier finding earns one reviewer re-run
(2 passes total max, then stop).
When project context names a constitution (cached in `hex.md › Pointers`),
this same reviewer also applies the
[constitution gate](../hex-core/references/protocol.md#constitution-gate).

**Gate** — the plan is ready for `/hex-execute`.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
re-point any `hex.md › Pointers` entry this run revealed as drifted and
update `hex.md › Memory`. Then emit the handoff from [`SKILL.md`](SKILL.md)
with:

```
- Scope: small (two-way door)
- Tier: medium
- Overlays: (none)
```

Its only required artifact is the plan itself (Status block initialized,
active-plan pointer recorded in `hex.md › Memory`). No ADR or research
artifact at this tier — if the classifier called for one, it should have
picked a higher tier; re-run if so.
