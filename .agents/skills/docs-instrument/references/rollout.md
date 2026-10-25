# Rollout

Read this when wiring any check into CI. It holds the two-severity rule, the
ratchet, the blocking posture, and the schema of every artifact the procedure
produces outside the manifest.

Contents: [The file list](#the-file-list) ·
[The retrofit commit](#the-retrofit-commit) ·
[Two severities, always](#two-severities-always) · [The ratchet](#the-ratchet) ·
[What blocks a merge](#what-blocks-a-merge) ·
[Pull request template](#pull-request-template) ·
[Lighthouse ratchet](#lighthouse-ratchet) ·
[A new check of your own](#a-new-check-of-your-own)

## The file list

Every check reads the same list. Build it once, in the runner.

```sh
docs_files() {
  git ls-files "$DOCS_ROOT" README.md CHANGELOG.md 2>/dev/null
}
```

`DOCS_ROOT` is the directory holding the generator config. Where there is no
generator config, name the committed docs directory here instead, and expect the
DOC-NAV family to report not applicable.

Assert the list excludes `.agents`, `.claude`, `.serena`, `.worktrees`,
`node_modules`, `dist`, `target` and every build output directory. A naive
directory walk on one measured repository loaded 420 generated audit reports and
257 stale worktree files.

## The retrofit commit

One commit, and it changes nothing but declaration lines. Reviewers can then read
it as a mechanical change, and a later `git blame` on a page still points at the
person who wrote the prose.

- Subject names the mechanic, for example `docs: add doc_type declarations`.
- Body records three counts: pages seeded from the nav config, pages seeded from
  a path heuristic and reviewed, pages that needed a content read.
- One or two added lines per page. Nothing else in the diff.
- Expect roughly 1.3 added lines per page across the tree, because only
  `tutorial`, `how-to` and `landing` pages take a `doc_tier` line.

Run `python3 checks/doc_declaration.py --root . --seed` for the proposal, review
every value, then run the check without `--seed` until it exits 0.

Nav-label seeding measured 94.3 percent accurate over 122 pages
(`wave2-declaration-key.md` section 10). A path classifier measured 68.1 percent
over the same fleet, which is why the check never reads a path at runtime.

## Two severities, always

A new check runs twice on every pull request.

| Pass | Scope | Severity |
|---|---|---|
| Diff | `git diff --name-only` intersected with the file list | error |
| Whole tree | the full file list | warning |

A check may launch at error whole-tree only when the current violation count is
zero. Any check with standing violations launches diff-scoped.

The reason is measured, not cautious. One measured baseline had 229 of 249
pages failing the punctuation rule and 211 of 249 failing the sentence-length
rule. Another 132 of 249 sat below the readability floor. A whole-tree red gate
on day one blocks every open pull request. A gate that blocks everything gets
switched off.

## The ratchet

Record the whole-tree warning count for every check the day it lands. That number
is the baseline.

1. The gate fails when the count rises above the baseline.
2. Lower the baseline whenever a backfill lands. Never raise it.
3. When the count reaches zero, promote the check to error whole-tree and delete
   the baseline entry.

Store the baselines in one file beside the runner, one line per check ID, so a
change to a baseline is visible in review.

Structural and drift checks fail red from the start. Readability scores and tell
densities report as warnings under the ratchet, because a red prose gate gets
switched off rather than satisfied.

## What blocks a merge

The default posture is pinned and the adopter may override it once, in one place.

- General documentation drift does not block a merge. Merge the change and open
  a tracked issue for the gap in the same action (DOC-OBS-04). Each deferred
  finding carries an issue reference. An empty reference fails the check.
- Drift in a page declaring `doc_type: runbook` does block the merge
  (DOC-OBS-05). A wrong runbook step costs incident minutes, and nothing else
  pages anyone when it rots.

That split is by blast radius, not by vote. Copying the strictest posture onto
every page is a measured agent failure.

Where the repository has runbook pages, retrofit the declaration onto them
first, in step 2. A runbook rule with no page declaring that type is permanently
inert, and inert is indistinguishable from clean.

## Pull request template

Two keys, checked by CI, plus one owned by the prose family.

```markdown
Added:
Removed:
AI assistance: no
```

- `Added:` and `Removed:` are required and may not be empty. The literal value
  `none` passes (DOC-OBS-09). Unreviewed growth reads as improvement while it
  buries the pages that already worked.
- `AI assistance:` takes `yes`, `no` or `partial` (DOC-PLAIN-22). A second check
  greps the commit trailers and fails on any co-author trailer naming a model or
  an assistant. Disclosure belongs on the pull request, never on the page.

## Lighthouse ratchet

Only where the repository already builds a site. Assert the measured category
scores, set a point or two below the current median, and raise the floor as the
site improves.

```js
assertions: {
  'categories:accessibility':  ['error', { minScore: 0.97 }],
  'categories:best-practices': ['error', { minScore: 0.93 }],
  'categories:seo':            ['error', { minScore: 0.97 }],
  'categories:performance':    ['warn',  { minScore: 0.85 }],
}
```

Those numbers are the measured floors from the one real instance found, against
medians of 1.00, 0.96, 1.00 and 0.88. Replace them with your own site's measured
medians. A threshold nobody measured is a threshold nobody can defend.

Prove the gate red once and record the proof in a comment beside the config. On
the measured instance, a missing `alt`, an empty button and an unlabelled input
moved accessibility from 0.92 to 0.77 and failed the gate.

## A new check of your own

This section applies to a check you write, not to the shipped ones.

**Fixtures.** Under `checks/fixtures/<script>/`, at least one `fail-*.md` the
check must reject and one `pass-*.md` it must accept. `--self-test` runs the
script over its own fixtures. It exits 1 unless every fail fixture is rejected
and every pass fixture accepted. A check with no rejecting fixture has never been
proven able to fail. Three real classes of unfailable check were measured here.
A count compared to itself, a grep on a beacon with no listener, and a probe that
reports "cannot verify" and then passes.

**Measurement before severity.** Run the check over the real corpus. Record two
numbers, the raw hit count and the false-positive rate on a sampled subset. Only
then choose a severity. Assigning severity from how important a rule feels is a
measured failure mode.

| False-positive rate | Ceiling |
|---|---|
| Not yet measured | warning only |
| Above about 20 percent | not a merge gate, ship it as a lead list |
| Measured low, on a real corpus | eligible for error |

Two measured examples of why. A bare-identifier grep returned 1,621 hits over
seven files and was demoted. A metric-shaped grep returned 7 hits of which 5 were
false, at 71 percent, and lost its merge-gate severity.

**Message shape.** Findings print as `path:line: DOC-XXX-nn: message`. Every
number in the message names its source in parentheses. A number with no source is
the defect this convention exists to prevent.

**Scope.** The check reads source markdown only, never build output, and never a
path to decide a page's type. No absolute path and no repository-specific path
appears in the script or in any fixture.
