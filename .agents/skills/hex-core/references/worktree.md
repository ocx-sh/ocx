# hex Worktree Mechanics

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Pipeline worktree mechanics

Every run integrates through **one feature branch**; each **pipeline** runs
on its own branch in its own worktree:

- **One feature branch per plan** — the integration target for every
  pipeline. Resolve it once, at execution start: the non-trunk branch
  already checked out, else create `hex/<plan-slug>` from the trunk. The
  contract wave commits onto it; its tip then is the **frozen base** every
  pipeline branches from — never a moving baseline.
- **One branch + worktree per pipeline** — branch
  `hex/<plan-slug>--<pipeline-slug>` (hyphenated, not
  `hex/<plan-slug>/<pipeline-slug>`: git refs are paths, so the feature
  branch `hex/<plan-slug>` cannot also be a ref *directory*), worktree
  `.agents/worktrees/<pipeline-slug>/` (the default; a deviation location is
  documented in project context (cached in `hex.md › Pointers`) — it
  describes repo layout, not hex behavior). All pipelines start together
  from the frozen base. The pipeline's steps share its worktree and commit on
  its branch, serially.
- Concurrently-running pipelines **must own disjoint file sets** — never two
  pipelines on the same file.
- **Hub and generated files** (lockfiles, baselines, goldens; `/hex-init`
  lists them in project context) are **never committed by a pipeline**. They
  are regenerated **once, at integration, minimally** — no dependency
  upgrade — as one commit on the feature branch.
- **Job budget** — parallel builds share one build's budget: **live
  pipelines × jobs per pipeline ≤ the project's single-build jobs** (its
  `jobs` setting). The orchestrator sets each pipeline's share at launch and
  the step brief names it. RAM stays at about one build; shared caches do not
  fix serialization and are not relied on.
