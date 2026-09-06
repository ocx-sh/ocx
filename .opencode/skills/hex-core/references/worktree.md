# hex Worktree Mechanics

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Worktree work-package mechanics

Every plan integrates through **one feature branch**; each **work package
(WP)** runs on its own ephemeral branch in its own worktree:

- **One feature branch per plan** — the integration target for every WP.
  Resolve it once, at execution start: the non-trunk branch already
  checked out, else create `hex/<plan-slug>` from the trunk. Its tip at
  that moment is the **frozen base** every wave-1 WP branches from — never
  a moving baseline.
- **One ephemeral branch + worktree per WP** — branch
  `hex/<plan-slug>--<wp-slug>` (hyphenated, not `hex/<plan-slug>/<wp-slug>`:
  git refs are paths, so the feature branch `hex/<plan-slug>` cannot also be
  a ref *directory* holding a `<wp-slug>` child — a branch cannot be both a
  ref and a ref-directory), worktree `.agents/worktrees/<wp-slug>/`
  (the default; a deviation location is documented in project context
  (cached in `hex.md › Pointers`) — it describes repo layout, not hex
  behavior, so any bundle that spawns worktrees reads the same source). A
  launching WP bases on the **current feature-branch tip** — by serialized
  integration it already holds every merged dependency; wave-1 WPs base on
  the frozen initial tip. Basing on a
  dependency WP's tip instead is the never-taken pipelining option:
  dependency-ready launch requires every dep `merged` (already on the
  feature branch), so that clause would only apply to an *unmerged*
  dependency — documented, unused.
- Concurrently-running WPs **must own disjoint file sets** — never two WPs
  on the same file.
