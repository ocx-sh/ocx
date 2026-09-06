# The go/no-go gate

Read this when running steps 1 to 3. It holds the five signals with the command
that produces each, the narrower-fix ladder per language, the evidence base for
why no numeric threshold exists, and the schema of the decision file.

Contents: [Why there is no threshold](#why-there-is-no-threshold) ·
[The five signals](#the-five-signals) ·
[The narrower-fix ladder](#the-narrower-fix-ladder) ·
[Reading the answers](#reading-the-answers) ·
[The decision file](#the-decision-file)

## Why there is no threshold

Checked end to end as of 2026-09-06 against the vendor writing, the company case
studies, the tool READMEs and the one peer-reviewed study: **no source gives a
numeric go/no-go cutoff** for Bazel adoption. Not a repository count, not a
language count, not a target count, not a share of CI minutes.

What is sourced, and worth carrying:

| Finding | Weight |
|---|---|
| Of Bazel projects that configure a CI service, **31.23 percent never invoke Bazel in it**; of those that do, 27.76 percent need wrapper tooling to make it work ([arXiv:2405.00796](https://arxiv.org/abs/2405.00796), 383 projects; the denominator is the CI-adopting subset, verified) (BZL-CI-04) | Measured, peer-reviewed |
| **1.5 percent** of ~35,000 surveyed public projects ever adopted Bazel, and **11 percent of those** churned out around year two | A dated conference talk, no paper located |
| A departure is not always a defect: at least one large project removed Bazel calling it "a great tool" and consolidating on its language-native build instead | Primary, dated |
| The only vendor scale anchor argues **against** a threshold existing at ordinary scale: a customer at 2M SLOC and 500 engineers runs whole-repo shared green with no predictive tooling | The vendor's own claim, not neutral evidence |
| Teams that adopted and stayed are multi-language, high-headcount monorepos; **no source names the floor below which staying stops working** | Case studies only |

So the gate is a checklist. Each row is a yes/no with a measured value beside it,
and the verdict names which row decided it.

The one number this program does publish — ~40 minutes median whole-repo CI
wall-clock, with ~300 rule targets as the tripwire — is about **target
selection**, not about adoption, and it is a derived tripwire computed from one
team's stated pre-adoption context (BZL-CI-01). Never present it as a published
threshold, and never repurpose it as an adoption cutoff.

## The five signals

### 1. Whole-repo CI wall-clock (the lead signal)

Measurable before the repository is in Bazel, and the thing that actually costs.

```sh
gh run list --workflow=<gating-workflow> --limit 50 \
  --json startedAt,updatedAt,conclusion --jq \
  '[.[]|select(.conclusion=="success")|((.updatedAt|fromdate)-(.startedAt|fromdate))]|sort|.[length/2|floor]'
```

Prints the median in seconds. **Empty output or `null` means no successful run
history**, so the lead signal does not exist yet and the gate cannot pass. Collect
runs before deciding.

Target count is deliberately not a signal here: it is only obtainable once the
repository is already in Bazel.

### 2. Where the time actually goes

A build-time number only justifies a build system if build time is where the time
goes. Isolate it before believing it.

```sh
gh run view <run-id> --json jobs \
  --jq '.jobs[]|{name,secs:((.completedAt|fromdate)-(.startedAt|fromdate))}'
```

Read the per-job split. **A run whose largest job is install, provisioning,
container pull or a deploy step is not a build-time problem**, and Bazel does not
address it. Empty output means the run id was wrong, not that the jobs are free.

### 3. Generator maturity, per language in the repository

Granularity is downstream of generator maturity, not an independent choice
(BZL-ARCH-11). Quote the module you will actually depend on, never a more-mature
sibling — one ecosystem's language module is past 1.0 while the prebuilt
distribution its own README recommends is still `0.0.x`.

```sh
curl -s https://bcr.bazel.build/modules/<module>/metadata.json | jq -r '.versions[-1]'
```

**A 404 or empty output means no module exists under that name** — a recalled or
invented name, not a missing plugin. A `0.y.z` answer is pre-1.0 and makes no
semver promise; compare the *second* component when judging staleness
(BZL-FLAG-29).

Maturity as measured on 2026-09-06:

| Language | Generator | Verdict |
|---|---|---|
| Go, protobuf | bazel-gazelle core, first-party, roughly monthly releases | Production |
| Python | the Python ruleset's own extension, first-party | Production, **only** paired with a pytest wrapper |
| JS/TS | a vendor plugin; language module past 1.0, prebuilt distribution still 0.0.x | Usable with care |
| Rust | an independent single-maintainer plugin, one tag, `0.1.0` | Experimental |
| C++ | an early plugin, `0.1.0` | Experimental |

Production means fine-grained targets are supportable. Anything below it means
coarse, package-per-directory, hand-maintained BUILD files are the correct
default — hand-fine-graining buys the maintenance cost and none of the tooling.
No ecosystem ships the freshness gate; that is always hand-wired (BZL-ARCH-12).

### 4. Cross-repository coupling

Repositories that read as independent are frequently one dependency lineage two
submodules deep, and the migration has to model the lineage, not the appearance.

```sh
git config -f .gitmodules --get-regexp path
```

**Empty output = no submodule coupling to model.** Each path printed is checked
against the language manifest's dependency graph; a submodule that is also a
build dependency is an open Bzlmod question, not a settled pattern — Bzlmod has
no submodule-equivalent primitive, and the nearest analogue requires the vendored
fork to itself declare a `MODULE.bazel` (BZL-ARCH-28).

```sh
git ls-remote <fork-remote> >/dev/null && \
  curl -sfI https://raw.githubusercontent.com/<owner>/<repo>/HEAD/MODULE.bazel
```

**A 404 here means the precondition is unmet** and the coupling stays invisible
until the fork itself becomes a module. The `bazel mod graph --extension_info`,
`bazel mod explain` and `bazel mod show_repo` subcommands all exist and behave
identically on 8.7.0, 8.8.0 and 9.2.0 — read off the pinned binary with
`bazel help mod`, never off a versioned docs page (BZL-MOD-13). The query has
never been the blocker.

### 5. Team shape

Not a command, and not a number this procedure can supply. Two questions for the
owner, recorded verbatim in the decision file:

- Who owns the build after adoption, by name or by rotation? An unowned Bazel
  build is the documented churn mode.
- Does anyone on the team already read Starlark? A migration whose only Starlark
  reader is an agent has no reviewer for the diffs it produces.

## The narrower-fix ladder

Per language, the cheaper answer and what the evidence says about outgrowing it.

| Shape | Cheaper fix | Evidence it stops being enough |
|---|---|---|
| Rust workspace | cargo's incremental build, `sccache` for a shared compiler cache, `cargo nextest` for parallel tests | **None.** Whether a Rust workspace needs Bazel at all is treated as an open question by every source in the corpus |
| Python project | `uv`'s resolver and its cache; no cross-project import graph to break | The comparison cluster steers Python-heavy monorepos to **Pants before Bazel** for exactly this shape |
| TypeScript / JS | pnpm's content-addressable store, plus Nx or Turborepo for task-level caching | The consensus argues a JS-only team is **permanently** well served there, not that it eventually outgrows it |
| Any repo on plain CI caching | the CI system's own cache steps | Not evidenced as a pre-Bazel rung at all; graph-aware merge queues assume Bazel's graph already exists |

The reverse evidence is worth stating out loud: the corpus holds more documented
instances of teams reverting from Bazel to a simpler system than of teams naming
the moment a simpler system stopped working and Bazel was the fix.

## Reading the answers

There is no scoring rubric, because there is no threshold to score against. Three
readings that are decidable:

- **No-go, and stop.** No cheaper fix has been tried; or the largest CI job is not
  a build job; or no successful CI history exists to measure.
- **No-go for now, with a named condition.** A single-language repository whose
  own ecosystem tool is argued sufficient, and whose wall-clock is not the
  complaint. Record what would reopen it.
- **Go, with the pilot named.** Multi-language, the build is the measured cost,
  the cheaper fix is in place and named as insufficient, and at least one language
  has a production generator or is small enough to hand-write.

Anything else is a decision the owner takes, not one this procedure takes. Say so
in the file rather than inventing a tie-break.

## The decision file

One file, fixed shape. Every row carries the command that produced its value.

```markdown
# Bazel adoption decision

Repository: <name> · Decided: <ISO date> · Decided by: <owner>

### Signals

| Signal | Measured value | Command | Answer |
|---|---|---|---|
| Cheaper fix tried and named | | | yes/no |
| Median whole-repo CI wall-clock | | | |
| Largest CI job is a build job | | | yes/no |
| Generator maturity per language | | | |
| Cross-repo coupling to model | | | |
| Build owner after adoption | | (owner statement) | |

### Verdict

<go | no-go | no-go for now>, because <the one signal that decided it>.

### What would change it
- <condition> — reopens the <signal> row.

### Explicitly not decided here
- Target selection: deferred, see BZL-CI-01.
- Remote execution: deferred, see BZL-CACHE-07.
```

Check it before moving on:

```sh
grep -nE '\|[[:space:]]*\|' <decision-file> | grep -v -- '---'
```

**Empty output = every signal row carries a measured value.** Any line printed is
a row with an empty cell, and the verdict is not writable yet.
