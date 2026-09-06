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
| [`worktree.md`](worktree.md) | Worktree work-package mechanics |
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
| `coordinator` worker | `loop.md`, `decompose.md` — **and the spine**, which no worker persona loads by default |
| every other worker persona | — |

## Shared shape

Every orchestrator runs the same outer loop:

> parse args → classify tier → resolve overlays → **single meta-plan
> approval gate** (never mid-flow questions) → announce the resolved config
> with per-axis source attribution → dispatch to the tier's phases.

Each skill ships this as a `SKILL.md` dispatcher plus `classify.md`,
`overlays.md`, and `tier-{low,medium,high,xhigh,max}.md` files.

## Tier grammar

Five tiers plus `auto` (`adr_0017` C-991):

| Tier | Intent | Typical spawns | Gate depth |
|---|---|---|---|
| `low` | Trivial two-way door: one file, ≤30 lines, no structural marker, no security-sensitive or hot path | **none** — the orchestrator is the worker, inline | 1 approval; the orchestrator answers the `spec` + `quality` checklist itself; one optional `L1` backstop when a non-doc file changed; no adversary |
| `medium` | Two-way door: flag/option change, doc edit, ≤3 files, one area | 1 explorer; inline design; 1 reviewer, single pass | 1 approval; join-level review; no adversary |
| `high` | One-way-door, medium blast radius: new command, new storage/index layout, 1–2 areas | architecture-explorer + 2–4 explorers; 1 researcher; architect; review panel | 1 approval; join-level review, `full` `L2` checklist; adversary on one-way-door signals |
| `xhigh` | One-way-door, high blast radius: new module/package, breaking API, cross-area, protocol change | `high` set + mandatory architect, mandatory multi-axis research | 1 approval; join-level review, `adversarial` `L2` checklist; adversary a default part of the flow |
| `max` | Everything `xhigh` is, bought explicitly: **every** configured adversary, five research axes with `competitive-research` mandatory, and usage simulation by four user patterns | `xhigh` set + one `researcher` per extra axis + `simulator` ×4 | `xhigh`'s, plus the simulators' one merged fix pass; **never auto-selected** |
| `auto` (default) | Classifier picks `low` … `xhigh` from signals; never `max` | — | — |

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
`hex-execute`, `hex-review`, `hex-architect`); three skills are exempt,
each named here with its own stated ground and no criterion to
interpret — `/hex-init`, a configuration wizard, not an orchestrator,
which spawns nothing; `hex-discuss`, which keeps exactly one
approval gate, positioned at the drain, with workers that are read-only,
capped by its own contract (C-706), and never on the critical path, so
there is no swarm to strand; and `/hex-finalize`, whose single approval
gate is positioned at the local/remote boundary on every degrade rung,
because the concrete commit plan it must disclose does not exist until
the rewrite is computed, and everything before that gate is local apart
from one read-only fetch and a credential probe, mutates nothing on any
remote, spawns nothing, and is undone from the backup ref — so there is
no swarm to strand and nothing on any remote has changed (see
[`finalize.md`](finalize.md#consent-model)). The list is closed — a skill
not named here is not exempt, whether or not it spawns workers, and a
fourth member is added by amending this sentence, never by analogy.

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
Models: <fast-balanced model> default; architect + reviewer:security →
        <deep-reasoning model> (hex.md preference instantiated — see models.md)
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

**Per-WP effective tiers.** In a plan carrying the generation marker
([the effective tier](decompose.md#the-effective-tier)) `Tier:` announces the
**ceiling** and the block gains two lines: one attributing each work
package's own resolved tier to the inputs that produced it —
`WP1 medium (derived: S, no flags) · WP4 xhigh (derived: sec)` — and the budget
histogram that run prints,
`effective tier: medium 6 · high 2 · xhigh 1 (ceiling xhigh)`, whose grammar
lives at the histogram bullet in [Parallel-by-default
decomposition](decompose.md#parallel-by-default-decomposition) and is not restated
here. The value is recomputed at each WP's own spawn time, so gate-time
and run-start listings are snapshots, labelled as one.

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
Federation: lead `.` + mirror, mcp — 3 repos, 4 WPs (2 satellite)
            ../acme-mirror on `feat/pypi-mirror` — unrelated in-flight work
            back-pointer to be written in: mirror, mcp   (hex.md Pointers)
```

Line 1 is mandatory (the lead, every distinct `Repo` value in table order,
then repo / WP / satellite-WP counts). Line 2 appears once per satellite found
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
  effective-tier cell → one line naming the role, the class, the WP's
  effective tier, and `hex.md preference` as the source; no override is
  blocked, weakened or reordered.

See [`config.md`](config.md#tiers).

**Risk-flag degrade — a trigger class of its own, never a member of the
enumeration above.** A run in which any risk flag degraded ([the effective
tier](decompose.md#the-effective-tier)) prints one line, once, naming which flag, which
convention was unreadable, the consequence per flag — a degraded `sec` or
`hot` resolves that WP at the ceiling, a degraded `hub` floors it at
`min(T, high)` — and the one-line remedy. It is filed here rather than
in that list because an absent or malformed Pointers row is not a
`Preferences` config block, and filing it there would widen a closed
enumeration by analogy.

(`codex-adversary` is only an example value — the adversary skill name
always comes from `hex.md › Preferences`; see the
[adversary contract](adversary.md#adversary-contract).)

**Models line contract.** Shipped files name only capability classes and
placeholders; the **running orchestrator resolves each class to the
literal model** ([`models.md`](models.md) resolution order) and the
announce block prints the resolved literal per spawn, with every
escalation above a matrix cell carrying its announced reason — a silent
escalation is a spec violation ([`models.md`](models.md#rules)).

The user approves, adjusts, or cancels. On a client with native
plan-approval, use it; otherwise present the block as a single structured
question. The gate is also where the user can add or drop perspectives and
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
structured returns. Workers never share state, and never spawn workers —
with one bounded exception: a `coordinator` (spawned by the orchestrator for
a qualifying work package) fans out exactly one level of leaf workers within
its work package. Leaves never spawn. The depth chain is fixed at
orchestrator → coordinator → leaf and is **hex-enforced** — the spawn
prompts bound it, not the harness (harness nesting caps vary, and one is
unbounded). One level because hex enforces it uniformly and because spawn
cost and error amplification compound per level: a second level of fan-out
rarely clears the granularity gate.

- **Concurrency cap: at most 8 workers at once.** Dispatch a concurrent
  batch in a single step. Keep prompts focused for fast startup. The cap
  counts **recursively** — leaves running inside a coordinator count toward
  it — so the orchestrator hands each coordinator a **fan-out budget** no
  larger than its wave allocation, keeping the recursive total within the cap.
  **The cap counts live model-compute — agents in state `working`.** An agent
  in state `blocked` — waiting on its children, on a heavy slot, or on a lock
  — **does not occupy a slot**: it is burning no model compute, and charging a
  blocked parent against the very pool its children need is the documented
  nested-pool deadlock (the parent holds a slot while it waits on work that
  can only run in that same pool, so neither side ever advances). Recursive
  counting, the clamp above 8 and the federated single-lead read below are
  otherwise unchanged.
- **Allocation across the hierarchy: a guaranteed floor plus bounded
  borrowing, partitioned at schedule time.** Every live coordinator gets a
  floor of 1 leaf slot it can never be starved of, and may borrow above that
  floor up to a ceiling from the unused share of idle siblings. **Allocation
  is partitioned per wave at schedule time, never contended at runtime** —
  the fan-out budget above is handed out once per wave, never raced for.
  Effective parallel work-package count is
  `min(|ready set|, effective max-workers)` — those two terms and no third.
  **`limits.heavy` is never a term in that formula and never a cut applied to
  spawns**: it bounds the **concurrent heavy commands** the launched work
  packages may run, worker-side rather than at spawn time
  ([`resources.md` § 3](resources.md#3-the-heavy-semaphore) owns the
  mechanism). The two caps **compose rather than substitute** — one bounds how
  many work packages run, the other how many heavy commands run inside them.
  Blocked agents do not count, per the
  bullet above. This is the invariant's one home; [Parallel-by-default
  decomposition](decompose.md#parallel-by-default-decomposition) reads against it and
  restates none of it.
- **A `max-workers` value in `hex.md › Preferences` lowers the cap and
  never raises it**: the effective cap is `min(8, max-workers)`, counted
  recursively like the shipped 8 — that 8 is the ceiling [`config.md` merge
  rule 9](config.md#merge-rules) clamps and announces against. The cap
  bounds **how many workers run at once, not how many a phase spawns** —
  a phase whose resolved set exceeds the effective cap runs in **sequential
  batches of at most the cap, in declaration order**, and the phase gate
  holds until the last batch returns (the same cap bounds WP launch —
  [Parallel-by-default
  decomposition](decompose.md#parallel-by-default-decomposition)); it never silently
  drops a perspective the tier baseline calls for, and it announces the
  batch split and the cap's source. **Federated (a plan carrying a `Repo`
  column):** this cap counts **across all participating repos**, and the
  **lead's** `max-workers` is the only one read — a satellite's `hex.md` is
  never resolved (C-318, [`memory.md`](memory.md)), so the effective cap
  stays `min(8, lead's max-workers)`.
- **The phase ceiling is a separate limit from the cap above** — it
  bounds what preference-added spawns (`always` entries, `counts` above
  baseline) may add to a phase. Its displacement procedure is defined
  once, in [`config.md` § Merge rules, rule 6](config.md#merge-rules).
- Workers return structured results; the orchestrator does the synthesis —
  it never dumps raw worker output onward.
- **Federation adds no recursion level (C-319).** The orchestrator that owns
  a federated plan is *the* orchestrator; it drives satellites with `git -C`
  and ordinary leaf workers and **never spawns a per-repo `/hex-execute`** —
  that would put an orchestrator under an orchestrator and break the fixed
  `orchestrator → coordinator → leaf` chain. A `coordinator` may own a
  satellite WP under its unchanged gate, with sub-WPs in the parent's repo;
  the join hierarchy stays three levels. Vacuous without a `Repo` column.

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
Escalation recommendations (e.g. `reviewer:security →
deep-reasoning`) are still *shown* so the user sees what would escalate,
but they are not independently routable.

**Fan-out mechanism (coordinator recursion).** Two capabilities are detected
per run — **never stored** — and gated on the **capability class, not the
primitive name** (the harness surface churns). At Schedule time the
orchestrator picks the coordinator's fan-out mechanism:

1. **`programmatic orchestration`** available → the **preferred** mechanism:
   coordinators fan out via orchestration primitives inside the WP's single
   worktree (no sub-refs).
2. else **`nested subagent spawning`** available → the **fallback**:
   coordinators fan out via nested subagent spawns (watch session budget and
   ref collision; isolation only for true-isolation sub-WPs).
3. else **degraded flattening**: no coordinators — every WP runs a single
   builder plus its `L1` leaf review. Announce:

```
Degraded: flat execution — no nested spawn; coordinators inlined
```

This stacks with the other degraded lines; a harness with neither nesting
nor per-spawn override announces both:

```
Degraded: flat execution — no nested spawn; coordinators inlined
Degraded: single session model — no per-spawn override; matrix advisory
```

Capability is **detected per run and announced, never stored** — same
pattern as the existing subagent-spawn gate. **Axes compose:** each gated
capability contributes its own `Degraded:` line; a harness lacking both
subagent spawning and per-spawn override prints both (inline workers *and*
single session model). The gate stacks one line per degraded axis rather
than defining a combined state. The set of gated capabilities is **open**,
not the harness-probe list alone — a blocking adversary call is one
([§ Adversary contract](adversary.md#adversary-contract)). `Degraded: no — <capability>
available` is the **no-axis rendering**: a gate that prints any axis line
prints no `Degraded: no`.

**Preflight before every spawn wave.** A **spawn wave** is the launch of one
ready-order batch — the eligible work packages the ready-set releases
together, bounded by the cap above ([Parallel-by-default
decomposition](decompose.md#parallel-by-default-decomposition)). Before **every** spawn
wave the orchestrator runs three checks, in under 2 seconds total:

| Check | Source | Trips when |
|---|---|---|
| Disk headroom | the worktree and scratch volumes | free space is under `max(5 GB, measured per-worktree artifact size × the wave's width)` — 5 GB is a floor, never the threshold |
| Memory pressure | `/proc/pressure/memory`, `full avg10` | it reads above 10 % |
| Stale worktrees | the plan's active set | a worktree exists that the plan does not list |

On any trip the orchestrator **holds, never spawns**: wait 60 seconds and
re-check, at most 3 times, then surface to the human naming the tripped check
rather than holding forever — an orchestrator that waits indefinitely on a
machine the human is also using is a hang, not a safeguard. **A missing check
is never a passed check**: a host whose source does not exist
(`/proc/pressure` is Linux-only) skips that check and **announces the reduced
check set**, because the whole value of the preflight is that its output is
believable. Preflight lives here rather than in `resources.md` because it is
**scheduling, not a resource knob** — two of its three checks do not care
whether the gate is heavy, and `resources.md` is conditional-load.

**Heavy commands — see [`resources.md`](resources.md).** That file is
**conditional-load**, in the shape [`config.md`](config.md) already uses: read
it **only when a run will issue a heavy command**, so a parse-only project
never pays its bytes. It owns the measured profile, the semaphore, the
per-run scratch, the containment ladder, the knob sheet, teardown and the
[output signals](resources.md#9-output-as-a-resource-signal) — and **not**
preflight, which is scheduling and is defined above. One thing to know here: a
heavy command takes one of `limits.heavy` `flock` slots
([`resources.md` § 3](resources.md#3-the-heavy-semaphore)).

### Worker liveness

Every live agent maintains one heartbeat file, so the orchestrator can tell a
working agent from a hung one. **This section is the sole definition of that
contract**; every other file links here rather than restating it.

**The file — one flat directory per run, one JSON object per live agent.**

```
${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/hb/<agent-id>.json
```

**It sits outside every checkout, and that is a security decision, not a
tidiness one.** A fixed in-tree path that is *read as a control surface* is
plantable by the repository under work: a hostile clone ships a beat naming
itself a child of the orchestrator, ages it to L3, and its attacker-authored
`checkpoint` string is interpolated into a re-spawn prompt as "where to
resume"; a planted directory symlink redirects the write out of the checkout
entirely. `<run-id>` removes a second defect the in-tree path had — two runs
sharing one checkout collided on a single per-role filename.

**Paths come from the spawn prompt, never from a worker's own environment.**
The orchestrator resolves every path under `${XDG_CACHE_HOME:-$HOME/.cache}/hex/`
**once, from its own unredirected environment**, and passes each as an
absolute path in every spawn prompt; **a worker never expands it itself**,
because a worker's own copy of that variable is redirected into the per-run
scratch and its expansion would land back inside that scratch. `<run-id>` and
every `<agent-id>` are **minted by the agent that spawns the agent the id
names** — the orchestrator for `<run-id>` and for its own children, a
coordinator for each leaf it spawns and returns — under one rule for all of
them: slugified to `[a-z0-9][a-z0-9-]{0,63}`, and **never taken verbatim
from a plan cell, a filename or any worker-supplied text** — these strings
reach a delete target and a composed shell. An id that arrives *from* a
coordinator — a leaf id in its returned `agent=<id>` lines — is
worker-supplied text by that same rule, so it is **re-checked against
`[a-z0-9][a-z0-9-]{0,63}` before it is used or echoed**.

**Eight fields, and no schema-version field** (presence checks, never a
version marker):

| Field | Value |
|---|---|
| `seq` | int, monotonic from 1 — the torn-write guard, and nothing else |
| `parent` | string, or `null` for the top orchestrator |
| `state` | one of the five values below |
| `step` | short string: the phase or step it is on now |
| `checkpoint` | string or `null` — a path for a leaf, a commit SHA for a coordinator |
| `ts` | ISO-8601 UTC — with file completeness, the only thing that grades an agent |
| `expect_next_s` | int: seconds until the next beat is due |
| `blocked_on` | string, present only in state `blocked` |

There is deliberately **no `id` field** — it would be byte-identical to the
filename stem — and, by the same rule, **no `children` field**: the directory
is flat and every beat carries `parent`, so any agent reconstructs any subtree
with one glob and a filter. A second copy that can disagree with the thing it
names earns nothing. **`seq` is the torn-write guard and never a liveness
grade**: an agent whose context is compacted and which restarts its counter at
1 must not be gradeable as dead for it.

**Every field is worker-authored, so every one the orchestrator echoes or
interpolates — `checkpoint`, `step`, `blocked_on`, and the `agent=<id>` a
coordinator returns — is quoted and truncated per
[§ Untrusted-text echoes](#untrusted-text-echoes)** — linked, never
restated.

Beats are written **temp-then-rename** — write `<agent-id>.json.tmp`, then
move it into place — so a reader never observes a partial object, a move
within one directory being atomic on POSIX. **Exactly one writer, always —
the agent the filename names.** No parent, no sibling and no sweep ever writes
into another agent's beat file, not even to mark a child it has just stopped:
a parent writing into a child's file races a child that may not in fact be
dead. The file is **ephemeral** — deleted by teardown, never inside a
repository, and **never read by resume**. **Teardown deletes the whole
`${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/hb/` directory**, and that
obligation is stated here rather than only in
[`resources.md` § 8](resources.md#8-teardown): a parse-only run issues no
heavy command and so never loads that conditional-load file at all, and
still deletes its beats. That section keeps listing the same directory for
the runs that do load it.

**The state enum — five values, two of them load-bearing.**
`spawning | working | blocked | done | failed`.

- `spawning` carries the **cold-start budget**: a cold start and a
  steady-state deadlock are different numbers and must not share a threshold.
- `working` is live model-compute and **occupies a concurrency-cap slot**.
- `blocked` is load-bearing for exactly one thing, and it is contract, not
  convention: **it exempts the agent from the concurrency cap** (the cap
  bullet above), which is what keeps nested fan-out from deadlocking. **The
  exemption rides on the declared state carrying a `blocked_on` value, never
  on an inference** — without one the agent still occupies its slot. An
  agent in `blocked` sets `blocked_on` naming what it waits for, and **a
  `blocked` beat without `blocked_on` is graded as nothing** — not a
  violation, not a death; it is simply a less useful beat, and the ladder
  reads `ts`.
- `done` and `failed` are terminal for the file: it stops advancing, and
  **the orchestrator's teardown deletes the heartbeat directory; no parent
  deletes a child's beat file.** **`failed` means "the agent reported its
  own failure before exiting" — never "the parent declared it dead"**, since
  every beat file has exactly one writer. The terminal state of the *work*
  lives where work state has always lived: the plan's Parallelization-table
  **Status** column.

**Cadence — four obligations, one default, and nothing running in the
background.** An agent writes a beat:

1. **At spawn**, within **2 minutes**, in state `spawning`.
2. **At every phase boundary**, with `step` changed.
3. **At least every 5 minutes** while `working`.
4. **Before any tool call it expects to exceed its current deadline** — a
   beat declaring a new, larger `expect_next_s`. The worker moves its own
   deadline *before* it blows it, rather than the orchestrator guessing one
   global timeout for every worker.

Where a worker is free to choose, it beats at `expect_next_s / 2`, so a
single lost beat is not a miss. **The default `expect_next_s` is 300.** A
beat that repeats the same `step` is a healthy beat: **freshness is the only
test the ladder applies**, so a long phase that beats on time never trips
anything.

**Every beat is one foreground tool call the agent makes itself, at its own
tool rounds — a delta the agent produces, never a pulse something else
keeps.** No background process, no timer, no daemon, no client hook:
nothing outside the agent can keep the signal alive after the
agent has hung, and that is exactly why a fresh beat is evidence the agent is
still executing. The whole contract's value rests on it. An agent that was
passed no path writes no beat and is exempt from the ladder.

**The checkpoint — the resume anchor.** `checkpoint` is the anchor a re-spawn
resumes from, and it names no new mechanism. It carries **two different
values, and the contract says which agent writes which**:

- For a **coordinator** of either kind: a **commit SHA**, a coordinator
  being the one worker that commits. For a **decomposing** coordinator it is
  the SHA of its last sub-WP join commit, which
  [`workers/coordinator.md`](workers/coordinator.md) already writes and
  already calls a reset point. A **pipeline** coordinator joins nothing, so
  its value is the SHA of **the last phase commit on its work package's
  branch** — the same reset point one level down, already made by the phase
  that landed, so this kind is never left permanently `null`. **This clause
  is the source**; the coordinator persona's Liveness clause follows it.
- For a **leaf**: **a path, never a SHA** — the path of the last durable
  artifact it produced. A **leaf** has no commit to point at:
  [`workers.md`](workers.md) § Universal worker protocol rule 5 is *never
  auto-commit*, so offering a leaf the SHA form would put this field in
  direct contradiction with a universal worker rule.

`null` where nothing durable exists yet, and a re-spawn restarts the phase
from its beginning. The value is **advisory to the re-spawn prompt and never
authoritative over the plan**: a re-spawned agent still reads its plan row for
scope, and the checkpoint only tells it where to pick up.

**The escalation ladder — four rungs, applied to every orchestration depth
by the single runner named below.**

- **L0 fresh** — the last beat is within `expect_next_s`. Nothing happens.
- **L1 stale** — `now − ts > 2 × expect_next_s`. Wait **3 minutes**, and
  best-effort ping the agent over the *agent-messaging* capability. **The wait
  is the rung; the ping is not a gate** — L1 proceeds on its timer whether or
  not a message can be delivered, and whether or not one is answered; an
  answered ping short-circuits back to L0. **No beat file 2 minutes after
  spawn — the cadence obligation above — is a missed startup beat, and it is
  the only L1 that skips straight to L2** — there is nothing to ping, because
  the agent never announced itself.
- **L2 unresponsive** — no new beat after that wait. Read the agent's output
  *if the harness exposes it*. **The discriminator: tool calls still flowing
  means the agent is alive and violating the beat contract — log a `Warn`
  finding (`adr_0006` `C-502`) and do NOT kill it.** Silent means L3.
  **Where the output cannot be read at all, the rung holds at L2** — log the
  same `Warn` and do **not** kill: an absent observation is not a silent
  agent, and a missing check is never a passed check (the preflight table
  above). Announce
  `Degraded: no agent output — L2 holds at Warn, never escalates to L3`. Any
  worker text this rung reads or echoes is untrusted, and is quoted and
  truncated per [§ Untrusted-text echoes](#untrusted-text-echoes) — linked,
  never restated.
- **L3 dead** — stop the agent over the *agent-termination* capability and
  **re-spawn it from `checkpoint`** under a **newly minted `<agent-id>`,
  never the dead agent's**. The stop is best-effort — and where the
  *agent-termination* capability is absent it is only a report — so reusing
  the id would put two live agents on one beat file, breaking **exactly one
  writer, always** on precisely the path where it matters, and would
  overwrite the stale beat kept for the post-mortem. **L3 on a coordinator
  stops its children first**, identified by `parent` **cross-checked against
  the orchestrator's own spawn record** — `parent` is self-reported, so an
  unchecked field lets a confused child evade the cascade or drag a sibling
  into it, and the orchestrator already holds the record of what it spawned.
  **A coordinator's leaves are absent from that record** — the coordinator
  minted and spawned them — and its per-phase `agent=<id>` lines ship in
  its Return block, so **at the moment this rung fires they do not exist
  yet**: the coordinator is still running, which is why the rung fired. At
  depth 2, therefore, **`parent` alone identifies the leaves** and the
  cascade is that much more best-effort. The returned `agent=<id>` lines
  ([`workers/coordinator.md`](workers/coordinator.md) return template)
  corroborate **afterwards** — retry accounting and the post-mortem — and
  are re-checked as untrusted ids per the minting rule above.
  Orphaned agents holding worktrees, daemons and containers are the
  documented failure of every kill path that skips this.

**One retry per phase. A second death in the same phase marks the work
package `failed` and surfaces it — never a third auto-retry.**

**Nobody writes a dead agent's beat.** The evaluator stops the agent, retires
it from its own glob, and records the outcome in the plan's
Parallelization-table **Status** column — the writer, the column and the
vocabulary it already owns. It never writes `failed` into the child's file:
one writer per beat file is absolute, and the "dead" child may be alive enough
to write the next beat. The stale file is left on disk for the post-mortem and
removed by teardown.

**Who runs the ladder.** A coordinator is **a worker like any other for
liveness: it writes its own beat and runs no ladder.** **The top orchestrator
is the sole ladder runner for the whole fleet, at every orchestration depth.**
The directory is flat and every beat carries `parent`, so **one glob gives
every agent's state at every depth**, and the top orchestrator is the one
agent that reliably takes turns — it schedules, it merges, it runs the gates.
A coordinator awaiting its children is `blocked` and takes no turns, so an
evaluator sitting there would never run at the moment it was needed. Nothing
here needs a second mechanism, per-level tuning or a reach-past rule: a dead
coordinator's children are already in the glob the evaluator just read, and
the spawn tree is unchanged.

**The evaluation mechanism — three rungs, resolved once per run and
announced.** Capability classes only: the ladder is gated on *what a client
can do*, never on a primitive's name.

1. A **condition-waiting** capability → arm **one wait per spawn wave**,
   returning when the wave completes **or** a beat goes stale. Preferred.
2. Else a **scheduled-wake** capability → one wake per 5 minutes.
3. Else **turn-boundary checks** → glob the whole heartbeat directory,
   **unfiltered**, at each phase boundary and at each turn the orchestrator
   takes anyway. One glob, no polling loop, no background process, **zero
   added turns**. Rung 3 is **not a failure mode** — it is the contract
   working with no client features at all — but its detection latency is
   bounded by the orchestrator's own turn cadence and is honestly larger than
   rung 1's.

**A blocking spawn primitive degrades rung 3 further, and it is the common
case.** Where the client's spawn call returns only when the worker returns,
the orchestrator takes **no turns while that worker runs**, so "bounded by
the orchestrator's own turn cadence" collapses to **the worker's own
completion**: nothing is observed until the thing being observed has
finished, and a worker that hangs for its whole budget is indistinguishable
from one that worked for it. Two mitigations, neither a new mechanism.
**The binding one is a per-worker wall-clock budget stated in the brief** —
one line telling the worker how long its whole assignment may take and to
return a partial result rather than run past it. Under a blocking spawn
that budget, not the ladder, is what bounds the stall; it needs no new
field and no new file. **The beat file is still written and still readable**, so an agent
that *does* take turns — a sibling, or the orchestrator one level up —
observes the stall this parent cannot see. The ladder itself still runs
where **Who runs the ladder** above puts it — nothing here adds a second
runner.

Each absent capability announces its own line, in the shape the fan-out
ladder above already uses:

```
Degraded: turn-boundary liveness checks — no condition-waiting or scheduled-wake capability
Degraded: no agent messaging — L1 waits out its 3 minutes, no ping
Degraded: no agent termination — L3 reports instead of killing
Degraded: no agent output — L2 holds at Warn, never escalates to L3
Degraded: blocking spawn — no liveness check runs while a worker runs; the brief's per-worker wall-clock budget bounds it
```

Where messaging is absent, L1 simply waits out its 3 minutes. Where output is
not exposed, **L2 holds**: it logs its `Warn` and never escalates. Where
termination is absent, **L3 does not kill**: it reports the dead agent, marks
the work package `failed`, and lets `adr_0010` `C-913`'s cascade govern.

## Traceability IDs

Plans and specs number their requirements so coverage is a mechanical
check, not a prose judgment:

- **Component contracts are numbered `C-001, C-002, …`; UX scenarios
  `S-001, S-002, …`** — assigned when the artifact is written (in the
  spec when one exists, carried into the plan unchanged), stable within
  the artifact, never renumbered. IDs never originate in a
  discussion artifact; an orchestrator consuming one **assigns** IDs and
  never inherits them.
- **Every ID maps to at least one WP and at least one test**: the
  Parallelization table's Scope column cites the IDs a WP delivers, and
  every Specify-phase test names the IDs it covers.
- **Coverage is checked at three gates**: the plan review
  (`reviewer:spec` — an ID with no covering WP or no covering test is an
  actionable finding), the Specify gate in execution (every ID has at
  least one failing test before Implement), and the convergence check in
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
`…` past 120 characters, and never allowed to break its own line. **This is
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

**An execution run's block carries a timing rollup — six figures, read from
the plan the run just wrote** (the schedule log's `phase` and `merged`
lines, [Parallel-by-default
decomposition](decompose.md#parallel-by-default-decomposition)): total wall clock,
per-work-package wall clock, per-phase wall clock, the work/wait split,
review rounds, and adversary-gate time — that last figure alone from the
orchestrator's own `date -u +%FT%TZ` bracket around the adversary
invocation, the adversary pass being a run-level gate no `<WP>/<phase>`
line can carry. **Where the `phase` lines are absent or partial the block
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
the next `/hex-init` run. Three candidate classes, named so a run knows what
to record (`adr_0016` C-990): a perspective that should become an always-on
hint; a `review.<level>.*` value the run's budget residue, expiry or
round count argued for (`budget-minutes`, `rounds`, `class`, `checklist`);
and a finding class that recurred across seats or runs and belongs in the
project's own rules as a checklist item
([`checklist.md`](checklist.md#composition)). This is part of the flow (portable, no hooks needed); because the file
holds pointers rather than copies, upkeep is cheap. The section specs and
staleness rules are in [`memory.md`](memory.md).

The upkeep duties in this section belong to the four orchestrators; a
**non-orchestrator hex skill makes an upkeep write only where its own
contract names one**, and `hex-discuss`'s post-gate discussion hand-off
record and index rows (C-708) are the single such case today.

**Which sections bind a spawning non-orchestrator skill.** Bound:
[Worker
coordination](#worker-coordination) — it governs spawning itself — and this
section, per the sentence above. Exempt: [Shared shape](#shared-shape)
(written for a skill that resolves a whole config up front; this one
resolves none); [The meta-plan approval gate](#the-meta-plan-approval-gate)
— but **only for a skill named in that section's own closed exemption list**;
for `hex-discuss`, the one named spawning member, the gate is relocated to the
drain, not removed, and its resolved-literal-model disclosure is carried in
substance by [`models.md`](models.md#rules) rule 1's quiet-form clause; and
[Spawn-selection precedence](#spawn-selection-precedence) — exempt in its
*carriers* (an announce block showing the resolved set, a user picking
perspectives at an entry gate) and in its layered precedence, moot for a
skill with no tier baseline and no overlays, while `models.md`'s resolution
order and persona loading still bind (C-706). [Handoff
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
