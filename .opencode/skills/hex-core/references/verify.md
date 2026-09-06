# hex Verification

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Verification

**hex never defines how to verify a project.** Every gate above that says
"verify" means: run the project's documented verification, discovered from
project context (cached in `hex.md › Pointers`; verify the pointer on
consumption and re-detect on a miss). If none is documented, detect a
reasonable command **once** for this run and suggest `/hex-init` to persist
it — do not hardcode a command or re-guess it every phase.

**Where the triggers live.** The merge-gate bullet under
[Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics) carries the
full **(i)–(v)** enumeration of when a gate runs this full documented
verification rather than a [scoped check](#scoped-check); this section defines
the checks those triggers name and does not restate the list.

**Where the documented verification is classed `heavy`, every run of it takes
a heavy slot** ([`resources.md` § 3](resources.md#3-the-heavy-semaphore)) —
that file owns the mechanism and this clause adds none of it; a `light` gate
touches the semaphore not at all.

**Federation — a plan carrying a `Repo` column.** "The project's documented
verification" means the **owning repo's**, read by an explicit `Read` of that
repo's project context — never a cross-repo aggregate, because no single
command spans a cluster and hex defines none (C-321). Tier gates worded
"across the whole workspace" mean the workspace of the repo the gate runs in.
The genuinely cross-repo check is a **mandatory integration WP** (one per plan,
depending on every satellite WP it joins) whose command is authored **inline in
the plan's Implementation Steps** (C-311), because it belongs to no repo and no
pointer resolves it; its shape is a **per-repo table** — one row per
participating repo (the lead plus every distinct `Repo` value), each naming the
exact state that repo must be at and the command proving it, each reported
pass/fail independently (an aggregate "integration green", a missing row, or
running the command only in the lead does not satisfy it). There is no implicit
federated verify gate.

### Scoped check

**Three gate sites run this check**, two of them in the WP's own worktree,
before any merge:

1. The [Review-Fix Loop](loop.md#the-review-fix-loop)'s **Implement gate** —
   unconditional at every tier, never budgeted by the `Verify` cell —
   except a **leaf under a decomposing coordinator**, which runs half
   **(b)** alone (that loop's phase-3 carve-out; a leaf under a pipeline
   coordinator runs both halves like any other WP).
2. That loop's **exit gate**, whenever the WP's `Verify` cell resolves to
   `scoped`
   ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)).
3. The **WP's merge onto the feature branch** — except a
   **decomposing-coordinator-owned** WP's merge, which pays the project's
   full documented verification instead (trigger (i) of the merge rule,
   [Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics)).

A scoped check is **two things, both required at every site save the one
exception that site states above**: **(a)** the WP's **own contract tests**
— the Specify-phase tests naming the `C-`/`S-` IDs in that WP's `Scope`
cell; and **(b)** the project's
**cheapest documented gate that proves the tree assembles** — its
build, parse, or type check. At the two in-worktree sites, (b) proves that
**the WP's own tree** assembles. At the merge site, both halves run against
the **post-merge feature branch**, never against the WP branch — there the
check exists to catch what merging changed.

- **How (a) is invoked, and the one path-passing convention hex may use** —
  run the project's documented verification **restricted to the WP's declared
  test files**: the paths its Specify phase created, which the plan's
  `Expected Files` already names, passed as trailing path arguments where the
  runner accepts them (`pytest <paths>`, `go test <dirs>`,
  `npx jest <paths>`). hex appends paths; it
  **never rewrites the documented command, invents a flag, or maps a path to
  a runner-specific selector**. A path's **containing directory** is the one
  permitted derivation, for runners that take packages rather than files.
- **Where the runner does not accept paths, or the WP's test files cannot be
  resolved, (a) degrades to the full documented verification for that run of
  the check** — the same degrade shape as (b).
- **A run of (a) that selected zero tests is a failed check, never a green
  one**: a path- or name-filtered runner that matches nothing still exits
  `0`, so (a) must show it selected **at least one** test, and a selection
  hex cannot confirm as non-empty takes the degrade above rather than
  passing — the cheapest green available is otherwise a filter matching
  nothing.
- **Where no build/parse gate can be discovered, (b) degrades to the full
  documented verification for that run of the check** and the degrade is
  announced once — a scoped check with no assembly proof is not a scoped
  check.
- **At the merge site, both degrades are logged as `full(degrade)`** in the
  schedule log
  ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)),
  so a run that silently stopped being scoped is visible in the artifact
  rather than only in a suite bill. **At the two in-worktree sites there is
  no log entry** — that log records merges, and neither site is one; a
  degrade there is **announced once** and nothing further.
