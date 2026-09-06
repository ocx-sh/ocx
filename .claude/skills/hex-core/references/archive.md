# archive

The single definition site for the **spec fold-back** mechanics: the delta
grammar, destination resolution, the safety envelope and its four commands,
halt semantics, idempotence and the fold receipt, revert, and the plan
archive. `hex-execute` (the delta producer, `adr_0005` C-419), `hex-review`
(the Fold-Back phase, C-401/C-402), and [`protocol.md`](protocol.md) link
here and never restate a rule this file owns. **Where and when the phase
runs is `hex-review`'s; what it does is here.** Rationale, prior art, and the
rejected options are in `adr_0005`.

**Conditional-load.** Read this file **only** when the plan under review
carries a `## Spec Deltas` block — the same carve-out
[`config.md`](config.md) gets for its `yaml` block (`adr_0003` C-203). A
plan with no deltas never folds, nothing here is consulted, and a default
review pays zero context for it.

**Silent corruption of human-owned truth is the failure to design against.**
Every guard below is a **halt**, not a warning: a refusal to fold is always
acceptable, a wrong fold is not. hex has no parser — each guarantee is an
instruction a model may skip, so the mechanical checks are discharged by
**pasted command output, never narration**, and every semantic judgment is
backed by halt.

## Delta grammar

**Adopt `ADDED` / `MODIFIED` / `REMOVED`. There is no `RENAMED` operation.**
(C-403) The delta block is authored into the plan under a `## Spec Deltas`
section (append-only, same discipline as the convergence rows) and is the
fold's **only** input:

```markdown
## Spec Deltas

Target: docs/specs/spec_export.md   <!-- exactly one target per plan (C-415) -->

### ADDED
- C-004 — Export component. Streams rows to a writer; back-pressure via
  the writer's own flow control.
  <!-- full body, exactly as it will appear in the spec -->

### MODIFIED
- C-002 — Base: live {C-002.1, C-002.2, C-002.3}, 14 lines.
  Title: "Row source" → "Row source (paged)".
  <!-- full replacement body for C-002, restating every part it keeps -->

### REMOVED
- C-003 — superseded by C-004; no consumer remained at merge.
```

Rules:

1. **Every entry is keyed by an ID** (`C-###`/`S-###`), never by header
   text. An entry whose ID is absent from both the plan and the spec is
   malformed: **halt** (C-406).