- **Merge back onto the feature branch, serialized** — one pipeline at a
  time, as each lands, never a batch: each merge changes the base under the
  next. Merges use `--no-verify` and run **no gate and no check**
  ([commits and hooks](verify.md#commits-and-hooks)); the integration gate
  runs once, after the last merge and the hub-file regeneration
  ([the two gates](verify.md#the-two-gates)).
- **Merge-time file-set re-validation** — before merging a pipeline, run
  `git diff --name-only <base>..<pipeline-branch>` (`<base>` is the frozen
  base, never the trunk) and require every listed file to sit inside the
  pipeline's declared file set. Anything outside → do not merge; reconcile
  first: justify the extra files in the plan's table, or re-scope the
  pipeline.
- **Merge conflict playbook** — on a merge conflict the orchestrator judges
  the collision semantically (a real design conflict versus a textual
  overlap) and applies at most **one** fix pass on the feature branch. Never
  loop past the one pass, never force-push, never rebase a published
  pipeline branch. A failing integration gate follows
  [the two gates](verify.md#the-two-gates), not this playbook.
- **Delete the pipeline branch and remove its worktree after it merges.**
  **Removing a worktree removes its build outputs** — target directories and
  caches live inside it — so none outlives its pipeline. The feature branch
  is what survives; landing it on the trunk is the human's step (their PR or
  merge flow) — hex never pushes, except `/hex-finalize`'s force-push of the
  one feature branch it was invoked on, consented by that invocation and
  approved at its gate — see [`finalize.md`](finalize.md#scope).
  **Teardown is the *top* orchestrator's — never a worker's, by `trap` or
  otherwise**, and it sweeps and **reports rather than deletes** on
  ambiguity — the checklist is [`resources.md` § Worktree cleanup](resources.md#worktree-cleanup)'s
  and is not restated here.
- **The plan table's Status column is the pipeline-level state of record**
  (`pending | active | merged | failed`): execution sets `active` when a
  pipeline's worktree is created, `merged` after its merge, `failed` when the
  orchestrator defers the pipeline as residue after escalation. Branches and
  worktrees are only its evidence — resume reads the column, not the refs.
  **A `failed` pipeline does not stop the run**; the run continues and
  escalates at the end.

**Presence checks, not a version field.** Optional plan fields — the
`Reviewed:` Status line and the `Repo` column — are read by presence: absent
`Reviewed:` ⇒ never reviewed ⇒ a full-branch review. None is an error, and a
plan without them is a permanently valid shape, not a migration backlog.

Ignore `.agents/worktrees/` specifically (transient checkouts); never
ignore `.agents/` wholesale — `.agents/memory/hex.md` is shared
memory (see [`memory.md`](memory.md)).

**Federation — a plan carrying a `Repo` column.** Everything above is
per repo; a federated plan spans the lead (`.`) and one or more satellite
repos named by the lead's `Federation:` pointers
([`memory.md`](memory.md#the-three-sections)). Absent a `Repo` column every
clause below is inert and single-repo behaviour is byte-identical.

- **`Repo` column (C-302)** — the plan table's second column names the repo
  each pipeline runs in: a Federation key, or `.` (the empty-cell default)
  for the lead. `Expected Files` are **repo-relative to that repo**, because
  merge-time re-validation runs `git -C <repo> diff --name-only`. A pipeline
  lives in one worktree, so it runs in one repo; a cross-repo split is at
  pipeline grain. No schema-version marker — the column's presence is the
  signal.
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
  `git -C <path> worktree add .agents/worktrees/<pipeline-slug> hex/<plan-slug>--<pipeline-slug>`.
  The worktree lives under the **satellite's** own `.agents/worktrees/` — the
  owning repo records the checkout and already gitignores that path. Removal
  and branch delete after merge, as today. hex never fetches.
- **Merge serialization spans repos (C-306)** — **one merge in flight at a
  time, across all repos.** *Correctness* needs only per-repo serialization
  (each merge moves the base under the next); global one-at-a-time is an
  operability choice — one sequential orchestrator, resume reconstructible
  from the Status column — and is the first rule to relax if merge
  wall-clock ever dominates. Each merge is `git -C <repo> merge --no-verify`
  onto that repo's `hex/<plan-slug>`, with no gate after it. The owning
  repo's documented verification, read by an **explicit `Read`** of that
  repo's project context (never ambient — `--add-dir` does not load a
  satellite's `CLAUDE.md`), runs at the integration gate, as a row of the
  cross-repo table ([Verification](verify.md#verification)). The job budget
  is per repo.
- **`Hex-Plan:` commit trailer (C-307)** — every commit hex makes in a
  satellite carries `Hex-Plan: <remote-slug>:<repo-relative plan path>` in the
  trailer block. Ordinary git-trailer syntax, no new format,
  `git interpret-trailers`-compatible, recoverable via
  `git -C <repo> log --grep`. The pipeline is derivable from the branch
  name, not the trailer. This is the only satellite-side record of the plan —
  there is never a plan copy. Lead commits do not need it but may carry it
  harmlessly.
- **`(Repo, path)` disjointness key (C-316)** — the concurrent-pipeline
  invariant above ("disjoint file sets") compares **`(Repo, path)` pairs**,
  not bare paths: `Expected Files` are repo-relative, so satellites routinely
  declare textually identical paths (`Cargo.toml`, `src/**`) that are
  nonetheless disjoint across repos — FM5's free parallelism. Merge-time
  re-validation is unchanged and already repo-scoped (`git -C <repo> diff
  --name-only` against that pipeline's satellite-relative set). Vacuous
  single-repo: every pair is `(., p)`. The plan-time set-intersection check
  states the same key — see [`decompose.md`](decompose.md).
- **One frozen base per participating repo (C-317)** — the frozen-base rule
  above is per feature branch, and C-304 gives a federated plan one feature
  branch per repo, so there are **N frozen bases**, one per participating
  repo, **all resolved together in the C-303 pre-gate step** — never lazily at
  first touch, which would branch a satellite pipeline from a moving
  baseline. Each is **persisted as a full 40-character SHA in the plan's
  `Repos:` ledger** (C-324) so resume, review, merge-time re-validation and
  convergence all read the same `<base>` rather than re-resolving a trunk ref
  that may have moved. A pipeline's base is its own repo's row.
