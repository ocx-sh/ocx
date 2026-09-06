---
name: hex-plan
description: Tiered multi-agent planning orchestrator — decomposes a feature, issue, or PR into a contract-first TDD plan through discover, research, design, decompose, and adversarial-review phases. Use for multi-step feature planning, task decomposition, swarm planning, multi-perspective research, meta-plan preview, or ADR scaffolding. Tier (low|medium|high|xhigh|max, auto by default) scales research depth, whether an architect designs, and review breadth.
license: Apache-2.0
metadata:
  summary: Tiered swarm planning — prompt/issue to reviewed contract-first plan
  keywords: planning,swarm,multi-agent,decomposition,research,adr,tdd
  repository: https://github.com/michael-herwig/arcana
---

# hex-plan — Planning Orchestrator

Thin dispatcher. It parses arguments, classifies the target's tier, resolves
overlays, runs the single meta-plan approval gate, announces the resolved
config, and hands off to the matching tier file. The phase plans live in
the five `tier-<tier>.md` files; the shared vocabulary
(tiers, the Review-Fix Loop, worker roles, model classes, the memory file)
lives in the `hex-core` reference library and is **linked here, never
copied**.

Shared contracts:
[`protocol.md`](../hex-core/references/protocol.md) ·
[`workers.md`](../hex-core/references/workers.md) ·
[`models.md`](../hex-core/references/models.md) ·
[`memory.md`](../hex-core/references/memory.md).
If `hex-core` is not installed: `grim add ghcr.io/michael-herwig/arcana/hex-core:latest`.

## Argument syntax

```
/hex-plan [tier] <target> [flags]
```

- **tier** (optional): `low | medium | high | xhigh | max | auto`. Default
  `auto` — the classifier picks `low` … `xhigh`. **`max` is explicit
  only** — `--tier=max`, or a plan whose Status block says `Tier: max`; the
  classifier never emits it
  ([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)).
- **target** (one of):
  - free-text prompt;
  - `<N>` or `#<N>` — a number; probe PR first, fall back to issue;
  - `PR <N>` / `pull/<N>` — an explicit pull request;
  - `issue <N>` / `issues/<N>` — an explicit issue;
  - a full GitHub URL `https://github.com/<owner>/<repo>/(pull|issues)/<N>`.
- **flags** (before the target, by convention):
  - `--architect=inline|on` — override the Design phase (see
    [`overlays.md`](overlays.md)).
  - `--research=skip|1|3` — override Research breadth.
  - `--adversary` / `--no-adversary` — force the cross-model plan-artifact
    pass on or off.
  - `--dry-run` — make the meta-plan gate block for explicit approval and
    stop there (preview only, no workers launched).

## Dispatch

