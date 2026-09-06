---
name: hex-review
description: Tiered adversarial review orchestrator for a working tree, branch diff, pull request, or plan/markdown artifact before it lands on main — a staged reviewer panel (spec, quality, security, performance, docs), root-cause analysis, and an optional cross-model adversarial gate scale with diff size via tier (low|medium|high|xhigh|max, auto by default). Use for pre-merge checks, code review, branch or PR review, diff review, plan or ADR review, adversarial review, root-cause analysis, or any "review this before I merge" request. Never edits the code or diff under review, and never commits — its writes are the plan's Status block, an append-only convergence check, and — on an approved, converged fold — the resolved spec file and a fold receipt; the fix loop is /hex-execute's job.
license: Apache-2.0
metadata:
  summary: Tiered swarm review — diff or artifact to verdict via adversarial reviewer panel
  keywords: review,swarm,multi-agent,adversarial,rca,pre-merge,verdict
  repository: https://github.com/michael-herwig/arcana
---

# hex-review — Adversarial Reviewer

Thin dispatcher. It resolves the review target and baseline, classifies the
diff's tier, resolves overlays, runs the single meta-plan approval gate,
announces the resolved config, and hands off to the matching tier file.
Unlike `hex-plan` and `hex-execute`, hex-review never edits its own output
or loops on it — the reviewer panel runs **once** per invocation and
reports; iterating on the findings is
[`/hex-execute`'s job](../hex-core/references/loop.md#the-review-fix-loop).
The phase plans live in the five `tier-<tier>.md` files; the shared vocabulary (tiers, worker roles, model classes,
the memory file) lives in the `hex-core` reference library and is **linked
here, never copied**.

Shared contracts:
[`protocol.md`](../hex-core/references/protocol.md) ·
[`workers.md`](../hex-core/references/workers.md) ·
[`models.md`](../hex-core/references/models.md) ·
[`memory.md`](../hex-core/references/memory.md).
If `hex-core` is not installed: `grim add ghcr.io/michael-herwig/arcana/hex-core:latest`.

## Argument syntax

```
/hex-review [tier] <target> [flags]
```

- **tier** (optional): `low | medium | high | xhigh | max | auto`. Default
  `auto` — the classifier picks `low` … `xhigh` from diff metrics. **`max` is explicit
  only** — `--tier=max`, or a plan whose Status block says `Tier: max`; the
  classifier never emits it
  ([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)).
- **target** (one of):
  - empty — the **working tree**: uncommitted changes (staged + unstaged)
    against `HEAD`. If the working tree is clean, fall back to the current
    branch's diff against the resolved baseline and say so in the
    announce block.
  - a **branch name** — diffed against the resolved baseline.
  - `<N>` / `#<N>` / `PR <N>` / `pull/<N>` / a full GitHub PR URL — an
    explicit pull request.
  - a path to a **markdown plan, ADR, or spec artifact** — review the file
    directly, not a code diff. Diff metrics do not apply; see
    [`classify.md`](classify.md#plan-and-artifact-targets).
- **flags** (before the target, by convention):
  - `--base=<git-ref>` — the diff baseline. Pipeline input, not an
    overlay axis: everything downstream (metrics, tier, review scope,
    adversary scope) reads from it. Default resolution in step 2.
  - `--breadth=minimal|full|adversarial` — override the Stage 2 panel
    breadth (see [`overlays.md`](overlays.md)).
  - `--rca=on|off` — override root-cause (Five Whys) analysis.
  - `--adversary` / `--no-adversary` — force the cross-model pass on or
    off.
  - `--dry-run` — make the meta-plan gate block for explicit approval and
    stop there (preview only, no workers launched).

## Dispatch

The outer loop every hex orchestrator shares
([`protocol.md`](../hex-core/references/protocol.md#shared-shape)):

### 1. Parse arguments and resolve memory

Parse the tier, target, and flags. Locate `.agents/memory/hex.md` by
searching upward from the working directory; a missing file is normal
([`memory.md`](../hex-core/references/memory.md#location-and-resolution)) —
fall back to shipped defaults and note that `/hex-init` can create one. When
present, read the whole file: `hex.md › Pointers` for where verification and
review conventions are documented, and for the constitution pointer when
present (activates the
[constitution gate](../hex-core/references/protocol.md#constitution-gate));
`hex.md › Preferences` for the instantiated model matrix, the adversary skill
name, limits, and always-on perspectives or path-triggered escalations; the
project's product knowledge (located via `hex.md › Pointers`) when present (it
grounds the `user-feedback` perspective); and `hex.md › Memory` for any active
plan this review might trace back to.

If the resolved `hex.md` carries a `Federation lead:` bullet, **halt** per
[`memory.md` § Location and resolution](../hex-core/references/memory.md#location-and-resolution)
— this repo is a federation satellite and its memory is not the plan's.

### 2. Resolve the target and baseline

Detect the target (order above). For a GitHub reference, fetch title, body,
labels, base ref, and file list in this order of preference:

1. the client's **GitHub MCP tools** (PR read) when it exposes them;
2. the **`gh` CLI** (`gh pr view --json baseRefName,title,body,labels,files`)
   as fallback;
3. neither available — treat the input as a branch name or free text and
   continue.

Resolve the baseline: user `--base=<ref>` wins; else, where the traced plan
carries a valid `Reviewed:` anchor and the invocation is a review round
continuing that plan's loop, the anchor is the baseline — validity per
[`loop.md`](../hex-core/references/loop.md#anchor-validation), which
resolves the fallback below *first* and validates the anchor against it; else
a PR target uses its fetched base ref; else `main`. Fast-fail when the
baseline doesn't resolve (`git rev-parse --verify <base>` fails) — print
remediation and stop. An empty baseline-to-HEAD diff reports
"nothing to review" and exits clean where no converged pass is outstanding;
an empty `anchor..HEAD` delta proceeds to that pass instead
([`loop.md`](../hex-core/references/loop.md#delta-round-scope)).
A markdown-artifact target skips baseline resolution entirely.

**Federation refusal (C-323).** A run is federated only if its own resolved
`hex.md` carries `Federation:` bullets; if the resolved plan (or the branch's
traced plan) carries a `Repo` column but this run's does not, **halt** per
[`memory.md` § Location and resolution](../hex-core/references/memory.md#location-and-resolution)
— it cannot reach every participating repo and must not review a silently
narrower union. Every `Repo` value must resolve to a declared Federation key,
or halt the same way.

**Federation pre-flight (C-320).** For a federated target — one tracing to a
plan that carries a `Repo` column — resolve nothing about the union scope until
every participating repo is proven reachable. Run
[C-303's pre-flight](../hex-core/references/worktree.md#worktree-work-package-mechanics)
in its **read-only** form for **every distinct `Repo` value the plan uses** —
clauses (i) `--show-toplevel` and (ii) `--git-common-dir`, plus a
`status --porcelain` read check — and **halt** with the same `Error:`/`Fix:`
`--add-dir` relaunch line on any failure. A satellite unreachable from this
session would otherwise resolve a **silently narrower** union — full coverage
reported over a partial diff, the exact failure C-303 exists to stop. The
**write probe does not apply**: `/hex-review` never writes in a satellite —
every write it makes is lead-scoped, including the Fold-Back write (the lead's
resolved spec file and fold receipt, C-415 clause 5; see the
[Review-only contract](#constraints)), so satellite writability is never
checked.

### 3. Classify (only when tier is `auto`)

Read [`classify.md`](classify.md). For a diff target, compute file count,
lines changed, and areas touched once, then apply its tier signals,
structural markers, and overlay triggers. It emits a candidate tier
(**only** `low`, `medium`, `high`, or `xhigh`), a confidence flag, and an overlay
set. Low confidence forces the gate in step 5. Never ask a mid-flow
question during classification — ambiguity is resolved at the single gate.

### 4. Resolve overlays

Final config = the tier's baseline from [`overlays.md`](overlays.md) +
classifier-inferred overlays + `hex.md › Preferences` hints + user
flags. Later wins; **user flags always override** (see
[`protocol.md`](../hex-core/references/protocol.md#spawn-selection-precedence)).

### 5. Meta-plan gate (the single approval point)

Exactly one gate, before any worker launches
([`protocol.md`](../hex-core/references/protocol.md#the-meta-plan-approval-gate)).
Its weight scales:

- **Confident `low` / `medium` / `high`** (an explicit user tier always counts as
  confident — the classifier never ran) — announce the resolved config (step 6)
  and proceed; the user can still abort.
- **`xhigh` or `max` tier, low-confidence classification, or `--dry-run`** — block
  for explicit approval.

On a client with a **native plan-approval mechanism**, use it. Otherwise
present the announce block as **one structured question** (approve / adjust
/ cancel). The gate is also where the user can add or drop perspectives and
adjust the breadth axis. On adjust, re-resolve and re-present once. Never
split this into sequential questions.

### 6. Announce the resolved config

Print the resolved config with per-axis source attribution before loading
the tier file:

```
hex-review
  Tier:      high                          (auto — 6 files, 180 lines, 1 area)
  Baseline:  main                             (default)
  Target:    HEAD (branch: feature/export)
  Overlays:  breadth=full                     (tier baseline)
             rca=on                           (tier baseline)
             adversary=on                     (classifier: dependency-manifest change)
  Spawn set:
    reviewer: spec (post-implementation)      (tier baseline)
    reviewer: quality (test-coverage)         (tier baseline)
    reviewer: quality                         (tier baseline)
    reviewer: security                        (classifier: auth path touched)
    doc-reviewer                              (classifier: CLI flag doc trigger)
  Models:    fast-balanced default; reviewer:security → deep-reasoning  (models.md)
  Adversary: codex-adversary, code-diff scope  (hex.md preference)
  Degraded:  no — subagent spawning available
```

The announce block prints one config-disclosure line per change a
`hex.md › Preferences` block makes; the full trigger set is defined once in
[`protocol.md` § the meta-plan approval gate](../hex-core/references/protocol.md#the-meta-plan-approval-gate).
For this skill, for example:

```
  [project-redefined: Stage 2.reviewer:performance 1→2 (hex.md tiers)]
  [reviewer:security dropped — phase ceiling 6 reached]
  [Stage 2 batched 3+3 — concurrency cap 3 (hex.md)]
  Error: never [reviewer:security] refused (hex.md preference)
  Fix:   see config.md#merge-rules for the attestation requirement
```

Every spawn line carries its source (`tier baseline` / `classifier` /
`hex.md preference` / `user flag`) per
[spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence).
Model names above are class placeholders — shipped files never hardcode
literals; the running orchestrator resolves and prints each spawn's
literal model per the
[Models line contract](../hex-core/references/protocol.md#the-meta-plan-approval-gate). On a client that cannot spawn subagents, announce
`Degraded: inline workers` and run each worker prompt inline and
sequentially
([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).

### 7. Dispatch to the tier file

Read `workflows.hex-review.<tier>` from the resolved config first. When set
and the named file passes the seven validation checks
([`config.md` § Workflows](../hex-core/references/config.md#workflows)), `Read`
that forked file in place of the shipped one; on validation failure or when
unset, `Read` the matching `tier-{low,medium,high,xhigh,max}.md` — config.md owns the
check list and the on-failure fallback. Announce which ran: `Workflow: shipped
tier-<tier>.md`, or for a fork, `Workflow: <path> (forked from <shipped tier
file> @ <stamped version>)`. Execute the loaded file's phase plan; no phase
content is duplicated in this file.

## Worker assignment (shared across tiers)

Roles are indexed in [`workers.md`](../hex-core/references/workers.md);
the orchestrator loads a full persona (spawn-prompt template, output
contract) only for roles in the resolved spawn set
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).
The model class for each role × tier is in
[`models.md`](../hex-core/references/models.md). This table maps roles to
review stages; the tier files set the actual counts and firing conditions.

| Stage | Role | Count | Purpose |
|---|---|---|---|
| Stage 1 — Correctness | `reviewer` (focus `spec`, phase `post-implementation`) | 1 | design ↔ implementation traceability; also runs the [convergence check](../hex-core/references/loop.md#convergence-contract) when the target traces to a plan |
| Stage 1 — Correctness | `reviewer` (focus `quality`, test-coverage emphasis) | 0–1 | Specify-phase test adequacy (`high` and above) |
| Stage 2 — Panel | `reviewer` (focus `quality`) | 1 | naming, style, pattern compliance |
| Stage 2 — Panel | `reviewer` (focus `security`) | 0–1 | fires on security-sensitive paths |
| Stage 2 — Panel | `reviewer` (focus `performance`) | 0–1 | fires on hot-path / async changes |
| Stage 2 — Panel | `doc-reviewer` | 0–1 | fires on a doc-trigger match |
| Stage 2 — Panel (`high` and above) | `architect` | 0–1 | boundary / dependency-direction review |
| Stage 2 — Panel (`high` and above) | `researcher` | 0–1 | SOTA / known-pitfall gap check |
| Adversary | configured adversary skill (`code-diff` or `plan-artifact`) | 0–1 (every configured entry at `max`) | cross-model review |
| Stage 2 — Panel (`max`) | `simulator` | 0 (4+ at `max` only) | one user pattern each against the target; reported, never fixed |

A project's `tiers.hex-review.<tier>.counts` can override any Count cell
above against the baseline this table sets
([`config.md` § Merge rules](../hex-core/references/config.md#merge-rules)).

Concurrency cap and degraded mode:
[`protocol.md`](../hex-core/references/protocol.md#worker-coordination). The
adversary skill name comes from `hex.md › Preferences`
([adversary contract](../hex-core/references/adversary.md#adversary-contract));
`codex-adversary` is only an example value. A markdown-artifact target uses
the `plan-artifact` scope; every other target uses `code-diff`.

## Project rules and conventions

hex never carries a hardcoded rule or area table. Every phase that needs the
project's invariants, quality rules, or verification command discovers them
from **project context** (the client's ambient instructions / project
rules) and the pointers in the Pointers section of
`.agents/memory/hex.md`
([`memory.md`](../hex-core/references/memory.md#the-three-sections)).
"Verify" anywhere below means **run the project's documented verification**
([`verify.md`](../hex-core/references/verify.md#verification)). Path
hints from `hex.md › Preferences` (e.g. "security review mandatory under
`src/auth/**`") fold into perspective selection at classification time
([`classify.md`](classify.md)).

## The review report

**Where it goes.** The report is presented inline as the response to this
invocation — it is not a persisted plan artifact. hex-review's writes land
only here, on the plan artifact itself, never on the code or diff under
review. When the target traces to an active plan (the `hex.md › Memory`
active-plan pointer, or a `## Status` block found in a referenced markdown
file — see [`memory.md`](../hex-core/references/memory.md)),
mutate that block: on round entry, flip `State` to `review`; on verdict,
set `State` to `done` with `Next` cleared (Approve — or `State: landing`,
not `done`, for a plan carrying a `Repo` column, whose `Next` names the
landing enumeration, C-324), or leave `State: executing` and set `Next` to
`/hex-execute <plan path>` (Needs Work / Request Changes). Skip silently when
no active plan is found — never invent one. An Approve never writes the
terminal review state — `done`, or `landing` for a plan carrying a `Repo`
column — where the run ended with a non-empty stranded set
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)).
This skill is the `L3` trunk level of [Review by join
level](../hex-core/references/loop.md#review-by-join-level): it runs only
when invoked, nothing in `/hex-execute` requires it, and it remains the sole
writer of the terminal review state.
Writing the `Reviewed:` anchor is a Status-block write, the class of write
`hex-review` already performs — a branch-scope pass only
([`loop.md`](../hex-core/references/loop.md#the-last-reviewed-anchor)).

**Federated diff scope (C-309).** When the target traces to a plan carrying a
`Repo` column, the resolved review scope is not one repo's diff but the
**union** over the lead (`.`) plus every distinct `Repo` value — each computed
as `git -C <repo> diff <base>...hex/<plan-slug>` and presented to the panel as
one diff set with each hunk labelled by its repo. `<base>` is the frozen
per-repo base SHA **read from the plan's `Repos:` ledger**
([`worktree.md` C-317/C-324](../hex-core/references/worktree.md#worktree-work-package-mechanics)),
**never re-resolved** from a trunk ref that may have moved since execution
started. The C-320 pre-flight (step 2) has already proven every participating
repo reachable, so the union is never silently narrower than the plan. Absent a
`Repo` column the scope is step 2's single diff, byte-identical. Only the
scope's construction generalizes — the never-edits and stay-in-scope contracts
below are unchanged.

When the target traces to a plan artifact, also run the
[convergence check](../hex-core/references/loop.md#convergence-contract):
the orchestrator — never a worker — appends any gap as new WP rows at the
end of the plan's Parallelization table (their wave derives as the next
topological level); the plan stays byte-identical when nothing is
outstanding ("Converged").

**Fold-Back phase.** After the convergence check and before Upkeep, the
[Fold-Back phase](../hex-core/references/archive.md) folds the plan's
`## Spec Deltas` into the project's documented spec home. It runs **only**
when all four preconditions hold **and** the plan carries a `## Spec Deltas`
block (the archive.md conditional-load trigger, S-410); otherwise it does not
run and the Fold-Back block below says so in one line (`not performed —
<reason>`):

1. the target traces to a plan artifact (as above);
2. the verdict is **Approve**;
3. the convergence check reported **Converged**;
4. this run is about to write the plan's **terminal review state** — `done`,
   or `landing` for a plan carrying a `Repo` column (a federated plan, whose
   Approve writes `landing`, never `done` — so keying on `done` alone would
   strand it, never folding).

Order is not cosmetic: convergence is the mirror of this phase and runs
first, always
([`loop.md`](../hex-core/references/loop.md#convergence-contract)),
because folding an incompletely delivered plan would write aspiration into
truth. **Every mechanic is defined in
[`archive.md`](../hex-core/references/archive.md) and is not restated here** —
the delta grammar, destination resolution and containment, the safety
envelope and its four pasted-command checks, halt semantics, idempotence,
the fold receipt, and revert. The phase's only writes are the one resolved
spec file and the fold receipt it appends to the plan's `## Spec Deltas`
block; both land uncommitted, and every run emits the mandatory Fold-Back
block in the report skeleton below.

**Synthesis self-check.** Before emitting the report, the orchestrator
confirms on its own draft:

- every duplicate `file:line` finding resolved
  [max-wins](../hex-core/references/severity.md#finding-severity), never
  averaged;
- every `doc-reviewer` grade mapped onto the
  [severity ladder](../hex-core/references/severity.md#finding-severity);
- every cross-model escalation is noted in the Cross-Model Adversarial
  section;
- every empty findings bucket printed `(none)`.

**Required skeleton** (tier files add or drop sections per the breadth they
run — absence of a section is a tier contract, not a bug):

```markdown
## Code Review: <target>
### Summary
- Verdict: Approve | Needs Work | Request Changes
- Tier / Baseline / Diff: N files, +L / -L lines, S areas
### Stage 1 — Correctness
### Stage 2 — <perspectives run at this tier>
### Convergence   <!-- when the target traces to a plan artifact -->
### Fold-Back     <!-- mandatory when the target traces to a plan; `not performed — <reason>` if a precondition fails -->
### Cross-Model Adversarial               <!-- if adversary=on fired -->
### Root-Cause Analysis                   <!-- if rca=on -->
### Deferred Findings (human judgment required)
```

The Convergence section holds one line per unmet requirement ID (`missing`
| `partial` | `contradicts` | `unrequested`), or the single word
`Converged` — see the
[convergence contract](../hex-core/references/loop.md#convergence-contract).

The **Fold-Back section is mandatory whenever the target traces to a plan**,
and prints `not performed — <reason>` when any precondition fails (verdict not
Approve; unconverged; not the terminal review state; the target does not trace
to a plan; or the plan carries no `## Spec Deltas` block). When the phase runs, every line is **pasted command output, never
narration** — see
[`archive.md` § evidence](../hex-core/references/archive.md#evidence-is-command-output-not-narration-c-414):

- **target** and how it resolved (`Related Spec:` / carries the IDs / plan
  slug);
- the **ID-marker convention** and its source (`default` / `hex.md ›
  Preferences`);
- the **census** — `live` (pasted `grep` transcript) and `incoming` (the
  delta block's IDs);
- **per-ID disposition** (`C-002 modified`, `C-003 removed`, `C-004 added`);
- a **per-touched-ID content excerpt** — the live body's first line and its
  pasted non-blank line count beside the incoming body's, so prose loss shows
  in the report;
- a **per-`MODIFIED` sub-ID census** — the live body's sub-ID set and the
  incoming body's, both as pasted command output (C-405), beside that
  line-count pair, so a dropped inline sub-clause shows in the report;
- the **containment verdict** with the resolved path and its pasted
  `git ls-files` transcript — printed on **every** run, not only on failure;
- **destination cleanliness** as pasted `git status --porcelain` output **at
  step 4 and again at step 6**;
- **`targets: 1`** with prepared-then-written;
- the **fold receipt** it appended to the plan (`Folded: …`);
- the **revert** command, `git checkout -- <target>`.

An absent Fold-Back block, or a prose assertion standing where a command
transcript belongs, is a spec violation.

**Verdict rules** (tier files refine thresholds):

- **Request Changes** — any unresolved
  [Block-tier](../hex-core/references/severity.md#finding-severity) finding,
  a security vulnerability, a breaking change without a migration note, new
  behavior without tests, or an **unjustified constitution violation** (a
  violation with no adequate Constitution Deviations row is automatic
  Request Changes — see the
  [constitution gate](../hex-core/references/protocol.md#constitution-gate)).
- **Needs Work** — **High- or Warn-tier** findings remain, the cross-model
  pass surfaced actionable findings not yet addressed, or the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  found unconverged gaps — unconverged gaps cap the verdict at Needs Work,
  never Approve, with `Next: /hex-execute <plan path>`.
- **Approve** — otherwise.

**Review-only contract.** hex-review **never edits the code or diff under
review, and never commits.** Its writes are the plan-artifact mutations
above — the Status block, including the `Reviewed:` anchor line on a
branch-scope pass, and the append-only convergence rows — plus, under
the [Fold-Back phase](#the-review-report)'s four preconditions only, the one
resolved spec file and the fold receipt appended to the plan's `## Spec
Deltas` block; a fold is never inferred from silence, and every write lands
uncommitted (revert is `git checkout -- <file>`, see
[`archive.md`](../hex-core/references/archive.md#revert)). Every finding is
reported, not applied; the fix loop belongs to
[`/hex-execute`](../hex-core/references/loop.md#the-review-fix-loop).

## Constraints

- **Never edits the code or diff under review, and never commits** — its
  writes are the plan-artifact Status block, including the `Reviewed:`
  anchor line on a branch-scope pass, and the append-only convergence
  rows, and — under the [Fold-Back phase](#the-review-report)'s four
  preconditions only — the one resolved spec file and the fold receipt (see
  [`archive.md`](../hex-core/references/archive.md)).
- **No approving with an unresolved Block-tier finding** — the verdict
  rules above are binding.
- **A federated plan's Approve writes `State: landing`, not `done`** (C-324) —
  a plan carrying a `Repo` column stops at `landing` so the satellite locks
  outlive the broken-integration window (removed only on `done`, per
  [`protocol.md` § Upkeep](../hex-core/references/protocol.md#upkeep-step));
  single-repo plans keep `review → done` byte-identically. `landing` is the
  terminal review state the [Fold-Back phase](#the-review-report) already keys
  on.
- **No mid-flow questions** — ambiguity is resolved at the single gate.
- **Never exceed the concurrency cap**
  ([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).
- **Always** cite specific files and lines; **always** pair a finding with
  a remediation, not just a problem.
- **Always** classify every finding actionable or deferred — nothing
  reaches the report unclassified.
- **Always stay within the resolved diff scope** (`<base>...<target>`), and
  **across every participating repo** for a federated plan — the C-309 union,
  not one repo's slice. A regression the diff introduces in an unchanged file
  is in scope; pre-existing issues in unrelated code are not.
- Don't nitpick style the project's own formatter or linter already
  enforces (discovered from project context).
- **Upkeep**
  ([`protocol.md`](../hex-core/references/protocol.md#upkeep-step)): as the
  final phase, update any `hex.md › Pointers` entry this run revealed as
  drifted. When this run wrote the plan's **terminal review state** (`done`,
  or `landing` for a plan carrying a `Repo` column), also **clear the
  `hex.md › Memory` active-plan pointer** — the first clear in the bundle —
  and record the plan plus its fold target in the artifact index
  ([`archive.md` § Plan archive](../hex-core/references/archive.md#plan-archive)).

## Handoff

The [handoff contract](../hex-core/references/protocol.md#handoff-contract)
applies: this block is the run's required final message; one optional
proceed question may follow it.

```markdown
## Review Complete: <target>

### Classification
- Scope: small | medium | large
- Tier: low | medium | high | xhigh | max
- Baseline: <ref>
- Overlays: breadth=<minimal|full|adversarial>, rca=<on|off>, adversary=<on|off>

### Verdict
- Approve | Needs Work | Request Changes

### Findings   <!-- [severity] prefix absent below tier high -->
- Actionable:
  - [severity] file:line — issue — remediation      <!-- one line per finding; `(none)` if the bucket is empty -->
- Deferred:
  - [severity] file:line — issue — why human input is needed   <!-- same; `(none)` if empty -->

### Cross-model review
- ran — <n> actionable, <n> deferred, <n> dropped   |   skipped: <reason>

### Fold-Back   <!-- mandatory when the target traces to a plan; full block per § The review report -->
- target: <file> · marker: <default | hex.md › Preferences> · targets: 1
- census / disposition / line-counts / containment / cleanliness: pasted transcripts (see report)
- receipt: <the `Folded: …` lines appended to the plan>   |   not performed — <reason>
- revert: git checkout -- <file>

### Next step
    /hex-execute <plan path> "apply review findings"    <!-- actionable findings exist -->
    /hex-finalize                                       <!-- clean Approve, branch or PR target -->
    (none — approved)                                   <!-- clean verdict, plan-artifact or working-tree target -->
```

Consumers: `/hex-execute` (actionable findings, the Review-Fix Loop); the
human (verdict and deferred findings).

$ARGUMENTS
