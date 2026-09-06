# hex-init Audit Checklist

The checklist `/hex-init` runs in Step 1, and the copy-ready blocks it
proposes in Steps 2 and 6. Every item names what to look for, where to
look, and what "documented" (or "resolved") actually looks like — a
project passes an item only when the concrete bar below is met, not on a
vague impression that "it's probably in there somewhere."

## Audit items

### Verification documented?

- **Look for:** a section describing how to build, test, and lint the
  project.
- **Where:** the project-context file(s) directly, or a doc they point to
  (README, CONTRIBUTING.md, a docs/ page).
- **Documented looks like:** a runnable command (or a short named set of
  them) stated explicitly — "run `make check`" — not just "there's a test
  suite" or a bare mention that tests exist. A command buried three links
  deep with no pointer from project context does not count as documented.
- **De facto discovery:** before proposing a best-practice block, scan
  where verification actually lives undocumented — CI workflow files
  (`.github/workflows/*`, `.gitlab-ci.yml`), `Makefile` / `Taskfile.yml` /
  `justfile` targets, package-manifest scripts (`package.json` scripts,
  `pyproject`/`tox`, Cargo aliases). A found command is proposed for
  **adoption via pointer** — document what exists, don't invent a new one.

### Selective test command documented?

- **Look for:** a command that runs the tests a change affects, rather
  than the whole suite — the project's own selective / affected-tests
  entry point, named alongside the full one.
- **Where:** project context and checked-in files only. This item performs
  no network read.
- **Documented looks like:** a runnable template with its placeholders —
  `nx affected -t test --base={base}`, `pytest --testmon`, or
  `npx jest --findRelatedTests {files}`. Not "we use Nx", and not a suite
  name with no command behind it.