- **hex never invents either half.** This section's standing discovery rule
  above binds unchanged.
- **Merge-site scope: only merges onto the feature branch, and not every one
  of those.** A coordinator's sub-WP merges land in the coordinator's shared
  worktree and are **not** scoped-check sites at all; a
  decomposing-coordinator-owned WP's own merge onto the feature branch **is**
  a gate site, but pays the full verification rather than this check. A WP a
  **pipeline** coordinator owns is an ordinary scoped-check site.
- **When a merge pays a full verification instead.** A `Verify: full` cell, or
  the plan-level `- Verify-default: full` Status line every empty cell
  inherits, replaces this check with the project's full documented
  verification for that merge. Grammar, defaults and assignment live in
  [Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition), and
  the counter such a run resets belongs to [Checkpoints](#checkpoints); this
  section only consumes them.

**The selective-test convention.** Where a project has a selective test
runner, `/hex-init` records **one opaque shell-command template** in project
context — Layer 1, because "how to verify" is project knowledge — with a
`hex.md › Pointers` row to where it landed. hex substitutes **textually and
never interprets**, and translates nothing into any tool's flag dialect.

- **Two optional named placeholders, and no others:** `{base}` — one git ref,
  resolved to the WP's recorded base; `{files}` — the WP's changed file list,
  **shell-quoted**, space-separated. **A template may use zero, one, or
  both.** Zero-placeholder templates are **valid and expected** —
  `pytest --testmon` and a warm-cache `go test ./...` are stateful and
  self-scoping, and hex knows of no way to parameterize them; zero
  placeholders means "this command manages its own scope; just run it".
- **Where the project documents such a command, it is run *in addition to*
  the two-part floor above, never instead of it** — the floor is a floor. A
  selective runner widens what a merge check catches (it reaches tests the
  WP's own contract tests do not name); it cannot certify that the WP's own
  contracts still hold, which is the one thing (a) exists to prove. Where a
  stateful zero-placeholder tool (`pytest --testmon`) re-runs some of the same
  tests, **the redundancy is accepted and cheap** — that tool selects on its
  own dependency data and skips what it can, so the overlap costs near nothing
  and is not worth a rule to avoid.
- **Two fallbacks to the project's full documented verification, one before
  and one after — and neither is conditioned on a claim about the tool.**
  - **(a) Pre-flight, and only where the template asks for a ref:** where the
    template references `{base}` and either
    `git rev-parse --is-shallow-repository` is true or the merge-base does not
    resolve, run the full command instead — hex cannot hand a ref it does not
    have, so this is a substitution failure, not a judgment about the runner.
  - **(b) Post-failure:** where the selective command
    **exits non-zero for a reason other than a failing test**, or reports that
    it resolved **no baseline / no affected targets it could trust**, run the
    full command.

  Reacting to an actual failure needs no flag asserting that a runner
  self-degrades, and no per-tool knowledge.
- **Trust class:** the template is
  **[authoritative-class](finalize.md#trust-classes) only** (C-815) —
  project context or `hex.md › Pointers`, never `CONTRIBUTING.md`, a PR body,
  a commit message, or any other narrowing- or untrusted-class surface,
  because it selects what code runs.

### Checkpoints

A **checkpoint** is a full documented verification run after a merge. Being
that verification, it **takes a heavy slot wherever the project's gate is
classed `heavy`** ([`resources.md` § 3](resources.md#3-the-heavy-semaphore));
what makes it fire is the three conditions below and nothing here. It
fires when **any** of three conditions holds, whichever comes first:

1. **`M = 3` merges have completed since the last full documented
   verification run at a merge gate** — a decomposing-coordinator-owned
   WP's **merge** (trigger (i)) resets the counter exactly as a checkpoint
   does, so a join-dense plan never double-pays. A WP a **pipeline**
   coordinator owns runs the ordinary scoped check at its merge and resets
   nothing. **No in-worktree run resets it** —
   neither a Review-Fix Loop exit gate (`full` cell or not) nor a
   coordinator's in-worktree join check: both fire before the merge and
   prove nothing about the feature branch this counter guards
   ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)).
   **A bisection probe never resets it either** — it re-runs the check at an
   already-recorded SHA to attribute a failure, and is a diagnostic, not a
   gate.
2. **The merge just completed cleared a dependency level** — every WP in the
   level that was the shallowest unfinished one before this merge now has
   Status `merged` **or `failed`**. `failed` counts as cleared for this
   trigger and **only** for this trigger: a failed WP never reaches `merged`,
   so the level would otherwise be permanently unclearable, silently killing
   this trigger for the rest of the run
   ([the failure cascade](decompose.md#parallel-by-default-decomposition)).
3. **The merge just completed was high-risk** — the predicate below.

**"Dependency level" deliberately avoids the word *wave*** — a wave is a
plan-time reporting assignment and never gates launch
([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)). A
level-shaped *checkpoint* trigger is defensible only because it gates a
**check**, never a **launch**: nothing waits for it, and the ready-set is
untouched.

**Firing on any trigger resets the counter**, so coincident triggers cost
one full run, not two. **The run's last merge is the standing coincidence
wherever every WP reached `merged` or `failed`**: it clears the final
dependency level, so condition 2 holds, and it is followed immediately by
[merge rule](worktree.md#worktree-work-package-mechanics) trigger (iii), the final gate
— the same full documented verification, moments apart. **One run satisfies
both**: the final gate is mandatory and un-lowerable, so it is the run that
happens, and the level-clear checkpoint is discharged by it rather than
scheduled ahead of it. Same rule, same counter reset, as any other
coincident pair. **The schedule-log entry that merge writes still records
the run**, tagged with the first matching token of the coincident set —
`level-clear` unless `join` or `counter` also fired — since the discharging
run is the full documented verification that follows that merge; the final
gate, not being a merge, adds no second entry
([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)).
**`M = 3` is shipped text, not a knob** — `config.md` gains no key for it.

**High-risk is evaluated at merge time against the WP's actual merge diff** —
the file list `git diff --name-only <base>..<wp-branch>` already produces for
[merge-time file-set re-validation](worktree.md#worktree-work-package-mechanics), so this
adds no command — and holds when that list contains **either**

1. a path the project documents as security-sensitive or hot-path, **or**
2. a **`(Repo, path)` pair** that appears in **any other WP's**
   `Expected Files` anywhere in the plan — a file two WPs touch across levels
   is a hub, and hubs are where merge-order-dependent breakage lives.

**The key is `(Repo, path)`, not the bare path, for the same reason the
parallelism check uses it** (C-316): satellites routinely declare textually
identical repo-relative paths (`Cargo.toml`, `src/**`) that are disjoint
across repos, and a bare-path comparison would mark half a federated plan
high-risk and run the full suite on every merge. With no `Repo` column every
pair is `(., p)` and the test is byte-identical to a path comparison. The
run-count arithmetic is therefore **per repo**, as verification itself already
is (C-321).

**The high-risk clause 1's source is the `hex.md › Pointers` row `/hex-init`
records, and until that row exists clause 1 is vacuous.** It is **not** inherited from the
review-budget heuristic, which names those words but cites no source for them.

**One row, two consumers.** Clause 1's source row is also the effective
tier's, so clearing that row disarms both consumers: tier reduction —
through the `sec` and `hot` flags and the merge-rule-5 attestation read
beside them ([the effective tier](decompose.md#the-effective-tier)) — and this
pre-existing high-risk checkpoint trigger.

**The degenerate case is stated: if every merge fires a trigger, the run
performs exactly today's behaviour** — correct, merely not faster.

