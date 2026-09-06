# hex config vocabulary

The single definition site for the `hex.md › Preferences` config block —
its keys, their effects, and the ten rules that resolve them. Roles are in
[`workers.md`](workers.md); model classes in [`models.md`](models.md); the
memory file in [`memory.md`](memory.md); spawn precedence and the
concurrency cap in [`protocol.md`](protocol.md).

**Conditional-load.** Read this file **only** when `hex.md › Preferences`
contains a fenced `yaml` block (C-203). An unconfigured project has no block
— shipped defaults apply and nothing here is consulted, so a default run
pays zero context for it.

**No parser reads the block.** The yaml is a *convention an orchestrator
reads*, not a schema — sections are conventions, not schema
([`memory.md`](memory.md)). Every rule below is one an LLM applies while
reading a small file; a missing, misspelled, or malformed key degrades
gracefully ([merge rules](#merge-rules) 8–10), never aborts a run.

## Carrier and placement

One fenced `yaml` block, the **first content** under `## Preferences` in
`.agents/memory/hex.md`. Prose bullets continue below the block and
still carry nuance the keys cannot ("security review means the
token-exchange path, not the CRUD handlers"). A missing file, section, or
block is **normal** — shipped defaults apply and skills suggest `/hex-init`
([`memory.md`](memory.md)). The block is written **only by `/hex-init` with
consent**; orchestrators read it and never edit it. Not frontmatter:
`hex.md` has three sections with three owners, and frontmatter would hoist
user-owned config above the skill-managed sections. (C-202)

## Key vocabulary

**Two versions; only v1 freezes at first release.**

- **v1 — the six Tier A keys** (`models`, `adversary`, `limits`,
  `perspectives`, `research-axes`, `tiers`). Ships in Wave 2 and **freezes
  at the first `grim release`**: renaming a frozen key is a silent no-op in
  every consumer `hex.md`.
- **v2 — v1 plus `workflows`**, whose file format is defined in
  [§ Workflows](#workflows) below. A **v1 reader** — one whose merge rule 8
  enumeration predates `workflows` — treats it as an unknown key (merge rule 8
  — warn once, ignore, continue), which is correct because that reader has no
  dispatch interception to run. `workflows` ships in Wave 3 and freezes at the
  release after. (C-223)
- **v3 — v2 plus `review`**, the per-join-level review settings
  ([`loop.md` § Review by join level](loop.md#review-by-join-level),
  `adr_0015` C-985). A v2 reader treats it as unknown the same way.
- **v4 — v3 under the five-value tier segment** (`adr_0017` C-997): the
  `<tier>` segment of `tiers.<skill>.<tier>` and `workflows.<skill>.<tier>`
  is `low | medium | high | xhigh | max`, and `adversary` also accepts a
  list. **No new top-level key** — merge rule 8's enumeration is unchanged.
  A block whose comment says `v3` or lower is read with every tier segment
  shifted one step up (`low` → `medium`, `medium` → `high`, `high` →
  `xhigh`), announced once at the gate, never rewritten
  ([`protocol.md` § Tier grammar](protocol.md#tier-grammar)).

The block's opening `# hex config, vocabulary vN` comment states the version
the file was **written** against; merge rule 8's key enumeration lists the
version the **reader** implements.

| Key | v1 | Type | Default | Effect |
|---|:--:|---|---|---|
| `models.fast-balanced` | ✅ | string (literal model name) | unset → shipped class prose | Instantiates the class for this harness. |
| `models.deep-reasoning` | ✅ | string | unset | As above. Never an orchestrator-class model (`adr_0001` C-002). |
| `models.overrides` | ✅ | map `role[:focus]` → capability class | `{}` | Per-role class override at every tier, for every **non-review** role. A `reviewer[:focus]` entry is superseded by `review.<level>.class` for the join-level seats — review seats are configured per level, never per role. Escalation above the shipped cell still requires the announced reason ([`models.md`](models.md)). |
| `adversary` | ✅ | string skill name, a list of skill names (v4 — every entry launches at tier `max`, the first entry only below it, `adr_0017` C-995), or `none` | unset → adversary contract's per-tier default | Names the cross-model adversary skill ([`adversary.md`](adversary.md#adversary-contract)). |
| `limits.max-workers` | ✅ | int ≥ 1 | 8 | **A concurrency ceiling, never a floor and never a panel size.** Its enforcement (effective cap `min(8, max-workers)`, recursive count, batch-never-drop-baseline) is C-201, defined in [`protocol.md` § Worker coordination](protocol.md#worker-coordination); its ceiling is that section's 8 ([merge rule 9](#merge-rules)). This file adds only the phase ceiling that bounds preference-added spawns ([merge rule 6](#merge-rules)). |
| `limits.loop-rounds` | ✅ | int ≥ 1 | 3 | Ceiling over every `review.<level>.rounds` **and** on `--loop-rounds`. Never raises a level's `rounds`; its own ceiling is the hard maximum 3 ([merge rule 9](#merge-rules), [`loop.md`](loop.md#the-review-fix-loop)). |
| `limits.artifact-loop-rounds` | ✅ | int ≥ 1 | 1 | Plan/ADR-scope loop rounds. Opt-in only; `1` is shipped behaviour. **No ceiling** — the one limit a value raises, honoured as written ([`loop.md`](loop.md#the-review-fix-loop)'s explicit re-enable). |
| `limits.adversary-timeout` | ✅ | int minutes ≥ 1 | 5 — the mode-(b) stall window | **The stall window for a pollable background adversary call, and only that.** The key name is frozen; [`adversary.md` § Adversary contract](adversary.md#adversary-contract) owns the observation modes, which call this key governs and which it does not, how a mode resolves and the enforcement — beyond the default this table's `Default` column carries, this row restates none of it. Its ceiling is that default ([merge rule 9](#merge-rules)). |
| `limits.heavy` | ✅ | int ≥ 1 | the value derived in [`resources.md` § 2](resources.md#2-the-measured-resource-profile) from this host's measured profile; `1` where nothing could be measured | **A ceiling on concurrent *heavy commands*, not on agents** — an additive leaf under the frozen `limits` key, and the one value in this vocabulary that is a property of the **host** rather than of the work, which is why a plan cell cannot carry it. What takes a slot, the host-global semaphore, and the derivation are [`resources.md`](resources.md)'s; this row restates none of it. Unset means the derived value, so an unconfigured project gets a measured number rather than a guess. A reader predating this key meets an unknown key under `limits` and degrades by [merge rule 8](#merge-rules) — warn once, ignore, continue — to unbounded heavy commands. Its ceiling is the derived value ([merge rule 9](#merge-rules)). |
| `perspectives.always` | ✅ | list of rule objects | `[]` | Adds a perspective: a checklist section in the `L2` aggregate seat's brief for `/hex-execute` ([`checklist.md`](checklist.md#composition)), a seat in `/hex-review`'s `L3` panel and in `/hex-plan`'s and `/hex-architect`'s artifact panels ([`loop.md`](loop.md#review-by-join-level)). See [Perspectives](#perspectives). |
| `perspectives.never` | ✅ | list of `role[:focus]` | `[]` | Removes a perspective from the resolved panel. Applied last. `reviewer:security` additionally requires the attestation below (merge rule 5). |
| `perspectives.security-sensitive-paths` | ✅ | the literal `none`, or unset | unset | **Attestation, required to suppress the security reviewer.** `none` asserts this project has no security-sensitive path. Any other value, or absence, makes `never: [reviewer:security]` fail closed (merge rule 5). |
| `research-axes` | ✅ | list of strings | `[]` | Pre-seeds hex-architect's and hex-plan's candidate axis list; never auto-selects. |
| `review.<level>.seats` | v3 | int ≥ 1 | `l1` 1, `l2` 1 | Seats at that join level; `<level>` ∈ `l1 \| l2` ([`loop.md` § Review by join level](loop.md#review-by-join-level), C-985). `l0` and `l3` take no keys. |
| `review.<level>.class` | v3 | capability class | `l1` fast-balanced, `l2` deep-reasoning | The seat's model class. Wins over `models.overrides` for review seats. |
| `review.<level>.rounds` | v3 | int 1–3 | `l1` 1, `l2` 1 | Fix rounds at that level, under the `limits.loop-rounds` ceiling. |
| `review.<level>.budget-minutes` | v3 | int ≥ 1 | `l1` 10, `l2` 20 | Wall-clock budget; expiry ends the loop with residue recorded, never a failure. |
| `review.<level>.delta-only` | v3 | bool | `true` | `false` lets the seat read finding-adjacent files in full beyond the diff. Findings must still sit on a diff line. |
| `review.<level>.checklist` | v3 | list of focus names (`spec`, `quality`, `security`, `performance`, `docs`, `architecture`, `pitfalls`, `user-feedback`) | derived from the level ([`checklist.md` § Composition](checklist.md#composition)) | Replaces the derived section set for that level's brief (`adr_0016` C-990). The only breadth knob `L1` has; at `L2` an explicit `--review` flag still wins under the ordinary later-wins precedence. |
| `tiers.<skill>.<tier>.inherits` | ✅ | one of `low\|medium\|high\|xhigh\|max` | unset | This project's `<tier>` starts from the shipped `<inherits>` baseline instead of its own. Resolved **first** ([tiers](#tiers)). |
| `tiers.<skill>.<tier>.counts` | ✅ | map `<Phase>.<role[:focus]>` → int ≥ 0 | `{}` | Sets the spawn count for that role in that phase, against the resolved `inherits` baseline. `0` removes the spawn. `<Phase>` is a [phase identifier](#phase-identifiers). A `Review-Fix.*` entry for `hex-execute` is superseded by `review.<level>.seats` — ignored with the merge-rule-9 warning shape. |
| `tiers.<skill>.<tier>.overlays` | ✅ | map axis → value | `{}` | Per-project default for an overlay axis (e.g. `review: full`), below a user flag in precedence. Applied last within `tiers`. |
| `workflows.<skill>.<tier>` | **v2** | path to a markdown file | unset | Dispatch reads this file instead of the shipped `tier-<tier>.md`. File format: [§ Workflows](#workflows). A v1 reader warns and ignores it (merge rule 8). |

Literal model names appear **only** as `models.*` values a user sets — never
as a hardcoded default anywhere in this file or a shipped one; the currency
elsewhere is the capability classes `fast-balanced` / `deep-reasoning`
([`models.md`](models.md)).

## Dotted keys

**Nested and flat spellings are one key (C-222).** `tiers: {hex-plan:
{medium: …}}` and `tiers: {hex-plan.medium: …}` denote the same leaf path; a
reader normalizes every dotted segment to the nested form **before** any
merge rule runs, so `models.overrides` written flat and nested merge as one
map (merge rule 2).

- **Duplicate leaf** — normalization producing two declarations of the same
  leaf with different values: the **later declaration in block order wins**,
  and the collision prints `Error: duplicate key
  'tiers.hex-plan.medium.counts' declared as both nested and flat. Fix: keep
  one spelling; the later one (line N) was used.`
- **Type conflict** — a flat scalar colliding with a nested *map* (e.g.
  `tiers.hex-plan.medium` scalar while `tiers: {hex-plan: {medium: {counts:
  …}}}` exists) falls to merge rule 9: the scalar is dropped, the map kept,
  warned. Never a silent merge of two shapes.

The `<skill>` segment is `hex-plan` / `hex-execute` / `hex-review` /
`hex-architect`; the `<tier>` segment is `low` / `medium` / `high` / `xhigh` / `max` (never
`auto` — `auto` is a classifier input, not a tier).

## Perspectives

**`perspectives.always` rule object** (C-205).

| Field | Type | Default | Effect |
|---|---|---|---|
| `role` | `role[:focus]`, required | — | A shipped role ([`workers.md`](workers.md)) or a project-local persona (`.agents/workers/<role>.md`). **This is the persona→panel wiring** — naming a project persona here is how it enters a launch list. |
| `when` | path glob (below) | unset = every run | Rule fires only when at least one path in the run's **target file set** matches. |
| `skills` | list of skill names | all four orchestrators | Limits the rule's scope. |
| `phase` | [phase identifier](#phase-identifiers) | the skill's review phase | Which phase the spawn joins. |

**Glob semantics (C-217).** Not a regex and not a shell expansion; the
gitignore-style subset every hex harness already reads:

- Paths are **repository-relative**, `/`-separated, no leading `/` or `./`.
  The root is the run's git worktree root (inside `hex-execute`, the WP's
  worktree — paths identical to the main tree's).
- `?` matches one character within a segment. `*` matches zero or more within
  one segment and **never crosses `/`**. `**` matches zero or more whole
  segments, including none (`a/**/b` matches `a/b`). `{x,y}` is alternation,
  `[abc]` / `[a-z]` character classes.
- No-wildcard pattern matches that exact path. A pattern ending in `/` or
  `/**` matches the whole subtree. A bare directory (`src/ui`) reads as
  `src/ui/**`.
- **Case-sensitive**; dotfiles are ordinary (`*`/`**` match a leading `.`);
  **symlinks match by their own path, never their target's**.
- Anything else — an unterminated class, a `!` negation, a `..` segment, an
  absolute path — is unparseable and falls to merge rule 9 (ignore that
  rule, warn, continue).

**Target file set, per skill.** `when:` needs a path set:

| Skill | Target file set |
|---|---|
| `hex-review` | Every path added, modified or deleted in the diff. A **rename contributes both** old and new path; a deletion contributes its path. |
| `hex-execute` | The union of the plan's work-package file lists, plus every path written so far. Evaluated **per phase**, so a rule can fire once the diff that makes it relevant exists. |
| `hex-plan` | The file lists named by the input issue/PR/task, plus every path Discover read, plus every path named in a drafted work package. |
| `hex-architect` | Every path named in the decision input plus every path Discover read. |

An **empty target file set** (a green-field plan naming no files) makes every
`when:`-carrying rule not fire; a rule with no `when:` is unaffected. Normal
outcome, not a warning.

## tiers

`inherits`, `counts` and `overlays` may all appear in one
`tiers.<skill>.<tier>` block; their order is **fixed and total** (C-219):

1. **`inherits` first.** The layer-1 baseline for `<skill>.<tier>` becomes
   the *shipped* `<skill>.<inherits>` tier file **in full** — its phase set,
   phase identifiers, roles, counts and overlay defaults. The declaring
   tier's own shipped file contributes nothing; a replacement, not a merge.
   `hex-review.high: {inherits: medium}` resolves to
   `hex-review/tier-medium.md`'s baseline; high's shipped phases are not
   consulted.
2. **`counts` second, against the result of step 1** — never the declaring
   tier's own shipped file. Under `inherits: medium`, `counts` keys are
   validated against *medium's* phase identifiers and deltas computed against
   *medium's* baseline; a phase existing only in `high` is an unknown phase
   (merge rule 9). This is why the order is stated: the two readings differ
   in which keys are valid and what delta is announced.
3. **`overlays` third**, applied to the result of steps 1–2, still below any
   user flag in precedence.
4. Then the panel resolution of [merge rule 3](#merge-rules) (`baseline ∪
   always(matching) − never`), then merge rule 6's phase ceiling.

**`inherits` is not transitive and never chains.** It may name only a
*shipped* tier, and that tier's own project redefinition (if the same
`hex.md` also declares it) is **not** applied to the inheriting tier — a
redefinition is always one sentence: "high here = shipped medium, plus these
deltas." `inherits` naming the declaring tier itself, or a value outside
`low|medium|high|xhigh|max`, is malformed (merge rule 9): ignored, warned, shipped
baseline stands. Tier *vocabulary* stays `low|medium|high|xhigh|max` + `auto`; only
tier *content* is redefinable (C-206).

## Phase identifiers

`tiers.*.counts` keys and `perspectives.always[].phase` both name a phase, so
"what is a valid phase name" is answerable from the files already open
(C-220):

> **A phase identifier is the text of a tier file's `## Phase N:` heading,
> taken after the colon and cut at the first `(` or ` — `, then trimmed.**
> The identifier set for a `<skill>.<tier>` pair is exactly that tier file's
> heading identifiers, in file order.

`## Phase 3: Verify-Architecture — skipped` → `Verify-Architecture`.
`## Phase 6: Review-Fix Loop (by join level, `full` checklist)` → `Review-Fix
Loop`. `## Phase 2: Stage 1 — Correctness (2 workers)` → `Stage 1`. Matching
is case-insensitive and whitespace-normalized; identifiers are unique within
a tier file by construction.

**hex ships no phase-index file, deliberately.** A shipped enumeration would
be a second source of truth for a fact each tier file's headings already
state, and would go stale on the first edit. The heading *is* the index; a
reader validating a `counts` key opens the tier file it is already opening to
dispatch. Identifiers are **per skill and per tier, not portable** — a key
naming a phase absent from the target tier file is malformed (merge rule 9):
dropped, warned, siblings unaffected — `Error: unknown phase 'Reserch' in
tiers.hex-plan.medium.counts. Fix: valid phases for hex-plan medium are
Discover, Research, Classify, Design, Decompose, Review.`

**Under an active workflow fork, the fork is the phase index.** When
`workflows.<skill>.<tier>` resolves to a valid workflow file, every phase
identifier for that pair resolves against the **workflow file's** `Phase`
column. A preference naming a phase the fork removed is **ignored per entry**
— never a fallback to the shipped phase (it is not running) and never a
relocation to a neighbour (silently moving a security reviewer is worse than
not running it) — warned once naming the workflow file, and the run
continues. When a workflow file **fails** validation, dispatch falls back to
the shipped tier file and phase identifiers fall back with it.

## Merge rules

The three-input precedence is **unchanged** ([`protocol.md`](protocol.md#spawn-selection-precedence)).
`tiers` and `workflows` do not add a fourth input — they *rewrite layer 1*
before layers 2 and 3 apply. (C-204)

1. **Scalars** (`adversary`, all `limits.*`, all `models.*` leaves,
   `inherits`, `workflows.<skill>.<tier>`) — later layer replaces earlier. A
   `workflows` entry never composes: the last-declared path for a given
   `<skill>.<tier>` wins outright; there is no stacking of two workflow files.
2. **Maps** (`models.overrides`, `tiers.*.counts`, `tiers.*.overlays`) —
   merged per key. An override for one key never disturbs another.
3. **Lists** (`perspectives.always`, `perspectives.never`, `research-axes`) —
   **replace wholesale across layers, never append**. Within a layer the
   panel resolves once: `baseline ∪ always(matching) − never`.
4. **`never` cannot remove `reviewer:spec`.** The traceability check is a
   contract gate, not a perspective ([`protocol.md`](protocol.md#traceability-ids)).
5. **`never: [reviewer:security]` is fail-closed (C-218).** Honoured **only**
   when both hold: (a) the block declares
   `perspectives.security-sensitive-paths: none`, and (b) no
   security-sensitive path matched this run — no `always` rule whose `when:`
   names a security path, and no hit on `classify.md`'s shipped security
   markers. Every other case, **including the ambiguous one where the reader
   is unsure whether a path is sensitive**, is a refusal: the security
   reviewer runs and the announce block prints the reason.
   - Missing attestation → `Error: never [reviewer:security] refused — no
     perspectives.security-sensitive-paths attestation. Fix: add
     'security-sensitive-paths: none' to assert this project has no
     security-sensitive path, or drop the never entry.`
   - Attestation present but a path matched → `Error: never
     [reviewer:security] refused — <glob> matched, contradicting
     security-sensitive-paths: none. Fix: narrow the always rule's when:
     glob, or drop the attestation and the never entry.`
   - Both conditions met → honoured, printed as a **warning line**, never a
     quiet omission.

   The attestation puts the *forgetful* path on the safe side: a reader that
   skips this rule finds no attestation and runs the reviewer. This is
   guidance to an AI reviewer, **not an enforced policy** — a project with a
   compliance obligation must not treat `perspectives` as satisfying it.
6. **The concurrency cap batches; the phase ceiling displaces (C-216).** Two
   different limits — do not conflate them.
   - `min(8, max-workers)` is a **concurrency cap** on workers running *at
     once*, not a panel size: a phase exceeding it runs in sequential
     batches and **never drops a baseline perspective for the cap alone**
     (mechanics in [`protocol.md`](protocol.md#worker-coordination)).
   - The **phase ceiling** is what preferences may not exceed:
     `ceiling = max(shipped phase baseline total, min(8, max-workers))`. The
     baseline term keeps a saturated shipped phase runnable; the cap term
     preserves headroom for a project that raised `counts` on a small phase.
     Only **preference-added** spawns displace — `always` entries and
     `counts` *above* baseline. `counts` at or below baseline (including `0`)
     are the project's own reduction and are never restored.
   - **Reduction procedure**, run when a phase's resolved total exceeds its
     ceiling:
     1. `overshoot = resolved total − ceiling`.
     2. While `overshoot > 0` and any `always`-added spawn remains in this
        phase: drop the **last-declared** one (reverse declaration order
        within `perspectives.always`), `overshoot -= 1`, announce the drop
        with `— phase ceiling N reached`.
     3. While `overshoot > 0`: among this phase's `counts` entries currently
        **above** their shipped baseline, reduce the one with the largest
        absolute excess by **one**; `overshoot -= 1`; ties break by the
        fully-qualified key `<Phase>.<role[:focus]>` in ascending ASCII.
        Re-evaluate after every single step — never reduce an entry to its
        baseline in one move.
     4. Stop. Termination is guaranteed: steps 2 and 3 each strictly
        decrease `overshoot` by 1, their combined supply is exactly the
        preference-added spawn count, after which the resolved set equals the
        shipped baseline (total ≤ ceiling by the `max(…)` definition).
   - **Never** silently exceed the ceiling, and **never** reduce a baseline
     spawn below its shipped value. `reviewer:spec`'s unremovability (rule 4)
     is a separate, stronger guarantee that also survives an explicit
     `counts: 0` (rule 7).
7. **`counts` of `0`** removes a spawn — except for a role the tier file
   marks mandatory, where it is refused with a warning naming the file.
8. **Unknown key** — warn once, ignore, continue. The warning pairs
   complaint and remedy: `Error: unknown key 'perspective.always'. Fix: did
   you mean 'perspectives.always'? Valid top-level keys: models, adversary,
   limits, perspectives, research-axes, tiers.` The enumeration lists the
   vocabulary version the **reader** implements — a v1 reader omits
   `workflows` and treats it as unknown; a v2 reader appends it, a v3 reader
   appends `review`.
9. **Malformed value** (wrong type, a value outside a non-numeric key's
   stated vocabulary, a value below the floor the key's `Type` column
   states, unparseable glob, unknown role, unknown skill/tier/phase segment,
   or a `workflows` path naming no readable file) — ignore that key only,
   keep the shipped default for it, warn with the same `Error:`/`Fix:`
   shape. A bad field never blocks a run and never invalidates its siblings.

   **A well-formed value above a limit's ceiling is not malformed.** Where a
   `limits.*` row names a ceiling, this rule owns the outcome and the row
   restates none of it: the value **clamps to that ceiling**, announced
   under the [clamp grammar](protocol.md#the-meta-plan-approval-gate). A row
   naming no ceiling — `limits.artifact-loop-rounds` — has none, and a
   well-formed value stands as written. So `limits.adversary-timeout: 0` is
   malformed (below the `≥ 1` floor: ignored, warned, default 5 stands),
   while `limits.adversary-timeout: 45` clamps to 5 and `loop-rounds: 5`
   clamps to the tier default.
10. **Unparseable YAML block** — the whole block is skipped, announced once,
    prose bullets in the section still apply, run continues on shipped
    defaults.

## Workflows

`workflows.<skill>.<tier>` points dispatch at a project-authored **workflow
file** in place of the shipped `tier-<tier>.md`. This section is the single
definition of that file's format; the four `SKILL.md` dispatch steps and
`/hex-init` point here and never restate it. `workflows` is **vocabulary v2**
— see [the version note](#key-vocabulary) for the v1/v2 split.

A workflow file is a **fork of a shipped tier file**: markdown, in the shape
tier files already use, with one header table added at the top. It is
*executed, not read for guidance* — every table cell must resolve to a
definite value before its phase runs.

### Node schema (C-208)

One header table, one `## Phase: <id>` section per node, and a stamp.

**Stamp** — `Forked from <shipped tier file> @ hex <version>`, naming the file
the fork descends from and the hex version it was cut at. Both validation
(check 6) and drift detection read it; a fork missing the stamp cannot be
validated and falls back (below).

**Header table** — one row per node:

| Column | Type | Meaning |
|---|---|---|
| `Phase` | [phase identifier](#phase-identifiers), unique in file | Node id; matches its `## Phase: <id>` heading verbatim. |
| `Depends on` | comma-separated `Phase` ids, or `—` | Incoming happens-before edges. |
| `Roles` | `role[:focus]` list, or `—` | Spawn set for the phase. |
| `Count` | a [Count](#count-grammar-c-221) form | Per-role spawn count. |
| `Gate` | free text | Machine/verification condition that must hold before a dependent phase starts — never a user question (check 7). |

Each `## Phase: <id>` section holds the phase body (what its workers do, its
gate). `<id>` is the [C-220 identifier](#phase-identifiers) and matches its
table row verbatim; a renamed heading is a renamed phase.

**Edges** are happens-before, **within one file only** — no cross-file or
cross-skill edges. A phase is eligible once every phase it `Depends on` has
passed its gate; phases with no unmet dependency run concurrently under the
concurrency cap. This *phase*-level DAG is orthogonal to hex-execute's
WP-level DAG (`adr_0002` C-101): the WP DAG runs inside whichever phase
declares the builder roles, and neither graph references the other's ids.

### Count grammar (C-221)

Three forms, no others:

| Form | Resolves to | Available in |
|---|---|---|
| `<int>` | Exactly that many spawns **of each role** in `Roles`. `0` is not legal — delete the row instead. | all skills |
| `<int>×WP` | `<int>` spawns of each role **per work package eligible in this phase** (eligibility per the WP-level DAG, `adr_0002` C-101); a coordinator's leaves count recursively toward the concurrency cap ([`protocol.md`](protocol.md#worker-coordination)). Resolves once the plan's WP list is known, before dispatch. | `hex-execute` only |
| `—` | No spawns; the phase runs inline in the orchestrator. `Roles` must also be `—`. | all skills |

A **range is not a `Count`.** Shipped tier files write ranges ("2–4 explorer
workers") as prose guidance to a classifier; a workflow table is the resolved
answer. A range in a `Count` cell is malformed: read as its **lower bound**
and warned — `Error: Count '2-4' in phase Discover is a range, not a count.
Fix: write one integer; 2 was used.` The lower bound is used because it cannot
exceed the phase ceiling. `<int>×WP` outside `hex-execute`, a non-integer, or a
negative is likewise malformed: the row's phase falls back to the shipped tier
file's count for the same identifier if one exists, else to `1`, warned either
way.

### Validation — the seven checks (C-209)

A skill applies these while reading the file — a reader, not a parser — **in
this order** (the first three are OpenSpec's only three structural checks):

1. Every `Phase` id is unique.
2. Every `Depends on` target names a declared `Phase`.
3. No cycle — walk the edges; report the full cycle path.
4. Every `Roles` entry resolves to [`workers.md`](workers.md)'s role index or
   a file under `.agents/workers/`.
5. Every `Count` cell resolves to a **definite, finite integer** under the
   grammar above (so every phase's resolved total is finite). The concurrency
   cap is *not* checked here: a phase over the ceiling is **batched and
   displaced by [merge rule 6](#merge-rules) at run time**, not rejected as an
   invalid file.
6. **Skill invariants hold.** The **required phase-identifier set is derived at
   validation time** from the shipped tier files the stamp names (per
   [§ Phase identifiers](#phase-identifiers) / C-220): the *intersection* of
   the skill's three shipped tier files' `## Phase N:` headings is required,
   the rest optional. hex ships **no** static required-phase list here — the
   derivation tracks the tier files and never goes stale. Each required
   identifier must appear both as a `Phase` row **and** as a non-empty
   `## Phase: <id>` section that discharges the duty below (a renamed phase is
   a missing phase, matched case-insensitively; an empty or duty-less section
   fails even when the row exists). Optional identifiers may be deleted
   freely; forbidden ones may not appear at all.

   The table below is keyed by identifier — it is **not** the required-set
   declaration. Set membership comes from the derivation; a phase absent from a
   skill's derived required set is not checked, and a future required phase
   enters the check the moment the shipped tier files carry it as a `## Phase
   N:` heading, with no edit here.

   | Skill | Duty each required identifier must discharge (dependency order) | Forbidden |
   |---|---|---|
   | `hex-execute` | **Stub** signatures/types compile with no implementations · **Specify** tests that **fail** against the stub · **Implement** makes those tests pass · **Review-Fix Loop** ≥1 reviewer role, a bounded round count, an exit gate · **Merge and commit** merges the worktree and commits. (`Discover` leads; required present.) | — |
   | `hex-review` | **Stage 1** ≥1 reviewer role producing classified findings · **Verdict & Output** emits the verdict, resolves one fold target in the `### Fold-Back` sub-block it carries and applies the delta set or states why not, prints that Fold-Back block, and **writes nothing** but the plan Status block, the convergence rows, and — under the Fold-Back phase's four preconditions — the one resolved spec file and fold receipt. (`Discover` leads.) | any phase writing a file outside `hex-review/SKILL.md` § Constraints |
   | `hex-plan` | **Decompose** emits work packages with file lists · **Review** ≥1 reviewer role. (`Discover`, `Classify`, `Design` lead.) | — |
   | `hex-architect` | **Reason & Design** emits the ADR or system design · **Review** ≥1 reviewer role. (`Discover`, `Classify` lead.) | `Decompose` |

   **Security-reviewer invariant — fail-closed, the same guard as [merge rule
   5](#merge-rules) / C-218.** A fork is a second path to the outcome merge
   rule 5 refuses: dropping `reviewer:security`. So the same attestation gates
   it. Where the stamped shipped tier file spawns `reviewer` (focus
   `security`) — conditionally, on security-sensitive paths — a fork that
   omits that role from a **kept required-duty phase** (`hex-execute`
   Review-Fix Loop, `hex-review` Stage 1) is **malformed unless** the block
   declares `perspectives.security-sensitive-paths: none`. Without the
   attestation: `Error: fork <file> drops reviewer:security from <phase> with
   no security-sensitive-paths attestation. Fix: keep the role in the fork, or
   add 'perspectives.security-sensitive-paths: none' to the block.` On failure
   the fork falls back to the shipped tier file (below), so the security
   reviewer returns either way — a fork is never the quiet path around C-218.

   **Fold-safety invariant — fail-closed, the same fallback as the
   security-reviewer guard above.** A `hex-review` fork whose **Verdict &
   Output** phase writes a spec file — i.e. performs the fold — is **malformed
   unless** that phase discharges [`archive.md` § Safety
   envelope](archive.md#safety-envelope) **in full**: the census, the
   stale-base guard (C-405), the no-shrink guard (C-416), containment (C-418),
   destination cleanliness, and the single whole-file write. A fork that keeps
   the fold but drops any envelope step is a second path to the silent wrong
   write archive.md exists to prevent — the same shape as a fork dropping
   `reviewer:security`. On this failure: `Error: fork <file> folds in <phase>
   without archive.md § Safety envelope. Fix: keep the envelope discharge in
   the phase, or drop the fold.` — the fork is rejected and dispatch falls
   back to the shipped tier file (below), so the fold either runs under the
   full envelope or does not run as a fork at all. The envelope steps are not
   restated here — archive.md owns them.

7. **No phase declares a user gate.** The single meta-plan approval gate is the
   only approval point ([`protocol.md`](protocol.md#the-meta-plan-approval-gate));
   a `Gate` cell is a machine/verification condition, never a question.

**On any failure**: announce `Error: <what>. Fix: <where>`, **fall back to the
shipped `tier-<tier>.md`** for that skill and tier, and run. A broken workflow
file degrades to shipped behaviour — never an abort, never a half-applied
fork. Phase identifiers fall back with it (see
[§ Phase identifiers](#phase-identifiers)).

### v2 vocabulary (C-223)

`workflows` is the **v2** addition to the config vocabulary (`review` is
the v3 one — [Key vocabulary](#key-vocabulary)). The six Tier A
keys (v1) froze at the first `grim release`; `workflows` and this format ship
in Wave 3 and freeze at the release after. A **v1 reader** — one whose merge
rule 8 enumeration predates `workflows` — treats the key as unknown ([merge
rule 8](#merge-rules): warn once, ignore, continue), which is correct: without
this section a reader has no dispatch interception to run. The block's
`# hex config, vocabulary vN` comment states the version it was **written**
against; merge rule 8's enumeration states the version the **reader**
implements.

## Complete example

````markdown
## Preferences

```yaml
# hex config, vocabulary v4. Unknown keys warn once and are ignored.
# (v1 = these keys minus `workflows` and `review`; see Key vocabulary.)
models:
  fast-balanced: <this harness's fast-balanced model>
  deep-reasoning: <this harness's deep-reasoning model>
  overrides:
    builder:implement: deep-reasoning
adversary: codex:rescue

limits:
  max-workers: 6            # effective cap = min(8, 6) = 6
  loop-rounds: 2            # ceiling over review.<level>.rounds and --loop-rounds
  artifact-loop-rounds: 1   # 1 = shipped behaviour
  adversary-timeout: 3      # below the default, so it lowers the stall
                            # window. See Key vocabulary

review:                    # v3 — per join level, loop.md § Review by join level
  l1:
    budget-minutes: 5        # leaf review must return in 5 min here
    checklist: [spec, quality, security]   # checklist.md sections in the L1 brief
  l2:
    rounds: 2                # one extra fix round over the aggregate

perspectives:
  always:
    - role: reviewer:security
      when: "hex/hex-core/**"
    - role: reviewer:accessibility     # .agents/workers/ persona
      when: "src/ui/**"
      skills: [hex-review, hex-execute]
  never:
    - doc-reviewer

research-axes:
  - registry ecosystems
  - OCI spec evolution

tiers:
  hex-plan.medium:
    counts:
      Research.researcher: 3           # shipped baseline is 1
      Discover.explorer: 2             # shipped baseline is 2-4
    overlays:
      adversary: on
  hex-review.high:
    inherits: medium                   # this repo's "high" is medium's panel

workflows:                 # v2 — file format in § Workflows
  hex-execute.medium: .agents/workflows/exec-docs.md
```

- Security review means the contract files under `hex-core/references/`,
  not the prose in the skill dispatchers.
````

## Reproducing a role/phase swarm as config

hex was generalized from a per-client, per-skill swarm (`DESIGN.md`).
A project that ran that swarm — one `SKILL.md` per role, hardcoded phase and
agent counts, per-role model literals, a cross-model adversary, path-scoped
roles — can **drop those skills and reproduce their whole role/phase
structure as `hex.md › Preferences`**, using only the vocabulary defined
above. This section maps each swarm construct to its config key and links to
the key's definition; it **restates no grammar and no merge rule** — those
live once, above.

| Swarm construct | hex config key | Definition |
|---|---|---|
| A role that always runs (a `builder` / `qa-engineer` / `security-auditor` skill) | `perspectives.always[].role` — a shipped role ([`workers.md`](workers.md)) or a project persona at `.agents/workers/<role>.md` | [Perspectives](#perspectives) |
| A path-scoped role (security-auditor only under `auth/**`) | the same rule's `when:` path glob | [Perspectives › Glob semantics](#perspectives) |
| Per-phase / per-agent spawn counts (2 builders, 3 reviewers) | `tiers.<skill>.<tier>.counts` | [tiers](#tiers) |
| Per-role model choice | `models.overrides` — a map to a **capability class**, never a literal model name | [Key vocabulary](#key-vocabulary), [`models.md`](models.md) |
| "This project's `high` behaves like the shipped `medium` panel" | `tiers.<skill>.<tier>.inherits` | [tiers](#tiers) |
| The cross-model adversary (a `codex-adversary` skill) | `adversary` | [Key vocabulary](#key-vocabulary) |
| A wholesale per-tier **phase reshaping** (different phases, different order) | `workflows.<skill>.<tier>` — a fork of the shipped tier file | [Workflows](#workflows) |

Which role fills which swarm seat: a swarm `builder` → hex `builder`
(focus `stub`/`implement`); `qa-engineer` → hex `tester` (and
`reviewer:quality`); `security-auditor` → `reviewer:security`; `code-check`
→ `reviewer:quality` / `doc-reviewer`; `architect` → `architect`;
`codex-adversary` → the `adversary` skill, not a perspective role. Roles the
swarm shipped that hex has no built-in for become project personas under
`.agents/workers/`, wired into a panel by naming them in
`perspectives.always` ([`workers.md`](workers.md) § Project-local personas).

**`workflows` is v2 and not yet built** — it is schedule-gated (Wave 3), not
design-gated: the format is fully specified in [§ Workflows](#workflows)
above and freezes the release after, but a project needing full phase
reshaping today has only `tiers.counts` (reshape spawn *counts* within the
shipped phases), not a phase fork. Everything else in the table ships in
Wave 2 (the [six Tier A keys](#key-vocabulary)).

A worked reproduction — a swarm that ran a security-auditor scoped to auth
paths, two builders and three reviewers at its default tier, its architect on
the strongest class, and a Codex adversary — is the [Complete
example](#complete-example) above, read as config rather than as new
vocabulary: `perspectives.always` with a `when:` glob is the scoped auditor,
`tiers.*.counts` are the agent counts, `models.overrides` is the per-role
routing, `adversary` is the cross-model pass.

### Two known fidelity gaps

These are honest limits of the mapping, not gaps to be closed:

1. **hex has two model classes; a three-class swarm loses one distinction.**
   The shipped matrix is `fast-balanced` / `deep-reasoning`
   ([`models.md`](models.md)); a swarm with a third, cheaper class (a
   haiku-tier for the most mechanical work) has **no separate hex class** —
   such a role **folds into `fast-balanced`**. `models.overrides` can pin any
   role to either hex class, but cannot recreate a third cost tier hex does
   not define.
2. **Config adds a role to a panel; it cannot add a slash command.** A swarm
   that shipped standalone single-role commands (`/builder`, `/qa-engineer`)
   let a user invoke one role directly. hex ships no such per-role commands,
   and **no config key can create one** — `perspectives.always` enrolls a
   role into an orchestrator's launch list, so the reproduced role runs
   *inside* `/hex-plan` / `/hex-execute` / `/hex-review`, never as its own
   command.

### What config cannot express: milestone autonomy

Every key above reshapes the inside of **one** hex run. The one swarm
capability that is not a run's internals — the outer autonomous loop across
**many** hex invocations (a milestone split into a dependency-ordered issue
set, delivered issue-by-issue on a long-living branch, with a resumable
cursor: the `swarm-x` / `swarm-loop` layer) — is categorically a new
orchestrator, not configuration, and no fork of a tier file reaches across
run boundaries ([§ Workflows](#workflows): edges are within one file only).
That gap is the subject of
`adr_0007`
(Proposed), not this config surface.
