# hex Model Matrix

The single source of model guidance for the whole hex bundle. No other
file recommends models — [`workers.md`](workers.md) and the tier files
link here.

## Capability classes

Cells hold **capability classes, never literal model names** — the actual
model behind a class differs per client harness, so the shipped file stays
portable. Two classes:

- **`fast-balanced`** — the capable default workhorse: fast, strong on
  single-pass review and mechanical work, the right choice unless deeper
  reasoning clearly earns its cost.
- **`deep-reasoning`** — the deliberate-reasoning class: for one-way-door
  design, novel reasoning, and security-critical judgment. **Not a
  superlative:** it is the strongest *worker-appropriate* class, distinct
  from any orchestrator-class model that runs the session.

## The matrix

Rows are worker purposes at focus-mode granularity; columns are the tiers
from [`protocol.md`](protocol.md#tier-grammar).

| Role / focus | low | medium | high | xhigh | max |
|---|---|---|---|---|---|
| explorer | — | fast-balanced | fast-balanced | fast-balanced | fast-balanced |
| architecture-explorer | — | fast-balanced | fast-balanced | fast-balanced | fast-balanced |
| researcher | — | fast-balanced | fast-balanced | fast-balanced | fast-balanced |
| builder:stub | — | fast-balanced | fast-balanced | fast-balanced | fast-balanced |
| builder:implement | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| tester | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| reviewer:quality | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| reviewer:security | — | fast-balanced | deep-reasoning | deep-reasoning | deep-reasoning |
| reviewer:performance | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| reviewer:spec | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| reviewer:user-feedback | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| doc-reviewer | — | fast-balanced | fast-balanced | fast-balanced | fast-balanced |
| architect | — | deep-reasoning | deep-reasoning | deep-reasoning | deep-reasoning |
| coordinator | — | — | deep-reasoning | deep-reasoning | deep-reasoning |
| coordinator:pipeline | — | fast-balanced | fast-balanced | deep-reasoning | deep-reasoning |
| simulator | — | — | — | — | fast-balanced (`adversarial` pattern: deep-reasoning) |

`—` = never spawned at that tier. **`low` spawns nothing** — the
orchestrator is the worker (`adr_0017` C-994); its one optional `L1`
backstop reads `review.l1.class` like every `L1` seat. `max` is `xhigh`
plus the `simulator` row (C-995). Column resolution is per spawn, not per
plan: **a spawn made for a work package reads that WP's effective tier; a
spawn made for the run reads the plan tier**
([`decompose.md`](decompose.md#the-effective-tier)). **Review-Fix `L1`/`L2`
seats do not read the `reviewer` rows** — their class is
`review.<level>.class` ([`loop.md`](loop.md#review-by-join-level)); the
rows govern Verify-Architecture, `/hex-review`'s `L3` panel and the artifact
panels.

## Rules

1. **Cells are recommendations, not floors or ceilings — but never
   silently.** The orchestrator escalates on judgment and announces the
   reason at the meta-plan gate. Example: tier=medium but the diff touches
   security-critical auth code — run `reviewer:security` at
   `deep-reasoning`, announced as
   "reviewer:security → deep-reasoning (security-critical diff)".
   A spawn that runs above its cell **without an announced reason is a
   spec violation** — silent escalation is the failure mode this rule
   exists to prevent. The announce block prints each spawn's **resolved
   literal model**
   ([`protocol.md`](protocol.md#the-meta-plan-approval-gate)).
   A skill that prints **no announce block** discloses the same resolved
   literal model as one line under its declared quiet form, at the first
   spawn of the role (C-712).

2. **Resolution order**:
   1. **Instantiated matrix in `hex.md › Preferences`** — literal model
      names written by `/hex-init` for this harness, including any per-row
      or per-cell overrides. Wins over the shipped default when present.
   2. **Shipped class default** — this table, mapped through the harness's
      class→model mapping; applies when nothing is instantiated.
   3. **Judgment escalation** — the orchestrator's per-run bump on top of
      either, announced at the gate.

   This order also outranks any **harness-global model-routing policy**
   (a user- or client-level routing table): for hex spawns, the matrix
   decides; global routing applies outside hex runs. A global rule like
   "multi-file tasks → strongest model" must not silently override a
   cell — route around the matrix only via the instantiated table or an
   announced escalation.

   `models.overrides` ([`config.md`](config.md#key-vocabulary)) is the
   config-block form of step 1's per-cell override — a map `role[:focus]`
   → capability class merged into the instantiated matrix, not a separate
   resolution path.

   Which tier column step 2 reads is fixed by [the split above](#the-matrix);
   that resolution is disclosed at the announce block like any other axis
   (Rule 1).

3. **Instantiation.** `/hex-init` maps each class to the harness's literal
   models and stores the instantiated table in the Preferences section of
   `.agents/memory/hex.md` (see [`memory.md`](memory.md)). On a
   Claude harness, for example, fast-balanced → Sonnet and deep-reasoning →
   Opus. Skipping instantiation is fine — the shipped class defaults apply.
   Per-row or per-cell overrides live in `hex.md › Preferences` too (e.g.
   `reviewer:security` pinned to the deep-reasoning model at every tier).

4. **Cells never resolve to an orchestrator-class model.** Some harnesses
   expose a model tier above their strongest *worker* model — an
   orchestrator seat that runs the session (e.g. Claude's Mythos-class:
   Fable / Mythos, above Opus). Those models are never spawn targets; the
   matrix governs spawned workers only. `deep-reasoning` maps to the
   strongest *worker-appropriate* model (e.g. Opus), never the
   orchestrator tier. The session/orchestrator model is the user's or
   harness's choice, outside this matrix.

5. **The tier gate belongs to the *decomposing* coordinator only.** The bare
   `coordinator` row is that kind: it runs only at `high` and above —
   decomposition is a fan-out optimization, absent at the single-WP shapes
   of `low` and `medium`
   — and resolves to the `deep-reasoning` **worker** class. The
   `coordinator:pipeline` row is the other kind and carries **no tier gate**:
   a pipeline coordinator owns one work package at any tier, so its column
   is read from that work package's own effective tier. Neither row ever
   resolves an orchestrator-class model (the exclusion rule, Rule 4 above).
   See the [`coordinator`](workers/coordinator.md) persona — the sole
   definition site for the two kinds — for the full role contract.
