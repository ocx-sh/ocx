# Tier: xhigh

The full treatment for **one-way-door-high** decisions — a public API
shape, a wire protocol, an auth boundary redesign, a data model migration
with no rollback. Mandatory architect, mandatory 3-axis research with
user-selected axes, an adversarial design panel, and cross-model design
review as a default gate before handoff. Five phases, not six —
hex-architect never decomposes into executable tasks; that is `/hex-plan`'s
job once it consumes the ADR.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

**Meta-plan preview is mandatory.** At `xhigh`, the gate in
[`SKILL.md`](SKILL.md) step 4 always blocks for explicit approval — this tier
is expensive, and the preview catches a misclassification, and confirms the
research-axis selection, before workers launch.

## Phase 1: Discover (single worker, full)

Launch **1** `architecture-explorer` covering every area the decision
touches — its dependency-graph step already traces what an area imports and
what imports it, so cross-module reach is covered by one worker, not a
per-area fleet. In parallel, read directly the project's architectural
conventions for every area touched, and all prior ADRs/plans/research in the
convention-resolved artifact home for the whole domain.

**With a dossier**, Phase 1 shrinks to the bounded claim diff exactly as the
`high` tier defines it — containment-first canonicalization, both failure
modes, the git-history discriminator, and the derived staleness anchor live in
[`tier-high.md` Phase 1](tier-high.md#phase-1-discover-single-worker) and
are not restated here. This tier's delta is the **residual ground**: what the
dossier does not cover is larger here, spanning every area the decision
touches plus the whole domain's prior ADRs/plans/research as above, not the
`high` tier's single feature area.

Which makes the Phase-1 shrink **marginal at this tier**: the
`architecture-explorer` still runs at full breadth over that residual ground,
so the claim diff displaces reads rather than removing them. The fast path's
real saving here is
[Phase 2](#phase-2-research-3-axes-gate-selected--mandatory)'s skipped
researchers — read the gate's cost preview that way, and never promise a
cheap Discover at `xhigh`.

**Gate** — a full architecture map is produced, or, with a dossier, the claim
diff (per [`tier-high.md` Phase 1](tier-high.md#phase-1-discover-single-worker))
covers every path the dossier names and the residual ground is explored;
every prior decision record in the domain is enumerated.

## Phase 2: Research (3 axes, gate-selected — mandatory)

[`classify.md`](classify.md) proposes at least 3 ranked candidate axes
(folding in any `hex.md › Preferences` research axes of interest and the
project's product knowledge — keywords/comparable tools, located via
`hex.md › Pointers`; a researcher assigned the product/competitive-landscape
axis runs focus `competitive-research`); at the meta-plan
gate the user **selects 3** — this is the defining interaction of
hex-architect at this tier, always presented as an explicit choice even
though the rest of the gate is a blocking approval regardless. Default on
plain approval: the classifier's top 3. Launch **3** `researcher` workers in
a single concurrent batch, one per selected axis. Each returns an
opinionated recommendation with trend analysis, adoption signals, and
citations. Persist each as a research artifact — mandatory at this tier;
substantial findings are expected.

**With a dossier**, the per-axis research skip applies here unchanged — the
cite-and-unexpired predicate and its announcement format are defined in
[`tier-high.md` Phase 2](tier-high.md#phase-2-research-1-axis-gate-selected)
and are not restated here. The delta is count: **3** axes are selected at this
tier rather than 1, so up to **3** independent skip announcements are
possible, and a dossier covering all three may legitimately resolve to
**zero** researchers at this tier too — the mandatory-research language above
does not override the skip.

**Gate** — every selected axis either persisted a research artifact or was
announced skipped with its cited dossier source (C-724, per
[`tier-high.md` Phase 2](tier-high.md#phase-2-research-1-axis-gate-selected));
axis choices recorded with their source (classifier default or user pick).

## Phase 3: Classify (sequential)

Confirm **one-way-door high** in the record header. If the classification
fails (the decision is actually medium), **downgrade and re-run** as
`/hex-architect high "…"` — never silently over-specify a medium decision
or under-specify a large one.

Required artifacts this tier:

- the **ADR** (mandatory);
- a **system-design doc** (when blast radius is cross-area or
  external-contract, per [`classify.md`](classify.md) / [`overlays.md`](overlays.md));
- **persisted research** (from Phase 2).

**Gate** — blast radius, reversibility, and all required artifacts listed in
the record header.

## Phase 4: Reason & Design (architect mandatory, ADR + system-design)

Launch **1** `architect` worker; its model class is `deep-reasoning`
([`models.md`](../hex-core/references/models.md)). A downward override is
honored but never silent — the announce block flags it. Produce the ADR
and, when scope warrants, the system-design doc.

Design must include everything the `high` tier requires, plus:

- **Trade-off matrix** — at least **3** options (not 2), weighted criteria,
  risks, reversibility, and a recommendation with rationale.
- **Migration / rollout plan** — how existing code, data, and consumers move
  to the new shape without breakage, or with explicit breakage communicated.
- **Full C4** — Context, Container, Component, and Code-level detail when the
  decision is significant enough to warrant it.

**Gate** — the ADR (and system-design doc, when produced) is written, the
migration plan is present, and the contracts are testable.

## Phase 5: Review (adversarial design panel + mandatory cross-model)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
on the design artifact — **plan-artifact scope: one panel round**; fix
application, conditional re-validation, and escalation follow the
canonical loop's artifact-scope rule, never restated here.

**Round 1** — launch concurrently:

- `reviewer` (focus `spec`) — are the contracts testable? Internally
  consistent with the stated decision?
- `reviewer` (focus `quality`, prompted adversarially) — steelman every
  rejected option; is the reversibility claim honest; does the design
  introduce a boundary or convention violation? With a dossier, one further
  duty is **mandatory**: steelman against the dossier's own `## Decisions` —
  two-party agreement is *unexamined*, not evidence.
- `researcher` — does the design miss a trending pattern, a known pitfall,
  or a state-of-the-art approach the research axes didn't surface?
- `reviewer` (focus `security`) — mandatory when
  [`classify.md`](classify.md) fired the compliance/security-touch signal or
  `hex.md › Preferences` names the touched area as always-security-sensitive.

**Cross-model design review — a default part of this tier's flow.**
Launched **in the Round 1 panel batch, last** — never after the panel — and
run once in `plan-artifact` scope on the ADR (and system-design doc, if
produced) (`adr_0016` C-987). One-shot, no loop; 4-way triage (actionable /
deferred / stated-convention / trivia); its actionable findings join the
same fix application as the panel's and are re-validated by the same single
`reviewer` (focus `spec`) pass
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
If the adversary produces no review — the skill is unavailable, or it ran and
did not complete one — log
`Cross-model design review skipped: <reason>` and continue — but **surface
the skip prominently in the handoff**, since one review layer was missed.

**Gate** — the design artifact is ready for handoff; both panel and
cross-model deferred findings are documented. Then run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step) and emit the
handoff from [`SKILL.md`](SKILL.md).
