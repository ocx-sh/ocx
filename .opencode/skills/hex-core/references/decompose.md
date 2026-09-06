# hex Decomposition

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Parallel-by-default decomposition

Plans are decomposed to **maximize parallel execution** — sequential is
the exception, not the default shape:

- **Decompose by structural boundary** (a directory, module, package, or
  data-model boundary), never by user-facing feature slice — two slices
  that touch the same file from different angles cannot parallelize.
- **Every WP declares its expected file set at planning time.** Parallel
  eligibility is a plan-time set-intersection check, not a merge-time
  discovery: two WPs may share a wave only if their file sets are
  disjoint. Two tasks that need the same file become sequential steps of
  **one** WP — never two parallel WPs. **When the plan carries a `Repo`
  column** the comparison is over `(Repo, path)` pairs, not bare paths
  (C-316) — satellites routinely declare textually identical repo-relative
  paths that are disjoint across repos; with no `Repo` column every pair is
  `(., p)` and the check is byte-for-byte as before (same key, worktree side:
  [Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics)).
- **Every WP declares its size at planning time.** The Parallelization
  table's `Size` column holds `S | M | L`, the plan-time estimate of the WP's
  diff, assigned when the WP is cut: docs-only or tiny work (~≤50 expected
  lines) → `S`; a single-area change of ordinary scope → `M`; large or
  cross-area work → `L`. **Who reads the cell is conditioned on the plan's
  generation.** In a plan without the generation marker **`Size` is never
  read at launch, worktree or verification time** — it is reporting
  vocabulary only, since the budget guard that once read it is retired
  (`adr_0015` C-986). In a plan carrying the marker the cell is read at
  each WP's own spawn time, as the first derivation input of [the
  effective tier](#the-effective-tier); worktree and verification time
  still never read it, and the merge-time re-derivation reads this cell
  once more — the plan-time value it compares against is recomputed, never
  stored (**Nothing is persisted**, below). **A missing column or cell
  reads `L`** in that derivation and resolves to the ceiling; the histogram
  below buckets every WP.
- **The `Review` column is an optional risk hint, not a budget.** The cell
  holds `risk` or is empty; `risk` is an author-declared risk source that
  raises the WP's review **one join level**, exactly as `sec`, `hot` and
  `door` do ([Review by join level](loop.md#review-by-join-level), C-983).
  Legacy cells are read, never migrated: `panel` reads `risk`, `self` and
  `light` are inert, a missing column or cell is no hint, and a sub-WP
  inherits its parent's cell. The former `self | light | panel` budget,
  its two-direction guard and the `panel` escape hatch are retired
  (`adr_0015` C-986) — nothing at the Decompose gate, at spawn or at merge
  reads them, and the column is not renamed.
- **Merge-time re-derivation.** Alongside the merge-time file-set
  re-validation, the effective-tier function is re-run against the actual
  merge diff — `Size` from the diff against the size vocabulary above,
  `sec` and `hot` from the actual changed-file list, `hub` from the actual
  changed-file list too, never carried forward from planning time, because
  the declared set is not a guaranteed upper bound and file-set
  re-validation's own remedy for an out-of-set diff is to *widen* the
  declaration, and `door` unchanged as an authored declaration. It uses
  the file list `git diff --name-only <base>..<wp-branch>` that
  re-validation already produces, so it adds no command. If `sec`, `hot`
  or `door` fires there, the WP's review runs **one join level up before
  the merge** ([Review by join level](loop.md#review-by-join-level)) — a
  plan-time estimate never caps review of what was actually built. **The
  limit is stated rather than papered over: only review can be restored —
  the collapsed phases and the model class are already spent.** It runs in
  every plan shape; the generation marker gates the spawn-time
  derivation, not this one.
- **The histogram — one line, one grammar, stated once here.** Buckets are
  keyed by the WP's effective tier, ordered `medium`, `high`, `xhigh`, `max`, each
  key followed by its count, a zero-count bucket omitted, and the plan's
  ceiling closing the line:
  `effective tier: medium 6 · high 2 · xhigh 1 (ceiling xhigh)`.
  In a plan without the generation marker every WP runs at the ceiling, so
  the line degenerates to one bucket. `/hex-execute` prints it in its
  announce block and `/hex-plan` at the Decompose gate; both link this
  grammar rather than restating it.
- **Every WP declares its merge-verification budget at planning time.** The
  Parallelization table's `Verify` column holds `scoped | full` and sits
  **immediately after `Review`** — the positional discipline C-302 applied
  in fixing `Repo` at the second position. It is the one **budget column**
  left (`Review` is a risk hint, above). A budget column scales one axis of
  per-WP effort away from the shipped default **in exactly one direction,
  fixed per column**, and the direction is chosen so the *unsafe* direction
  is unreachable: a column whose baseline is the minimum may only raise.
  `Verify` is **raise-only**
  — there is deliberately no value below `scoped`, because a merge check
  with neither contract tests nor an assembly proof is not a check. It sets
  **one verification budget for one merge boundary** — the WP's **merge
  gate** ([Verification](verify.md#verification)) and the [Review-Fix
  Loop](loop.md#the-review-fix-loop)'s **exit gate that immediately precedes it**,
  and nothing beyond those two: `full` runs the project's full documented
  verification at **both** gates, and the **merge** run — never the
  in-worktree exit-gate run — **resets the checkpoint counter like any other
  full run**; `scoped` is the default and is written only for readability. A
  missing column or cell means the plan's `Verify-default:`, else `scoped`
  — and so does a **literal `—` in the cell**, at any row grain: `—` is
  outside the `scoped | full` vocabulary the cell is read against, so it
  resolves by that same chain rather than naming a budget.
  **A third *reader*, never a third gate** — `door` **reads** this cell
  ([the effective tier](#the-effective-tier)): the cell now also decides
  whether the WP may reduce, which adds no gate and changes no verification
  budget.
  **Assignment is plan-time and justified in one line when used** —
  `full` declares author judgment the merge-time high-risk predicate cannot
  see: a WP that changes a default, a schema, or a config value nothing
  textually references. **One plan-level escape, so reverting the policy is
  one edit rather than N cells:** an optional Status-block line
  `- Verify-default: full` sets the default every empty `Verify` cell
  inherits; individual cells still override it, and absence means `scoped`.
  Both columns and the default line are **plan-artifact fields, not config** —
  `config.md`'s frozen key vocabulary is untouched.
- **The schedule log — one append-only plan section, one entry per merge
  and one per completed phase.**
  The plan carries a `## Schedule log` section (the plan template's, which
  replaces its `## Progress Log`): append-only, one bullet per merge and one
  per completed phase, never edited or reordered. Grammar, one line:
  `- <ISO-8601 UTC> · merged <WP> @ <post-merge SHA> · verify <scoped | full(<trigger>)> [<elapsed>] · ready: <ids | —> · blocked: <id (<blocker>), … | —>`,
  where `<trigger>` is one of `join`, `counter`, `level-clear`, `high-risk`,
  `column`, `degrade`. **Coincident triggers write one entry**, tagged with
  the first matching token in that order — the same reason firing on any
  trigger costs one full run, not two ([Checkpoints](verify.md#checkpoints)).
  **A second line kind, one per completed phase.** Grammar, one line:
  `- <ISO-8601 UTC> · phase <WP>/<phase> · model <class> · work <elapsed> [· wait <elapsed>] [· rounds <n>]`,
  where `<class>` is always a **capability class**
  ([`models.md`](models.md)), never a literal model name, and the bracketed
  `rounds <n>` appears on review phases only. **The two kinds are
  discriminated by the first word after the first `·`** — `merged` or
  `phase` — and **the compatibility filter is written out rather than
  implied: every existing consumer reads only `merged` lines and skips
  every other first word.** `adr_0010` `C-904`'s bisection walk
  ([Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics)) is
  one such consumer and is unaffected. A section carrying no `phase` line is
  a run that predates them, never an error.
  **The per-phase field set**, read from the coordinator's returned result
  with `run` and `wp` supplied by the parent — **not a line in any new file,
  and no second writer**: `ts`, `run`, `wp`, `phase`, `event`, `model`, `agent`,
  `work_ms`, `wait_ms`, plus `rounds` on review phases.
  **`wait_ms` has exactly one definition: the interval from a phase becoming
  runnable to its worker beginning work**; every coarser formulation is that
  same span at a coarser grain, never a second field. **Where the start is
  unknown the field is absent, never zero** — a zero would read as a phase
  that never waited. The parent times what it runs itself from its own
  `date -u +%FT%TZ` brackets, the same cheap bracketing the `merged` line
  already uses. OpenTelemetry GenAI attribute **names** may be borrowed
  where they fit — the names, not the wire format, and no SDK dependency.
  **The post-merge SHA is mandatory** — it is what makes
  the post-merge-failure playbook's bisection free
  ([Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics)), and
  recording it costs the `git rev-parse HEAD` the merge already implies. It
  is the feature-branch tip **after the merge and after any fix pass** — the
  state the check actually passed against — so a bisection probe never
  convicts a WP whose merge was already repaired.
  **Capture is deliberately cheap and best-effort:** the orchestrator brackets
  each merge-plus-check — and, on the `phase` line, each phase — with
  `date -u +%FT%TZ` and `<elapsed>` is the difference — **wall-clock only, never CPU, never a benchmark** — and it is
  **optional**: a step that lost its start stamp writes the entry without it
  rather than omitting the entry or inventing a number. Nothing gates on
  `<elapsed>`. **It lives in the plan, not in a state file**: the plan is
  already the WP-level state of record and is already mutated per merge by the
  Status column, so this is the existing writer touching the existing
  artifact, and being committed is a feature — the drift evidence lands in the
  PR and survives the session. The five facts are one line on purpose:
  `ready:` and `blocked:` make a wave barrier visible as drift, and `verify`
  and `<elapsed>` make the full-run count and phase attribution mechanically
  checkable from the artifact rather than from a transcript. Bounded by
  construction: `merged` entries = feature-branch merges and `phase` entries
  = completed phases, and dotted sub-WP rows produce neither.
  **It replaces the plan template's `## Progress Log`** — a
  free-prose `Date | Update` table no contract has ever written to or read;
  two logs of the same events, one structured and one not, is the drift the
  sole-definition rule exists to prevent, so the unwired one is **retired,
  not kept alongside**. An absent section means a run that predates the log,
  never an error, and a plan carrying a hand-written progress log keeps it
  untouched.
- **Failure cascade — a `failed` WP blocks its dependents without stopping
  the run.** Dispatch needs no new mechanism: the ready-set rule already makes
  a WP eligible only when every `Depends-on` is `merged`, so a dependent of a
  `failed` WP simply never becomes eligible, and independent siblings keep
  flowing. `failed` also counts as level-cleared for the checkpoint trigger,
  and only there ([Checkpoints](verify.md#checkpoints)). **Strandedness is derived,
  never stored.** The four statuses
  (`pending | active | merged | failed`) are unchanged and no fifth is added;
  a stranded WP is `pending`, which is already true and already correct. The
  stranded set is computed **once, eagerly, in a single pass over the plan
  table's own static `Depends-on` edges**, at report time — the direct fix for
  the bug class every DAG runner that materialized an `upstream_failed` state
  per node has shipped, and it leaves the parent rollup rule untouched, which
  a fifth status would have broken. **Report shape:** the failed WP(s) named
  first, then one line per stranded WP naming its **direct** blocker —
  `WP7 — blocked by WP4 (stranded) ← WP2 (failed)` — plus what did complete,
  read from the plan's existing `Shippable after wave` line. **Terminal
  rule:** a run that ends with a non-empty stranded set **never presents a
  green final gate as plan completion** and
  **never reaches the plan's terminal review state — `done`, or `landing`
  for a plan carrying a `Repo` column** — `/hex-review` remains the sole
  writer of that state and gains this precondition.
- **The counterweight: no WP below its own overhead.** Every WP pays a
  fixed cost — worktree, stub/specify/implement spawns (one collapsed
  builder spawn at effective tier `medium`, [the effective
  tier](#the-effective-tier)), review, merge, verification. A WP whose
  whole scope is a single trivial concern (~≤50 expected lines) **folds
  into its nearest sibling as sequential steps**; keeping it isolated
  needs a one-line justification, checked by the plan review. Maximize
  parallelism *between* right-sized WPs — never by slicing below the
  overhead floor.
- **Launch on dependency-ready; waves are a derived reporting view.** Waves
  stay computed — a WP is in wave N iff every WP it depends on sits in an
  earlier wave and N is minimal (topological levels), wave 1 = no
  dependencies — but a wave is a **derived reporting view**, shown in the
  table and the mermaid index for readability, never a launch gate. A WP
  becomes eligible the instant every WP in its `Depends-on` has Status
  `merged` — not when its whole wave is ready. The orchestrator maintains a
  **ready-set** and launches eligible WPs immediately, within the
  concurrency cap, recomputing on every merge. The ready-set is ordered
  **critical-path-first** (longest remaining dependency chain first), so a
  shallow WP never starves the critical path (marked in the plan). The
  concurrency cap still bounds fan-out
  ([Worker coordination](protocol.md#worker-coordination)) — when the ready-set is
  wider than the cap allows, launch in ready-order batches. Merge stays
  serialized in a valid topological order — the DAG changes launch timing,
  not merge discipline. A linear or all-wave-1 plan schedules identically to
  the old barrier — its ready-set equals wave membership — so it is
  backward compatible.
  **What a ready WP launches into is one of two coordinator kinds —
  `pipeline` or `decomposing` — decided by two questions.** This file only
  **names** the pair; both kinds are **defined once**, in the Mission clause
  of [`workers/coordinator.md`](workers/coordinator.md). **Q1 — does this WP
  get a coordinator at all?** — belongs to `hex-execute/SKILL.md`
  § Coordinator spawn. **Q2 — does that coordinator decompose?** — belongs
  to that same Mission clause. Every rider in this
  file that fires on a coordinator keys on Q2's answer, which is why the
  distinction is named here. How wide the launched set may be is the
  allocation invariant in [Worker
  coordination](protocol.md#worker-coordination) — its one home; nothing here restates
  it.
- **Progress surface** — with no wave barrier, surface live state from the
  Status column (the data is already there): a compact rollup line per
  coordinator (e.g. `WP3 [coordinator]: 2/4 sub-WPs merged`), and a WP left
  `active` far beyond its peers carries a **staleness flag**, so a hung WP no
  longer blocks anything visibly. Surface only — never speculative
  re-execution.
- **The critical path** — the longest dependency chain — is identified
  and marked; it bounds wall-clock time no matter how wide the waves are.
- **Under-parallelization is justified, never silent**: a decomposition
  with fewer parallel WPs than its file-disjointness allows carries a
  one-line justification in the plan's Parallelization section.

### The effective tier

Every work package runs at its own **effective tier** — the tier that scales
its phases and its model class. **Review depth is keyed on join level, not
tier** ([Review by join level](loop.md#review-by-join-level)), and the tier
does not scale it. The effective tier is
derived from cells the plan already carries, never written into the table:
the plan's Status-block `Tier:` is a **ceiling**, not the baseline. **The
effective tier is never above the ceiling and is never authored.** The
vocabulary is [§ Tier grammar](protocol.md#tier-grammar)'s own, ordered
`low < medium < high < xhigh < max` — five values and no sixth
(`adr_0017` C-991), and `auto` never
reaches this function because the classifier has already resolved it.

The ceiling `T` is the plan's Status-block `Tier:` value, explicitly
independent of `/hex-execute`'s optional run-tier argument: that argument
selects the orchestrator's own phases for the run, and neither lowers nor
raises `T`. The whole function is **recomputed at each WP's own spawn
time**, never once per run — a Discover-time or gate-time listing is a
snapshot and says so. **Nothing is persisted — no cache, no derived
column, no state file, at any depth.** The read split is fixed here too:
**a spawn made for a work package reads that WP's effective tier; a spawn
made for the run reads the plan tier.**

**Five derivation inputs, and no others**: the WP's `Size` cell, and four
**risk flags**. Each flag reads a cell, a declared file set, or a pointer
this protocol already reads — the ceiling `T` bounds the derivation and is
not an input to it. **No flag adds a command.** The two floors below are
not derivation inputs — they read the WP's own structure and apply
**after** the derivation, to its result. The flag enumeration is **closed and versioned**: exactly
four, named `sec`, `hot`, `hub`, `door`; a fifth arrives by amending this
text in a later ADR, never by analogy at an edge case.

**`Size`.** `S` is ~≤50 expected lines **and** ≤3 expected files,
deliberately more conservative than `classify.md`'s `medium` row of ≤3 files,
≤100 lines — the two thresholds have different jobs, an actual diff for a
review against an estimate for a reduction, and the reduction side is the
more conservative of the two on purpose, so this is a stated divergence and
not one shared table. `M` is ~≤500 expected lines and ≤15 expected files;
`L` is anything else. **Both halves must hold** — a 40-line change spread
over six files is `M`, not `S` — and an absent, empty, unrecognized or
ambiguous cell reads `L`. No new numbers are introduced: ≤3 files is
[§ Tier grammar](protocol.md#tier-grammar)'s own `medium` row, ~≤50 lines is this
section's overhead floor, and ≤15 files / ≤500 lines
are `hex-review/classify.md`'s shipped `high` row.

**The four flags.**

- **`sec`** — a **union**: (a) any path in the WP's `Expected Files`
  matching **hex's own shipped, project-independent triggers**, which are
  `hex-review/classify.md`'s structural-marker table — auth/crypto/signing
  paths, dependency manifests and lockfiles, CI-workflow files, new package
  manifests — **or** (b) the project's documented security-sensitive
  convention, located through the `hex.md › Pointers` row. **The project
  may widen hex's sensitivity and may never subtract from it**: no
  attestation, no empty set and no narrow convention clears (a).
  **The attestation read.** A project's
  `perspectives.security-sensitive-paths: none` clears `sec` only, and only
  under both of `config.md` merge rule 5's conjuncts, with conjunct (b)
  evaluated per WP over that WP's own `Expected Files`. That is a read of
  rule 5 for the `sec` flag, not an amendment to it: `config.md` gains no
  key and no clause, and rule 5's own per-run grain is untouched at its own
  site. What the read changes is stated rather than left to be inferred —
  it is nonetheless the looser grain, so a run in which one WP touches auth
  no longer refuses the attestation for the other WPs. It is acceptable
  because the flag it feeds is inherently per-WP, and because the shipped
  triggers under (a) still fire per WP and cannot be cleared by any
  attestation.
- **`hot`** — the same `hex.md › Pointers` row's hot-path convention, and
  that source alone, because hex ships no project-independent hot-path
  trigger.
- **`hub`** — any `(Repo, path)` pair in this WP's `Expected Files` also
  appearing in another WP's `Expected Files`, in any wave — the same
  predicate as [Checkpoints](verify.md#checkpoints)' high-risk clause 2 on a
  different left operand, the declared set rather than the actual merge
  diff, and with no `Repo` column every pair is `(., p)`.
- **`door`** — the WP's `Verify` cell resolving to `full` through the
  shipped cell → `Verify-default:` → `scoped` chain above.

**Resolution — one pass, in this order.**

1. **Baseline from `Size`** — `S` ⇒ `medium`, `M` ⇒ `high`, `L` ⇒ `T`.
   **Effective `low` is never derived** (`adr_0017` C-993): `medium` is the
   collapsed builder, byte-identical to what the old `low` ran; a WP reaches
   the inline `low` only through a plan ceiling `Tier: low`, which is a
   single-WP plan whose one WP runs at the ceiling.
2. **`sec`, `hot` or `door` true ⇒ `T`.** Any one of the three is decisive:
   they name one-way-door risk, which is what the ceiling was authored for.
3. **First floor — `hub`.** A true `hub` floors the WP at `min(T, high)`,
   never at the ceiling: a shared file is a merge-order risk, which four
   phases already cover, not the one-way-door risk the
   other three flags name.
4. **Second floor — the coordinator floor**: a WP a coordinator splits
   into dotted sub-WPs floors at `min(T, high)`. Applied after `hub`, so
   the two compose as *the highest floor that fired*; the reasoning and the
   no-sub-WPs case are below.

Every step is capped by `T`, and that cap is applied last. The WP's
`Review` cell is not an input and raises nothing here — it is a review-level
hint, read only by [Review by join level](loop.md#review-by-join-level).

**Sub-WPs and decomposing-coordinator-owned parents.** A sub-WP's effective
tier is its parent's — no new inheritance mechanism, no fifth status. A decomposing-coordinator-owned
parent derives from its own cells, and because the plan template writes `—` in its
`Verify` cell, `door` reads `false` there. That is harmless: that row's
merge already pays the project's full documented verification under merge
trigger (i) ([Worktree work-package
mechanics](worktree.md#worktree-work-package-mechanics)), so nothing the flag would
have bought is lost. **The floor is stated behaviourally, never by naming a
coordinator kind**: never a flat `high`, which at
`T = medium` would break the ceiling invariant, and a coordinator that owns
only a WP's phase pipeline and adds no sub-WPs does not raise the floor, or
the collapse could never fire. The reason is written with the rule:
`models.md` Rule 5 gives `coordinator` no `medium` cell, so a `medium` derivation
would leave a fan-out with no defined spawn, and the shipped granularity
gate has no size floor, so an `S` or `M` coordinator parent is authorable.

**The seam, stated once, here:** `adr_0012` decides which phases a work
package runs and at what model class and review breadth; `adr_0013` decides
how the workers running them are supervised, resourced and sub-orchestrated.
`adr_0013`'s `C-1219` and `C-1220` are named **only** as the forward
reference for the coordinator-kind question; the two kinds themselves are
defined once, in the Mission clause of
[`workers/coordinator.md`](workers/coordinator.md).

**The degrade rule — a flag whose source is absent, unreadable, or
malformed reads `true`, never `false`**, never inferred from a sibling flag
and never borrowed from another WP. The read rule is stated rather than left
to the reader:

1. the `hex.md › Pointers` combined entry is read as **two independent
   halves**, each resolved on its own, and each half's value is a
   repo-relative location, never the convention itself — a row naming only
   one convention leaves the other absent, and `sec` and `hot` never share
   a resolution;
2. a half's value is the text between its convention label and the first
   field separator (`·`, `&nbsp;`, or end of line), trimmed — **not** an
   inline glob set, and **not** the literal `none`;
3. anything else ⇒ `true`, including a half present but empty, a value that
   is not a repo-relative path, and a value hex does not recognise;
4. the target file is matched against a closed enumeration — the literal
   `none`, or one or more globs — with missing ⇒ `true`, empty ⇒ `true`,
   resolving to a directory ⇒ `true`, content matching neither ⇒ `true`,
   resolving outside the repository root ⇒ `true` and never followed, and
   **any** invalid glob making the whole set unreadable ⇒ `true`; a target
   declaring `none` makes that half read **`false`** — for `hot` that is
   the whole flag only when the WP's `Expected Files` set resolves, the
   degrade below dominating otherwise; for `sec` it clears (b) alone and
   never (a);
5. `none` is declared in the target file, never inferred from an empty or
   missing target, and never written into the Pointers row;
6. [`memory.md`](memory.md#staleness)'s repair-and-proceed rule runs
   **first** — if re-detection succeeds the repaired pointer is what rules
   1–5 read, and if it finds nothing this rule wins and the flag reads
   `true`.

Rules 1–6 govern the `hex.md › Pointers` sources — the project half of
`sec` and the whole of `hot`. The sources that are not pointers degrade by
the headline rule alone: `sec`(a) reads a shipped marker table that is
always present, and `hub` reads the plan's own `Expected Files`, so an
unreadable or absent file set reads `true` for that WP rather than
resolving to no overlap. **That last degrade is not `hub`'s alone** — `sec`
and `hot` match their own sources against that same declared set, so an
absent, empty or unreadable `Expected Files` reads `true` for `sec`, `hot`
and `hub` alike, never `false` for the two whose conventions resolved.

**`door`'s fail-open is the single argued exception**, on three grounds: the
cell → `Verify-default:` → `scoped` chain is the cell's own documented
default rather than a missing source; inverting it would resolve every
legacy plan's empty `Verify` column at the ceiling, which is the bulk
migration this design refuses; and `door` is the only one of the four whose
source is an authored cell rather than a discovered convention. The
reconciliation with the rule above is recorded as **residual risk, not
direction**: three independent backstops survive the checkpoint trigger's
vacuous clause, but nothing automatic survives a fail-open reduction here:
the merge-time re-derivation leaves `door` unchanged as an authored
declaration, so it cannot see this failure, and the trunk review is opt-in
([Review by join level](loop.md#review-by-join-level)). A mis-declared
`Verify` cell is caught by the human reading the handoff's `Reduced:`
lines, or not at all.

**The generation marker.** One optional Status-block line,
`- Effective-tier: derived`. **Presence** ⇒ this subsection's semantics.
**Absence** ⇒ pre-`adr_0012` semantics byte-for-byte, forever: **never a
prompt, never an error, never a migration step, never a rewrite of the plan
to add the line** — a permanently valid shape, not a migration backlog.
`derived` is the only value v1 accepts, and an unrecognized value is a
refusal: `Error: unrecognized Effective-tier value '<value>' — derived is
the only value this version understands. Fix: write '- Effective-tier:
derived', or delete the line to run the plan on pre-adr_0012 semantics.`
The marker is **read line-initial and only from the Status block above the
first `##`**, so a marker quoted in a code fence or in prose is not a
marker; the value is the text between the `:` and the first field
separator, trimmed, matched exactly. **A line present with an empty value
is a refusal, not an absence.** **A marker on a plan with no `Verify`
column is a refusal** — that column is `door`'s only source, and
hand-adding the marker to a legacy plan would flip every WP from the
ceiling to a derived tier, silently and in bulk; that refusal's `Fix:` line says to run the plan through `/hex-plan`.
**No `Plan-Schema:` field is added and none may be inferred.**

