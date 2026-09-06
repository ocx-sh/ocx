# Tier: medium

Minimal plan for **two-way-door** changes — a flag or option, a doc edit, a
fixture, a single-area tweak of ≤3 files. Keep the contract-first TDD skeleton
(Stub → Specify → Implement → Review) so `/hex-execute` runs the plan
unchanged; only scale the worker count and research depth down.

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

Produce a single Stub → Specify → Implement → Review cycle in the plan. For
≤3 files this may collapse into one task. A Parallelization section is still
required, even if it is one work package with no dependencies
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics))
— the tier's ≤3-file scope is itself the justification, no extra line
needed. Single WP: the "Shippable after wave" line is exempt — delete it,
since the sole WP is the shippable unit. The WP still carries a `Verify` cell
and a `Review` cell — the latter empty, or `risk` when the author knows
more than the file set shows
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition));
a plan carrying the generation marker needs the `Verify` column present.

**Federation.** When `hex.md › Pointers` carries `Federation:` bullets and
the target's scope lies in a satellite, Decompose offers that satellite's
key in the WP's `Repo` column and adds the mandatory integration WP row it
depends on (C-311) — the plan then carries more than the single WP this
tier otherwise collapses to. Wave-cutting, once more than one WP exists,
applies the `(Repo, path)` disjointness key, not bare paths (C-316).
`/hex-plan` never runs the C-303 pre-flight and never writes into a
satellite (C-314). Absent `Federation:` bullets, unchanged.

Print the **effective-tier histogram** at this point, linking rather than
restating its grammar
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)).

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