The outer loop every hex orchestrator shares
([`protocol.md`](../hex-core/references/protocol.md#shared-shape)):

### 1. Parse arguments and resolve memory

Parse the tier, target, and flags. Locate `.agents/memory/hex.md` by
searching upward from the working directory; a missing file is normal
([`memory.md`](../hex-core/references/memory.md#location-and-resolution)) —
fall back to shipped defaults and note that `/hex-init` can create one. When
present, read `hex.md › Pointers` for cached locations (where verification
and plan/ADR conventions are documented; verify on consumption, re-point on
a miss), `hex.md › Preferences` for the instantiated model matrix, the
adversary skill name, limits, and always-on perspectives, and
`hex.md › Memory` for any active plan already in flight. Product context
(what the product is, users, related repos, research keywords) lives in
project context, located via `hex.md › Pointers`.

If the resolved `hex.md` carries a `Federation lead:` bullet, **halt** per
[`memory.md` § Location and resolution](../hex-core/references/memory.md#location-and-resolution)
— this repo is a federation satellite and its memory is not the plan's.

### 2. Resolve the target

If the target is a GitHub reference, fetch its title, body, labels, and
(for PRs) file list, in this order of preference:

1. the client's **GitHub MCP tools** (PR/issue read) when it exposes them;
2. the **`gh` CLI** (`gh pr view` / `gh issue view --json
   title,body,comments,labels,files`) as fallback;
3. neither available — treat the input as **free text** and continue.

Probe order for a bare `<N>`/`#<N>` is a PR then an issue; PRs and issues are
equal-class targets. The fetched body and labels feed classification; a PR's
file list feeds the Discover scope, not the tier decision.

### 3. Classify (only when tier is `auto`)

Read [`classify.md`](classify.md). Apply its tier signals and overlay
triggers to the prompt plus any fetched GitHub context. It emits a candidate
tier (**only** `low`, `medium`, `high`, or `xhigh`), a confidence flag, and an overlay
set. Low confidence forces the gate in step 5. Never ask a mid-flow question
during classification — ambiguity is resolved at the single gate.

### 4. Resolve overlays

Final config = the tier's baseline from [`overlays.md`](overlays.md) +
classifier-inferred overlays + `hex.md › Preferences` hints + user flags.
Later wins;
**user flags always override** (see
[`protocol.md`](../hex-core/references/protocol.md#spawn-selection-precedence)).

### 5. Meta-plan gate (the single approval point)

Exactly one gate, before any worker launches
([`protocol.md`](../hex-core/references/protocol.md#the-meta-plan-approval-gate)).
Its weight scales:

- **Confident `low` / `medium` / `high`** — announce the resolved config (step 6) and
  proceed; the user can still abort. An explicit user tier always counts as
  confident — the classifier never ran.
- **`xhigh` or `max` tier, low-confidence classification, or `--dry-run`** — block for
  explicit approval.

On a client with a **native plan-approval mechanism**, use it. Otherwise
present the announce block as **one structured question** (approve / adjust /
cancel). The gate is also where the user can add or drop perspectives, choose
research axes, and adjust models. On adjust, re-resolve and re-present once.
Never split this into sequential questions. Open questions are presented
together with their `Recommended: <answer> — <reason>` line; a plain
approval accepts every recommendation as-is
([the meta-plan approval gate](../hex-core/references/protocol.md#the-meta-plan-approval-gate)).

### 6. Announce the resolved config

Print the resolved config with per-axis source attribution before loading the
tier file (format:
[`protocol.md`](../hex-core/references/protocol.md#the-meta-plan-approval-gate)):

```
hex-plan
  Tier:      high                         (auto — classifier: new subcommand, 2 areas)
  Overlays:  architect=on                   (classifier: cross-area design)
             research=1                      (tier baseline)
             adversary=on                    (hex.md preference: one-way-door signals)
  Spawn set:
    architecture-explorer                    (tier baseline)
    explorer ×3                              (tier baseline)
    researcher ×1                            (overlay research=1)
    architect                                (overlay architect=on)
    reviewer: spec                           (tier baseline)
  Models:    fast-balanced default; architect → deep-reasoning   (models.md)
  Adversary: codex-adversary, plan-artifact scope            (hex.md preference)
  Degraded:  no — subagent spawning available
```

The announce block prints one config-disclosure line per change a
`hex.md › Preferences` block makes; the full trigger set is defined once in
[`protocol.md` § the meta-plan approval gate](../hex-core/references/protocol.md#the-meta-plan-approval-gate).
For this skill, for example:

```
  [project-redefined: Research.researcher 1→3 (hex.md tiers)]
  [architect dropped — phase ceiling 3 reached]
  [Review batched 2+2 — concurrency cap 4 (hex.md)]
  Error: never [reviewer:security] refused (hex.md preference)
  Fix:   see config.md#merge-rules for the attestation requirement
```

Every spawn line carries its source (`tier baseline` / `classifier` /
`hex.md preference` / `user flag`) per
[spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence).
Model names above are class placeholders — shipped files never hardcode
literals; the running orchestrator resolves and prints each spawn's
literal model per the
[Models line contract](../hex-core/references/protocol.md#the-meta-plan-approval-gate). On a client that cannot spawn subagents, announce
`Degraded: inline workers` and run each worker prompt inline and sequentially
([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).

### 7. Dispatch to the tier file

Read `workflows.hex-plan.<tier>` from the resolved config first. When set and
the named file passes the seven validation checks
([`config.md` § Workflows](../hex-core/references/config.md#workflows)), `Read`
that forked file in place of the shipped one; on validation failure or when
unset, `Read` the matching `tier-{low,medium,high,xhigh,max}.md` — config.md owns the
check list and the on-failure fallback. Announce which ran: `Workflow: shipped
tier-<tier>.md`, or for a fork, `Workflow: <path> (forked from <shipped tier
file> @ <stamped version>)`. Execute the loaded file's phase plan; no phase
content is duplicated in this file.

## Worker assignment (shared across tiers)

Roles are indexed in [`workers.md`](../hex-core/references/workers.md);
the orchestrator loads a full persona (spawn-prompt template, output
contract) only for roles in the resolved spawn set
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).
The model class for each role × tier is in
[`models.md`](../hex-core/references/models.md). This table maps roles to
planning phases; the tier files set the actual counts.

| Phase | Role | Count | Purpose |
|---|---|---|---|
| Discover | `architecture-explorer` | 0–1 | Map the current architecture, dependencies, reusable code |
| Discover | `explorer` | 1–4 | Deep-dive each involved area |
| Research | `researcher` | 0–3 | Technology / patterns / domain landscape |
| Design | `architect` | 0–1 | ADR or system design (when delegated) |
| Review | `reviewer` (focus `spec`) | 1 | Plan ↔ design consistency |
| Review | `architect` | 0–1 | Trade-off honesty (one-way-door) |
| Review | `researcher` | 0–1 | SOTA / known-pitfall gap check |
| Adversary | configured adversary skill (`plan-artifact`) | 0–1 | Cross-model review |

A project's `tiers.hex-plan.<tier>.counts` can override any Count cell above
against the baseline this table sets
([`config.md` § Merge rules](../hex-core/references/config.md#merge-rules)).

Concurrency cap and degraded mode:
[`protocol.md`](../hex-core/references/protocol.md#worker-coordination). The
adversary skill name comes from `hex.md › Preferences`
([adversary contract](../hex-core/references/adversary.md#adversary-contract));
`codex-adversary` is only an example value.

## Project rules and conventions

hex never carries a hardcoded rule or subsystem table. Every phase that needs
the project's invariants, verification command, or artifact conventions
discovers them from **project context** (the client's ambient instructions /
project rules), cached in the Pointers section of
`.agents/memory/hex.md`
([`memory.md`](../hex-core/references/memory.md#the-three-sections)). "Verify"
anywhere below means **run the project's documented verification**
([`verify.md`](../hex-core/references/verify.md#verification)) — the
work-package table's `Verify` column is the exception: its cell grammar is
the plan template's (C-905).

## The plan artifact

**Location.** Write the plan where the project's documented plan/ADR
conventions say (discovered from project context, cached in
`hex.md › Pointers`). When nothing is documented, use the default artifact
home `.agents/plans/`
([`memory.md`](../hex-core/references/memory.md#location-and-resolution)).
Record the active-plan pointer in `hex.md › Memory`. The plan template shipped
with `/hex-init` is the scaffold; the project's own format wins when it has
one. A plan carrying a `Repo` column lives in the **lead only** — one
artifact, one Status block — with its active-plan pointer recorded solely in
the lead's `hex.md › Memory`
([`memory.md`](../hex-core/references/memory.md#federation-satellites)).

**Status block.** Every plan opens with a self-contained status block the
execute and review skills read and mutate — no external state file:

```markdown
## Status
- State:   plan-approved      <!-- planning → plan-approved → executing → review → done -->
- Tier:    high
- Tier-grammar: 5
- Effective-tier: derived
- Updated: 2026-07-19
- Next:    /hex-execute <this plan path>
```

hex-plan initializes it at `plan-approved` on handoff, writes
`- Effective-tier: derived` and `- Tier-grammar: 5` (the tier grammar the
`Tier:` value was written in, [`protocol.md` § Tier grammar](../hex-core/references/protocol.md#tier-grammar), `adr_0017` C-997)
into every new plan's Status block, and records
the pointer in `hex.md › Memory`; `/hex-execute` advances `State` and `Next`
as it runs. The field is written explicitly rather than left absent because
every ecosystem this marker copies tells authors to set it by hand, so the
absent case's meaning can never safely change later
([`decompose.md`](../hex-core/references/decompose.md#the-effective-tier)
holds the value's semantics).

**Required content** (every tier — the tier files scale depth, not presence):

- **Component contracts** — the public surface touched (types, signatures,
  error variants) with expected behavior and edge cases, testable enough that
  a tester could write failing tests from them without reading any code.
  Numbered `C-001, C-002, …`; UX scenarios below are numbered
  `S-001, S-002, …` — both are the coverage join keys checked at review and
  convergence
  ([`protocol.md`](../hex-core/references/protocol.md#traceability-ids)).
- **User-experience scenarios** — action → expected outcome → error cases for
  each user-facing behavior.
- **Executable phases** — a Stub → Specify → Implement → Review cycle per
  task, runnable by `/hex-execute` without further decomposition.
- **Parallelization** — decomposed to maximize parallel execution
  ([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)):
  a work-package table (id, repo, scope, expected files, size, wave,
  depends-on, review and verify — the optional `risk` review hint and
  the `scoped | full` verify budget, one budget over the WP's merge gate and
  the Review-Fix Loop's exit gate that immediately precedes it, and nothing
  beyond those two — and status, initialized `pending`), its Scope column
  citing the C-/S- IDs each WP covers,
  a wave-grouped mermaid `graph TD` as its visual index
  (the table stays canonical), the critical path, a "Shippable after wave:
  N" line (tier medium and below exempt — single WP), the serialized topological-order
  merge plan (waves derived), and — when fewer parallel WPs than
  file-disjointness allows, or a sub-overhead WP stays isolated — a
  one-line justification
  ([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
- **Open questions** — unresolved ambiguities as `[NEEDS CLARIFICATION: …]`
  markers, **hard cap 3**. More than three means the target is underspecified;
  raise it at the gate rather than guessing.

## Constraints

- Every task carries **testable acceptance criteria** — no vague behaviors.
- **Discover runs at every tier** — never assume context.
- **Never skip Review**; never exceed the concurrency cap
  ([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).
- **No mid-flow questions** — ambiguity is resolved at the single gate.
- **Persist substantial research** (more than a paragraph) as a research
  artifact in the convention-resolved location.
- **Always** include component contracts, UX scenarios with error cases, and
  the Parallelization section.
- **Always** initialize the plan's Status block and record the active-plan
  pointer in `hex.md › Memory`.
- **Constitution gate** — when project context names a constitution (cached
  in `hex.md › Pointers`), the Design and Review phases gate the plan
  against it
  ([`protocol.md`](../hex-core/references/protocol.md#constitution-gate));
  violations need a Constitution Deviations row.
- **Never commit and never push** — this skill plans only.
- **Upkeep** ([`protocol.md`](../hex-core/references/protocol.md#upkeep-step)):
  as the final phase, re-point any `hex.md › Pointers` entry this run
  revealed as drifted (a changed verification command, a new artifact home)
  and update `hex.md › Memory`; a perspective gap belongs in
  `hex.md › Memory` as a note for the next `/hex-init` run, never written to
  `hex.md › Preferences` directly.

## Handoff

The [handoff contract](../hex-core/references/protocol.md#handoff-contract)
applies: this block is the run's required final message; one optional
proceed question may follow it.

```markdown
## Plan Complete: <feature | "Resolves #N">

### Classification
- Scope: small | medium | large
- Reversibility: two-way | one-way (medium) | one-way (high)
- Tier: low | medium | high | xhigh | max
- Overlays: architect=<inline|on>, research=<skip|1|3>, adversary=<on|off>

### Artifacts
- <plan path> (Status block initialized; active-plan pointer in hex.md › Memory)
- <research artifact path(s)>
- <ADR path> (one-way-door only)

### Executable phases (for /hex-execute)
- Stub: components to create as the public surface
- Specify: tests to write from the design record
- Implement: stub bodies to fill
- Review: perspectives to run
- Parallelization: <N> WPs in <M> waves; critical path <WP a → WP b>

### Deferred findings (need human judgment)
- Review panel: …
- Cross-model review: …

### Next step
    /hex-execute <plan path>
```

Consumers: `/hex-execute` (the plan artifact); the human (deferred findings).

$ARGUMENTS
