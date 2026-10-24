# hex Swarm Protocol

The shared vocabulary and contracts for every hex orchestrator. Roles are
defined in [`workers.md`](workers.md); model classes in
[`models.md`](models.md); the memory file in [`memory.md`](memory.md).

**This file is the spine.** Six sibling files carry the contracts a mode
opens only when it runs them; everything every mode needs stays here.

| Sibling | Sections it now holds |
|---|---|
| [`loop.md`](loop.md) | The Review-Fix Loop, Convergence contract |
| [`decompose.md`](decompose.md) | Parallel-by-default decomposition |
| [`worktree.md`](worktree.md) | Pipeline worktree mechanics |
| [`verify.md`](verify.md) | Verification |
| [`adversary.md`](adversary.md) | Adversary contract |
| [`severity.md`](severity.md) | Finding severity |

**The load map is a budget, not a permission.** A mode may follow any link;
it *opens* a topic file when a phase it is running executes against that
contract, never because prose mentions it. Two corollaries make the rows
reproducible: a citation the citing file carries with the one-clause
qualifier the single-source rule allows — never a restatement — is
provenance, not an open; and defining a word is not running a phase — four
`SKILL.md`s define `"Verify"` by linking [`verify.md`](verify.md), yet a
mode opens it only when a phase it runs actually verifies. Considered and
excluded on the first: `/hex-plan`'s four [`worktree.md`](worktree.md)
citations for the mandatory Parallelization section and the serialized
merge plan — every contract they reach is execution-time, unreachable from
a plan-authoring phase, and each citing file enumerates the table's columns
in situ. This is [`workers.md`](workers.md)'s **Load only what runs** one
layer up.

| Mode | Opens beyond the spine |
|---|---|
| `/hex-plan` | `decompose.md`, `loop.md` |
| `/hex-execute` | `decompose.md`, `worktree.md`, `loop.md`, `verify.md` |
| `/hex-review` | `decompose.md`, `loop.md`, `severity.md` |
| `/hex-architect` | `loop.md` |
| `/hex-finalize` | `verify.md` |
| any of the above with `adversary=on` | `+ adversary.md` |
| any of the above composing a review brief | `+ checklist.md` — read by the orchestrator, inlined into the brief; the seat never opens it |
| `/hex-review` on a federated target | `+ worktree.md` |
| `builder` worker | `verify.md` only |
| `reviewer` worker | `severity.md` only |
| `simulator` worker | `verify.md` only |
| every other worker persona | — |

## Shared shape

Every orchestrator runs the same outer loop:

> parse args → classify tier → resolve overlays → **single meta-plan
> approval gate** (never mid-flow questions) → announce the resolved config
> with per-axis source attribution → dispatch to the tier's phases.

Each skill ships this as a `SKILL.md` dispatcher plus `classify.md`,
`overlays.md`, and `tier-{low,medium,high,xhigh,max}.md` files —
except `/hex-execute`, which has no tier: it skips classify and ships none.

## Tier grammar

Five tiers plus `auto` (`adr_0017` C-991). The grammar serves `/hex-plan`,
`/hex-architect` and `/hex-review`; `/hex-execute` has no tier:

| Tier | Intent | Typical spawns | Gate depth |
|---|---|---|---|
| `low` | Trivial two-way door: one file, ≤30 lines, no structural marker, no security-sensitive or hot path | **none** — the orchestrator is the worker, inline | 1 approval; the orchestrator answers the `spec` + `quality` checklist itself; no adversary |
| `medium` | Two-way door: flag/option change, doc edit, ≤3 files, one area | 1 explorer; inline design; 1 reviewer, single pass | 1 approval; no adversary |
| `high` | One-way-door, medium blast radius: new command, new storage/index layout, 1–2 areas | architecture-explorer + 2–4 explorers; 0–1 researcher; architect only for a one-way-door decision no accepted ADR covers; 1 reviewer, or the panel for a one-way-door ADR | 1 approval; `full` checklist; adversary on one-way-door signals |
| `xhigh` | One-way-door, high blast radius: new module/package, breaking API, protocol change | `high` set; 3 research axes only when the user names the tier | 1 approval; `adversarial` checklist; adversary on |
| `max` | Everything `xhigh` is, bought explicitly: **every** configured adversary, five research axes with `competitive-research` mandatory, and usage simulation by four user patterns | `xhigh` set + one `researcher` per extra axis + `simulator` ×4 | `xhigh`'s, plus the simulators' one merged fix pass; **never auto-selected** |
| `auto` (default) | Classifier picks the **lowest** tier that fits, `low` … `xhigh`; never `max` | — | — |

