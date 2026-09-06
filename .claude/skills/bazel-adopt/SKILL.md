---
name: bazel-adopt
description: Go/no-go gate and staged migration procedure for moving a repository onto Bazel, covering the measured adoption signals, the pilot choice, MODULE.bazel and lockfile setup, the .bazelversion pin and the 8.x-versus-9.x call, rc-file layering, cache wiring order, CI lanes, and one branch per language for Rust, Python, TypeScript and C++. Use when someone asks whether a repo should move to Bazel, wants a Bazel migration plan or adoption plan, asks how to start with Bazel, how to add a MODULE.bazel, which Bazel version to pin, how to set up a remote cache or remote execution for the first time, what to migrate first, or whether Bazel is worth it against Nx, Turborepo, Pants, sccache or plain cargo and uv caching. Not for diagnosing or speeding up a build that is already on Bazel, which is bazel-diagnose, and not for the standards themselves, which are the bazel-quality rules.
license: Apache-2.0
metadata:
  summary: Decides whether a repository should adopt Bazel from signals you measure, then migrates it in an order that keeps the tree green
  keywords: bazel,adoption,migration,go-no-go,monorepo,bzlmod,module-bazel,lockfile,bazelversion,bazelrc,remote-cache,rbe,ci-lanes,gazelle,pilot,rules_rust,rules_python,rules_js,rules_ts,rules_cc
---

# bazel-adopt

Two questions, in order. Should this repository move to Bazel at all, and if
yes, in what order does it move without ever leaving the tree red.

The first question has no numeric answer. No published source gives a target
count, a repository count, a language count or a team size above which Bazel
becomes correct — verified end to end across the vendor blogs, the case
studies and the one peer-reviewed study, as of 2026-09-06. So the gate here is
a checklist of signals you **measure**, each with the command that produces it,
and a written decision that records the measured value beside each answer.

This skill assumes the `bazel-quality` rule set is installed. It cites rules by
ID and never restates their tables; the handful a migration violates on day one
are duplicated below as a short list, deliberately.

