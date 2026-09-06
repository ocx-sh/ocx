---
name: hex-init
description: Initialize or reconfigure a project for the hex swarm - audits project context (CLAUDE.md/AGENTS.md) for the knowledge the orchestrators need (how to verify, spec/plan conventions), provisions what is missing into project context, optionally seeds default templates, and bootstraps the AI-maintained swarm memory at .agents/memory/hex.md (cached pointers, orchestration preferences, perspectives of interest). Re-entrant - run again anytime to re-audit pointers or change the setup.
license: Apache-2.0
metadata:
  keywords: init,setup,configuration,conventions,templates,swarm,onboarding
  repository: https://github.com/michael-herwig/arcana
  summary: Project-context audit and setup for the hex swarm bundle
disable-model-invocation: true
user-invocable: true
---

# hex-init — Project Initializer

`hex-init` audits and bootstraps a project for the hex swarm. Run it only
when the user explicitly invokes it — it never triggers itself from
another skill's flow. It is re-entrant: every run re-audits, reports
drift, and fixes it with the user's consent.

## Core stance

Knowledge about the *project* — how to verify it, where specs, plans, and
ADRs live and in what format, which rules carry architectural weight, and
what the product is and who uses it — belongs in the project's own context
(CLAUDE.md, AGENTS.md, or docs they point to), where every agent and tool
already reads it. hex never keeps a second copy of that knowledge; a
second source of truth would drift and rot. `hex-init` is an **auditor and
provisioner of project context first**, and a config writer only for what
genuinely belongs to the swarm — model preferences, cross-model review,
perspectives of interest. The swarm file (`.agents/memory/hex.md`)
holds only three things: a cache of pointers to where that project
knowledge lives, swarm config, and the orchestrators' working memory (see
[`../hex-core/references/memory.md`](../hex-core/references/memory.md)).

Every fact has exactly one home: useful to *any* agent → project context;
useful only to the swarm → `hex.md › Preferences`. Never both — project
knowledge is recorded in `hex.md › Pointers` only as a pointer to where
it lives, never copied.

## The wizard