- **De facto discovery:** `nx.json` or `turbo.json` at the repo root, a
  `.testmondata` entry in `.gitignore`, an `affected`-shaped CI job. A
  found command is proposed for **adoption via pointer**, never invented;
  only what the user consents to record in project context is what hex
  runs — see
  [`verify.md` § Verification › Scoped check](../../hex-core/references/verify.md#scoped-check).

### Commit and landing requirements documented?

- **Look for:** whether the project requires DCO sign-off, signed commits,
  or a commit-message convention; which suites count as release-grade; and
  which workflows are the release gate.
- **Where:** project context and checked-in files only. This item performs
  **no network read** — nothing here queries a forge, and every forge read
  in hex lives inside `/hex-finalize`, behind its gate, where it is
  disclosed.
- **Documented looks like:** a named requirement paired with its
  enforcement point — "commits must carry `Signed-off-by`, enforced by the
  `dco` check" — not "we use conventional commits, probably". A convention
  nobody enforces and a check nobody documented are both gaps.
- **De facto discovery:** commitlint-family configs
  (`commitlint.config.*`, `.commitlintrc*`, `cog.toml`, `.gitlint`),
  `CONTRIBUTING.md`, and the last ~20 non-merge commits' own dialect. A
  found requirement is proposed for **adoption via pointer**, never
  invented.
- **Recommend the control that actually holds:** protection on the target
  branch — "restrict force pushes" plus a required pull request. Say why:
  it is enforced server-side, so it binds regardless of what any agent's
  prompt says, which no shipped instruction text can claim. Recommend it
  whether or not the project already documents commit requirements.
- **Series-shape offer (conditional, consent-gated).** Only when discovery
  finds **both** axes undocumented — the default commit count per PR
  (squash-to-one versus a bisectable series) and which test triggers the
  squash decision — offer to record the team's preference as prose in
  `hex.md › Preferences`, with consent. Name the alternative in the offer:
  without it, `/hex-finalize` ships a **minimal bisectable series** — one
  commit per user-facing change, riders split out. A project whose
  convention is already documented is **not** asked; that documentation
  wins over any hint recorded here.

### Spec / plan / ADR conventions documented?

- **Look for:** where specs, plans, and ADRs live, and what format or
  template they follow.
- **Where:** same as above.
- **Documented looks like:** a named location plus a named format — even
  "plain markdown, no fixed template" counts — not silence, and not "look
  at the last one we wrote."
- **De facto discovery:** glob for practiced-but-undocumented homes —
  `docs/adr*`, `docs/decisions*`, `docs/plans*`, `specs/`, an existing
  `.agents/` artifact tree. A found scheme is proposed for
  **adoption via pointer**; the shipped hex templates are the last
  resort, never the first proposal.

#### Spec home documented (conditional)

A **conditional** sub-check, not a standalone item — asked only when a
plan carries an unresolved `## Spec Deltas` block (`Target: unresolved`),
**or** the conventions entry found above names plans and ADRs but no
specs. A project that has never planned anything is asked nothing.

- **Proposal order:** an existing practiced location first — the de facto
  glob above, `docs/specs/`, `specs/` — else `.agents/specs/` as
  the **last resort**, with consent.
- **ID-marker question:** does the resolved home's contracts use the
  default heading shape, or a project-specific marker? A non-default
  answer is recorded as `Spec ID marker:` prose in `hex.md › Preferences`
  (user-owned, written only by `/hex-init`, never edited by a run).
- **Seed offer:** when the resolved home is empty, offer to copy
  [`../assets/templates/spec.md`](../assets/templates/spec.md) to
  `<home>/spec_<slug>.md`, **copy-only-if-absent**, with consent.
- The mechanics this question feeds — resolution order, the ID-marker
  convention, containment, the no-spec-home defer — are defined once in
  [`archive.md`](../../hex-core/references/archive.md#destination-resolution);
  this item only asks and records the answer, never restates them.

### Discussions home documented (conditional)

A **conditional** item — asked only when a discussion artifact already
exists, **or** the user asks for one. A project that has never discussed
anything is asked nothing, and hex never raises the question on its own.

- **Proposal order:** an existing practiced location first — a
  `docs/discussions*` or `discussions/` tree the project already uses —
  else `.agents/discussions/` as the **last resort**, with consent.
- **Recorded as:** one `hex.md › Pointers` row —
  ``- Discussions: `<home>` — pre-plan discussion artifacts (/hex-discuss).``
  `<home>` is the location the user consented to, `.agents/discussions/`
  only when the last resort was taken.
- **Seed offer:** when the resolved home is empty, offer to copy
  [`../assets/templates/discussion.md`](../assets/templates/discussion.md)
  to `<home>/_template.md`, **copy-only-if-absent**, with consent. The
  underscore prefix marks the file as never a discussion — say so in the
  offer, so nothing scanning the home for a live one picks it up.
- Verify-on-consumption and the re-audit apply unchanged — this item adds
  no staleness mechanism of its own. The mechanics it feeds are defined
  once: home resolution in
  [`memory.md`](../../hex-core/references/memory.md#location-and-resolution),
  containment in
  [`archive.md`](../../hex-core/references/archive.md#destination-resolution),
  whose **path** conditions bind every write under `<home>` — the seed
  copy above and `/hex-discuss`'s own `<home>/<slug>.md` alike (its
  already-exists and git-tracked conditions belong to the fold; the
  no-symlink, no-directory clause still binds — a dangling symlink reads
  as absent). This item only asks and records the answer, never restates
  them.

### Rules carry architectural context?

- **Look for:** rules or conventions that state module boundaries,
  invariants, a golden path, or which paths are security-sensitive — not
  just formatting or lint rules.
- **Where:** project-context file(s), or a rules/ or docs/ directory they
  point to.
- **Documented looks like:** a rule that would change how a reviewer or
  architect reasons about a diff (e.g. "code under `src/auth/**` is
  security-sensitive," "never call X directly, use Y"). Style-only rules
  (naming, formatting) don't count toward this item, however plentiful.
- **De facto discovery:** check `CONTRIBUTING*`, `docs/`, and any
  design/architecture docs for rules that carry weight but aren't linked
  from project context — propose a pointer to them, not a rewrite.

### Product context documented?

- **Look for:** what the product *is*, who uses it and where it runs,
  related repositories, spec/doc homes beyond the code, useful
  web-research keywords, and comparable tools.
- **Where:** the project-context file(s) directly (a short product
  section), or a product doc — README, `docs/`, or a provisioned
  `.agents/product.md` — reached from a one-line index entry in the
  context file.
- **Documented looks like:** a reader who has never seen the repo could
  say in two sentences what the product does and for whom, and name at
  least one comparable tool. Missing pieces are gathered from the user in
  Step 2's wizard questions — this is the one audit item whose answers
  usually cannot be discovered from the repo alone.

### Constitution / governing principles pointer?

- **Look for:** a governing-principles or constitution doc — a named
  file of binding decisions that plans are checked against.
- **Where:** the project-context file(s), or its cached location in the
  Pointers section of `.agents/memory/hex.md`.
- **Documented looks like:** a named file of binding principles
  (boundaries, non-negotiables) — not a style or lint guide. **Optional**
  — absent is fine and the gate stays off silently; see
  [`protocol.md#constitution-gate`](../../hex-core/references/protocol.md#constitution-gate).

### Worktree path gitignored?

- **Look for:** whether the path `/hex-execute` uses for parallel work
  packages (`.agents/worktrees/` by default, or a project-declared
  alternative) is excluded from version control.
- **Where:** the ignore file (`.gitignore` or equivalent), and
  `hex.md › Pointers` for a declared alternative path.
- **Resolved looks like:** the exact path (with trailing slash) present in
  the ignore file. Flag it if `.agents/` is ignored wholesale — that drops
  the team-shared `.agents/memory/hex.md` from version control and
  must be narrowed to `.agents/worktrees/` specifically.

### Cross-model adversary skill installed?

- **Look for:** an installed skill's `SKILL.md` frontmatter carrying a
  `metadata` map with a `hex-adversary-scopes` key; whether
  `hex.md › Preferences` already names an `adversary:` skill.
- **Where:** `grim status --format json`'s top-level `items[]` array — each
  entry's `outputs[]` carries the installed paths (`outputs_pending[]`
  before install) — when the project uses grim: the supported way to script
  against install locations, because the on-disk vendor layout is not a
  stable contract.
  Without grim, the client's own skill install roots — for Claude Code,
  `.claude/skills/*/SKILL.md` and `~/.claude/skills/*/SKILL.md`. This item
  performs **no network read**, and executes nothing — frontmatter only.
- **Documented looks like:** an explicit `adversary: <skill-name>` line
  naming an installed skill.
- **De facto discovery:** a marker found with **no pin** proposes
  `adversary: <skill-name>` for **adoption via pointer**, folded into Step
  4½'s single consent-gated diff. Never writes unasked, and never removes or
  overwrites a user-typed pin. A marker-less skill such as `codex:rescue` is
  untouched.
- **Drift:** a pin naming a skill that is **not installed** is reported, not
  repaired — the no-overwrite rule above owns that pin, so there is nothing
  to propose. Report it, because a dangling pin makes every adversary pass
  log a skip forever and no one sees why.
- **Optional** — no marker found, no pin proposed, and the item stays
  silent.

### Review settings tuned?

- **Look for:** whether `hex.md › Preferences` carries a `review:` block
  ([`config.md`](../../hex-core/references/config.md#key-vocabulary)
  `review.<level>.*`), and whether `hex.md › Memory` records a review-config
  or checklist candidate a prior run surfaced under the
  [upkeep step](../../hex-core/references/protocol.md#upkeep-step).
- **Where:** `.agents/memory/hex.md` only. No network read; executes
  nothing.
- **Documented looks like:** a `review:` block whose values differ from the
  shipped defaults for a stated reason, or an explicit note that the
  defaults are accepted.
- **De facto discovery:** a Memory entry naming a budget expiry, a residue
  line, a round count, or a finding class that recurred — each proposes the
  matching `review.<level>.*` key, or a project-rule line the
  [checklist](../../hex-core/references/checklist.md#composition) will pick
  up, for adoption, folded into Step 4½'s single consent-gated diff.
- **Optional** — no candidates and no block means the shipped defaults
  apply and the item stays silent.

### Resource profile measured?

- **Look for:** whether `hex.md › Pointers` carries a `Resource profile:`
  entry — the peak RSS, wall time, and `light`/`heavy` class this
  project's own documented verification gate measured, plus the
  heavy-command ceiling derived from them.
- **Where:** `.agents/memory/hex.md › Pointers`.
- **Documented looks like:** the four values
  [`resources.md` § 2](../../hex-core/references/resources.md#2-the-measured-resource-profile)
  defines, recorded from an actual run on **this host** — never a number
  picked because it looks right for the ecosystem or the project's size.
  A project classed `light` (parse-only verification) is no exception;
  what its ceiling is for is that section's, not restated here.
- **De facto discovery:** where no `Resource profile:` entry exists,
  measure it — run the project's own documented verification gate once
  under the portable peak-RSS ladder and derive the ceiling from what
  this host measured; see
  [`resources.md` § The measured resource profile](../../hex-core/references/resources.md#2-the-measured-resource-profile).
  Where no rung on this host can measure it, record the profile
  **absent** and announce the degrade — never fabricate a number for a
  host that could not be measured; the heavy-command ceiling then falls
  back to its documented floor rather than a guess.

### Agent worktrees excluded from watchers and indexers?

- **Look for:** whether the path `/hex-execute` uses for parallel work
  packages (`.agents/worktrees/` by default, or a project-declared
  alternative) is excluded from IDE file watchers, project-wide search
  indexes, and other background indexing tools — a concern distinct
  from version control: an untracked worktree can still be watched,
  indexed, and churned on by an editor or a search tool.
- **Where:** editor/IDE workspace settings (e.g. `.vscode/settings.json`'s
  `files.watcherExclude` and `search.exclude`, a JetBrains excluded-folder
  mark), and any project-wide file-watch or search-index config the
  project already carries.
- **Documented looks like:** the worktree path named in at least one
  such exclusion list, or an explicit note in project context that no
  watcher or indexer runs against this repo. Silence, with an editor
  known to index the repo, does not count as documented.
- **De facto discovery:** a run creates and destroys several worktrees
  in quick succession; an unexcluded watcher or indexer churns on that
  traffic, burning cycles re-scanning generated build artifacts and, on
  a platform with a low file-watch limit, exhausting it outright. Scan
  the exclusion configs above for what the project already carries and
  propose adding the resolved worktree path to it — **adoption via
  pointer**, never inventing a new watcher or indexer config for a
  project that runs none.

### Scratch / temp convention documented?

- **Look for:** the project's own scratch/temp convention — where
  build tools, test runners, and the project's own scripts are expected
  to write transient files — and whether the project has opted `HOME`
  into the per-run scratch redirect.
- **Where:** project context, and `hex.md › Pointers`'s `Scratch:`
  entry — the disk-backed per-run root (`TMPDIR`, `XDG_CACHE_HOME`,
  `XDG_STATE_HOME`) a run redirects its scratch environment to by
  default.
- **Documented looks like:** a named scratch/temp location, or an
  explicit "no convention, tools use their own defaults" — not silence.
  Where the project takes the `HOME` opt-in below, one line recording
  that consent in project context.
- **De facto discovery:** `HOME` is deliberately **not** among the
  three variables redirected by default, because redirecting it breaks
  every tool that reads real credentials from it — git identity, `gh`
  auth, registry tokens, ssh. Offer the `HOME`-redirect **opt-in**,
  recorded as project-context prose only with consent, and name **two**
  reasons a project might take it: a test suite **known to write to
  `$HOME`**, and **credential exposure to a verification command the
  project does not fully trust** — hex runs that command unattended and
  N-way concurrent, with read access to `~/.ssh`, `~/.config/gh`, and
  `~/.aws`. Never redirect `HOME` without that consent, and never
  propose it as the default.

### Existing `hex.md › Pointers` and index lines still resolve?

- **Look for:** every pointer in the `hex.md › Pointers` section
  (verification location, conventions location, doc/product homes, key
  rules, worktree deviation) and every one-line index entry hex seeded
  in the context file still points at something that exists and still
  says what it claims.
- **Where:** `.agents/memory/hex.md` and the project-context
  file(s), cross-checked against the current tree.
- **Resolved looks like:** the file or section a pointer or index line
  names still exists and still covers what it describes. A pointer to a
  moved or rewritten section is drift — report it, don't silently trust
  it.

### Federation pointers still resolve? (lead repos)

- **Look for:** in a repo whose `hex.md › Pointers` carries one or more
  `Federation:` bullets, whether each declared satellite `<path>` still
  exists and each bullet's verification clause still points at where that
  satellite documents verification.
- **Where:** the lead's `.agents/memory/hex.md` `Federation:`
  bullets, cross-checked against the current tree and each satellite's
  project context. The `<remote>` is recorded for identity only and is
  never fetched — nothing here reaches the network.
- **Resolved looks like:** the `<path>` resolves to the satellite and the
  verification clause still covers what it claims. A moved path, or a
  verification target that no longer exists, is drift — re-detect from the
  satellite's project context and re-point in the same run, the same
  verify-on-consumption re-audit shape used for the Existing
  `hex.md › Pointers` item above; never silently trust it.

### Federation back-pointer slugs still live?

- **Look for:** in a repo whose `hex.md › Pointers` carries a
  `Federation lead:` bullet, whether each plan slug the bullet lists still
  names a live, unfinished plan in the lead. `/hex-init` is **exempt** from
  the satellite halt ([`memory.md` § Location and
  resolution](../../hex-core/references/memory.md#location-and-resolution))
  precisely so it can run this audit and offer removal.
- **Where:** the satellite's `.agents/memory/hex.md`
  `Federation lead:` bullet, and the lead repo it names (resolved by the
  bullet's own path) — check each slug against the lead's plans and their
  Status `State`.
- **Resolved looks like:** every listed slug names a plan that still exists
  in the lead and has not reached State `done`. Offer removal for any slug
  whose lead-side plan is absent or `done`; when the slug list empties,
  delete the bullet — and the `hex.md` file too, if it held nothing else
  (C-313). A second `Federation lead:` bullet naming a different lead is a
  design smell — report it, never auto-merge.

### Workflow fork stamps current?

- **Look for:** every file a `workflows.<skill>.<tier>` pointer names, and
  its `Forked from … @ hex <version>` stamp.
- **Where:** `.agents/workflows/` (the fork files) and the
  `workflows` key in `hex.md › Preferences`.
- **Resolved looks like:** the fork file exists, carries a stamp, and the
  stamped hex version matches the installed one. A stamped version older
  than installed is **drift** — report it with the [fork-drift
  block](#workflow-fork-drift-report) below and show what changed in the
  shipped tier file; never auto-merge or rewrite the fork. A pointer naming
  a missing file, or a fork with no stamp, is reported the same way (the
  run itself falls back to the shipped tier file per
  [`config.md` § Workflows](../../hex-core/references/config.md#workflows)).

## Best-practice blocks

Copy-ready templates `/hex-init` proposes for the gaps above. Fill the
placeholders from what the audit actually found; never paste these
unfilled.

### Verification section

```markdown
## Verification
Run `<command>` before considering any change complete. Covers: <build |
test | lint - whatever it actually runs>. <Anything it doesn't cover, if
relevant.>
```

### Selective test command block

Extends the Verification section above. The fenced line below is proposed
only when the audit found a selective command, or the user named one.

```markdown
Selective tests: run `<template, e.g. nx affected -t test --base={base}>`
for the tests a change affects; `<command>` still runs the full suite.
```

Two `hex.md › Pointers` rows record where each is documented, in this
grammar:

``- Selective tests: `<location>` — where the selective test command is documented.``
``- Sensitive paths: `<location>` — where the project's security-sensitive / hot-path convention is documented.``

The second row is the named source the high-risk merge trigger reads —
distinct from the `Key rules` pointer, which names the rule files
themselves. It records **where the convention is documented**, never a
path list and never a judgment, and it is proposed off the [rules
item](#rules-carry-architectural-context) whether or not a selective
command was found.

### Spec / plan / ADR conventions block

```markdown
## Spec / plan / ADR conventions
- Specs and plans live in `<path>`, format: `<format>`.
- ADRs live in `<path>`, format: `<format, e.g. MADR>`.
- Shipped hex templates (`hex-init/assets/templates/`) are the fallback
  only, used when nothing above is documented.
```

### Constitution pointer block

```markdown
- Constitution: `<path>` (optional; plans gated against it when present).
```

### Product doc (provisioned)

A standalone product doc, written to a de-facto home when one exists
(`README`, `docs/`) or to a provisioned `.agents/product.md` — no hex
markers; it is ordinary project documentation.

```markdown
# Product
- What: <one or two sentences: what the product is>.
- Users: <who, and where it runs — local, CI, server, embedded>.
- Related repos: <repositories this product depends on or serves>.
- Docs / spec: <where the spec and user docs are maintained>.
- Research keywords: <terms researchers should search for>.
- Comparable tools: <tools solving the same problem>.
```

### Product index line (context file)

One line in the project-context file so every agent discovers the doc
ambiently; `hex.md › Pointers` records the same location.

```markdown
Product overview: `<path/to/product-doc>` — what it is, who uses it,
comparable tools.
```

Short facts may instead live directly as a `## Product` section in the
context file when there is too little to warrant a separate doc
([`memory.md`](../../hex-core/references/memory.md#destination-of-knowledge)).

### Worktree gitignore line

```gitignore
.agents/worktrees/
```

Never `.agents/` wholesale — `.agents/memory/hex.md` is team-shared
memory and belongs in version control; only the transient worktree
checkouts are ignored.

### Discovery note block

```markdown
<!-- hex:start -->
Swarm memory: `.agents/memory/hex.md` (search upward; pointers + preferences).
Commands: `/hex-init`, `/hex-discuss`, `/hex-plan`, `/hex-execute`, `/hex-review`, `/hex-architect`, `/hex-finalize`.
<!-- hex:end -->
```

### Workflow fork drift report

Emitted by the re-audit for a fork whose stamped version differs from the
installed hex version (or whose file is missing / unstamped). It points at
what changed upstream and never applies a patch — hex never auto-merges a
fork.

```
Workflow drift: .agents/workflows/<skill>-<tier>.md
  forked from hex-<skill>/tier-<tier>.md @ hex <stamped-version>
  installed hex <current-version>
  shipped changes: <phases added / removed / re-pointed, gates changed>
  Fix: re-fork to adopt the new baseline, or hand-edit the fork; runs use
  the fork as-is until you do.
```