2. **A title change is an ordinary `MODIFIED` body edit.** hex's identity is
   the ID — stable within the artifact, never renumbered
   ([`protocol.md`](protocol.md#traceability-ids)) — so a rewording cannot
   break a join. `RENAMED` is therefore **not an operation**: no apply-order
   slot, no `FROM:`/`TO:` pairing, and none of the unpaired-drop failure mode
   that pairing carries. The human-readable half survives as the optional
   `Title: "<old>" → "<new>"` first line of a `MODIFIED` entry.
3. **`MODIFIED` bodies are complete replacements, not patches.** A
   `MODIFIED` entry restates the contract's **full** body, including every
   part it keeps unchanged. A partial `MODIFIED` is the "intelligent
   merging" this design refuses. Two guards make replacement safe: the
   stale-base guard (C-405, lost IDs) and the no-shrink guard (C-416, lost
   prose under a retained ID) — both in the [safety envelope](#safety-envelope).
4. **`REMOVED` carries the ID plus a one-line reason.** A removal with no
   reason is malformed: **halt**.
5. **Scope: `C-###` only, plus `S-###` when and only when the destination
   already carries `S-###` rows** — "carries" meaning locatable under the
   recognized [ID-marker convention](#id-marker-convention-c-413), not merely
   mentioned in prose. The shipped spec template has no scenario section, and
   **inventing a section in a human-authored file is the exact silent
   restructuring this design exists to prevent**. An unfoldable `S-###` delta
   is reported `not folded — destination carries no scenario section`, never
   written and never dropped from the plan.

**Producer obligation — the `Base:` line.** Every `MODIFIED` entry carries a
`Base:` line enumerating the **live** sub-IDs and the **non-blank line
count** it was authored against, read from the live destination body by the
producer (`hex-execute`, C-419). Without it, C-405 and C-416 have no
well-formed base to compare against. A `MODIFIED` entry with no `Base:` line
is malformed: **halt** (C-406). `ADDED` and `REMOVED` entries carry no
`Base:` line — there is nothing live to enumerate for an `ADDED`, and a
`REMOVED` names the whole contract. Under a federated plan (a `Repo` column)
a `MODIFIED` that folds into the lead reads its `Base:` from the **lead
repo's** destination, not the satellite worktree; a satellite-delivered entry
(`Repo` ≠ `.`) defers (C-415) and needs no lead base.

## Destination resolution

### ID-marker convention (C-413)

Every mechanic here presumes the fold can **locate a `C-###` inside the
destination file**. hex ships a delta grammar, not a destination format, so
that presumption is discharged explicitly.

**Default marker shape.** A destination is fold-compatible when every
contract it carries is introduced by a markdown ATX heading whose text begins
with the bare ID, the shape the shipped spec template uses:

```
### C-001 — [Title]
```

Concretely, a heading matching `^#{1,6}\s+(C|S)-[0-9]+\b`. The contract's
**body** is everything from that heading to the next heading of the **same or
shallower depth**, or end of file. That span is what a `MODIFIED` replaces,
what `REMOVED` deletes, and where an `ADDED` contract is appended. **Sub-IDs**
(for the stale-base guard, C-405) are every `C-###`/`S-###` occurring
anywhere within that span, marker or inline.

**Project override.** A project whose specs use a different but still
mechanical marker records the regex plus its body-extent rule in
`hex.md › Preferences` — **user-owned** config, written only by `/hex-init`
with consent and **never edited by a run** — not in `hex.md › Pointers`,
whose entries are a skill-managed location cache any Upkeep step rewrites
freely (see [`memory.md`](memory.md)). An ID-marker regex is authoritative
user configuration, not a location: recording it in a cache a re-point may
clobber would silently delete it, and the fold would then fall back to the
default against a file that does not match it. The Pointers spec-home entry
carries only a pointer:

```
# hex.md › Preferences  (user-owned; only /hex-init writes, runs never edit)
Spec ID marker: ^#{1,6}\s+(C|S)-[0-9]+\b
  (body = heading to next heading of same or shallower depth)

# hex.md › Pointers  (skill-managed location cache)
Spec home: docs/specs/ — ID marker: see Preferences › Spec ID marker
```

The recorded regex is the **only** alternative accepted. hex does **not**
infer one, does **not** relax the default on a near-miss, and does **not**
fall back to matching a bare ID in running prose — an ID mentioned in a
sentence is a cross-reference, not a contract boundary.

### Resolution order (C-404)

1. Read `hex.md › Pointers` for the spec/plan/ADR conventions entry.
2. **Verify on consumption:** the pointed-at location exists and actually
   holds specs. On a miss, re-detect from project context — including the
   de-facto glob (`docs/specs*`, `specs/`, an existing `.agents/`
   tree) — and re-point in the same run. No documented home and none
   discovered → the [no-spec-home defer](#no-documented-spec-home-c-407).
3. **Select the target file**, in this order:
   1. the spec the plan was planned from, when the plan's Overview names one
      in its `Related Spec:` field **and that path passes the containment
      check** (C-418);
   2. else the file in the spec home carrying the `C-###` the deltas touch;
   3. else — the **pure-`ADDED` case**, where no delta entry names an
      existing ID and rule 3.2 yields zero candidates by construction — the
      file in the spec home whose name matches the plan's slug, when exactly
      one does (`plan_export.md` → `spec_export.md`); this is a name match,
      not a content guess, printed as `target: <file> (resolved by plan
      slug)` so a reader can reject it on sight.
4. **More than one candidate target is ambiguity: halt** (C-406). Never fold
   into a guess.
5. **Zero candidates degrades, it does not halt** (see the
   [pure-`ADDED` defer](#no-documented-spec-home-c-407)).
6. **Confirm the ID-marker convention** for the resolved file and print it
   with its source (`default` / `hex.md › Preferences`). Not fold-compatible
   → **halt** (C-406) with
   `not folded — destination format not fold-compatible (<file>): no recorded
   ID marker and no <depth># C-### headings found`, and a fix line naming both
   remedies: record an ID-marker regex in `hex.md › Preferences`, or fold by
   hand. hex never guesses at a destination's structure.
7. **Confirm containment** (C-418) for the resolved file, and confirm it
   again immediately before the write.

### Containment: the resolved path never leaves the spec home (C-418)

Rule 3.1 reads a path out of a model-written field in a model-written
artifact. Without a boundary check a malformed or invented `Related Spec:`
(`../../src/config.ts`, an absolute path, a symlink) redirects the whole
write into an arbitrary project file while every other guard still passes.
Containment is a **hard refusal**, never a warning. Let `home` be the spec
home verified in steps 1–2 and `target` the file selected in step 3; the
fold proceeds only when all hold:

1. `target` is **inside `home`** — after normalizing both to
   repository-relative paths, `target` begins with `home` plus a path
   separator. Equality is not containment.
2. `target` contains **no `..` segment** before or after normalization, and
   is not absolute.
3. `target` is a **regular file that already exists** — not a symlink, not a
   directory. hex creates no destination file, so a non-existent target is
   the zero-candidate defer, not a write.
4. `target` is **tracked by git**, discharged by pasting the output of:

   ```
   git ls-files --error-unmatch <target>
   ```

   run **in the lead repo** under a federated plan (C-415). A file git does
   not track cannot be reviewed as a diff or reverted by `git checkout --`.

**Re-verified at write time.** The check runs at resolution (step 7) **and
again as the first action of the write** (envelope step 6), beside the
cleanliness re-check. Any failure is a C-406 **halt** with the resolved path
printed verbatim beside the spec home and a fix line. The containment verdict
prints on **every** run (C-411), not only on failure, so its absence is
itself the alarm.

### No documented spec home (C-407)

When step 2 finds no documented spec home and discovery finds no practiced
one **— or** resolution step 3 yields zero candidates (the pure-`ADDED`
new-component case) — the fold **defers, it does not halt**:

- **No destination is invented and none is created.** hex does **not** create
  `.agents/specs/`, does not promote the template's fallback path, and
  does not write to any doc that merely looks spec-shaped.
- **The deltas are preserved.** The `## Spec Deltas` block already lives in
  the plan (written by `hex-execute`, C-419); the fold leaves it untouched
  and its `Target:` reads `unresolved`.
- **The terminal review state is still written.** A missing home is a project
  configuration fact, not a review failure; it must not cap the verdict.
- **The block names the fix.** No home →
  `not folded — this project documents no spec home` with the `/hex-init` fix
  and its proposal order (an existing practiced location — `docs/specs/`,
  `specs/` — else `.agents/specs/` as the last resort, with consent).
  Zero candidates →
  `not folded — no destination file for a new component; name one with
  Related Spec: in the plan, or create <spec home>/spec_<slug>.md`.
- **Recovery is one command and idempotent.** After `/hex-init` records a
  home (and, when the home is empty, seeds `<home>/spec_<slug>.md` from the
  shipped template, copy-only-if-absent, with consent),
  `/hex-review <plan path>` re-runs the phase; C-408 makes a double
  invocation a no-op. **The file's first byte is `/hex-init`'s, never
  fold-back's** — the write step amends an existing file and creates neither
  directory nor file.

## Safety envelope

Written as an ordered procedure. **Steps 1–5 produce no writes. The first
byte is written at step 6.**

A [workflow fork](config.md#workflows) (`adr_0003`) whose folding phase
performs this write runs this same envelope **in full** — config.md check 6
rejects a fork that folds without it, so the fork surface never bypasses these
steps.

### One destination per fold (C-415)

A plan carries exactly **one** `## Spec Deltas` block naming exactly **one**
`Target:` file. Multi-file folds are an explicit non-goal. OpenSpec's
prepare-all / write-all atomicity is **not ported and not claimed**: without
a runtime, "write-all" is N model-issued tool calls that can stop between
files — a silent partial fold, the exact failure this design prevents.
Consequently:

1. **The write is a single whole-file write** (step 6). There is no sequence
   to interrupt, so nothing to make atomic: the file is either the old body
   or the new one.
2. **More than one `## Spec Deltas` block, or a block naming more than one
   `Target:`, is a halt** (C-406):
   `not folded — this plan targets N spec files; one destination per fold`.
   Fix: split the plan, or fold the extra targets by hand.
3. **Delta entries whose ID lives outside the single target** are reported
   `not folded — ID lives outside <target>` and left in the plan. Nothing is
   ever written outside the single target.
4. **Interruption between the spec write and the plan update** is recovered
   by re-running `/hex-review <plan path>`: the destination census is the
   state of record (the file itself, not a claim about it), and C-408/C-417
   classify the re-run. A resumed run never trusts a ledger.
5. **A federated plan folds lead-scoped.** Under a `Repo` column, resolution
   (C-404), containment (C-418) and the census (C-405, C-416) all run **in
   the lead repo**, against the lead's `hex.md › Pointers`; the one `Target:`
   is a lead file. Every delta entry originating from a work package whose
   `Repo` ≠ `.` is reported `not folded — delivered in <repo>; fold by hand
   into that repo's spec` and left in the plan; only entries delivered in the
   lead (`Repo` = `.`) fold.

### Evidence is command output, not narration (C-414)

The mechanically expressible steps — the census (1), the stale-base sub-ID
sets (3), the no-shrink counts (3b), destination cleanliness (4), the ID half
of the self-check (5), and containment's `git ls-files` (C-418) — are
discharged by **literal shell calls whose raw output is pasted into the
Fold-Back block**, not summarized.
A step that merely *says* it ran is produced by the same pass that could have
skipped it. `git`, `grep` and `awk` are generic tools every harness already
runs — no parser, no runtime, no binary. A prose assertion standing where a
command transcript belongs is a spec violation (C-411).

### The ordered steps

1. **Census (mechanical, pasted).** Enumerate two ID sets: every
   `C-###`/`S-###` currently in the destination (`live`) and every ID the
   delta block names (`incoming`). `live` is produced by a literal call,
   using the recognized marker convention, and its **output is pasted
   verbatim**:

   ```
   grep -nE '^#{1,6}[[:space:]]+(C|S)-[0-9]+\b' <target>
   ```

   `incoming` is read from the in-message delta block. **Both sets appear
   before any write.**
2. **Conflict matrix (C-406).** An ID appearing in two delta sections is a
   conflict: `ADDED ∩ MODIFIED`, `ADDED ∩ REMOVED`, `MODIFIED ∩ REMOVED`, or
   twice within one section. Four rules, not OpenSpec's five — the fifth
   (`MODIFIED` referencing a `RENAMED` old name) cannot exist without
   `RENAMED`. Any hit: **halt**.
3. **Stale-base guard (C-405) — the ID-loss guard (mechanical, pasted).** For
   every `MODIFIED` entry, compare the **live** sub-IDs the destination
   carries under that contract against those the incoming body restates.
   Neither set is narrated: the step-1 heading census cannot see an inline
   sub-ID like `C-002.1` written in a contract's prose, yet those are exactly
   what this guard diffs, so the live set is produced by a literal call over
   the contract's **body span** (the C-413 span — heading to the next heading
   of the same or shallower depth) that extracts every marker-or-inline ID,
   and its **output is pasted verbatim**:

   ```
   awk 'NR>=<start> && NR<=<end>' <target> | grep -oE '\b(C|S)-[0-9]+(\.[0-9]+)?\b' | sort -u
   ```

   The incoming set is produced by the **same** command over the rebuilt
   incoming body, pasted beside it. **Any live sub-ID absent from the incoming
   set halts the fold**, with both pasted sets shown. Without the pasted span
   scan, a `MODIFIED` that drops a sub-clause but pads its line count clears
   both step 3b and the heading census silently — the class this guard exists
   to catch, the one OpenSpec's shipped skill omitted. It compares ID sets
   only; prose loss under a retained ID is step 3b's job.

   **Step 3b — no-shrink guard (C-416), the prose-loss guard.** For every
   `MODIFIED` entry, compare the **non-blank line count of the live body**
   against that of the incoming body. Both are counted, not estimated, by a
   literal call over the body span (heading to next heading of same or
   shallower depth), pasted beside the incoming count:

   ```
   awk 'NR>=<start> && NR<=<end>' <target> | grep -c '[^[:space:]]'
   ```

   **The trigger is exact: `incoming < live` halts the fold.** Not
   "significantly shorter" — any reduction at all, by one line. No tolerance
   band, no judgment call: a threshold a model estimates is one it can talk
   itself past. Equal or greater passes (C-405 has already established no ID
   was dropped). A legitimate narrowing is expressed as `REMOVED` with its
   reason, or folded by hand; the halt line names both routes plus "restate
   the full body". Same-size meaning drift remains the ceiling (see
   `adr_0005` Data-integrity NFR).
4. **Destination cleanliness (pasted).** The target must be unmodified
   relative to `HEAD`:

   ```
   git status --porcelain <target>
   ```

   Empty output is the pass; **any output halts**, and the output itself is
   the printed reason. Since hex-review never commits, revert-by-discard is
   the user's only undo, and discarding would destroy their uncommitted work
   along with hex's.
5. **Prepare and self-check the one target.** Build the complete rebuilt body
   of the single target in-message and self-check it — IDs unique, every
   `live` ID either present or explicitly `REMOVED`, ordering preserved. The
   ID half is discharged by command output, not assertion: write the rebuilt
   body to a **scratch path**, run the **same census command** against it,
   and paste both transcripts adjacently so the reader compares two `grep`
   outputs. A scratch file is not a destination write and does not breach
   "steps 1–5 produce no writes"; it is deleted after the check. Any failure:
   **halt, write nothing.**
6. **Write, once.** Immediately before writing, **re-run** the two one-line
   checks whose truth may have changed since steps 3–4 — containment (C-418)
   and `git status --porcelain <target>` — and paste both transcripts; either
   failing is a halt with nothing written. Then write the prepared body in
   **exactly one whole-file write** that replaces the target's full contents
   (C-415): no incremental edits, nothing that can stop halfway. The rebuilt
   body is composed in fixed order **`REMOVED → MODIFIED → ADDED`**, **ordering
   preserved**: an existing contract keeps its position even when modified;
   new contracts append at the end in delta order. Then append the
   [fold receipt](#idempotence-and-the-fold-receipt) to the plan's
   `## Spec Deltas` block (C-417).
7. **Print the Fold-Back block** — mandatory (C-411), emitted even when the
   phase did not run.

## Halt semantics

**Halt (C-406) means: write nothing, keep the terminal review state (e.g.
`State: done`), keep the deltas in the plan, print the reason and the fix,
and continue to Upkeep and the handoff.** A halted fold **never** fails the
review, **never** re-opens the verdict, and **never** attempts a repair pass:
**zero fix passes.** The precedent is the merge-conflict playbook
([`worktree.md`](worktree.md#worktree-work-package-mechanics): judge
semantically, at most one fix pass, still failing → halt) — deliberately
tightened to zero here, because there the fix edits code hex owns on a branch
it owns, while here the target is human-authored truth in the working tree,
so the cheapest correct move is always to stop.

**The halt triggers**, all of them:

- a conflict-matrix hit (step 2);
- a dropped live ID (C-405);
- an **ambiguous** target — two or more candidates (C-404 rule 4);
- a destination **not fold-compatible** — no recognized ID-marker convention
  (C-413);
- a **malformed entry** — no ID, `REMOVED` with no reason, or `MODIFIED` with
  no `Base:` line (C-403, C-419);
- a **dirty destination** (step 4);
- a **self-check failure** (step 5);
- a **shrinking `MODIFIED` body** — `incoming < live` (C-416);
- **more than one target** (C-415);
- a **containment failure** (C-418);
- a **plan edited since the recorded fold** (C-417).

**Not triggers — these defer, keeping the terminal state and losing nothing:**
zero candidates (C-404 rule 5), no documented spec home (C-407), an unfoldable
`S-###` (C-403 rule 5), and every satellite-delivered delta under a federated
plan (`Repo` ≠ `.`, C-415).

## Idempotence and the fold receipt

**Idempotence (C-408).** Re-running against the **same, unchanged** plan
produces a byte-identical destination. An `ADDED` whose ID is already present
with an identical body is a no-op; with a different body it is a conflict →
halt. A `REMOVED` whose ID is already absent is a no-op. A `MODIFIED` whose
body already matches is a no-op. No date-stamping, no rename, no
append-on-rerun. What makes "same, unchanged" checkable rather than assumed is
the fold receipt.

**The receipt (C-417).** On a successful write, step 6 appends one line per
touched ID to the plan's `## Spec Deltas` block — append-only, same
discipline as the convergence rows:

```markdown
Folded: 2026-07-20 → docs/specs/spec_export.md
  C-002 written: {C-002.1, C-002.2, C-002.3}, 17 lines
  C-004 written: {C-004}, 9 lines
  C-003 removed
```

This is the **base identity** a resumed run compares against — deliberately
not a hash (hex has no hasher, and a fingerprint a reader cannot compute is
the Phase 0 OpenSpec designed and never shipped). Sub-ID set plus non-blank
line count is comparable **by reading**, produced by the same two commands
the census and C-416 already run.

**A re-run is a retry, never a revision.** On any run that finds a `Folded:`
receipt for the target, **before step 5**, classify:

| Live destination vs. receipt | Incoming delta vs. receipt | Verdict |
|---|---|---|
| matches | matches | **Already folded.** No-op, `no change — already folded` (C-408). |
| does **not** match | matches | **Destination moved on** since the fold. Not a retry: re-derive against the current live body — C-405 and C-416 run normally and halt if the delta is now stale. |
| matches | does **not** match | **The plan was edited after folding. Halt.** |
| does not match | does not match | **Halt** — both sides moved; nothing can be inferred. |

The edited-plan halt prints the receipt counts beside the incoming ones:

```
not folded — plan delta changed since the recorded fold
  C-002 receipt: {C-002.1, C-002.2, C-002.3}, 17 lines
  C-002 incoming: {C-002.1, C-002.2, C-002.3}, 11 lines
  A re-run replays a fold; it does not apply a revision.
  Fix: the spec is now the base. Author a fresh delta against the
  current docs/specs/spec_export.md under a new ## Spec Deltas block,
  or edit the spec by hand.
```

A plan with **no** receipt (every plan before its first successful fold, and
every legacy plan) is unaffected: no receipt, no comparison, ordinary first
fold. Two edits that preserve both the sub-ID set and the line count are
indistinguishable to the receipt and replay as a retry — the same
same-size-drift ceiling, bounded by the same backstop: the write is
uncommitted and both counts are printed.

## Revert

hex-review never commits and hex never pushes (see
[`protocol.md`](protocol.md)), except `/hex-finalize`'s force-push of the one
feature branch it was invoked on, consented by that invocation and approved at
its gate — see [`finalize.md`](finalize.md#scope). Therefore (C-409):

- Every spec write lands **unstaged in the working tree**. `git diff` is the
  review of the fold; **`git checkout -- <spec file>`** is the complete undo,
  exact because of the step-4 clean-destination precondition.
- The plan's `## Spec Deltas` block and its terminal `State:` are ordinary
  working-tree edits to a tracked file, reverted the same way.
- **There is no hex command that undoes a fold.** Reverting is a git
  operation the human performs, and the handoff states it literally. An
  "unfold" would be a second write path into the same human-owned file with
  none of steps 1–5 protecting it.
- **The `git checkout --` window closes at the commit.** A fold the human
  commits at `/hex-finalize`'s pre-flight prompt, and finalize then
  recomposes and publishes, is past that undo: from there it is ordinary
  committed history, anchored by the backup ref.

## Plan archive

The plan is **not moved and not renamed** (C-410). The terminal review state
(`State: done`, or `landing` for a plan carrying a `Repo` column) is the
archive marker. A terminal plan's Status block may still be **appended to** —
`/hex-finalize` mirrors its quality status there, the writer role the plan
template already names — and that post-archive append is **not a second
archive event**: no second pointer clear, no second index row, and the plan is
still neither moved nor renamed (C-822). In its final
[Upkeep step](protocol.md#upkeep-step) the run
**clears the `hex.md › Memory` active-plan pointer** — the first place in the
bundle that ever clears it — and records the plan plus its fold target in the
artifact index (see [`memory.md`](memory.md)). Pointer clear and index write
key on the **same terminal-review-state condition** the Fold-Back gate uses,
so the phase and its Upkeep stay together; the satellite `Federation lead:`
locks are a separate mechanism that persists past `landing` to `done` and are
**not** this pointer.

**One precondition on the terminal state:** a run that ends with a non-empty
stranded-WP set never reaches its terminal review state — `done`, or
`landing` for a plan carrying a `Repo` column. The failure cascade
([`decompose.md`](decompose.md#parallel-by-default-decomposition)) owns that
rule, and this phase stays its sole writer. No other precondition gates
the state: the trunk review is `L3` of [Review by join
level](loop.md#review-by-join-level), run only when invoked, and this
skill writes the state because it is that level's sole writer.

Moving the plan to a dated archive directory was considered and rejected: it
breaks every link that points at the plan for the sake of directory
tidiness a `State:` field already provides, and imports the double-date-prefix
bug class. No move, no guard needed.
