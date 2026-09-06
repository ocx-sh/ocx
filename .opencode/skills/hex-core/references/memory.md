# .agents/memory/hex.md — Swarm Memory

The AI-maintained memory file for the hex bundle. Its only reader is an
LLM — no parser ever consumes it — so it is one small markdown file,
structured by plain headings and maintained by the skills themselves. It
holds **cached pointers, swarm preferences, and working memory — never
the only copy of project truth.**

## Location and resolution

The file lives at `.agents/memory/hex.md`. To find it, **search
upward from the current working directory** (like `.git`). A **missing
file is normal** — skills fall back to their shipped defaults and suggest
`/hex-init` to create one. Each arcana bundle keeps its own file under
`.agents/memory/<bundle>.md`; hex owns `hex.md` outright — no
section sharing across bundles, and therefore no marker fences: plain
headings are enough, skills edit sections in place by heading.

The ancestor directory `.agents/` (e.g. `plans/`, `research/`) is
the default artifact home, used only when project conventions do not name
a better one; `.agents/workers/` may hold project-local worker
personas (same format as hex-core's `workers/` files), folded in via
spawn-selection precedence
([`protocol.md`](protocol.md#spawn-selection-precedence)).

### Federation satellites

When a change spans sibling git repos, one repo is the **lead** — it
holds the plan and the `Federation:` pointers under `## Pointers` — and
the others are **satellites**. Running a hex orchestrator from a
satellite's own cwd resolves the *satellite's* memory by the upward
search above (nearest-wins, ancestors never merged), so it silently
picks up a different active plan and different verification than the
one the change actually belongs to. That split brain is converted into a
**halt**, and this section is its single definition site.

**Back-pointer grammar (C-308).** `/hex-execute` writes one bullet under
`## Pointers` in each satellite's own `hex.md`, lazily at first satellite
worktree creation for a plan (creating the file holding only this bullet
if the satellite had none):

```markdown
- Federation lead: `../acme` — this repo participates as satellite key
  `mirror` for plan(s): `acme-lib-v050`. Run hex orchestrators from the
  lead, not here.
```

The trailing **plan-slug list is part of the grammar, not decoration**:
it is what makes removal (C-313) decidable and what tells a human reading
the satellite why it is locked. A slug is appended on first worktree
creation for that plan; the list never holds duplicates. The bullet is
written into the satellite's **main checkout working tree** and left
uncommitted — memory resolution reads the filesystem, not git, so the
guard is live the instant the file is written and never waits on a branch
landing.

**The refusal (C-308).** Every orchestrator, in its resolve-memory step,
**halts** if the resolved `hex.md` carries a `Federation lead:` bullet,
and prints:

```
Error: this repo is a federation satellite (key `mirror`) for active
       plan(s) `acme-lib-v050`; its swarm memory is not the plan's memory.
Fix:   cd ../acme && claude --add-dir <this repo> …   # then re-run
       (or, if that plan is finished, delete the `Federation lead:`
        bullet from this repo's hex.md — see C-313)
```

**This file is the single definition site for that text.** The four
orchestrator `SKILL.md` resolve-memory steps each add **one line linking
here** and never restate the `Error:`/`Fix:` block — four copies of a
printed halt would be exactly the copy-drift single-source forbids.

**Scope, and the one exemption.** The halt fires on the bullet's
presence, unconditionally, for `/hex-plan`, `/hex-execute`, `/hex-review`
and `/hex-architect` — a satellite mid-federated-change must not
accumulate a second, competing active plan, and telling "related" from
"unrelated" local work is the judgement call that produced this split
brain in the first place. **`/hex-finalize` is inside the halt too** —
the first non-orchestrator to be — on its own ground: it resolves no
plan, but it rewrites and force-pushes a branch that may be a row in a
lead's `Repos:` ledger, so the plan-state test is not what governs it
(C-824). A hex skill that is not an orchestrator — one that resolves no
plan, writes no plan or federation state, **and performs no act on a git
remote or forge** (the act set `finalize.md` enumerates) — sits
**outside** the halt rather than exempt from it; today that is
`hex-discuss`, whose memory writes are limited to the post-gate
discussion hand-off record and index rows (C-708). Those
writes land in whatever memory file the ordinary upward search resolves —
in a satellite, that repo's own — and are **advisory**: no federated run
reads a satellite's `hex.md` (§ Destination of knowledge › "One `hex.md`,
the lead's").
**`/hex-init` is exempt** rather than outside, because it *does* write
federation state — the `Federation lead:` bullet removal below is its
own: it writes no plan, resolves no active-plan pointer, and must stay
runnable so a satellite can be audited (and so that removal can be
offered). The two escape hatches are both printed in the `Fix:` line:
relaunch from the lead, or delete the bullet. **Halt, never degrade** — a
satellite with *no* memory file is the benign case: upward search finds
nothing, the run falls back to shipped defaults with no active-plan
pointer, so it stops and asks for a target. Visible, not silent.

**`/hex-finalize` prints its own `Fix:` (C-824).** The `Fix:` block above
tells the reader to relaunch from the lead, and for finalize that is a
wrong-repo action — re-running it there finalizes *the lead's* branch, not
this one. Its halt prints instead:

```
Error: this repo is a federation satellite (key `mirror`) for active
       plan(s) `acme-lib-v050`; /hex-finalize is single-repo, and this
       branch may be a row in the lead's `Repos:` ledger.
Fix:   finalize this branch by hand — the recomposition and the push are
       yours until federated finalize exists. Keep a backup ref before
       the rewrite and pin your own `--force-with-lease`. Do not re-run
       from the lead; that finalizes the lead's own branch, not this one.
```

**Two residual limits, stated rather than implied.** The halt keys on the
`Federation lead:` bullet, which a virgin satellite does not carry and which
the second escape hatch permits deleting, so for finalize it is a heuristic,
not a guarantee. And C-323's structural invariant reads the *plan*, which is
already terminal and pointer-cleared by the time finalize runs, so it is
unavailable here as a second check.

**Removal lifecycle (C-313).** The bullet is a **lock on running hex
locally in a satellite**, so it expires with the plan that justified it:

- **Remove:** `/hex-execute`'s upkeep step removes a plan's slug from
  every participating satellite's bullet when that plan reaches State
  `done` — which for a federated plan requires every repo confirmed
  landed, so the lock outlives the broken-integration window rather than
  expiring inside it. The delete-when-empty / delete-file-if-sole cascade
  that follows is defined in full in
  [`protocol.md` § Upkeep step](protocol.md#upkeep-step) (C-313).
- **Also removed** when a plan is abandoned: `/hex-init` in a satellite
  offers removal for any slug whose lead-side plan no longer exists, or
  exists with State `done`, resolving the lead by the bullet's own path —
  the re-audit shape already used for stale pointers.
- **Manual removal is always legitimate** — deleting the bullet by hand
  is supported and printed in the `Fix:` line; the bullet is
  skill-managed, never authoritative, and the worst case of removing it
  early is the pre-guard status quo, which the next federated run
  repairs.
- **Never** removed silently as a side effect of anything else, and never
  rewritten to point at a different lead: a satellite that would need two
  leads is a design smell, so a second `Federation lead:` bullet naming a
  different lead **halts** at the pre-flight.

**Fail-closed by construction (C-323).** The back-pointer cannot be the
guarantee, because it is written by the very execution it guards — a
**virgin** satellite (a fresh clone, a second machine, the first run ever
launched there) carries none. The load-bearing invariant is structural
instead: **a run is federated only if its own resolved `hex.md` carries
at least one `Federation:` bullet.** Satellite paths resolve from exactly
one place — the lead's `## Pointers` — so a run without those bullets
cannot name a satellite at all; it is an ordinary single-repo run.
Therefore a plan carrying a `Repo` column, executed or reviewed by such a
run, **halts**:

```
Error: this plan is federated (`Repo` column: `.`, mirror, mcp) but this
       run resolved no `Federation:` pointers — it cannot address those
       repos and must not execute a partial plan.
Fix:   cd <lead> && claude --add-dir <satellite> …   # run from the lead
```

Every `Repo` value must additionally resolve to a declared key, or halt
with the same pair — no key, no path, no write. C-308's bullet is
retained as the **earlier, better-worded** refusal (it fires at memory
resolution, before a target is resolved, and it names the lead so the
human gets a `cd` line instead of a puzzle) and as C-313's
garbage-collectable lock marker; it is a convenience and a diagnostic,
not the guard. Where the two disagree, C-323 is the invariant.

**Vacuous when absent.** No `Federation:` bullet and no `Repo` column
trips none of this: a single-repo project resolves memory exactly as
described above and behaves byte-identically.

## The three sections

Sections are heading conventions, not a schema. Skills read the whole
file (small by contract) and edit sections in place, respecting the
ownership column. Other hex files reference them as `hex.md › Pointers`,
`hex.md › Preferences`, and `hex.md › Memory`.

| Section | Holds | Owner |
|---|---|---|
| `## Pointers` | Cached locations discovered from project context, plus the one **measured** value a run caches beside them: where verification is documented, where the selective test command is documented, where the project's security-sensitive / hot-path convention is documented, where spec/plan/ADR conventions live, doc and product-knowledge homes, key rule files, any worktree-location deviation from the default, the constitution / governing-principles location (optional), **`Resource profile:`** — the measured peak RSS, wall time and `light \| heavy` class of the project's documented verification gate plus the heavy-command ceiling derived from them ([`resources.md` § 2](resources.md#2-the-measured-resource-profile)), re-measured at upkeep when it drifts — and **`Scratch:`** — the disk-backed per-run root a run redirects its scratch environment to. Those two are cache like every other entry here, never authoritative, under this row's ownership rule. The sensitive-path entry is the named source the high-risk merge trigger reads; `key rule files` above points at the rule files themselves — two entries, two jobs. The two conventions inside that entry resolve **independently** — silence on one leaves that half absent, therefore `true` — and each half's value is a repo-relative path to the file documenting the convention, never the convention's own value ([`decompose.md`](decompose.md#the-effective-tier)'s read rule); [Staleness](#staleness) re-detects first and fails closed only if re-detection finds nothing. **Federation** pointers to sibling repos, when a change spans them — one bullet per satellite in the **lead** repo, fixed grammar `- Federation: <key> → <path> (<remote>); verification documented in …`: the `<key>` is the plan table's `Repo` value (the lead is `.` and is **never** listed), `<path>` is lead-relative (absolute permitted), `<remote>` is identity only (hex never clones, fetches, pulls or pushes), and the verification clause is a **pointer, never a command literal** — unlike an own-repo verification pointer it does not cache the satellite's command. Vacuous when absent. | **Skills** — a self-managed cache. Never authoritative: on conflict, project context wins; a stale pointer means re-detect and re-point (upkeep step), never wrong behavior. |
| `## Preferences` | The instantiated model matrix (literal names) and per-cell overrides, the cross-model adversary skill, limits (max-workers, loop rounds, adversary-timeout), always-on perspectives / research axes. | **User** — written by `/hex-init` with consent; orchestrators read it, never edit it. |
| `## Memory` | The active plan pointer, an artifact index (what lives where), and learned facts worth persisting. | **Skills** — working memory, updated during runs. |

**Config carrier.** `## Preferences`'s first content, when present, is one
fenced ` ```yaml ` block; prose bullets continue below it and still carry
nuance the keys cannot. A missing file, section, or block is **normal** —
shipped defaults apply. The block is written **only by `/hex-init` with
consent**; orchestrators read it and never write it. The key vocabulary,
defaults, and merge rules it carries are defined in
[`config.md`](config.md), not here — a project with no block costs a run
nothing to skip.

## Example file

```markdown
# hex — swarm memory

Maintained by the hex skills. Small by contract: pointers and
preferences, not copies. Team-shared — commit it.

## Pointers

- Verification: `README.md` › "Checks" — run `make check` before any merge.
- Plan / ADR conventions: decisions in `docs/decisions/` (MADR format),
  plans in `docs/plans/`. Shipped hex templates are the fallback only.
- Spec home: `docs/specs/` — the fold-back target; ID marker: see
  Preferences › Spec ID marker.
- Product knowledge: `.agents/product.md` (indexed from `CLAUDE.md`).
- Key rules: `CONTRIBUTING.md` › "Conventions"; code under `src/auth/**`
  is security-sensitive.
- Worktrees: default `.agents/worktrees/` (gitignored).
- Resource profile: `make check` — peak RSS 6 GB, wall 7m10s, class
  `heavy`; heavy ceiling 3 (measured 2026-09-06; re-measure on drift).
- Scratch: per-run root under `${XDG_CACHE_HOME:-$HOME/.cache}/hex/<run-id>/`.
- Constitution: `docs/constitution.md` (optional — plans are gated
  against it when present; gate skipped silently when absent).
- Federation: `mirror` → `../acme-mirror`
  (`https://github.com/acme-sh/acme-mirror.git`); verification documented in
  its `CLAUDE.md` › "Verify" — a pointer, not the command (contrast the
  Verification bullet above, which caches `make check`).

## Preferences

- Models (instantiated for this harness): fast-balanced → Sonnet,
  deep-reasoning → Opus. Override: builder:implement → Opus at every tier.
- Review by join level: L1 budget 5 min, L2 rounds 2.
- Cross-model adversary: `codex-adversary` skill.
- Limits: max-workers 6, loop-rounds 2, adversary-timeout 3 (below the
  default, so it lowers the stall window).
- Always-on perspectives: security review mandatory under `src/auth/**`.
- Research axes of interest: registry ecosystems, OCI spec evolution.
- Spec ID marker: `^#{1,6}\s+(C|S)-[0-9]+\b` (body = heading to next
  heading of same or shallower depth) — user-owned; only `/hex-init`
  writes it, a run never edits it.

## Memory

- Active plan: `docs/plans/2026-07-add-export.md` (phase: Implement).
- Folded plans (artifact index): `docs/plans/2026-06-add-import.md`
  reached `done`; hex-review's fold-back Upkeep cleared its active-plan
  pointer and folded its deltas → `docs/specs/spec_import.md`
  (2026-06-30).
- Artifacts: export-format research →
  `.agents/research/export-formats.md`.
- Learned: the acceptance suite needs a running fixture registry
  (`make fixtures` first) — undocumented in the repo, worth persisting.
```

## Editing rules

- **Ownership per section** (table above): skills edit Pointers and
  Memory freely as part of their flow; Preferences changes — including its
  fenced `yaml` config block ([`config.md`](config.md)) — only through
  `/hex-init` with the user's consent; orchestrators read the block, never
  write it.
- **Keep it small — pointers, not prose dumps.** If a section wants to
  grow, move the content into a file under `.agents/` (or a
  project doc home) and point at it.
- **Team-shared: it belongs in version control.** Never gitignore
  `.agents/` wholesale — that would drop shared memory. Gitignore
  `.agents/worktrees/` specifically (transient checkouts).

## Destination of knowledge

Every fact hex learns has exactly one home — it is never duplicated,
because a second copy drifts:

- **Useful to any agent or human** (how to verify, spec/plan/ADR
  conventions, product identity, worktree location) → **project
  context**. Short facts go straight into the client's context file
  (CLAUDE.md / AGENTS.md / project rules) as a section; larger bodies get
  their own doc — a de-facto home when one exists (`docs/`, README), else
  a provisioned file such as `.agents/product.md` — plus a **one-line
  index entry in the context file** so every agent discovers it
  ambiently. `/hex-init` provisions both, with consent.
- **Only the swarm consumes it** (model overrides, the adversary skill,
  limits, perspectives, research axes) → **`hex.md › Preferences`**.
- **Where things live** (locations discovered from project context) →
  **`hex.md › Pointers`** — a cache the skills maintain themselves so a
  run doesn't re-discover the repo; never the truth, always re-pointable.
- **Run state and learned facts** → **`hex.md › Memory`**.
- **In-flight discussion state** → **the discussion artifact**, never
  `hex.md`. `hex.md` holds pointers, not prose (§ Editing rules — "keep it
  small"), and a discussion is prose by nature; `hex.md › Memory` records
  only the discussion hand-off record, the artifact-index row, and the
  promotion candidate a drain leaves behind (C-711). That candidate is
  proposed into **project context** by the next `/hex-init` **re-audit**,
  with consent ([§ Staleness](#staleness)) — the route for *project*
  knowledge. Destination is what separates it from
  [`protocol.md` § Upkeep step](protocol.md#upkeep-step)'s route for a
  surfaced *preference*, which is bound for `hex.md › Preferences`.

Product identity (what the product is, who uses it and where, related
repositories, research keywords, comparable tools) is project knowledge,
not swarm knowledge: it lives in the README / product doc / provisioned
product file, and `hex.md › Pointers` merely records where. Researchers
and reviewers follow the pointer.

**One `hex.md`, the lead's (C-318).** When a change spans repos, a
federated run resolves **exactly one** memory file — the **lead's** — and
the satellite halt ([§ Location and resolution](#location-and-resolution) ›
Federation satellites) guarantees no run ever resolves a satellite's. So
the destinations above keep their single owner unchanged across the repo
boundary: the lead's `Preferences` (model matrix, adversary, perspectives,
`max-workers`), the lead's `Pointers`, and the active-plan pointer in the
lead's `Memory` are the lead's **exclusively**. A satellite's own copies
are never read, merged, reconciled or ranked — there is **no cross-repo
merge, precedence or nearest-wins rule**; the single upward search of
§ Location and resolution is unchanged. A satellite's own verification and
project rules are not swarm knowledge: they are read from **that repo's
project context** by explicit `Read`, never assumed from ambient context
and never from its `hex.md`.

## Staleness

Keeping memory true is made cheap rather than impossible:

- **Pointers, not copies.** A stale pointer degrades to "re-detect and
  re-point", never to wrong behavior — which is why this file holds
  locations, not contents.
- **Verify on consumption.** A skill that acts on a Pointers entry first
  checks the target still exists and still covers what the pointer
  claims. On a miss it re-discovers from project context, **updates the
  pointer in the same run**, and proceeds — stale memory is recognized
  and repaired where it is found, never silently trusted or deferred.
- **Per-run upkeep.** Each orchestrator's final phase re-points any
  remaining Pointers drift this run revealed and updates Memory (see the
  upkeep step in [`protocol.md`](protocol.md#upkeep-step)). Portable —
  part of the flow, no hooks.
- **`/hex-init` re-audit.** Re-entrant runs re-verify that each pointer
  and each context-file index line still resolves, report drift, and fix
  it with the user's consent.