- **Merge back onto the feature branch, serialized, in a valid topological
  order** — one WP at a time, never a batch: each merge changes the base
  under the next.
  **The merge gate is a scoped check** —
  after each WP merge onto the feature branch, run the
  [scoped check](verify.md#scoped-check). The project's **full** documented
  verification runs on **three policy triggers** and **two override paths**:
  - **(i)** the merge of a **decomposing-coordinator-owned WP**, which pays
    a full post-merge verification rather than a scoped check — the `join`
    trigger, and the one place a merge's gate is decided by *who owns the
    WP* rather than by a counter. **This narrows `adr_0010` `C-901`'s firing
    condition**: the trigger keys on the decomposing kind, not on
    "coordinator" alone, because a pipeline coordinator performs no join and
    the trigger would otherwise fire on every work package that got one.
    `C-901`'s other triggers, the scoped/full distinction and `C-904`'s
    bisection walk are unchanged.
  - **(ii)** a [checkpoint](verify.md#checkpoints).
  - **(iii)** the **final gate** — the plan's terminal verification —
    **mandatory, un-lowerable, and reached by every run that completes**.
  - **(iv)** a `Verify: full` cell or a `Verify-default: full` Status line.
  - **(v)** a **degrade** to full in **the merge gate's own run** of the
    [scoped check](verify.md#scoped-check) (no discoverable assembly gate, no
    runner-addressable WP tests) or of the selective-test convention
    (shallow-clone pre-flight, or a selective command that failed). A
    degrade at either in-worktree site binds that run only and never this
    gate.

  **The merge-triggered ones appear in the schedule log's `<trigger>`
  vocabulary**
  ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)),
  (ii) contributing three of its own; **the final gate is not a merge and
  produces no log entry**, so it appears in that vocabulary not at all — nor
  does a [Review-Fix Loop](loop.md#the-review-fix-loop) exit-gate run, `full` cell
  or not, for the same reason: it fires in the WP's own worktree before the
  merge, not at one.
  Nothing else changes about merging: serialization, topological order, the
  frozen base, merge-time file-set re-validation and `(Repo, path)`
  disjointness are untouched. **The rationale the old rule carried is
  preserved, not deleted** — cross-file interactions surface only post-merge,
  which is precisely why (ii) exists and why its cadence is bounded rather
  than left to the end of the run. **The Implement-phase verification is not
  this gate**: the [Review-Fix Loop](loop.md#the-review-fix-loop)'s phase 3 runs the
  same [scoped check](verify.md#scoped-check) in the WP's own worktree, at every tier,
  and its compile-only carve-out for a leaf under a **decomposing**
  coordinator is unchanged.
  **Sub-WP merges are not merge-gate sites at all** — a coordinator's dotted
  sub-WPs merge into the coordinator's own **shared worktree**, never onto the
  feature branch, so they run neither a scoped check nor a counter increment,
  and they produce no log entry; the coordinator's own in-worktree join check
  ([`workers/coordinator.md`](workers/coordinator.md), unchanged) stays the
  coordinator's business.
  **The parent WP's merge onto the feature branch is the one gate site the
  subtree produces**, and trigger (i) makes it a full post-merge verification,
  since the in-worktree join proves nothing about the feature branch it has
  not yet merged into.
- **Merge-time file-set re-validation** — before merging a WP, run
  `git diff --name-only <base>..<wp-branch>` (`<base>` is that WP's
  recorded base — the feature-branch tip it launched from, the frozen
  initial tip for wave-1 WPs, never the trunk) and require every listed
  file to sit inside the
  WP's declared file set. Anything outside → do not merge; reconcile
  first: justify the extra files in the plan's table, or re-scope the
  WP.
- **Merge conflict / post-merge failure playbook** — on a merge conflict
  or a failed post-merge verification, the orchestrator judges the
  collision semantically (a real design conflict versus a textual
  overlap), applies at most **one** fix pass on the feature branch, and
  re-verifies. Never loop past the one pass, never force-push, never rebase a
  published ephemeral branch. What a still-failing state implicates depends on
  which check failed:
  - **A scoped-check failure implicates exactly the merge that just ran.**
    Mark the WP `failed` in the plan's table — the long-standing behaviour,
    unchanged.
  - **A failure detected at a full documented verification gets the window
    variant**, because a scoped merge gate leaves it implicating **any** merge
    since the last full verification. Still failing after the one fix pass,
    the orchestrator **bisects the window** — *when there is a window to
    bisect*. The window is the ordered list of merges since the last full
    verification, and **every one of them recorded its post-merge
    feature-branch SHA in the plan's `## Schedule log`**
    ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)),
    so the bisection needs no new bookkeeping and no `git bisect` invocation:
    check out an already-recorded intermediate SHA, run the same full
    verification, and halve. **Cost is bounded at `⌈log₂ M⌉` extra full
    runs — two, at `M = 3`.** With a known-good base and a known-bad tip, `M`
    merges leave `M − 1` unknown SHAs to probe, so three candidates take two
    probes in the worst case and one in the best. The culprit WP is named and
    marked `failed`; the cascade rule below then governs what happens next,
    and the escalation it carries names
    **the culprit, not the window**.
  - **Two cases have no window and therefore no bisection**, and both route to
    the ordinary root-cause path the
    [Review-Fix Loop](loop.md#the-review-fix-loop) already owns, rather than claiming
    a localization this playbook cannot deliver: **(i)** an **empty window** —
    the failing verification is the final gate and the last full verification
    already covered the last merge, so nothing merged since; **(ii)** a
    **post-review-fix failure**, where the change under suspicion is an edit
    made on the feature branch rather than a merge, so it has no WP to
    attribute and no recorded SHA to probe.
  - **A third case has a window but does not localize**: a non-deterministic
    or order-dependent failure that does not reproduce at an intermediate SHA.
    That one is reported as *"failure did not bisect"* over the window, which
    is itself the diagnosis. **No WP is marked `failed` on a non-bisecting
    failure** — naming the last-merged one would be a guess the four-status
    column then presents as fact.
  - **A `failed` WP does not stop the run.** It is marked `failed` as before,
    and **the run continues while any WP is eligible**, escalating **at the
    end** rather than immediately; the state summary that escalation carries
    becomes the
    [stranded-WP report](decompose.md#parallel-by-default-decomposition).
- **Delete the ephemeral branch and remove the worktree after its WP
  merges.** The feature branch is what survives; landing it on the trunk
  is the human's step (their PR or merge flow) — hex never pushes, except
  `/hex-finalize`'s force-push of the one feature branch it was invoked
  on, consented by that invocation and approved at its gate — see
  [`finalize.md`](finalize.md#scope). **Teardown is the *top*
  orchestrator's — never a worker's and never a coordinator's, by `trap` or
  otherwise**, and it sweeps and **reports rather than deletes**
  on ambiguity — the checklist is
  [`resources.md` § 8](resources.md#8-teardown)'s and is not restated here.
- **The plan table's Status column is the WP-level state of record**
  (`pending | active | merged | failed`): execution sets `active` when a
  WP's worktree is created, `merged` after its merge, `failed` per the
  playbook. Branches and worktrees are only its evidence — resume reads
  the column, not the refs.
- **Sub-WP rows (dotted IDs)** — a coordinator splits its WP into dotted
  sub-WPs (`WP3.1`, `WP3.2`, …) that are **ordinary table rows** — same
  columns, same four statuses. **Only leaf rows are branch- and
  worktree-eligible**; a parent with children is never itself branched. The
  default for sub-WPs is the parent WP's **single shared worktree, with no
  sub-branches**; a hyphenated leaf branch `hex/<plan>--<wp>--<sub>` (never a
  slash path) is created **only for a declared true-isolation need**. A
  sub-WP's `Depends-on` inherits the parent's when absent (override for a
  tighter edge). IDs are never renumbered — next sibling = next integer,
  append-only — and there is **no schema-version marker**: the presence of
  dotted IDs is the signal.
- **Parent Status is a computed rollup, never written directly** —
  recomputed on every child write: **failed** if any child failed;
  **merged** iff every child is merged *and* the parent's join check passes;
  **active** once any child has started (is active or merged); else
  **pending**. Genuine join work (more than the sum of the children) is an
  **ordinary sibling sub-WP row** that depends on the other children — no
  fifth status, no `.join` suffix. A parent with zero children (every old
  plan) rolls up to its own literal status — the rule is vacuous, old plans
  unchanged.

**Presence checks, not a version field.** The plan's four newer fields — the
`Verify` column, the `Verify-default:` Status line, the `Reviewed:` Status
line, and the `## Schedule log` section — follow the dotted-ID rule above and
the `Repo` column rule below: **no schema-version marker**, the presence of
the field is the signal. Every reader branches on presence, never on a
compared version number — absent `Verify` cell or column ⇒ the plan's
`Verify-default:`, else `scoped`; absent `Verify-default:` ⇒ `scoped`; absent
`Reviewed:` ⇒ never reviewed ⇒ a full-branch review; absent schedule log ⇒ a
run that predates it. None of the four is an error, and **a plan without them
is a permanently valid shape, not a migration backlog**: a markdown table has
no storage or index cost, so nothing ever forces a cleanup pass. The day a
plan-format change is *not* additive — a column renamed or removed, or the
table restructured — is when a real version marker plus a migration step
earns its keep.

The plan's `- Effective-tier:` Status line follows the same
presence-not-version rule, but with a value space (`derived`) and a hard
refusal on anything else, including a present-but-empty value ([the
effective tier](decompose.md#the-effective-tier)). Presence-plus-refusal is a version
marker under another name and this file does not pretend otherwise; what
the rule above genuinely preserves is that **absence is never a version
comparison** — it is legacy, permanently, with no migration and no prompt.

Ignore `.agents/worktrees/` specifically (transient checkouts); never
ignore `.agents/` wholesale — `.agents/memory/hex.md` is shared
memory (see [`memory.md`](memory.md)).

**Federation — a plan carrying a `Repo` column.** Everything above is
per repo; a federated plan spans the lead (`.`) and one or more satellite
repos named by the lead's `Federation:` pointers
([`memory.md`](memory.md#the-three-sections)). Absent a `Repo` column every
clause below is inert and single-repo behaviour is byte-identical.

- **`Repo` column (C-302)** — the plan table's second column names the repo
  each WP runs in: a Federation key, or `.` (the empty-cell default) for the
  lead. `Expected Files` are **repo-relative to that repo**, because
  merge-time re-validation runs `git -C <repo> diff --name-only`. Sub-WPs
  inherit the parent's `Repo` and never name a different repo than the parent
  (only leaf rows are worktree-eligible, so a cross-repo split is at WP
  grain). No schema-version marker — the column's presence is the signal.
- **Pre-flight access invariant (C-303)** — no cross-repo mutation (branch,
  worktree, commit, back-pointer) occurs until, for **every** Federation key
  the plan uses, six halting clauses pass. It is a **barrier over all keys,
  not a per-repo gate**: a partially accessible or partially writable cluster
  produces zero writes.
  - (i) `git -C <path> rev-parse --show-toplevel` **must equal `<path>`** —
    `git -C` walks *up*, so a non-repo path nested in another repo silently
    reports the enclosing repo (the one clause here whose omission is silent).
  - (ii) `git -C <path> rev-parse --path-format=absolute --git-common-dir`
    **must differ from the lead's** — equality means `<path>` is another
    *worktree of the lead*, not a separate repo.
  - (iii) `git -C <path> status --porcelain` must succeed (hex's own
    uncommitted back-pointer never counts as blocking).
  - (iv) a **non-destructive write probe** must succeed — a zero-byte file
    created and removed under `<path>/.agents/`, plus
    `update-ref refs/hex/write-probe HEAD` created and deleted — because
    (i)–(iii) prove readability only and every federated write comes later.
  - (v) the repo's trunk must resolve, per C-304's discovery order.
  - (vi) `git -C <path> check-ignore -q .agents/worktrees/` must succeed — a
    satellite may never have run `/hex-init` (exempt there), so nothing
    guarantees the path is ignored; on a miss, halt and offer to add
    `.agents/worktrees/` (never `.agents/` wholesale).

  Any failure **halts** with an `Error:`/`Fix:` pair carrying a pasteable
  `--add-dir` relaunch line — never degrade, never skip a repo. The outputs
  are **echoed per key** into the announce block so clause (i) is auditable.
  The step-by-step procedure is `/hex-execute`'s (Dispatch step 1) and
  cross-references this invariant; C-305 and C-306 depend on it.
- **Shared-slug branch rule (C-304)** — `<plan-slug>` is the git-level join
  key and is **identical in every participating repo**. The lead resolves its
  feature branch unchanged (above). A **satellite always creates
  `hex/<plan-slug>` from its own trunk, never from a checked-out non-trunk
  branch** — the checked-out-branch clause is suspended for satellites; a
  satellite found on a non-trunk branch is announced at the gate as unrelated
  in-flight work. **Trunk is discovered, never assumed to be `main`**, in the
  C-303 pre-flight, in order: (1)
  `git -C <path> symbolic-ref --short refs/remotes/origin/HEAD`, stripping
  the remote prefix, authoritative when present; (2) else the trunk documented
  in that repo's **project context**, read explicitly (C-318 forbids reading
  its swarm memory); (3) else **halt and ask**, naming the repo. Whatever (1)
  or (2) yields must exist as a local ref
  (`git -C <path> rev-parse --verify refs/heads/<trunk>`) or the same halt
  fires. The resolved trunk and its source are echoed in the pre-flight line.
- **Satellite worktree mechanics (C-305)** —
  `git -C <path> branch hex/<plan-slug> <base>` (the frozen base SHA from the
  plan's `Repos:` ledger, C-317/C-324 — never a branch name), then
  `git -C <path> worktree add .agents/worktrees/<wp-slug> hex/<plan-slug>--<wp-slug>`.
  The worktree lives under the **satellite's** own `.agents/worktrees/` — the
  owning repo records the checkout and already gitignores that path. Removal
  and branch delete after merge, as today. hex never fetches.
- **Merge serialization spans repos (C-306)** — **one global topological
  order over all WP rows regardless of repo, one merge in flight at a time.**
  *Correctness* needs only two things: per-repo serialization (unchanged —
  each merge moves the base under the next) and `Depends on` ordering wherever
  an edge crosses the boundary (the DAG already enforces it); two WPs in
  different repos with no edge between them have no correctness order. **Global
  one-at-a-time is an operability choice** — one sequential orchestrator, one
  halt-capable verification at a time, resume reconstructible from the Status
  column — and is the first rule to relax if merge wall-clock ever dominates.
  Each merge is `git -C <repo> merge` onto that repo's `hex/<plan-slug>`,
  followed by the **owning repo's** documented verification read by an
  **explicit `Read`** of that repo's project context (never ambient —
  `--add-dir` does not load a satellite's `CLAUDE.md`). Cross-repo
  `Depends on` edges are ordinary; the ready-set launcher and the merge
  playbook are unchanged; the concurrency cap counts across repos.
- **`Hex-Plan:` commit trailer (C-307)** — every commit hex makes in a
  satellite carries `Hex-Plan: <remote-slug>:<repo-relative plan path>` in the
  trailer block. Ordinary git-trailer syntax, no new format,
  `git interpret-trailers`-compatible, recoverable via
  `git -C <repo> log --grep`. The WP is derivable from the ephemeral branch
  name, not the trailer. This is the only satellite-side record of the plan —
  there is never a plan copy. Lead commits do not need it but may carry it
  harmlessly.
- **`(Repo, path)` disjointness key (C-316)** — the concurrent-WP invariant
  above ("disjoint file sets") compares **`(Repo, path)` pairs**, not bare
  paths: `Expected Files` are repo-relative, so satellites routinely declare
  textually identical paths (`Cargo.toml`, `src/**`) that are nonetheless
  disjoint across repos — FM5's free parallelism. Merge-time re-validation is
  unchanged and already repo-scoped (`git -C <repo> diff --name-only` against
  that WP's satellite-relative set). Vacuous single-repo: every pair is
  `(., p)`. The plan-time set-intersection check states the same key — see
  [Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition).
- **One frozen base per participating repo (C-317)** — the frozen-base rule
  above is per feature branch, and C-304 gives a federated plan one feature
  branch per repo, so there are **N frozen bases**, one per participating
  repo, **all resolved together in the C-303 pre-gate step** — never lazily at
  first touch, which would branch a wave-1 satellite WP from a moving
  baseline. Each is **persisted as a full 40-character SHA in the plan's
  `Repos:` ledger** (C-324) so resume, review, merge-time re-validation and
  convergence all read the same `<base>` rather than re-resolving a trunk ref
  that may have moved. A WP's base is its own repo's row.

