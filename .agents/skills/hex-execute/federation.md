# Federation — satellite repos in a run

Read by [`SKILL.md`](SKILL.md) only when the resolved plan carries a `Repo`
column. Absent that column none of this fires. A run is federated only if its
own resolved `hex.md` carries `Federation:` bullets; if the plan carries a
`Repo` column but this run's does not, **halt** per [`memory.md` § Location and
resolution](../hex-core/references/memory.md#location-and-resolution) — it
cannot address the plan's satellite repos and must not execute a partial plan.
Every `Repo` value must resolve to a declared Federation key, or halt the same
way.

## Pre-flight (C-303)

Run in the pre-announce window — after memory and target are resolved,
**before any branch, worktree, commit or back-pointer**. It *realizes* the
pre-flight access invariant defined once in
[`worktree.md` § Pipeline worktree mechanics](../hex-core/references/worktree.md#pipeline-worktree-mechanics);
protocol.md owns that invariant — the steps below are its executable
realization.
For **every** Federation key the plan's `Repo` column actually uses (resolved
against the lead's `Federation:` bullets in `hex.md › Pointers`), with `<path>`
the key's declared path, run:

```bash
git -C <path> rev-parse --show-toplevel                          # (i)   must equal <path>
git -C <path> rev-parse --path-format=absolute --git-common-dir  # (ii)  must differ from the lead's
git -C <path> status --porcelain                                 # (iii) must succeed
mkdir -p <path>/.agents && : > <path>/.agents/.hex-write-probe \
  && rm <path>/.agents/.hex-write-probe                          # (iv-a) working-tree writability
git -C <path> update-ref refs/hex/write-probe HEAD \
  && git -C <path> update-ref -d refs/hex/write-probe            # (iv-b) git-dir writability
git -C <path> symbolic-ref --short refs/remotes/origin/HEAD      # (v)   trunk discovery, step 1
git -C <path> check-ignore -q .agents/worktrees/                 # (vi)  worktree path must be ignored
```

- **(iv) the write probe** is why (i)–(iii) — readability only — is not enough:
  every federated write comes later, so the probe writes twice and undoes both
  immediately (a zero-byte file under `<path>/.agents/`, and a
  `refs/hex/write-probe` ref created from `HEAD` and deleted), leaving the repo
  byte-identical. A read-only grant fails here, not half-way through execution.
  Its `writable` outcome is disclosed in the announce block; on `.agents/`
  pre-creation a declined run leaves only that inert directory.
- **(v) trunk discovery, in order — `main` is never assumed:** (1) `origin/HEAD`
  above, stripping the remote prefix, authoritative when present; (2) else the
  trunk documented in **that repo's project context**, read by an explicit
  `Read` (C-318 forbids reading its swarm memory); (3) else **halt and ask**,
  naming the repo, with the `git -C <path> remote set-head origin --auto` fix.
  Whatever (1) or (2) yields must exist as a local ref
  (`git -C <path> rev-parse --verify refs/heads/<trunk>`) or the same halt fires.
- **(vi)** on a miss, **halt and offer to add `.agents/worktrees/`** to that
  repo's ignore file — never `.agents/` wholesale
  (`.agents/memory/hex.md` is version-controlled). A satellite may never
  have run `/hex-init` (exempt there), so nothing else guarantees the path is
  ignored.
- **Barrier, not a per-repo gate.** Issue the run's first
  `git -C <satellite> branch` / `worktree` / commit / back-pointer write only
  after the **last** key has cleared all six clauses — a partially accessible or
  partially writable cluster produces **zero** writes. On any failure **halt**
  with an `Error:`/`Fix:` pair carrying a pasteable `--add-dir` relaunch line
  (print the narrowest form that works — one ancestor grant, e.g.
  `--add-dir /home/mherwig/dev`, covers a whole sibling cluster); never degrade,
  never skip a repo.
- **Freeze and record the bases here.** In this same step, resolve each
  participating repo's frozen base (its trunk tip; the lead's is its resolved
  feature-branch tip) and trunk, and write them **once** into the plan's
  `Repos:` ledger ([below](#the-repos-ledger-c-324)) — never re-resolved later
  (C-317/C-324).
- **Echo per key.** Emit one pre-flight line per key into the announce block —
  `toplevel`, `common-dir`, `clean`, `writable`, `trunk=<branch> (<source>)` —
  so clause (i), the one whose omission is silent, is auditable:

  ```
  Pre-flight: mirror  ../acme-mirror  toplevel=/home/mherwig/dev/acme-mirror
                      common-dir=/home/mherwig/dev/acme-mirror/.git
                      clean  writable  trunk=main (origin/HEAD)
  ```

`/hex-plan` never runs this pre-flight (planning needs no satellite access);
`/hex-review` runs its read-only subset (C-320).

## The `Repos:` ledger (C-324)

A plan carrying a `Repo` column carries one `Repos:` sub-block inside its
Status block — one line per participating repo: key, absolute path, resolved
trunk (C-304), full 40-character base SHA (C-317) and a `landed:` flag.

```markdown
- Repos:   <!-- frozen at execution start; bases are never re-resolved -->
  - `.`      /home/mherwig/dev/acme         trunk `main` base `a1b2c3d4…`  landed: yes (2026-07-21)
  - `mirror` /home/mherwig/dev/acme-mirror  trunk `main` base `e5f6a7b8…`  landed: no
```

- **Written once**, by hex-execute in the pre-flight step, before any branch —
  the resolved trunk and full base SHA per repo, `landed: no`. Existing rows
  are never re-written.
- **Read, never re-resolved**, by resume, `/hex-review`, merge-time file-set
  re-validation and convergence: `<base>` for every diff is that SHA, never a
  trunk ref that may have moved since (C-317).
- **Resume halt.** A resumed run that finds the block **absent while the table
  carries a `Repo` column and any pipeline row is not `pending`** cannot
  recover its bases and **halts** — diffing against a moved baseline is silent
  corruption. With every pipeline still `pending`, the block is simply written
  fresh.

The field layout is the plan template's
([`plan.md`](../hex-init/assets/templates/plan.md)) and the persistence
invariant is worktree.md's (§ Pipeline worktree mechanics, C-317); this
section owns only the write moment and the read-never-re-resolve rule.

## A pipeline whose `Repo` is a satellite key

The mechanics are worktree.md's (§ Pipeline worktree mechanics —
satellite worktrees, merge serialization, the `Hex-Plan:` trailer); this
section owns only what hex-execute *does*.

- **Project context per owning repo (C-306, C-318).** A satellite pipeline's
  rules and verification are read by an explicit `Read` of **that repo's**
  project context, never ambient — `--add-dir` does not load a satellite's
  `CLAUDE.md`. Its steps run in the satellite worktree, commit and merge there
  via `git -C <repo>`, onto that repo's `hex/<plan-slug>`.
- **Satellite worktree from the frozen SHA (C-305).** Create the branch and
  worktree in the owning repo:
  `git -C <path> branch hex/<plan-slug> <base>` — where `<base>` is that repo's
  **frozen base SHA read from the `Repos:` ledger**, never a trunk ref — then
  `git -C <path> worktree add .agents/worktrees/<pipeline-slug> hex/<plan-slug>--<pipeline-slug>`.
  The worktree lives under the **satellite's** own `.agents/worktrees/`.
- **Lazy back-pointer write (C-308).** At the **first** satellite worktree
  creation for this plan, append the plan slug to that satellite's
  `Federation lead:` bullet under `## Pointers` in
  `<path>/.agents/memory/hex.md` — the satellite's **main checkout
  working tree**, never the ephemeral worktree and never on the
  `hex/<plan-slug>` branch, because memory resolution reads the filesystem, not
  git, so the FM6 guard is live the instant the file is written. Create the file
  holding only this bullet if the satellite has none; the slug list never holds
  duplicates. Leave it **uncommitted** — hex does not commit it, and its
  presence never blocks the (iii) `status --porcelain` check. The write is
  disclosed in the announce block (C-315); the bullet grammar and the
  `Error:`/`Fix:` halt it later triggers are defined once in
  [`memory.md` § Location and resolution](../hex-core/references/memory.md#location-and-resolution).
- **`Hex-Plan:` trailer (C-307).** Every commit hex makes in a satellite carries
  `Hex-Plan: <remote-slug>:<repo-relative plan path>` in its trailer block — the
  only satellite-side record of the plan.
- **Gates run per repo.** The integration gate runs in each participating
  repo against its own documented verification.

## Landing re-entry and upkeep

`landing` — a plan that `/hex-review` advanced past execution and that awaits
its satellite feature branches being landed into their trunks — is a
**finalize-only re-entry**: skip every execution step and run **only** the
Federation landing-confirmation
[upkeep step](../hex-core/references/protocol.md#upkeep-step) — the step that
advances the plan to `done` once every `Repos:` row is confirmed landed and
then releases the C-313 satellite locks. Upkeep cleared the active-plan
pointer at `landing`, so a no-argument re-run cannot resolve it: pass the
explicit plan path.

Upkeep additionally offers to confirm each `Repos:`-ledger row's landing,
advancing a `landing` plan to `done` only when every row is landed (C-324),
and on `done` removes the plan's slug from each satellite's `Federation lead:`
bullet — deleting the bullet, and the file if it held nothing else (C-313).

## Handoff addendum

The handoff additionally enumerates the per-repo feature branches in required
landing order and names the window in which the satellites do not build against
lead trunk (C-312); hex never pushes and records no landing it did not observe
locally. It also lists the **uncommitted `Federation lead:` back-pointer files**
written into each satellite's main checkout (C-308) for the human to commit —
until then the FM6 guard is machine-local.

```
Feature branches to land, in this order:
  1. acme-sh/acme          hex/acme-lib-v050
  2. acme-sh/acme-mirror   hex/acme-lib-v050
  3. acme-sh/acme-mcp      hex/acme-lib-v050
Between 1 and 2 the satellites do not build against acme trunk.

Uncommitted back-pointers to commit (working-tree change, not committed by hex):
  ../acme-mirror/.agents/memory/hex.md
  ../acme-mcp/.agents/memory/hex.md
```