Contents: [Stop condition](#stop-condition) · [Before you start](#before-you-start) ·
[The procedure](#the-procedure) · [The migration plan file](#the-migration-plan-file) ·
[What agents get wrong](#what-agents-get-wrong) · [References](#references)

## Stop condition

Stop when the adoption decision file exists on disk with every signal filled in
with a measured value, and either

- the decision is **no-go**, and the file names which signal decided it and what
  would have to change; or
- the decision is **go**, and a migration plan file exists in the repository
  naming the order, the Bazel pin, the lock mechanism per language, and the
  first CI lane.

Then stop. Do not migrate a second subtree, do not add target selection, and do
not wire remote execution. Each of those is a later decision this procedure
deliberately leaves open.

A decision file with an unmeasured signal — a number recalled, estimated or
inferred from a badge — is not a decision. Delete the row and measure it.

## Before you start

**The evidence rule.** Every number in the decision file comes from a command
you ran in this repository or from a registry you fetched. Never from memory,
never from a blog post, never from a threshold you synthesised because adoption
advice usually has one. Before writing any number as a go/no-go criterion, name
the source and quote it; if you cannot, drop the number and keep the checklist
answer.

**Read the pinned binary, never a page.** `bazel help build --long` and
`bazel help startup_options` on the exact pinned version are the authority for
whether a flag exists and what it defaults to. `bazel help all --long` is not a
command. `bazel help package` is not a command either. Both were tried
(BZL-MOD-13, BZL-FLAG-11).

**The rules a migration violates first.** These are merge-blocking and land
before any of them can be reviewed away. Repeated here because the scoped rule
may not have fired yet on a repository with no Bazel files in it.

| # | Finding | Rule |
|---|---|---|
| 1 | `.bazelversion` missing, or holding a channel keyword (`latest`, `rolling`, `last_green`) or a floating pattern (`8.x`, `9.*`) — the Bazel 8 *line* is a legitimate pin, `8.x` as a string is not | BZL-FLAG-01 |
| 2 | `MODULE.bazel.lock` gitignored instead of committed | BZL-MOD-01 |
| 3 | A `.bazelversion` bump commit that does not carry the regenerated lock | BZL-MOD-06 |
| 4 | A worktree or legacy build directory absent from `.bazelignore` — Bazel does not read `.gitignore` | BZL-ARCH-26 |
| 5 | The Bazel-side lockfile hand-edited to fix a drift | BZL-ARCH-24 |
| 6 | A pull-request lane holding the cache write credential | BZL-CACHE-01 |
| 7 | `--credential_helper` configured from a committed `.bazelrc` | BZL-CACHE-04 |
| 8 | Any `WORKSPACE*` file on a 9.x pin — a hard break, not a warning | BZL-FLAG-12 |

**Scope note that applies to every grep in this skill.** They read committed
workspace text only. BUILD and `.bzl` content a repository rule generates into
an external repository at fetch time is out of reach of all of them; only a
build from the generated repository sees that text.

## The procedure

Run the steps in order. Stop at the first one that fails and record why.

### 1. Name the narrower fix and why it was rejected

Every language has a cheaper answer that is already available: cargo's own
incremental build plus `sccache` and `cargo nextest` for Rust, `uv`'s resolver
and cache for Python, pnpm's content-addressable store plus Nx or Turborepo for
TypeScript. **No source states a point at which any of them stops being enough**,
and for two of the three shapes the sourced argument runs the other way — a
Python-heavy monorepo is steered toward Pants before Bazel, and the comparison
consensus holds Nx and Turborepo sufficient for a JS-only team permanently.

```sh
grep -rln 'actions/cache\|rust-cache\|sccache\|cache:' .github/workflows/
```

A non-empty list means some caching is already in place and you can state what
it does not cover. **Empty output means no cheaper fix has been tried at all** —
that is a no-go until one has been, because Bazel would be the first cache this
repository ever had and a far more expensive one.

Depth, including the per-language ladder and the reverse evidence (teams that
left Bazel for a narrower tool with no defect to point at):
[references/go-no-go.md](references/go-no-go.md).

### 2. Measure the signals

Five signals, each with its command, in
[references/go-no-go.md](references/go-no-go.md). Two that decide most cases:

**Whole-repo CI wall-clock, median over the last 50 runs of the gating
workflow.** This is the lead signal, because it is measurable before the
repository is in Bazel at all and it is the thing that actually costs.

```sh
gh run list --workflow=<gating-workflow> --limit 50 \
  --json startedAt,updatedAt,conclusion --jq \
  '[.[]|select(.conclusion=="success")|((.updatedAt|fromdate)-(.startedAt|fromdate))]|sort|.[length/2|floor]'
```

The printed integer is the median in seconds. **Empty output or a null means the
workflow has no successful history**, so the lead signal does not exist and the
gate cannot pass yet — collect runs first.

**Generator maturity for every language in the repository.** Granularity is
downstream of this, not an independent choice (BZL-ARCH-11).

```sh
curl -s https://bcr.bazel.build/modules/<module>/metadata.json | jq -r '.versions[-1]'
```

Run it for the module you will actually depend on, never a more-mature sibling.
**A 404 or empty output means the module does not exist under that name** — a
recalled or invented name, not a missing plugin. A `0.y.z` answer means pre-1.0:
compare the *second* component when judging staleness (BZL-FLAG-29).

### 3. Write the go/no-go decision

Fill the decision file (schema in
[references/go-no-go.md](references/go-no-go.md)) with one row per signal, each
carrying its measured value and the command that produced it. Then write the
verdict as a sentence naming which signal decided it.

```sh
grep -nE '^\| [^|]+ \| *\|' <decision-file>
```

**Empty output = every row carries a value = the decision is complete.** Any
line printed is a row with an unmeasured signal, and the decision is not yet
writable.

On a no-go, stop here. The file is the deliverable.

### 4. Choose the pilot

The pilot is the smallest subtree that produces something shippable, in one
language, with no compile-time dependency on a live service. Not the subtree
with the worst pain — that one is usually the one with the non-hermetic step,
and fixing hermeticity first is the sequencing every case study that succeeded
actually followed.

```sh
git grep -lE 'sqlx::query!|DATABASE_URL|localstack|testcontainers' -- \
  ':!*.md' ':!*.lock'
```

Any hit names a candidate that needs its offline cache or its container
converted into a cacheable target **before** it is a pilot. **Empty output = no
compile-time service dependency found in tracked source = any subtree is
eligible**, and you pick on size.

### 5. Pin the Bazel version, and decide 8.x against 9.x

Write an exact three-component semver, never a channel keyword and never a
floating `X.x` (BZL-FLAG-01).

```sh
cat .bazelversion
```

The output must match `^[0-9]+\.[0-9]+\.[0-9]+$`. **A missing file is equally a
finding** — the launcher then resolves "the latest official release", unpinned
by another route.

As of 2026-09-06: **9.2.0 is Active LTS** (9.0.0 shipped 2026-01-20), **8.8.0 is
Maintenance**. A new adoption starts on the Active LTS unless a consumer pins the
Maintenance major. Re-derive both stages from
[bazel.build/release](https://bazel.build/release) every time either is cited —
the table shifts on the Active-track minor cadence (BZL-FLAG-04).

What 9.x costs and what it removes from your options, with the commands that
prove each, is [references/workspace-setup.md](references/workspace-setup.md).
The headline: on 9.x all WORKSPACE logic is deleted, autoload is empty so every
`py_*`, `sh_*`, `cc_*` and `proto_library` symbol needs an explicit `load()`, and
`bazel sync` and `bazel analyze-profile` are gone.

### 6. Create MODULE.bazel and put the lockfile gate in on day one

Retrofitting a freshness gate onto a lock that has been drifting for months is a
different, worse job than starting with one.

```sh
bazel mod deps && git ls-files MODULE.bazel.lock
```

The command resolves the graph and writes the lock. **Empty output from
`git ls-files` = the lock is untracked = finding** (BZL-MOD-01); commit it in the
same change that adds `MODULE.bazel`.

Then wire `--lockfile_mode=error` on a dedicated CI leg and nowhere else, plus a
scheduled leg on `refresh` (BZL-MOD-02). Gate both on the **exit code**, never on
a matched stderr string: a stale lock caused by an existing dependency's version
changing exits 37 with an internal `IllegalStateException`, while a lock stale
because `MODULE.bazel` gained a new `bazel_dep` exits 37 with the clean
documented message. Same code, two texts.

Never hardcode the lock's schema version: it reads 24 on 8.7.0 and 28 on 8.8.0
and 9.2.0 alike, so a Maintenance patch bump rewrites the file (BZL-MOD-05,
BZL-MOD-06). Full setup, including the merge driver that stops a line-based git
merge from silently corrupting the digests, is in
[references/workspace-setup.md](references/workspace-setup.md).

### 7. Layer the rc files

One committed `.bazelrc`, and a `try-import` of a gitignored personal file as the
**last** non-comment line — import position is part of the precedence contract,
so a personal-override import placed anywhere else is silently defeated
(BZL-FLAG-19).

```sh
grep -vE '^\s*(#|$)' .bazelrc | tail -1
```

The printed line must be the `try-import`. **Empty output means `.bazelrc` has no
non-comment content**, which is a fine starting state, not a pass.

Then verify every flag you wrote actually exists on the pin, including any copied
from a community always-on list — 6 of the 8 flags in the most widely copied such
list return zero hits today and hard-fail with `unrecognized option`
(BZL-FLAG-11).

```sh
bazel help build --long | grep -c -- '--<flag>' ; bazel help startup_options | grep -c -- '--<flag>'
```

**A zero from both is the stop signal**: the flag does not exist on this version.
A flag can live on only one of the two surfaces, so check both.

### 8. Wire the cache, in this order

Four stages, and the order is the point: each one is verifiable before the next
one adds a failure mode. Commands, the outage semantics that surprise people, and
the remote-execution readiness gate are in
[references/cache-and-ci.md](references/cache-and-ci.md).

1. **`--disk_cache`**, locally and on the runner. Set an explicit
   `--experimental_disk_cache_gc_max_size` or `_max_age`; both default to `"0"`,
   which is unbounded growth even on a version that supports the flags
   (BZL-CACHE-17).
2. **Remote cache, read-only, everywhere.** Give the URI an explicit
   `http://`/`https://` scheme — both remote flags default to `grpcs` when the
   URI carries none — measured on 8.7.0, 8.8.0 and 9.2.0 (2026-09-06);
   re-derive on your own pin per BZL-CACHE-23.
3. **Writes, on one lane untrusted content cannot trigger**, with the credential
   absent from every other lane's environment rather than merely unused
   (BZL-CACHE-01, BZL-CACHE-02), carried by `--credential_helper` from a file the
   repository does not ship (BZL-CACHE-03, BZL-CACHE-04).
4. **Remote execution last, and only after its readiness gate passes**
   (BZL-CACHE-07). A structural "no" gets written into the repository's own docs
   rather than left as tribal knowledge.

```sh
grep -n 'remote_cache=\|remote_executor=\|credential_helper' .bazelrc*
```

**Empty output = nothing remote is configured yet = stage 1 only**, which is the
correct state until stage 1 is measured to help.

### 9. Stand up the CI lanes

The minimum is a lint gate and a test matrix, both required checks (BZL-CI-05).
Whole-repo `bazel test //...` is the default and stays the default (BZL-CI-01).

```sh
grep -rn 'bazel\|bazelisk' .github/workflows/
```

At least one hit must sit in a `run:`/`script:` step of a lane that gates a merge.
**Empty output on a repository that believes it has adopted Bazel is the finding**
— of Bazel projects that configure a CI service, 31.23 percent never invoke Bazel
inside it ([arXiv:2405.00796](https://arxiv.org/abs/2405.00796), denominator
verified as the CI-adopting subset) (BZL-CI-04). Adoption that never reaches CI bought nothing.

Target selection is **not** part of an adoption. Defer it until the whole-repo
median wall-clock is consistently past ~40 minutes, treating ~300 rule targets as
the tripwire that says measure the wall-clock now. Both numbers are this
program's derived tripwire, not a published threshold — no source publishes one
(BZL-CI-01). Lane-by-lane detail: [references/cache-and-ci.md](references/cache-and-ci.md).

### 10. Run the language branch for the pilot

Each branch names the ruleset version, the toolchain pin, the dependency-lock
mechanism, the generator or its absence, and the first three rules to satisfy.

- Rust: [references/branch-rust.md](references/branch-rust.md) — including the
  `crates_vendor` and prost sub-branches.
- Python, TypeScript and C++:
  [references/branches-python-ts-cpp.md](references/branches-python-ts-cpp.md).

Read only the branch for the pilot's language. A second language is a second
migration, planned after the first is green.

### 11. Write the migration plan file

Schema below. The plan is a committed file, not a transcript: the next run has to
be able to diff against it.

## The migration plan file

One file at the repository root or beside the decision file. Fixed sections, in
this order.

```markdown
# Bazel migration plan

Decided: <ISO date> · Bazel pin: <exact semver> · LTS stage on that date: <Active|Maintenance>

### Order
1. <pilot subtree> — <language> — <what shippable artifact it produces>
2. <next subtree> — blocked on: <what>
...

### Pins
| Thing | Value | Verified by |
|---|---|---|
| Bazel | <x.y.z> | `cat .bazelversion` |
| <ruleset> | <version> | `curl https://bcr.bazel.build/modules/<m>/metadata.json` |
| <language toolchain> | <version> | <the explicit pin call> |

### Lock mechanism per language
| Language | Bazel-side lock | Regenerated by | Freshness gate |
|---|---|---|---|

### First CI lane
Name, trigger, exact command, and the exit code it gates on.

### Deferred, with the condition that reopens it
- Target selection — reopens when the whole-repo median passes ~40 min.
- Remote execution — reopens when the BZL-CACHE-07 gate passes.
- <language> — reopens when <condition>.
```

Verify it before calling the run finished:

```sh
grep -cE '^\| Bazel \||^### Order|^### Lock mechanism|^### First CI lane' <plan-file>
```

The count must be at least 4. **A lower count means a required section is missing
or renamed**, and the plan does not satisfy the stop condition.

## What agents get wrong

Ranked by how often it happens.

1. **Invents a numeric adoption threshold.** "Above 500 targets" or "past ten
   engineers" is the shape adoption advice takes in training data, and no such
   number exists in any source for this question. The check: quote the source
   verbatim or drop the number.
2. **Proposes `bazel mod graph` against a repository with no `MODULE.bazel`.**
   The query is fine; there is nothing to query. `find . -maxdepth 2 -iname
   'MODULE.bazel*'` returning empty means every `bazel mod` invocation is
   aspirational, never that the coupling is absent (BZL-ARCH-28).
3. **Starts with the remote cache.** It is stage 2 of 4, and the disk cache
   answers most of the question for a single-machine or single-runner setup at a
   fraction of the operational surface.
4. **Wires remote execution because the docs page said to.** The official CI page
   still teaches a `bazel-toolchains` WORKSPACE dependency and `rbe_autoconfig` —
   untypeable on a 9.x pin, where the WORKSPACE code is deleted (BZL-CI-20).
5. **Picks the painful subtree as the pilot.** It is usually the one with the
   non-hermetic compile step, and that has to be fixed before anything downstream
   of it can be trusted.
6. **Adds target selection during the adoption.** Both production tools document
   real false-negative classes; below the switch point the correctness risk is
   unpaid-for (BZL-CI-21, BZL-CI-22).
7. **Reads a green build as proof the generator is wired.** No Gazelle ecosystem
   ships the freshness gate — `bazel query 'kind("sh_test", //:gazelle*)'`
   returning empty on a repository that believes it has one is itself the finding
   (BZL-ARCH-12).
8. **Treats a cache outage as a build failure.** Measured on both majors: the
   cache-only shape logs a WARNING, builds locally and exits 0, and neither
   fallback flag changes it (BZL-CACHE-26). A lane that must fail on an outage
   needs a check outside Bazel.
9. **Copies a flag list from a blog.** Six of the eight in the most-copied list
   no longer exist (BZL-FLAG-11).
10. **Ports a `bun.lock` repository as-is.** There is no `bun_lock` attribute and
    no ingestion path; the package converts to pnpm first or it does not adopt
    (BZL-JS-01, BZL-JS-03).
11. **Bumps `.bazelversion` alone.** The lock is discarded and rewritten with
    nothing printed, including across a Maintenance patch bump (BZL-MOD-06).

## References

Read one level down, on demand. These files do not link each other.

| File | Read it when |
|---|---|
| [references/go-no-go.md](references/go-no-go.md) | Running steps 1-3. Holds all five signals with their commands, the narrower-fix ladder per language, the evidence that no threshold exists, and the decision-file schema |
| [references/workspace-setup.md](references/workspace-setup.md) | Running steps 5-7. Holds the 8.x-versus-9.x cost table, the MODULE.bazel and lockfile setup with its merge driver, rc-file layering and the flag-existence discipline |
| [references/cache-and-ci.md](references/cache-and-ci.md) | Running steps 8-9. Holds the four cache stages with their verifications, the remote-execution readiness gate, the CI lane taxonomy and why target selection is deferred |
| [references/branch-rust.md](references/branch-rust.md) | The pilot is Rust. Holds the ruleset and toolchain pins, the crate_universe lock gate, the crates_vendor and prost sub-branches, and the first three rules |
| [references/branches-python-ts-cpp.md](references/branches-python-ts-cpp.md) | The pilot is Python, TypeScript or C++. Holds each branch's ruleset version, toolchain pin, lock mechanism, generator verdict and first three rules |