`hex-init` runs as an **interactive wizard**, not a batched form. The
single-gate rule ([`protocol.md`](../hex-core/references/protocol.md)) that
forbids mid-flow questions governs the four *orchestrators*, which run long
autonomous swarms where an interruption strands parallel workers.
`hex-init` runs no swarm and is **exempt** — it may ask its questions in
sequence. The exemption is narrow and named — `hex-init` is one of the two
skills it covers, each on its own ground; the list is owned by
[`protocol.md` § The meta-plan approval
gate](../hex-core/references/protocol.md#the-meta-plan-approval-gate).

Every question the Flow below asks obeys this shape:

- **Flags are validated before any prompting.** A bad flag surfaces its
  `Error:`/`Fix:` up front — it cannot discard a finished wizard walk.
- **`[n/N]` progress header** on each question, its label stating *why the
  step matters*, not just the field name.
- **Validation re-prompts in place** — a rejected answer re-asks with the
  validator's error string and prior answers intact; an accepted one echoes
  back (`Added 'reviewer:accessibility' (.agents/workers/)`).
- **Detectable answers are never asked** — the harness in use, whether
  `.agents/worktrees/` is gitignored, whether a project persona file exists,
  the verification command, anything answered on a prior run and unchanged.
  Asking a detectable thing is the tell of a badly designed init.
- **Conditional questions fire only on a Step 1 trigger** — the perspectives
  question only when the audit found a security-sensitive path or a project
  persona; the research-axes question only when research artifacts exist.
- **Current state prints before the first question**; each value carries a
  `[current]` label and is its default, and multi-selects are pre-checked
  from current state.
- **A diff prints before every write** — one line per changed key, old →
  new, derived keys named alongside touched ones.
- **Apply is a separate consent**, taken after the diff.
- **Cancellation is always available** and aborts with nothing written;
  every refusal pairs `Error:` with `Fix:`.

This replaces the former "one batched approval" in Steps 2 and 4½: the
questions are walked in sequence, then one diff and its apply consent gate
the write. The **non-interactive twin** (`--yes`, `-d`, and its triggers)
is documented under [Arguments](#arguments).

## Flow

### 1. Audit project context

Run the checklist in [`references/audit.md`](references/audit.md) against
the client's project-context file(s) (CLAUDE.md, AGENTS.md, or the
client's equivalent) and any docs they point to. Each item carries a
**de facto discovery** hint — scan the places a practiced-but-undocumented
scheme actually lives (CI workflows, task runners, `docs/adr*`, …) and
propose adopting what exists via pointer before proposing anything new:

- Is verification (build/test/lint) documented, or only discoverable by
  guessing?
- Is a selective test command documented — one that runs the tests a
  change affects, rather than the whole suite?
- Are the project's commit and landing requirements documented — sign-off,
  signing, message convention, which suites are release-grade, which
  workflows gate a release? Checked-in files only; this item reads no
  forge. See
  [`references/audit.md`](references/audit.md#commit-and-landing-requirements-documented).
- Are spec/plan/ADR conventions documented — location, format, template?
- Is a spec home documented — a **conditional** sub-check, asked only when
  a plan carries an unresolved `## Spec Deltas` block, or the item above
  names plans and ADRs but no specs; nothing is asked of a project that
  never planned anything. See
  [`references/audit.md`](references/audit.md#spec-home-documented-conditional).
- Is a discussions home documented — a **conditional** item, asked only
  when a discussion artifact already exists, or the user asks for one at
  invocation or at any wizard question in Step 2; hex never raises it
  first, and nothing is asked of a project that never discussed
  anything. See
  [`references/audit.md`](references/audit.md#discussions-home-documented-conditional).
- Do the project's rules carry architectural context (boundaries,
  invariants, security-sensitive paths), not just style/lint rules?
- Is product context documented — what the product is, who uses it and
  where, related repos, research keywords, comparable tools? (The one
  item usually not discoverable from the repo; gaps become Step 2 wizard
  questions.)
- Is a constitution / governing-principles doc pointed at (project
  context, cached in `hex.md › Pointers`) — or none used? Optional;
  absent is fine.
- Is the worktree path (default `.agents/worktrees/`) gitignored — and
  does this project use a different one? **And the converse: is that rule
  narrow enough to leave `hex.md` committable?** A bare `.agents/` line
  drops the team-shared memory file from version control and is a defect,
  not a safer-because-broader choice. See
  [`references/audit.md`](references/audit.md#worktree-path-gitignored).
- Is a cross-model adversary skill installed but not pinned — a skill
  carrying the `hex-adversary-scopes` marker with no matching `adversary:`
  line in `hex.md › Preferences`? And the reverse: does an existing
  `adversary:` pin name a skill that is not installed (report-only drift —
  a user-typed pin is never overwritten)? Reads installed frontmatter only;
  executes nothing. See
  [`references/audit.md`](references/audit.md#cross-model-adversary-skill-installed).
- Has the resource profile been measured on this host — peak RSS, wall
  time, `light`/`heavy` class, and the derived heavy-command ceiling,
  cached in `hex.md › Pointers`? See
  [`references/audit.md`](references/audit.md#resource-profile-measured).
- Are agent worktrees (`.agents/worktrees/` by default) excluded from
  IDE file watchers and search indexers, not just from version control?
  See
  [`references/audit.md`](references/audit.md#agent-worktrees-excluded-from-watchers-and-indexers).
- Is the project's scratch/temp convention documented, and has it
  opted `HOME` into the per-run scratch redirect — for a suite known to
  write to `$HOME`, or to keep credentials from a verification command
  it does not fully trust? See
  [`references/audit.md`](references/audit.md#scratch--temp-convention-documented).
- If `.agents/memory/hex.md` already exists: does every pointer in
  its Pointers section, and every index line it seeded in the context
  file, still resolve? (a re-audit item, not first-run-only)
- If a `Federation lead:` bullet exists: does each plan slug it lists
  still name a live, unfinished plan in the lead repo — or has the slug
  gone stale (the lead-side plan is absent or done)? (a re-audit item —
  see
  [`references/audit.md`](references/audit.md#federation-back-pointer-slugs-still-live))
- If any `workflows.<skill>.<tier>` pointer exists: does each fork file
  resolve, carry its `Forked from … @ hex <version>` stamp, and match the
  installed hex version — or has the shipped tier file moved on (drift)?
  (a re-audit item — see
  [`references/audit.md`](references/audit.md#workflow-fork-stamps-current))

Produce a gap report — found / missing / drifted, one line per item. This
step never writes anything.

### 2. Fill gaps, with consent

For every gap, propose the matching best-practice block from
[`references/audit.md`](references/audit.md#best-practice-blocks) and a
destination for it. Each proposal is a [wizard](#the-wizard) question —
walked in sequence with `[n/N]` headers and in-place validation, skipped
when the answer is already detectable, conditional ones gated on Step 1's
findings; the assembled set then prints as one diff before the write.

**Destination is the user's choice.** The rule of thumb: knowledge useful
to *any* agent (verification, worktree location, conventions, product
identity) → project context; knowledge only the swarm consumes →
`hex.md › Preferences`. Propose accordingly, but the user may redirect
it — e.g.
"put the worktree convention in CLAUDE.md so every tool uses it, not just
the swarm." See the destination-of-knowledge rule in
[`../hex-core/references/memory.md`](../hex-core/references/memory.md).

**Product knowledge is provisioned into project context, never captured
in swarm memory.** Short facts (what the product is, who uses it,
comparable tools) become a section in the client's context file; a larger
body gets its own doc — a de-facto home when one exists (`README`,
`docs/`), else a provisioned file such as `.agents/product.md` — plus a
one-line index entry in the context file so every agent finds it.
`hex.md › Pointers` then records only where it landed.

**Spec home (conditional).** When the [spec-home
sub-check](references/audit.md#spec-home-documented-conditional) fires,
propose in this order: an existing practiced location first, else
`.agents/specs/` as the **last resort** — with consent. Ask whether
the destination uses the default ID-marker heading shape or a
project-specific one; a non-default answer is recorded as `Spec ID
marker:` prose in `hex.md › Preferences`, never in `hex.md › Pointers`.
When the resolved home is empty, offer to seed it — copy
`assets/templates/spec.md` to `<home>/spec_<slug>.md`,
**copy-only-if-absent**. Mechanics (resolution order, the ID-marker
convention, containment) are defined once in
[`../hex-core/references/archive.md`](../hex-core/references/archive.md#destination-resolution);
this step only asks and records the answer, never restates them.

**Discussions home (conditional).** When the [discussions-home
item](references/audit.md#discussions-home-documented-conditional)
fires, propose in this order: an existing practiced location first, else
`.agents/discussions/` as the **last resort** — with consent. Record the
one outcome as a `hex.md › Pointers` row:
``- Discussions: `<home>` — pre-plan discussion artifacts (/hex-discuss).``
`<home>` is the location the user consented to, `.agents/discussions/`
only when the last resort was taken. Discussion artifacts carry no
`C-`/`S-` IDs, so there is no ID-marker question here and nothing is
written to `hex.md › Preferences`. When the resolved home is empty, offer
to seed it — copy `assets/templates/discussion.md` to
`<home>/_template.md`, **copy-only-if-absent**, saying in the offer that
the underscore prefix marks it as never a discussion, so nothing scanning
the home for a live one picks it up. Mechanics are defined once
elsewhere: home resolution and verify-on-consumption in
[`../hex-core/references/memory.md`](../hex-core/references/memory.md#location-and-resolution),
containment in
[`../hex-core/references/archive.md`](../hex-core/references/archive.md#destination-resolution),
whose **path** conditions bind every write under `<home>` — this seed copy
and `/hex-discuss`'s own `<home>/<slug>.md` alike (its already-exists and
git-tracked conditions belong to the fold; the no-symlink, no-directory
clause still binds — a dangling symlink reads as absent). This step only
asks and records the answer, never restates them.

**Selective test command.** A command found by the [selective-test
item](references/audit.md#selective-test-command-documented) is proposed
for adoption via pointer, with the matching block from
[`references/audit.md`](references/audit.md#best-practice-blocks); that
block pins the grammar of the two `hex.md › Pointers` rows this run
records — where the selective test command is documented, and where the
project's security-sensitive / hot-path convention is documented. Both are
wizard questions inside the existing sequence, written under the same
apply consent as the discussions row above. The second row is asked
whether or not a selective command was found — its trigger is Step 1's
rules question — and it names **where the convention is documented**,
never a path list and never a judgment.

**Commit and landing requirements.** A requirement found by the
[commit-and-landing
item](references/audit.md#commit-and-landing-requirements-documented) is
proposed for adoption via pointer, and two `hex.md › Pointers` rows record
what only the swarm consumes:

``- Forge: `<forge>` — CLI `<cli>`, used by /hex-finalize.``
``- Target branch: `<branch>` — where feature branches land (/hex-finalize).``

The forge row is proposed whenever a forge is in use, determined from the
checked-in remote alone (`git remote -v` and its URL host) — never from a
CLI auth probe, which would reach the network. The target-branch row
is proposed **only where the trunk is not the obvious default** — a repo
whose default branch is plainly `main` gets no row. Pair the proposal with
the item's branch-protection recommendation; that recommendation is the one
control enforced outside hex, so it is offered even to a project that
already documents its commit requirements.

**Series shape (conditional).** When the [commit-and-landing item's
series-shape
offer](references/audit.md#commit-and-landing-requirements-documented)
fires, offer to record the team's preference as prose in `hex.md ›
Preferences`, with consent. It is prose, never a config key: `config.md`'s
vocabulary is frozen and gains nothing here. Mechanics (the trigger, the
named alternative, the shipped default, the documented-convention defer)
are defined once in
[`audit.md`](references/audit.md#commit-and-landing-requirements-documented);
this step only asks and records the answer, never restates them.

After the apply consent, write each block into the chosen file. If a
hex-authored block from a prior run already lives there, replace it in
place; never touch content the user or another tool wrote. Declined items are skipped
and noted in the closing summary as still-missing — nothing is written
without consent.

### 3. Templates (only if wanted)

Only when Step 1 found no spec/plan/ADR conventions documented, **no
de-facto scheme to adopt**, **and** the user opts into shipped defaults
as part of the Step 2 approval: copy
[`assets/templates/`](assets/templates/) (`plan.md`, `adr.md`,
`discussion.md`, `research.md`, `spec.md`) to a location of the user's
choice, **only if that file is absent** — never overwrite an existing
file. Document the chosen location in the conventions block written in
Step 2.

This is a fallback, not a default push. A project with its own RST/Sphinx
templates keeps them and just documents where they live — hex never
forces its own formats onto a project that already has one.

### 4. Instantiate the model matrix

Present the shipped capability-class matrix
([`../hex-core/references/models.md`](../hex-core/references/models.md)):
two classes, `fast-balanced` and `deep-reasoning`, recommended per worker
role × tier. Detect the harness in use (from the client running this
skill, or ask if ambiguous) and propose literal model names per class —
for example, on a Claude harness: `fast-balanced` → Sonnet,
`deep-reasoning` → Opus. Let the user adjust per row or per cell (e.g. pin
`builder:implement` to the deep-reasoning model at every tier). Review
seats are the exception: they are configured **per join level** through
`review.<level>.class`, which supersedes a `models.overrides`
`reviewer[:focus]` entry
([`config.md`](../hex-core/references/config.md#key-vocabulary), C-985) —
offer that key here, never a reviewer override.

Skipping this step is fine — the shipped class defaults apply and every
orchestrator still runs unmodified. Nothing from this step is written
until Step 4½.

### 4½. Assemble the `Preferences` block, with consent

Gather every swarm-only choice from this run — the model matrix and
overrides from Step 4, plus (if raised) an adversary skill, `limits`,
`perspectives`, `research-axes`, and the per-join-level `review` settings
(`review.<level>.{seats, class, rounds, budget-minutes, delta-only,
checklist}`, plus any candidate a prior run recorded in `hex.md › Memory`
under the [upkeep step](../hex-core/references/protocol.md#upkeep-step)) —
into the single fenced `yaml` block
that carries `hex.md › Preferences`. Key set, types, defaults, and effects
are defined once, in
[`../hex-core/references/config.md`](../hex-core/references/config.md#key-vocabulary);
do not restate them here. The block is the **first content** under
`## Preferences`, prose bullets continuing below it
([placement](../hex-core/references/config.md#carrier-and-placement)). Use
vocabulary v4: v1's six keys (`models`, `adversary`, `limits`,
`perspectives`, `research-axes`, `tiers`), plus `workflows`, the fork
pointer written into this same block by the
[workflow-fork flow](#workflow-forks-hex-init-workflows)
([`config.md` § Workflows](../hex-core/references/config.md#workflows)),
plus `review` (`adr_0016` C-990); tier segments take the five-value grammar
and `adversary` may be a list (v4, `adr_0017` C-997).

Present the assembled block as one diff against the file's current state
(one line per changed key, old → new), gated by the
[wizard](#the-wizard)'s separate apply consent. On
consent, write the block; on decline, leave `hex.md › Preferences` as-is
and note it still-missing in the Step 7 summary. An unconfigured project
writes nothing here — empty is normal, shipped defaults apply.

### 5. Bootstrap `.agents/memory/hex.md`

Write or update `.agents/memory/hex.md` per
[`../hex-core/references/memory.md`](../hex-core/references/memory.md).
`hex.md › Preferences` is already written by Step 4½ — this step covers
the other two sections:

- **`hex.md › Pointers`** — a cache seeded from Step 1's findings: where
  verification is documented, where the selective test command is
  documented, where the project's security-sensitive / hot-path convention
  is documented, where spec/plan/ADR conventions live, the doc and
  product-knowledge homes provisioned in Step 2, the discussions home when
  one was resolved, key architectural rules, any worktree-location
  deviation, and the constitution location (optional). The sensitive-path
  row is the named source the high-risk merge trigger reads; the
  key-architectural-rules pointer beside it keeps its own job — naming the
  rule files — and neither replaces the other.
  Pointers only, never copies — the product-knowledge pointer is how
  researchers and reviewers reach the product facts that live in project
  context.
- **`hex.md › Memory`** — left empty at bootstrap; the orchestrators
  maintain it during their own runs.

On a re-entrant run, update only what Step 1 found drifted or what the
user changed in Steps 2–4½; never rewrite the whole file.
`hex.md › Pointers` and `hex.md › Memory` are skill-managed — the
orchestrators own them day-to-day; a re-run re-points
`hex.md › Pointers` where the audit found drift but clobbers neither
section beyond that, and never touches `hex.md › Memory`.

A missing `.agents/memory/hex.md` is normal and fully supported —
every hex skill falls back to its shipped defaults without one.

### 6. Discovery note (optional)

For a client that doesn't already load project context ambiently, or on
request: write a 3-line marker-fenced pointer block into the client's
context file (template in
[`references/audit.md`](references/audit.md#best-practice-blocks)). It
only points — never duplicates content from `.agents/memory/hex.md`
or from project context. On a re-run, replace the block between the
markers in place.

### 7. Summary

Close with three short lists: what was audited (pass/gap per item), what
was written and where (file → section), and what was proposed but
skipped (declined, or not applicable). No prose beyond that.

## Workflow forks (`/hex-init workflows`)

`/hex-init workflows` manages **workflow forks** — project-authored tier
files that redirect an orchestrator's phase plan. The fork *file format*
(node schema, the `Forked from … @ hex <version>` stamp, the `Count`
grammar, the seven validation checks) is defined once in
[`config.md` § Workflows](../hex-core/references/config.md#workflows);
this flow only creates and tracks the files, and asks through the
[wizard](#the-wizard):

1. **List** the forkable skill × tier pairs (the four orchestrators ×
   `low`/`medium`/`high`/`xhigh`/`max`), each with its shipped phase list and a
   `[forked]` marker where `workflows.<skill>.<tier>` already points at a
   file.
2. **Fork.** On selection, copy the shipped `hex-<skill>/tier-<tier>.md`
   to `.agents/workflows/<skill>-<tier>.md`, prepend the header
   table derived from its shipped phases, and stamp it per config.md
   § Workflows (`Forked from hex-<skill>/tier-<tier>.md @ hex <version>` —
   both validation and drift detection read the stamp). Never overwrite an
   existing fork.
3. **Edit.** The user edits the copy — delete a phase row and its section,
   add a role, re-point an edge.
4. **Diff, consent, write.** Print the diff **by cell, not just by phase** —
   added / removed / re-pointed rows **and** `Roles` / `Count` deltas inside a
   kept phase, each against the file the stamp names. A narrowed `Roles` cell
   (e.g. dropping `reviewer:security` from a retained Review-Fix Loop) must
   surface here — it is the human's one look at the fork before it is wired in,
   and it is the consent backstop for the [security-reviewer
   invariant](../hex-core/references/config.md#workflows) (config.md
   validation check 6). Take the wizard's apply consent, then write the
   `workflows.<skill>.<tier>` pointer into the Preferences block
   (Step 4½'s block; `workflows` is vocabulary v2).
5. **Drift on re-audit.** A later `/hex-init` re-audit
   ([`references/audit.md`](references/audit.md#workflow-fork-stamps-current))
   compares each fork's stamped version against the installed hex version;
   on a mismatch it **reports drift and shows what changed in the shipped
   tier file** — it never auto-merges and never rewrites the fork.

## Re-entrancy

Every property above holds on every run, not just the first: Step 1
always re-audits, including whether existing `hex.md › Pointers` entries
and the context-file index lines still resolve; nothing is overwritten
without consent; templates are only-if-absent; marker-fenced blocks (the
Step 2 best-practice blocks, the Step 6 discovery note) replace in place.
Run `hex-init` again any time drift is suspected or the setup needs to
change — there is no separate "reconfigure" flow.

**The `Preferences` block is user-owned; a re-run never clobbers it.**
Deterministic scaffolding (the `hex.md` section skeleton, seeded pointers,
marker-fenced blocks) regenerates every run; the fenced `yaml` block does
not. Keys the user did not touch keep their values **and their comments**
and their existing order; the wizard rewrites only the keys its diff named
— changing a value in place while keeping that line's quoting — and a newly
added key is appended in the [§ Complete
example](../hex-core/references/config.md#complete-example)'s style. A key
the block still carries that a later hex version dropped from the
vocabulary is **reported deprecated in the Step 7 summary, never silently
deleted** — unlike OpenCode/fast-agent, hex has no code layer that can
safely auto-rewrite a user-owned block, so deprecated keys are reported,
not migrated. If the hand-edited block already carries a key in both the
nested and flat spelling that
[C-222](../hex-core/references/config.md#dotted-keys) tolerates on read,
the wizard surfaces C-222's duplicate-key `Error:`/`Fix:` and leaves the
user's spellings untouched rather than collapsing them.

## Structure

- `SKILL.md` — this flow.
- [`references/audit.md`](references/audit.md) — the audit checklist and
  the best-practice block templates used in Steps 1, 2, and 6.
- `assets/templates/` — the fallback `plan.md`, `adr.md`,
  `discussion.md`, `research.md`, `spec.md` used in Step 3.

The reference links above resolve when `hex-core` is installed alongside
this skill; if it isn't: `grim add ghcr.io/michael-herwig/arcana/hex-core:latest`.

## Arguments

A positional concern narrows the run to one area: `verification`,
`conventions`, `worktrees`, `models`, `templates`, or `workflows` (the
[fork lifecycle](#workflow-forks-hex-init-workflows) above). Omitted,
`hex-init` runs the full flow above.

Two flags drive the **non-interactive twin** — the wizard's questions
answered without prompting:

- `--yes` accepts every recommendation the audit produces, writing the
  same diff the wizard would, without asking.
- `-d key=value` (repeatable) sets one config key directly, e.g.
  `-d limits.max-workers=6 -d adversary=codex:rescue`.

The twin engages automatically when there is **no TTY**, when any `-d` is
given, or when `CI` is set. A non-interactive refusal still pairs `Error:`
with `Fix:` and **enumerates the accepted values inline** so the correction
is paste-able (e.g. `Error: -d adversary=nope unknown. Fix: accepted:
<skill-name> | none`).

$ARGUMENTS