`auto` is the default; the classifier resolves it to one of the four
auto-selectable tiers and shows its reasoning at the gate. **`max` is
explicit only** — `--tier=max`, or a plan whose Status block says
`Tier: max` — because its cost is the point.

Tier **vocabulary** is fixed to the rows above: `low` / `medium` / `high`
/ `xhigh` / `max` plus `auto`. Only tier **content** — a tier's phase
counts and inherited baseline — is project-redefinable, via the `tiers`
key ([`config.md`](config.md#tiers)).

**The grammar a plan was written in** (`adr_0017` C-997). `adr_0017`
shifted every pre-existing tier one step up — old `low` → `medium`, old
`medium` → `high`, old `high` → `xhigh` — and inserted the inline `low`
below them. A plan's `Tier:` is read under the grammar it was written in:
`/hex-plan` writes `- Tier-grammar: 5` into the Status block, and a plan
**without** that line has its `Tier:` shifted one step up on read, disclosed
on the `Tier:` line's own source — `Tier: medium (plan, pre-adr_0017) →
high`. The same rule reads `hex.md › Preferences`: a block whose
`# hex config, vocabulary vN` comment is `v3` or lower has every
`tiers.<skill>.<tier>` and `workflows.<skill>.<tier>` segment shifted the
same way on read, announced once at the gate
([`config.md`](config.md#key-vocabulary)). Never a rewrite of the plan or
the block, never a refusal.


## Overlay grammar

Overlays are single-axis modifiers layered on the resolved tier — each
adjusts exactly one axis (for example: research depth, force/skip the
adversary pass, the architect's model). Overlays stack. A user-supplied
overlay always overrides the classifier-inferred value for that axis. The
concrete axes are defined per skill in that skill's `overlays.md`; this
file defines only the grammar.

## The meta-plan approval gate

Exactly **one** approval point, before any work starts. The orchestrator
never asks mid-flow questions — ambiguity is resolved here or by a
documented default, never by interrupting a running swarm. **This
single-gate rule scopes to the four orchestrators** (`hex-plan`,
`hex-execute`, `hex-review`, `hex-architect`); five skills are exempt,
each named here with its own stated ground and no criterion to
interpret — `/hex-init`, a configuration wizard, not an orchestrator,
which spawns nothing; `hex-discuss`, which keeps exactly one
approval gate, positioned at the drain, with workers that are read-only,
capped by its own contract (C-706), and never on the critical path, so
there is no swarm to strand; `/hex-loop`, which spawns nothing and starts
nothing, writes one goal file inside its own home, and whose approval is
the user's paste of the prompt it prints; `/hex-retro`, which spawns
nothing, whose one gate is a single structured question at the point
it would apply a local edit, and whose loop-mode edits land before
the loop branch's closing `/hex-review` and pass it and the human's
PR merge; and `/hex-finalize`, whose single approval gate is
positioned at the local/remote boundary on every degrade
rung and asks there — except under C-805a
([`finalize.md`](finalize.md#consent-model)), where it prints its disclosure
and proceeds — because the concrete commit plan it must disclose does not exist until
the rewrite is computed, and everything before that gate is local apart
from one read-only fetch and a credential probe, mutates nothing on any
remote, spawns nothing, and is undone from the backup ref — so there is
no swarm to strand and nothing on any remote has changed (see
[`finalize.md`](finalize.md#consent-model)). The list is closed — a skill
not named here is not exempt, whether or not it spawns workers, and a
sixth member is added by amending this sentence, never by analogy.

The gate announces the fully resolved config, each item attributed to its
source (`classifier` / `hex.md preference` / `user flag` / `tier baseline`
/ `derived`):

```
Tier: high            (auto — classifier: new subcommand, 2 areas)
Overlays: research=3     (user flag)
          adversary=on   (hex.md preference: one-way-door signals)
Spawn set:
  architecture-explorer  (tier baseline)
  explorer ×3            (tier baseline)
  researcher ×3          (overlay research=3)
  architect              (tier baseline)
  reviewer: quality, security, spec   (tier baseline + hex.md preference: src/auth/**)
Models: <standard model> default; architect + reviewer:security →
        <deep model> (hex.md preference instantiated — see models.md)
Adversary: codex-adversary, plan-artifact scope   (hex.md preference)
Limits: workers 8 (clamped from 12 · hex.md preference) · loop rounds 1
        (loop rounds · level default — a stored limit or a batched phase
        shows here with its source)
Degraded: blocking adversary call — no pollable output; process_only backstop 15 min
```

The `Limits:` line appears at every orchestrator's gate whenever a
`hex.md › Preferences` limit is in force **or** a phase's resolved set
exceeds the effective worker cap (a batched phase — see [Worker
coordination](#worker-coordination)); it is omitted only when neither
applies and the shipped defaults run unmodified. A blocking adversary call
announces itself on a `Degraded:` line instead ([§ Adversary
contract](adversary.md#adversary-contract)).

**Clamp grammar, one shape for every limit.** A configured value above a
limit's shipped or tier ceiling prints as `<name> <effective value>
(clamped from <configured value> · <source>)`; an unclamped limit keeps the
plain `<name> <value>` shape with its source in the line's trailing
attribution. `<name>` is the limit's leaf key with `limits.` dropped and its
hyphens read as spaces — `loop-rounds` → `loop rounds`,
`adversary-timeout` → `adversary timeout`; `max-workers` is the one
shortening, rendered `workers` as the block above shows. The derivation
governs this line and nothing else — `hex.md`'s own Preferences prose spells
the config keys themselves ([`memory.md`](memory.md)). A mode-(b) run in a
project that set `adversary-timeout: 45` therefore renders

```
Limits: adversary timeout 5 (clamped from 45 · hex.md preference)
```

This is the rendering every "announced as clamped" clause resolves to —
[`max-workers`](#worker-coordination) and
[`limits.adversary-timeout`](config.md#key-vocabulary) alike.

**Federation announce block.** When a plan carries a `Repo` column, the gate
gains one block **immediately after the last `Degraded:` line**, in the same
`<label>: <resolved value> (<source>)` shape:

```
Federation: lead `.` + mirror, mcp — 3 repos, 4 pipelines (2 satellite)
            ../acme-mirror on `feat/pypi-mirror` — unrelated in-flight work
            back-pointer to be written in: mirror, mcp   (hex.md Pointers)
```

Line 1 is mandatory (the lead, every distinct `Repo` value in table order,
then repo / pipeline / satellite-pipeline counts). Line 2 appears once per satellite found
on a non-trunk branch (C-304). Line 3 appears only when a back-pointer will be
written that does not already exist — the C-308 disclosure at the **existing**
gate, never a second gate. The C-303 pre-flight echo lines sit under this block
at execution. Absent a `Repo` column the whole block is absent and every other
line is unchanged. (C-315)

**Config-disclosure lines.** When a `hex.md › Preferences` config block
changes what the run does, the announce block prints one disclosure line per
change — this is the single statement of the trigger set; the four
orchestrator SKILL.md announce steps show a per-skill example and link here,
never restate it:

- `tiers` amended the layer-1 baseline → one `[project-redefined:
  <phase>.<role> <shipped>→<resolved>]` line per delta, **printed even when
  the resolved value equals the shipped one** (e.g. an `inherits` that happens
  to match). An amendment with no printed line is a spec violation, the same
  standard [`models.md`](models.md#rules) sets for a silent escalation.
- a preference-added spawn was displaced at the phase ceiling → one
  `[<role> dropped — phase ceiling N reached]` per drop
  ([`config.md`](config.md#merge-rules) rule 6).
- a reduced concurrency cap batched a phase → one
  `[<phase> batched N+M — concurrency cap C (hex.md)]`.
- a `never: [reviewer:security]` suppression failed closed → the
  `Error:` / `Fix:` refusal pair ([`config.md`](config.md#merge-rules) rule 5).
- a `hex.md › Preferences` model override that raises a spawn above its
  class cell → one line naming the role, the class, and `hex.md preference`
  as the source; no override is blocked, weakened or reordered.

See [`config.md`](config.md#tiers).

(`codex-adversary` is only an example value — the adversary skill name
always comes from `hex.md › Preferences`; see the
[adversary contract](adversary.md#adversary-contract).)

**Models line contract.** Shipped files name only capability classes and
placeholders; the **running orchestrator resolves each class to the
literal model** ([`models.md`](models.md)) and the
announce block prints the resolved literal per spawn, with every
escalation above a matrix cell carrying its announced reason — a silent
escalation is a spec violation ([`models.md`](models.md#rules)).

The user approves, adjusts, or cancels. On a client with native
plan-approval, use it; otherwise present the block as a single structured
question. **Under `/hex-loop` the gate never prompts**: the loop's pasted
prompt is the approval, so the orchestrator prints the announce block and
proceeds. The gate is also where the user can add or drop perspectives and
choose research axes.

Open questions travel with recommendations: every
`[NEEDS CLARIFICATION: <question>]` marker in the target or draft
artifacts carries a `Recommended: <answer> — <reason>` line, and the gate
presents each question together with its recommendation. A plain approval
accepts every recommendation as-is — the user spends attention only on
the ones they want to change. The hard cap of 3 open markers per artifact
is unchanged.

## Spawn-selection precedence

Three inputs decide which workers and perspectives run; later wins:

1. **Shipped tier baseline** — the tier file's default perspective set,
   with roles from [`workers.md`](workers.md) and class defaults from
   [`models.md`](models.md).
2. **Project hints** — the Preferences section of
   `.agents/memory/hex.md`: always-on perspectives, research axes,
   path-triggered escalations, model overrides. The classifier folds these
   into its suggestion. **Hints populate phases; they never define them** —
   project hints contribute perspectives, research axes, path-triggered
   escalations and model overrides; they
   **never add, remove, or reorder a phase or stage**.
   A hint naming a phase absent from the
   resolved tier file is **dropped** by [`config.md`](config.md#merge-rules)
   merge rule 9 — warn-once, that key only, run continues.
3. **User, at the gate** — the single approval point offers perspective and
   researcher selection; non-interactive flags override.

Skills never hardcode a spawn list beyond the tier baseline. The announce
block always shows the resolved set with per-item source.

`tiers` (and, in v2, `workflows`) do not add a fourth input — they
**rewrite layer 1** before layers 2 and 3 above apply
([`config.md`](config.md#merge-rules)). The **phase set is the resolved tier
file's**, and layer 1 is the only place it is rewritten.

**Running a stage absent from the resolved tier file is a spec violation**,
announced to the same standard [`models.md`](models.md#rules) sets for a
silent model escalation. A hint dropped under merge rule 9 is no licence to
run its phase anyway: the tier file *is* the phase set.

**Persona loading.** The orchestrator always reads the
[`workers.md`](workers.md) index; it loads a full persona file
(`workers/<role>.md`, or a project-local `.agents/workers/<role>.md`)
**only for roles in the resolved spawn set** — never the whole registry.
Project-local personas fold in as project hints (layer 2 above) and never
override a shipped role of the same name.

## Worker coordination

The orchestrator decomposes work, dispatches workers using the
spawn-prompt templates in [`workers.md`](workers.md), and synthesizes their
structured returns. Workers never share state. **Two orchestration levels**:
the orchestrator, which dispatches every step and review seat and merges,
and the workers it spawns. Workers are leaves and never spawn. The depth is
**hex-enforced** — the spawn prompts bound it, not the harness.

- **Workers never wait.** A worker that would wait on a lock, a gate or a poll
  returns instead, stating what it needs. Heavy or exclusive tools (bazel
  servers, a docker acceptance project) are **gate-only**
  ([`resources.md`](resources.md)); a step that needs one hands it to the
  orchestrator in the background and does not wait for it. No shipped text and
  no learned project lesson may add a worker-side lock.
- **The build-job budget.** Parallel builds share **one build's** budget: live
  pipelines × jobs per pipeline ≤ the project's single-build jobs. Shared
  caches do not substitute for it.
- **Concurrency cap: at most 8 workers at once**, counting agents doing model
  compute. Dispatch a concurrent batch in a single
  step; keep prompts focused for fast startup. The effective number of live
  pipelines is `min(|ready set|, effective max-workers, single-build jobs ÷
  jobs per pipeline)` — those three terms and no fourth. This is the
  invariant's one home; [Parallel-by-default
  decomposition](decompose.md#parallel-by-default-decomposition) reads against
  it and restates none of it.
- **A `max-workers` value in `hex.md › Preferences` lowers the cap and
  never raises it**: the effective cap is `min(8, max-workers)`, the ceiling
  [`config.md` merge rule 9](config.md#merge-rules) clamps and announces
  against. The cap bounds **how many workers run at once, not how many a phase
  spawns** — a phase whose resolved set exceeds it runs in **sequential
  batches of at most the cap, in declaration order**, and the phase gate holds
  until the last batch returns; it never silently drops a perspective the tier
  baseline calls for, and it announces the batch split and the cap's source.
  **Federated (a plan carrying a `Repo` column):** the cap counts **across all
  participating repos**, and the **lead's** `max-workers` is the only one read
  — a satellite's `hex.md` is never resolved (C-318,
  [`memory.md`](memory.md)).
- **The phase ceiling is a separate limit from the cap above** — it
  bounds what preference-added spawns (`always` entries, `counts` above
  baseline) may add to a phase. Its displacement procedure is defined
  once, in [`config.md` § Merge rules, rule 6](config.md#merge-rules).
- Workers return structured results; the orchestrator does the synthesis —
  it never dumps raw worker output onward.
- **Federation adds no recursion level (C-319).** The orchestrator that owns
  a federated plan is *the* orchestrator; it drives satellites with `git -C`
  and ordinary workers and **never spawns a per-repo `/hex-execute`**. A
  Satellite pipelines are driven by the same orchestrator. Vacuous without a `Repo` column.
- **Event-driven, never polling.** A worker's completion notice wakes the
  orchestrator; its main loop only dispatches and merges, and anything slow
  runs in the background. The fallback is a wake at most every **20 minutes**
  (never faster) that re-reads the plan's Status column, dispatches whatever
  is ready, and treats a worker that returned no notice as a failed step —
  [`models.md`](models.md) owns the escalation sequence.

**Degraded mode (no subagent spawning).** On a client that cannot spawn
subagents, the orchestrator runs each worker prompt block **inline, one at
a time**, in the same session — same perspectives, same contracts,
sequential instead of concurrent. Announce it at the gate as
`Degraded: inline workers — no subagent spawning`.

**Degraded mode (no per-spawn model override).** On a harness where all
agents in a session share one model (e.g. Copilot), the per-role matrix is
**advisory only** — every worker runs the session model. Announce at the
gate as `Degraded: single session model — no per-spawn override; matrix
advisory`, naming the one resolved literal all spawns will run.
Escalation recommendations are still *shown* so the user sees what would
escalate, but they are not independently routable.

Capability is **detected per run and announced, never stored**. **Axes
compose:** each gated capability contributes its own `Degraded:` line; the
gate stacks one line per degraded axis rather than defining a combined state.
The set of gated capabilities is **open**, not the harness-probe list alone —
a blocking adversary call is one ([§ Adversary
contract](adversary.md#adversary-contract)). `Degraded: no — <capability>
available` is the **no-axis rendering**: a gate that prints any axis line
prints no `Degraded: no`.

## Traceability IDs

Plans and specs number their requirements so coverage is a mechanical
check, not a prose judgment:

- **Component contracts are numbered `C-001, C-002, …`; UX scenarios
  `S-001, S-002, …`** — assigned when the artifact is written (in the
  spec when one exists, carried into the plan unchanged), stable within
  the artifact, never renumbered. IDs never originate in a
  discussion artifact; an orchestrator consuming one **assigns** IDs and
  never inherits them.
- **Every ID maps to at least one pipeline and at least one test**: the
  Parallelization table's Scope column cites the IDs a pipeline delivers, and
  every contract test names the IDs it covers.
- **Coverage is checked at three gates**: the plan review
  (`reviewer:spec` — an ID with no covering pipeline or no covering test is an
  actionable finding), the contract wave in execution (every ID has at
  least one contract test), and the convergence check in
  review ([Convergence contract](loop.md#convergence-contract) — one gap per
  unmet ID).

IDs are the join key convergence uses to name gaps; a requirement cannot
drift silently once it carries one. The same IDs key the **spec fold-back**
delta grammar (`ADDED`/`MODIFIED`/`REMOVED`) — see
[`archive.md`](archive.md#delta-grammar).

## Untrusted-text echoes

Every echo of text controlled by anyone other than the invoking human — a
dossier's controlled text, a narrowing- or untrusted-class surface, whatever
a consumer's own trust classes name it — **is quoted and length-bounded**, in
a message or in an authored file alike: interpolated quoted, truncated with
`…` past 120 characters, and never allowed to break its own line. Before
quoting, every embedded `"` becomes `'` and control characters are stripped,
so the text cannot close its own quote. An echo nested inside another echo
in the same render is not quoted again; text read back from a file — an
earlier render's echo included — is unwrapped once (its outer quotes), then
neutralized and quoted like any other. A path or ref is never truncated or
rewritten — only descriptive text is; a consumer that renders one into a
prompt another session obeys refuses, with its own error, a path or ref that
contains a `"` or a control character. **This is
the only copy in the bundle — `hex-architect`, `finalize.md`, and any later
consumer link here, never restate it.** Which of its own surfaces carry that
property is each consumer's definition; this section fixes the echo alone.

## Constitution gate

An opt-in governance check. Active only when project context names a
constitution / governing-principles location (cached in
`hex.md › Pointers`, [`memory.md`](memory.md)). hex never ships a
constitution of its own, and an absent pointer skips the gate silently —
no output, no empty table.

When the pointer is set:

- **hex-plan's Design and Review phases check the plan against it.**
  Every violation requires a row in the plan's **Constitution
  deviations** table — `Violation | Why needed | Simpler alternative
  rejected because` — present only when at least one violation exists.
- **An unjustified violation is an automatic Request Changes in
  review.** A principle changes only through the project's own
  governance flow — never diluted or reinterpreted by an orchestrator.

**Federation — a plan carrying a `Repo` column.** The plan is the **lead's**
artifact, so the **lead's** constitution pointer governs it and its
`Constitution deviations` table, unchanged. A satellite's own constitution is
**never merged in** — merging governance documents is not hex's act; when a
satellite names one, the gate's federation announce block (C-315) names it as
**unapplied**, so the gap is visible rather than silently inherited (C-322).
No new gate, no new table.

## Handoff contract

Every orchestrator run ends with its skill's handoff block — **the
required final message of the run, never omitted or summarized away**. A
degraded, escalated, or partially-completed run still emits it, stating
what stands. After emitting it, the orchestrator MAY ask **one** optional
proceed question (e.g. "run the `Next step` command now?") — the
single-gate rule governs *pre-work* approval and is not violated by a
post-completion offer. Never more than one question, and never a question
in place of the block.

**Retro nudge.** When the retro inbox ([`hex-retro` §
Entry](../../hex-retro/SKILL.md#entry)) holds at least the nudge count
([§ Thresholds](../../hex-retro/SKILL.md#thresholds)) of top-level
`*.json` entries — counted, never parsed — the block's last line is
`Retro inbox: <N> entries — /hex-retro`. No inbox, below the count, or
`hex-retro` not installed prints nothing.

**An execution run's block carries a timing rollup — six figures, read from
the plan the run just wrote** (the schedule log's `step` and `merged`
lines, [Parallel-by-default
decomposition](decompose.md#parallel-by-default-decomposition)): total wall clock,
per-pipeline wall clock, per-step wall clock, the work/wait split,
review calls and rounds, and gate time — the last two from the
orchestrator's own `date -u +%FT%TZ` brackets around each review call and
each gate, which no `<pipeline>/<n>` line can carry. **Where the `step` lines are absent or partial the block
names which figures are missing** rather than dropping
the rollup or estimating them — an absent line is a run that predates it,
not an error. The tier files link here and restate none of it.

## Upkeep step

Every orchestrator's **final phase** re-points any `hex.md › Pointers` entry
this run revealed as drifted — a verification command that changed, a new
artifact home, a moved rule file — and updates `hex.md › Memory` (active
plan pointer, artifact index, learned facts). On a review run reaching a
plan's **terminal review state** (`State: done`, or `landing` for a plan
carrying a `Repo` column), that update **clears** the active-plan pointer —
the first clear in the bundle; the archive mechanic and what is recorded are
defined in [`archive.md`](archive.md#plan-archive) (C-410).
Verify-on-consumption already
repaired whatever a phase acted on mid-run; upkeep sweeps the remainder.
`hex.md › Preferences` is user-owned and is **never edited here**: a
preference the run surfaced is recorded in `hex.md › Memory` and proposed at
the next `/hex-init` run. Two candidate classes, named so a run knows what
to record: a perspective that should become an always-on
hint; and a finding class that recurred across seats or runs and belongs in the
project's own rules as a checklist item
([`checklist.md`](checklist.md#composition)). This is part of the flow (portable, no hooks needed); because the file
holds pointers rather than copies, upkeep is cheap. The section specs and
staleness rules are in [`memory.md`](memory.md).

The upkeep duties in this section belong to the four orchestrators; a
**non-orchestrator hex skill makes an upkeep write only where its own
contract names one**: `hex-discuss`'s post-gate discussion hand-off
record and index rows (C-708) and `hex-retro`'s candidate lines (C-1444)
are the only such cases.

`hex-retro`'s candidate lines, in `hex.md › Memory`: `- Retro candidate
(<perspective|review-value|checklist-item|project-context>): "<≤120
chars>" — ledger <id>, <report path>.` One line per ledger id (skip when
a line naming that id exists). `project-context` rides the C-711
promotion route; the other three ride this section's own three candidate
classes (C-990). No `hex.md` on disk → no candidate lines are written; a
report note says so.

**Which sections bind a spawning non-orchestrator skill.** Bound:
[Worker
coordination](#worker-coordination) — it governs spawning itself — and this
section, per the sentence above. Exempt: [Shared shape](#shared-shape)
(written for a skill that resolves a whole config up front; this one
resolves none); [The meta-plan approval gate](#the-meta-plan-approval-gate)
— but **only for a skill named in that section's own closed exemption list**;
for `hex-discuss`, the one named spawning member, the gate is relocated to the
drain, not removed, and its resolved-literal-model disclosure is carried in
substance by [`models.md`](models.md#rules) rule 1; and
[Spawn-selection precedence](#spawn-selection-precedence) — exempt in its
*carriers* (an announce block showing the resolved set, a user picking
perspectives at an entry gate) and in its layered precedence, moot for a
skill with no tier baseline and no overlays, while persona loading still binds (C-706). [Handoff
contract](#handoff-contract) binds in substance via the skill's own drain
block — its `Next:` command and terminal-state report, not the
orchestrator-specific fields.

**Federation — a plan carrying a `Repo` column.** Upkeep additionally makes
the **only** upkeep writes that leave the lead repo: for a plan in `landing`,
it offers to confirm each `Repos:`-ledger row from locally verifiable evidence
only — `git -C <repo> merge-base --is-ancestor hex/<plan-slug> <trunk>` (the
feature branch is contained in that repo's **local** trunk; hex never fetches
— except `/hex-finalize`'s single pre-flight fetch of the branch it finalizes
and its target, which pins the force-push lease and never informs a landing
claim (see [`finalize.md`](finalize.md#scope)) — and never infers landing from
anything weaker) — advancing the plan to `done`
only when **every** row is confirmed landed (C-324); a row's `landed` flag may
**also** be set by an explicit human override (C-324), not only by this
mechanical `--is-ancestor` check, so a satellite permanently unreachable from
this session is not a hard dead end; and, once a plan reaches
`done`, it removes that plan's slug from every participating satellite's
`Federation lead:` bullet, deleting the bullet — and the satellite's `hex.md`
file, if the bullet was its only content — when the slug list empties (C-313).
Removal on `done`, not `landing`, keeps the satellite lock across the
broken-integration window. The active-plan-pointer clear (above) is unchanged,
and `hex.md › Preferences` stays never-edited in every repo.
