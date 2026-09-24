# ADR: Adopt Bazel 9 under Taskfile to cache test execution per crate

## Metadata

**Status:** Accepted (2026-09-21, meta-orchestrator under the owner's /goal; owner may revert) — the four-stage **scope** is owner-ratified (dossier G3 + the
`/goal`); the **go** is conditional on WP-0's decision file, which now has **one**
empty signal row left (the per-job decomposition of `verify-deep`'s measured 3407 s —
§ The go/no-go reading). Two decisions in this ADR were taken by the orchestration
chain, not by the owner, and are listed in § Orchestrator rulings pending owner
ratification. **This acceptance does not ratify them** — it was taken by the same
chain that took them, so it cannot. They stay pending until the owner reads that
section. The same caveat applies to this `Accepted` itself: `/hex-architect`'s own
contract is that an orchestrator never accepts its own design, and that rule is
deliberately overridden here under the autonomous `/goal`, which is why the status
line carries who accepted it and that it may be reverted.
**Date:** 2026-09-21
**Deciders:** Michael Herwig
**Blast radius:** cross-area, internal only — Rust/Python/TS build paths, CI lanes, Taskfile, telemetry, plus two sibling repos (rules_ocx, mirror-bazelbuild). No CLI, wire, or persisted-format change.
**Reversibility:** one-way (medium) — the Bazel artifacts are deletable and the nextest lane restorable; what does not revert cheaply is rules_ocx becoming a real consumer and the CI lane swap.
**Tier:** high (owner-set; dossier-ratified 2026-09-21). Cross-area breadth alone would signal xhigh — recorded, not silently upgraded; the fast path's compensating controls (claim diff, mandatory steelman, cross-model adversary) all ran.
**Domain Tags:** infrastructure | devops | build | observability
**Related:** `.agents/discussions/bazel-full-adoption.md` (ratified dossier), `.claude/artifacts/discover_bazel_full_adoption.md`, `.claude/artifacts/research_bazel_cache_trust_boundary.md`, `.claude/artifacts/research_bazel_toolchain_verification.md`, `.agents/research/bazel_*.md` (six 2026-09-20 lanes)
**Amends:** `.claude/artifacts/adr_crate_split_workspace.md` § Tech Strategy Alignment line 19 ("Bazel parked by the dossier") — reopened by this ADR
**Superseded By:** —

**Tech Strategy Alignment:**
- [x] Rust 2024 / Tokio, Bun for the website, uv + pytest for acceptance — every Golden Path in `.claude/rules/product-tech-strategy.md` is unchanged. Bazel does not replace a language toolchain; it schedules and caches invocations of the ones already chosen.
- [x] Deviation recorded: `product-tech-strategy.md` names no build-graph tool. Bazel is a net-new entry and spends one of `quality-core.md`'s three innovation tokens. § Considered Options carries the cost.

---

## Context

The dominant cost in this repository's inner and CI loop is test **execution**
time, not compile time. Compile time already has an answer: `sccache` in front
of the Garage S3 store at `sccache.ocx.sh`, wired into every Rust CI job
(`.github/workflows/verify-basic.yml:92-100`). Test execution has none — every
lane runs `cargo nextest run --workspace` in full, whatever the commit touched.

Two facts make that newly fixable.

**The crate split landed.** `b79abbe8` (2026-09-19, subject `refactor!: dissolve
ocx_lib into 17 responsibility-derived crates`) replaced one crate — 278,777 LOC at
the 2026-09-06 baseline, inherited from `adr_crate_split_workspace.md:25` and stale
by +25.1 % since (`discover_crate_split_file_map.md:940`) — with 20 workspace
members whose allowed edges are a declared table
(`scripts/crate_map.toml`, enforced by
`crates/ocx_test_support/tests/workspace_structure.rs::deps_direction`). Before
that commit there was no per-crate granularity to cache: a change anywhere in
`ocx_lib` invalidated everything downstream of `ocx_lib`, which was everything.

**The commit shape is measured, partially.** Of the last 300 non-merge commits
on `origin/main` since 2026-07-01, **128 (43 %) touch no Rust at all** — those
run the entire nextest suite today and would be full cache hits under a
per-crate graph. The other 172 touch Rust with a median of one crate, but that
crate was the pre-split monolith in 139 of them, so the post-split per-crate
payoff is **not** measured by that number. The post-split sample is 15 commits,
2 touching Rust — which the dossier itself calls "too small". The lead signal
`bazel-adopt` step 2 names — median wall-clock over the last 50 runs of the
gating workflow — **is still unmeasured**.

A remote cache already exists and is unused: `bazel-cache.ocx.sh` on hetzner1
runs `buchgr/bazel-remote-cache:v2.6.2`, HTTP only, 50 GB cap, nginx htpasswd on
writes, Prometheus already scraping it and Grafana already provisioning
`cache.json` / `test-time.json`. This is owner-attested and **not independently
verified from this repository** (see § Evidence and attestation status) — and the
entire cache-reuse decision rests on it.

**The read side is contradicted, and this ADR does not pick a side.** The
ratified dossier states anonymous reads
(`.agents/discussions/bazel-full-adoption.md:181`: "a PR lane with no secret still
hits the cache (anonymous read)"). The **immediately preceding decision in the same
series**, dated one day earlier and describing work *landed* on the server, says the
opposite — `/home/mherwig/dev/ocx-evelynn/.agents/discussions/bazel-adoption-timing.md:50`,
under the heading `## Landed on the server (2026-09-20)` naming commits `c0504b1`
and `f0b5446`, reads verbatim:

> Anonymous 403; **bazel-cache reads now 401.**

`.agents/memory/hex.md` corroborates it independently — in the row beginning
"**Discussion handed off (hex-discuss, 2026-09-20):
`.agents/discussions/bazel-adoption-timing.md`**", which reads "server half landed
(sccache.ocx.sh → Garage, **bazel-cache reads closed**, herwig-systems/server-hetzner1
#1–#3)". Cited by phrase rather than by line: `hex.md` is appended to at the top, so
a line number into it goes stale on the next entry and a stale citation is
indistinguishable from a wrong one.

Two sources against one, the two being the later ones — **and measurement has since
settled it in their favour.** See the next paragraph: reads are 401, and this ADR
asserts anonymous reads nowhere.

**Settled by measurement, 2026-09-21 — the timing dossier is right and the ratified
one is wrong.** Read directly off hetzner1,
`/srv/sh.ocx/nginx/config/main/conf.d/ocx-sh-10-bazel-cache.conf:29-33`: the
`location /` block carries `auth_basic` plus
`auth_basic_user_file /data/auth/bazel-cache.htpasswd` and **no `limit_except`**.
`limit_except` is the only directive that would exempt a method from that realm, so
**every method including `GET` returns 401 to an unauthenticated client.** Anonymous
reads do not work.

**Where the wrong premise came from, because it matters more than the premise.**
`/srv/sh.ocx/bazel-cache/README.md:7-8` still says "reads are anonymous". That line
is stale against the vhost beside it, and **it is what the ratified dossier's design
rested on**. This is the second time in this initiative that a ratified decision
traced to a stale record rather than to the artefact it described — the first being
`hex.md`'s pre-`test:parallel` median (§ The go/no-go reading). The general lesson,
recorded because it will recur: *a README describing a config is not the config*, and
a decision that rests on one has an unverified premise regardless of how ratified the
decision is.

**The consequence, and the branch taken.** Because reads are 401, **fork-PR lanes get
zero cache reach**: GitHub withholds secrets from fork PRs, so such a lane can present
no credential and every action executes locally against `--disk_cache` only. Branch
**(a)** is taken — a **read-only** credential for same-repo lanes and the local loop,
forks accepting disk-cache-only. This is an **orchestrator ruling pending owner
ratification**; its mechanism is § Cache staging ruling 5b, its fallback is branch
(b) if the owner rejects the server change, and both are recorded in § Orchestrator
rulings pending owner ratification.

Criterion 2 of the trade-off matrix (whole-suite skip on the 43 %) keeps its CI half
for same-repo lanes and loses it for forks. No score moves: this repository's PRs are
same-repo (§ Cache staging ruling 5), so the lanes that carry the measured workload
are the ones that keep the reach.

The repository carries the `bazel-essentials` grimoire bundle
(`grimoire.toml:6` → `ghcr.io/ocx-sh/lore/bazel-essentials:latest`): the
`bazel-quality` rule set with eleven depth files, plus the `bazel-adopt` and
`bazel-diagnose` skills. It carries no Bazel file of any kind — no
`.bazelversion`, `MODULE.bazel`, `BUILD.bazel`, `.bazelrc` or `*.bzl` anywhere.

## Decision Drivers

1. **Per-crate skip is the stated requirement**, not whole-suite skip: "rebuild
   only when a crate or a crate it uses changes". This is the requirement that
   discriminates between the options; a whole-suite fingerprint does not satisfy it.
2. **Test wall-clock, CI and local** — the owner-stated pain.
3. **Correctness of a shared cache.** A wrong entry is invisible and reaches
   every reader (`bazel-quality/caching.md` BZL-CACHE-34, research artifact 3 § 2).
4. **One entry point.** `task verify` stays the gate; a contributor who never
   types `bazel` is unaffected.
5. **rules_ocx gets a real consumer.** The owner's mandate: every gap this
   exposes is fixed in rules_ocx or in a mirror package, never worked around here.
6. **Reversibility while the evidence is thin.** The measured basis for reopening
   the 2026-09-20 **deferral** — research 5 records a sequencing decision, not a
   no-go — is one number (43 %); the rest is unmeasured, and the number the
   predecessor's own step (3) asks for has not been taken.

### Non-negotiables

- No CLI, wire-format, `ocx.lock`, index-format or metadata change. `ocx` is the
  artifact under test, never the `ocx` that provisions the toolchain.
- No remote execution. No new service. No BuildBuddy. sccache stays.
- No secret reachable from a lane an untrusted contributor can trigger
  (BZL-CACHE-01, MUST).
- Every gate this ADR creates has a demonstrable **red** state
  (`quality-core.md` § Unchecked Green; BZL-CORE-02, MUST).

## Relationship to `adr_crate_split_workspace.md`

`adr_crate_split_workspace.md:19` read, **before this ADR's amendment**, verbatim
and verified (the live line now carries the amendment this ADR wrote):

```
- [x] No new build system (Bazel parked by the dossier), no new dependency introduced by this ADR
```

That park was decided on 2026-09-06, when `crates/ocx_lib` was one crate with 63
reciprocated module cycles (`adr_crate_split_workspace.md:28`, inherited). Under that shape there was nothing for
a build graph to skip: the unit of invalidation was the whole library, so a
per-target cache and a whole-suite cache were the same cache. The crate split
that ADR delivered is exactly the precondition that reopens it — it is the
change that created the granularity this ADR proposes to cache. That, plus test
wall-clock becoming the dominant pain, is what changed.

**The crate-split ADR is Accepted and stands in full.** Only its Bazel clause is
reopened — the ADR is **amended, not superseded**, because every other decision it
took (the crate map, the satellite linking rule, the verification tiers) is still in
force and is in fact the precondition this ADR builds on. Its `**Superseded By:**`
field is therefore left at `—`, and the pointer is an `Amends:` field here plus an
in-place amendment on its line 19: the form `arch-principles.md` already uses
("**Amended 2026-09-10 by `adr_self_update_handoff.md`**" at `:133`, and
"**Amended 2026-09-04 by `adr_toolchain_activation.md`**" at `:139`). The claim
diff's recommendation to set `Superseded By:` (`discover_bazel_full_adoption.md:17`,
claim 3c) is overridden here, with that reason.

The only other Bazel mention in that ADR (line 113, citing bazel-diff and
turborepo #9234 as prior art against predictive test selection) is untouched and
remains correct — this ADR also defers target selection (BZL-CI-01).

## Industry Context & Research

Four artifacts, in precedence order. Where the 2026-09-20 lanes and the
2026-09-21 verification research disagree, **the verification research wins**;
where the dossier and the claim diff disagree, **the claim diff wins**.

**Citation scheme, defined once.** The numbering in the left column below is the
only one this ADR uses; "research 3 § 2" means artifact 3 in this table. The
`**Related:**` field's ordering is bibliographic and carries no numbers.

| # | Artifact | Axis | Precedence |
|---|---|---|---|
| **1** | `.agents/discussions/bazel-full-adoption.md` | The ratified dossier | The scope this ADR implements |
| **2** | [`discover_bazel_full_adoption.md`](./discover_bazel_full_adoption.md) | Claim diff: dossier vs live tree, 17 claims + 5-item sweep | Corrects the dossier |
| **3** | [`research_bazel_cache_trust_boundary.md`](./research_bazel_cache_trust_boundary.md) | Security & compliance, 2026-09-21 | Supersedes the 09-20 lanes |
| **4** | [`research_bazel_toolchain_verification.md`](./research_bazel_toolchain_verification.md) | Technology/tooling verification, 2026-09-21 | Supersedes the 09-20 lanes |
| **5** | `/home/mherwig/dev/ocx-evelynn/.agents/discussions/bazel-adoption-timing.md` | The predecessor decision, 2026-09-20: the measured CI cost and the agreed order | The record this ADR reopens; **wins on any measured number the dossier restates** |
| — | `.agents/research/bazel_*.md` (six lanes, 2026-09-20) | Codebase recon, prior art, patched deps, Bun/vitepress, PTY, BES/OTel | Context only |

Artifact 5 is the fifth source, and two numbers this ADR carries come from **neither**
of the four the previous draft named: the 278,777 LOC and the 63 reciprocated module
cycles are inherited from `adr_crate_split_workspace.md:25` and `:28`, and the LOC
figure is a **2026-09-06 baseline** the repo's own
`discover_crate_split_file_map.md:940` has since restated as 348,648 (+25.1 %). Both
are cited here as inherited, not as measured by this ADR, and neither is load-bearing
for any decision it takes.

Primary sources those artifacts name and this ADR relies on:
Bazel [9.0.0 release notes](https://github.com/bazelbuild/bazel/releases/tag/9.0.0)
(the four behaviour flips); `bazel.build/reference/be/common-definitions` (the
tag semantics); `bazel.build/remote/caching` ("You may want only your CI system
to be able to write"); [bazelbuild/bazel#4276](https://github.com/bazelbuild/bazel/issues/4276)
(a flaky *trusted* CI instance poisoned a shared cache — no adversary required);
[bazelbuild/bazel#29114](https://github.com/bazelbuild/bazel/issues/29114)
(closed 2026-09-15); [rules_rust#3732](https://github.com/bazelbuild/rules_rust/issues/3732),
[#3807](https://github.com/bazelbuild/rules_rust/issues/3807),
[#2753](https://github.com/bazelbuild/rules_rust/issues/2753);
[Calsign/gazelle_rust](https://github.com/Calsign/gazelle_rust) issues #5, #29, #15;
[rules_js#1258](https://github.com/aspect-build/rules_js/issues/1258) (open, `need: funding`,
last touched 2025-08-23); [bazel#5373](https://github.com/bazelbuild/bazel/issues/5373)
(PTY in `linux-sandbox`, closed `not_planned`);
[buchgr/bazel-remote#468](https://github.com/buchgr/bazel-remote/issues/468);
bazelisk's own README on `.bazelversion` resolution; GitHub's
`pull_request` vs `pull_request_target` secret-exposure documentation;
`bazelbuild/proposals` 2022-06-07 credential-helper design;
Stripe's "Fast builds, secure builds. Choose two."

**Key insight:** the cheap answer (sccache, `cargo nextest -p`, a Taskfile
`sources:` fingerprint) can skip *everything* when nothing Rust changed, and
cannot skip *one crate's dependents* when something did. The 43 % figure is the
first case; the crate split created the second, and only a build graph addresses it.

## The go/no-go reading — applied, not sequenced

`bazel-adopt/references/go-no-go.md` is a gate, not a work-package template, and this
ADR applies it here rather than deferring the whole of it to WP-0. Its
§ "Reading the answers" (`:155-170`) gives three decidable readings, verbatim:

> - **No-go, and stop.** No cheaper fix has been tried; or the largest CI job is not
>   a build job; or no successful CI history exists to measure.
> - **No-go for now, with a named condition.** A single-language repository whose
>   own ecosystem tool is argued sufficient, and whose wall-clock is not the
>   complaint. Record what would reopen it.
> - **Go, with the pilot named.** Multi-language, the build is the measured cost,
>   the cheaper fix is in place and named as insufficient, and at least one language
>   has a production generator or is small enough to hand-write.
>
> Anything else is a decision the owner takes, not one this procedure takes. Say so
> in the file rather than inventing a tie-break.

**None of the three fires. This lands on the fourth line.**

- **Reading 1 does not fire.** Its clause is "no cheaper fix has been **tried**", and
  one was tried and has been landing since 2026-09-20 (research 5). `verify-deep.yml`
  now runs `task test:parallel`; commit `91dea8ac` removed the four 30 s timeouts.
  The narrower fix is **mid-flight and materially complete on its largest item** —
  which is a different state from untried, and from exhausted. Its other two clauses:
  the largest CI job is **not** established as a build job (see reading 3), and
  successful CI history exists in quantity.
- **Reading 2 does not fire.** This is not a single-language repository — Rust,
  Python and TypeScript all carry CI cost — and wall-clock **is** the complaint.
- **Reading 3 is still not earned — but on narrower, better-evidenced ground than
  the draft before this one, because the median has now been measured.** Take its
  four clauses in turn:

  1. *Multi-language* — **satisfied.** Rust, Python and TypeScript all carry CI cost.
  2. *At least one language has a production generator or is small enough to
     hand-write* — **satisfied.** 23 BUILD files is hand-writable, and
     `go-no-go.md:95` rates the Rust generator Experimental, which makes
     hand-written the budgeted path rather than a failure (§ BUILD generation).
  3. *The cheaper fix is in place and named as insufficient* — **now evidenced
     rather than asserted.** Measured 2026-09-21 over the last 50 successful runs
     (`gh run list`): **verify-basic 1731 s, verify-deep 3407 s**. The predecessor
     recorded 3407 s's counterpart as **3347 s** *before* `task test:parallel`
     landed on `verify-deep`. **The honest claim is that the narrower fix shows no
     demonstrated movement in the median** — not that it made things worse. The two
     figures come from different measurement windows over a moving commit stream, so
     a 60 s delta is noise, not a regression, and the controlled before/after on one
     commit is still owed (WP-1c, and it runs **first**). What can be said without
     hedging: the largest item of the narrower fix landed and the median did not
     visibly fall.
  4. *The build is the measured cost* — **not established, and this is the clause
     that decides it.** 3407 s is a **total**, not a decomposition. The only
     decomposition anyone has measured is the predecessor's
     (research 5 `:15`, verbatim):

     > CI cost is ~half compilation, ~half non-compile: acceptance runs serial
     > (`task test`, 22 min; `task test:parallel` exists), schema-generate recompiles
     > without `--target=` (5.8 min), one 210 s unit test + four 30 s timeouts.

     ~half non-compile, with the acceptance suite the largest single chunk — and
     stage 4 was `local` + `external`-tagged with **no caching at all** (§ Stage 4;
     amended 2026-09-22 to `no-sandbox` + `exclusive` with results cached;
     `exclusive` superseded by `adr_test_speed_tiers.md` AM-9 — targets now run concurrently).
     So the one decomposition on record points at the half this design explicitly
     does not address, and a larger total does not convert into "the build is the
     cost" without one.

  **A total is not a decomposition.** Having the median makes reading 3 *closer*,
  and makes the remaining gap a single, cheap, named measurement instead of an open
  question — which is strictly better than the previous state, and still not a `go`
  this procedure can write.

**So the disposition is the table's own fourth line: "Anything else is a decision
the owner takes, not one this procedure takes."** The owner took it — by ratifying
the dossier on 2026-09-21 and setting the autonomous `/goal` whose definition of done
is G3, all four stages (research 1 `:203-205`). Per the table's own instruction, that
is **recorded here rather than tie-broken**: this ADR invents no rubric, adjusts no
weight, and does not represent the owner's scope decision as a procedure output.

What the procedure still binds is the *conditionality*, and the bind has narrowed to
**one row**. `go-no-go.md:210-211` states the check — "**Empty output = every signal
row carries a measured value.** Any line printed is a row with an empty cell, and the
verdict is not writable yet." Printed below with its remaining empty cell so the gap
is the reader's first impression, not a footnote.

**The one remaining measurement that would move the reading** is the per-job
decomposition of `verify-deep`'s 3407 s: which job is the largest, and how much of it
is compile. WP-0's decision file carries it as a required row, and it is the same
`gh run list` that produced the median plus per-job durations. If compile dominates,
reading 3 fires and this becomes a procedure `go`; if acceptance still dominates —
which the only prior decomposition says — it stays the owner's decision and the
honest framing is that Bazel buys the smaller half.

### The decision file, as far as it can be filled today (WP-0 fills the rest)

| Signal | Measured value | Command | Answer |
|---|---|---|---|
| Cheaper fix tried and named | `test:parallel` on `verify-deep`; `91dea8ac` (30 s timeouts); `CARGO_BUILD_TARGET` on schema-generate; sccache → Garage | research 5 `:57` | **yes — landed, never re-measured** |
| Median whole-repo CI wall-clock | **verify-basic 1731 s, verify-deep 3407 s** (last 50 successful runs, 2026-09-21) | `gh run list --workflow=<wf>.yml --limit 50 --json conclusion,startedAt,updatedAt` | **measured** — supersedes the 3347 s in `hex.md`, which predates `task test:parallel` on `verify-deep` |
| Largest CI job is a build job | | the same run list, broken down per job | **not established** — 3407 s is a total, not a decomposition; the only decomposition on record (research 5 `:15`) is ~half non-compile with acceptance the largest chunk. **The one row still open.** |
| Generator maturity per language | Rust: `gazelle_rust` `0.1.0`, one tag, single maintainer | `go-no-go.md:95` | **Experimental** → "coarse, package-per-directory, hand-maintained BUILD files are the correct default" (`:98-100`) |
| Cross-repo coupling to model | 3 `[patch.crates-io]` git submodules under `external/`; `rules_ocx` **v0.4.0** @ `825f20b`, `.bazelversion` 8.7.0, `ocx.project()` already implemented in `//ocx:extensions.bzl` | `git config -f .gitmodules --get-regexp path`; `../rules_ocx/MODULE.bazel:10` | **yes, two kinds** — and the rules_ocx half is a **version bump, not a build** |
| Build owner after adoption | Michael Herwig (sole owner) | owner statement | **Michael Herwig** |
| Does anyone already read Starlark | **yes** — "Owner already runs Bazel elsewhere (`rules_ocx`, `mirror-bazelbuild`) — tool novelty is low" | research 5 `:14` | **yes** — `go-no-go.md:138`'s failure mode ("a migration whose only Starlark reader is an agent has no reviewer") does not obtain |
| **Anonymous cache read** (this ADR adds the row) | **401 on every method including GET** — `nginx/config/main/conf.d/ocx-sh-10-bazel-cache.conf:29-33` on hetzner1: `location /` carries `auth_basic` + `auth_basic_user_file /data/auth/bazel-cache.htpasswd` with **no `limit_except`** | server-side config read, 2026-09-21 | **measured** — the dossier's "anonymous reads" is false; see § Orchestrator rulings R1 |

**Run the quoted check over that table and it now prints exactly one row** —
`Largest CI job is a build job`, whose Measured value cell is empty. Three rows that
printed in earlier drafts have since been filled by measurement: the median
(1731 s / 3407 s), the anonymous read (401), and `Build owner after adoption` (whose
**Answer** cell was empty while it carried a value and a source — the kind of gap the
mechanical check catches and prose does not).

The count above is the check's, not a reading of the table: citing a mechanical check
and then reporting a number from eyeballing is the shape this ADR polices elsewhere,
and it has already gone wrong twice in this document's history.

**One empty cell means the verdict is one `gh run list --json jobs` away** — and per
D1 above, that measurement is also the only one that could still move the go/no-go
reading off the fallthrough.

**Signal 3's reading matters most and is the one most
likely to return the unwelcome answer** — if acceptance is still the largest job
post-`test:parallel`, the per-crate skip buys the smaller half.

**Generator maturity, stated as the verdict rather than argued around it.** The
skill rates the Rust generator **Experimental**, and its consequence is explicit at
`go-no-go.md:98-100`: "Anything below [Production] means **coarse,
package-per-directory, hand-maintained BUILD files are the correct default** —
hand-fine-graining buys the maintenance cost and none of the tooling." This ADR's
target shape *is* package-per-directory, so the design is compatible with the
verdict — but the consequence a planner must carry is that **hand-written BUILD
files are the expected path and `gazelle_rust` is the optimisation**, not the other
way round (§ BUILD generation was previously written with that polarity inverted).

## Considered Options

### Option A — Full adoption, four stages, as the dossier proposes

Rust unit tests, the CI lane swap with BEP-derived floor/ceiling, the cast +
website chain, and the acceptance suite, all under Bazel; Bazel under Taskfile.

| Pros | Cons |
|---|---|
| Per-crate skip on the 57 % of commits that touch Rust | Four rule surfaces to own, one of them hand-written Starlark |
| Whole-suite skip on the 43 % that do not | The website rule is the only sandboxed cacheable stage — the one place an undeclared-input bug becomes a cross-machine-wrong cache entry |
| One dependency graph for schemas → casts → SBOM → site, already a 5-stage DAG in `website/taskfile.yml:45-63` | `BUILD.bazel` becomes a second source of truth beside `Cargo.toml`; needs a drift gate |
| rules_ocx gets the consumer that finds its gaps | Casts are `local`-tagged and acceptance is `local` + `external`, so neither gets **any** shared-cache reach — the casts' payoff is local input-hash skipping their Taskfile already provides, and acceptance's is selection with no caching at all |

### Option B — Bazel for the Rust unit-test stage only

Stages 1 and 2. Casts, website and acceptance stay on Taskfile.

| Pros | Cons |
|---|---|
| The whole measured case (43 % + per-crate skip) lands here | The website chain keeps re-running on unrelated edits unless Taskfile `sources:` guards are tightened |
| Only the most hermetic stage touches the shared cache | rules_ocx is exercised for rustc/cargo only — a thin dogfood |
| Friction B (BZL-JS-01/03) disappears entirely — no JS under Bazel | `agg`/Bun/uv never route through `@tools//`, so the tool-path mandate is unmet |
| Smallest maintenance surface of any go option | Leaves the owner's stated four-stage goal unmet |

### Option C — No Bazel: finish the predecessor's measured ladder

**C is not "a `sources:` guard".** The previous draft of this ADR said the cheapest
whole-suite skip "has never been tried" and priced it at "~5 lines". Both were
wrong, and the file it cited by line range says so. `taskfiles/rust.taskfile.yml:228-234`,
verbatim:

> No `sources:`. The bracket around this run — `test:floor` deletes the log,
> `test:ceiling` reads it — needs the run to happen every time: a
> fingerprint-cached skip would leave the ceiling no log and red an unchanged
> tree, and a subset run (`-- <filter>`) would otherwise stamp the full run up
> to date.

The absence of `sources:` is a **documented decision with a stated mechanism**, not
an unexplored option. Adding it as written breaks the floor/ceiling bracket — so the
previous draft's declared fallback ("the initiative stops at Option C, and the
`sources:` guard is built instead") specified a change the repository has already
ruled unsound. A `sources:` guard is still reachable, but only by first moving the
bracket so floor and ceiling read a **persisted** artifact rather than a
freshly-tee'd log. The repository already writes one:
`taskfiles/rust.taskfile.yml:254` sets `JUNIT: '{{.ROOT_DIR}}/target/nextest/default/junit.xml'`,
pushed to `otel.ocx.sh` by `telemetry:push`, which counts `testcase` elements to do
it. That is real design work with its own review, not five lines.

**The real Option C is the predecessor's ladder, and it is mid-flight.** Research 5
`:17`, verbatim:

> Order agreed in principle: (1) no-infra CI fixes, (2) sccache → existing cache
> server, local + CI, (3) **re-measure, run bazel-adopt gate, Rust-only pilot if
> warranted**. Nothing in (2) is discarded by (3): same server.

That is a **sequencing** decision, not a no-go — this ADR previously cited it as "the
2026-09-20 no-go", and research 5 records no such verdict. Its step (3) is literally
"re-measure, **then** run the gate", which is the same action as `bazel-adopt` step 2's
lead signal. Its `:57` enumerates what was owed before the gate:

> Remaining (ocx repo, gated on drain): verify-deep `task test:parallel`; schema
> `--target=`; sccache-action + multilevel config in `build-rust` composite and
> smoke; 210 s / 30 s tests; **re-measure; bazel-adopt gate**.

Four of those five have been landing (`verify-deep.yml` runs `task test:parallel`;
`91dea8ac` removed the 30 s timeouts). **The fifth — re-measure — has not.** So the
honest statement of C is: *the narrower fix is materially complete on its largest
item and has never been re-measured*, and the measurement is step (3) of the plan
this ADR is reopening, not a Bazel work package.

C's remaining rungs, in the predecessor's order:

1. **Re-measure** the `verify-deep` median post-`test:parallel` and break it down by
   job — the row that decides whether the largest CI job is a build job at all.
2. `cargo nextest -p <crate>` — **already present** at `taskfile.yml:223` under
   `for: { var: CRATES }`, with the `ROUTE_MANIFESTS` route at `:206` and a per-crate
   doctest at `:225`. The local loop already has per-crate selection; what it lacks
   is per-crate *caching across machines*.
3. Move the floor/ceiling bracket onto `target/nextest/default/junit.xml`, then add
   the `sources:` guard the bracket currently forbids, with `.task/` persisted
   through `actions/cache`.

**Why it still loses — narrowed to what is actually true.** C's ceiling is
all-or-nothing *across machines*. Even with rung 3, a checksum over `crates/**` either
matches and skips the whole suite or does not and runs it; it cannot express
"`ocx_util` changed, so run `ocx_util` and its dependents and skip the other
fourteen". That is the literal stated requirement and what the crate split made
possible. C addresses the 43 % and none of the 57 %.

**What would falsify the choice.** If rung 1's measurement shows the median low
enough that per-crate skip saves single-digit minutes; or shows acceptance still the
largest job (which Bazel's `local`-tagged stage 4 does not cache at all); or if the
post-split hub-crate churn re-measurement at ~100 commits shows most Rust commits
touch `ocx_util`/`ocx_exit` — invalidating nearly the whole graph anyway — C wins on
every remaining criterion. The matrix below makes that branch numerate rather than
rhetorical. **This ADR's `go` is conditional on WP-0's decision file, which cannot be
written until the median is measured and the read probe has run, and which retains
the authority to return no-go** (§ The go/no-go reading; § Decision Outcome states
what "no-go" then means for the autonomous chain). C's rungs 1 and 3 are built
regardless — rung 1 *before* any go, as the predecessor's own step (3).

| Pros | Cons |
|---|---|
| Zero new tooling, zero new failure modes, total reversibility | Cannot express per-crate invalidation — the stated requirement |
| Rungs 1 and 2 are cheap and rung 1 is owed before any go under either option | Rung 3 is a bracket redesign, not a five-line guard — `rust.taskfile.yml:228-234` |
| No second source of truth | `.task/` fingerprints need `actions/cache` to survive a fresh runner — re-implementing a worse local cache |
| Addresses the acceptance half the predecessor measured as dominant | Leaves the website/cast chains exactly as they are |

### Option D — A different graph tool (Buck2, Nx, Turborepo, Pants, Moon)

Dispatched on evidence, not preference.

**Buck2 is the strongest member of the family and the previous draft omitted it.**
That omission inflated the winner, because Buck2 is the only non-Bazel candidate that
could contest criterion 1 — the weight-6 row that decides the matrix. It has a
first-class Rust story (the prelude's Rust rules; `reindeer` for Cargo ingestion;
Meta's own Rust monorepo as the production reference), and the prior research lane
research 5 `:9` names covers it by title:
`.agents/research/research_graph_tools_middle_ground.md` — "moon/**Buck2**/Pants/Nx
lane". It is dispatched, not dismissed, on three grounds that are specific to this
repository rather than to Buck2's quality:

1. **No BCR-equivalent module ecosystem.** Every dependency this ADR takes —
   `rules_rust` 0.74.0, `rules_shell`, `gazelle_rust`, `buildifier_prebuilt` — is a
   versioned BCR module with a lockfile (`MODULE.bazel.lock`) and a freshness gate
   (`bazel mod deps --lockfile_mode=error`). Buck2's prelude is vendored per-repo;
   the pinning and drift story would be hand-built here.
2. **A markedly smaller external-ruleset surface for Python and TS** — the two
   languages stages 3 and 4 touch. This is a four-stage polyglot scope; a Rust-only
   comparison would favour Buck2 more.
3. **No `rules_ocx` analogue.** The owner's tool-path mandate (driver 5) is
   implemented as a Bazel module extension that already exists in another repository.
   Under Buck2 that is a rewrite of the mandate's mechanism, not a port.

The rest: Nx and Turborepo are JS-first — neither models a Rust crate graph, and
`bazel-adopt` step 1's own sourced consensus holds them "sufficient for a JS-only
team permanently", so they solve the cheap part. Pants is steered toward
Python-heavy monorepos by the same source (`go-no-go.md:147`: "The comparison
cluster steers Python-heavy monorepos to **Pants before Bazel** for exactly this
shape"); its Rust support is not a first-class ruleset. Moon's task graph is
package-level and has no per-crate Rust test-target model — asserted here from the
prior lane, not from a primary source, and flagged as the weakest dismissal in this
option.

### Trade-off matrix

Weights reflect the decision drivers: per-crate skip is the requirement that
discriminates, and shared-cache correctness is the risk that cannot be walked back.

**Read this matrix with two caveats stated before the numbers, not after.**

1. **The heaviest row scores an unmeasured magnitude.** Criterion 1 is weight 6 —
   23 % of the scale — and § Context says its size is not known post-split (the
   sample is 15 commits, 2 touching Rust). It is therefore scored as a **range**,
   `5 (2–5)` for A and B, and the pessimistic collapse is computed below rather than
   left as prose. The range collapses when the hub-crate churn is re-measured at
   ~100 post-split commits.
2. **A criterion chosen *because* it discriminates, then weighted highest, records a
   requirement rather than testing it.** Driver 1 says so in as many words. That is a
   legitimate way to state a requirement; it is not evidence, and it is not treated
   as evidence here.

A **delivery-cost row** is added, because "maintenance surface" prices the steady
state and nothing priced the build-out — an omission that systematically favoured
A and B.

| Criterion | Weight | A (four stages) | B (Rust tests only) | C (narrower fix) | D (other graph tool) |
|---|---|---|---|---|---|
| Per-crate skip on the 57 % touching Rust | 6 | 5 *(2–5)* | 5 *(2–5)* | 1 | 1 |
| Whole-suite skip on the 43 % touching no Rust | 4 | 5 | 5 | 3 | 3 |
| Correctness risk introduced (5 = least) | 5 | 3 | 4 | 5 | 3 |
| Maintenance surface / second source of truth (5 = least) | 4 | 2 | 3 | 5 | 2 |
| **Delivery cost / time to first benefit (5 = cheapest)** | 3 | 1 | 2 | 4 | 1 |
| Reversibility | 3 | 3 | 4 | 5 | 3 |
| Vendored-ruleset compliance (named deviations) | 2 | 3 | 4 | 5 | 2 |
| Operational surface (5 = least) | 2 | 3 | 3 | 5 | 3 |
| **Weighted total (max 145)** | | **97** | **114** | **110** | **63** |
| **If criterion 1 collapses to 2** | | **79** | **96** | **110** | **63** |

Weights sum to 29; the optimistic totals are 97 / 114 / 110 / 63 and the pessimistic
ones subtract `6 × 3 = 18` from A and B only (C and D already score 1 there).
Delivery-cost scores: A is 23 Rust `BUILD.bazel` files (20 `crates/` + 3 `external/`)
carrying 57 targets, 34 of them tests (§ Stage 1), plus three more BUILD files for
website/casts/acceptance, four bespoke scripts, one
hand-written Starlark rule, a Bazel-9 bump in `rules_ocx` and a mirror package that
does not exist yet; B drops the Starlark rule, the `agg` mirror and (per § rules_ocx)
the cross-repo blocker; C is rung 1 (one command) plus rung 3 (a bracket redesign);
D pays A's cost with a hand-built module-pinning story on top.

**The matrix's verdict is B under both branches, and C overtakes A under the
pessimistic one.** That is stated, not weighted away.

### Decision Outcome

**Chosen option: A — four stages, sequenced as B, with stages 3 and 4 gated on
stage 2's measured result and each carrying an abort condition with a number.**

#### The disagreement, recorded rather than resolved by arithmetic

**The trade-off matrix favours B (114) over the chosen A (97), and C (110) over A
too; under the pessimistic branch of criterion 1 the order is C (110), B (96), A
(79).** The Round 1 review panel reached the same conclusion independently: its
quality seat recommended dropping stages 3 and 4 to a follow-on decision, which is
Option B, and named it the change that dissolves three of its other findings.

**A is chosen anyway, because the four-stage end state is an owner scope decision
taken outside the matrix** — dossier G3 and the autonomous `/goal` whose definition
of done is all four stages (research 1 `:203-205`). The `/goal` is explicit on the
point: it states **"No stage dropped"** and names *cutting a stage to save time* as a
divergence it does not accept. So dropping stages 3–4 to a follow-on — Option B, and
what the Round 1 quality panel recommended — is not a trade this ADR is free to make.
That is the fourth line of the go/no-go table in operation: a decision the owner
takes, not one this procedure takes.

**The matrix result stands as recorded dissent, not as a decision.** It is named as
such here, and **no criterion weight has been adjusted to make A win**. A
reverse-engineered weight would be worse than a stated disagreement, because it would
hide from the next reader that the analysis and the scope point different ways. If the
owner later reverses the scope, B is where the analysis already points and nothing
here needs re-deriving.

What follows from recording it honestly: **if the owner ever reopens the scope, the
matrix already says what to do** — drop to B, and C is the terminal state if
criterion 1 collapses. No re-analysis is needed.

#### What the sequencing actually buys

- **Stages 1 and 2 carry the evidence.** They are where the 43 % and the per-crate
  skip land, and they touch the most hermetic subtree.
- **The casts half of stage 3 and all of stage 4 are `local`-tagged**, which (per
  `bazel.build/reference/be/common-definitions`) already implies `no-remote-cache`:
  they get **no shared-cache reach at all**, and their payoff is local input-hash
  skipping, which `subsystem-taskfiles.md` § "Caching Contract" already provides
  through `sources:`/`status:` — and, for the casts specifically, **already provides
  today** (`website/recordings.taskfile.yml:52-69` declares `doc_scripts/**/*.sh` in
  its own `sources:`). Stage 4 additionally buys **selection, not caching**
  (§ Stage 4). These two are justified by graph unification and the rules_ocx
  dogfood mandate, not by cache economics, and this ADR says so rather than implying
  a cache win the tags forbid.
- **The website rule is the one sandboxed, shared-cache-eligible target in the whole
  design**, and it carries the hermeticity gate as a **landing precondition**
  (§ Cache staging ruling 2). A planner must not read the `local` sentence above as
  covering it — it does not.
- **Every stage carries an abort condition, and each one aborts** (§ Acceptance).
  An aborted stage leaves the previous stages intact; an aborted stage 1 leaves the
  repository exactly as it is today plus a deleted directory.
- **If WP-1c's measurement fails its threshold (§ Orchestrator rulings R2), the lane swap does
  not land** and the terminal state is B-without-the-swap or C. Option C's rung 1
  runs before any go; rung 3 is built regardless.

#### What "WP-0 returns no-go" means for the autonomous chain

The conditional-go is only load-bearing if the chain that executes this ADR can stop
on it. The `/goal`'s completion criterion is G3 — all four stages green — which a
WP-0 no-go structurally cannot satisfy. **So this ADR states the terminus
explicitly: "WP-0 returned no-go; Option C's rungs built; initiative closed" is a
*successful* end state of the chain, not a failure to reach G3.** A chain that
cannot report that outcome has a decorative gate, and `/hex-plan` must carry this
sentence into WP-0's exit criteria or the conditionality is not real.

## Technical Details

### C4 sketch

**Context.** Two actors: a developer at a workstation and a GitHub-hosted CI
runner. Both invoke `task`. `task` is the only entry point either one is
expected to type; it dispatches to `bazel`, `cargo`, `uv` and `bun`. Four
external systems: the OCI registries `ocx` pulls tools from, `bazel-cache.ocx.sh`
(remote cache, Hetzner), `otel.ocx.sh` (Tempo, gRPC OTLP, basic auth) and
`sccache.ocx.sh` (unchanged, cargo-only). `PROBE` in the box below is literal: the
read posture is the § Context contradiction, unresolved until WP-0 runs the `curl`.

**Container.**

```
  developer / CI runner
        │  task verify | task verify:scoped
        ▼
  ┌──────────────────────────────────────────────────────────────┐
  │ Taskfile (entry point, unchanged contract)                   │
  │  ├─ lint phase       → cargo fmt/clippy, shellcheck, lychee  │
  │  ├─ pin drift check  → bazel:pin:check                       │
  │  ├─ build+test phase → bazel test //crates/...   (stage 1/2) │
  │  │                     cargo test --doc          (unchanged) │
  │  │                     bazel build //website/... (stage 3)   │
  │  │                     bazel test //test/...     (stage 4)   │
  │  └─ telemetry        → bep_to_otlp  +  junit2otlp (unchanged)│
  └──────────────────────────────────────────────────────────────┘
        │                        │                       │
        ▼                        ▼                       ▼
  ┌───────────┐          ┌──────────────┐        ┌──────────────┐
  │ Bazel 9.2 │          │ bazel-cache  │        │ otel.ocx.sh  │
  │  (engine) │◄────────►│   .ocx.sh    │        │   (Tempo)    │
  └───────────┘  AC/CAS  │ read: PROBE  │        └──────────────┘
    │      │             │ write: main  │
    │      │             └──────────────┘
    │      └─ rules_rust 0.74.0 → rustc/cargo 1.95.0 toolchain   [stages 1-2]
    └───────── rules_ocx (git_override) → ocx.toml/ocx.lock
               → @tools//:{bun,uv,agg,lychee,…}                  [stage 3 onward]
```

**Component (inside the Bazel container).**

| Component | Responsibility |
|---|---|
| `MODULE.bazel` + `.lock` | Module graph; the single `ocx.project()` call; `rust.toolchain()`; the `git_override` on rules_ocx |
| `.bazelrc` (committed) | Cache URI, download mode, strict action env; `try-import %workspace%/.bazelrc.user` as the **last** non-comment line |
| `crates/*/BUILD.bazel` | One `rust_library` per member, one `rust_test` for its unit tests, one more per `tests/*.rs` (34 total); generated, drift-gated |
| `external/*/BUILD.bazel` | `rust_library` only — the three patched submodules are dependencies, not test subjects |
| `website/BUILD.bazel` + one coarse `.bzl` rule | The 5-stage site DAG as one sandboxed, network-permitted action |
| `test/doc_scripts/BUILD.bazel` | Cast actions, `local`-tagged |
| `test/BUILD.bazel` | Acceptance `sh_test`s, one per `test/tests/test_*.py`, `local` + `external`-tagged (`SCOPED_ROWS`, `test/taskfile.yml:371-414`, stays the *selection* query, not the unit) |
| `scripts/bep_to_otlp.py` | BEP + execution-log reader → OTLP spans |
| `scripts/bazel_pin_check.py` | `.bazelversion` ↔ resolved-binary drift gate |
| `scripts/bazel_build_drift.py` | Generated `BUILD.bazel` vs `cargo metadata` |

### The file set this ADR creates

| Path | Contract | Tracked |
|---|---|---|
| `.bazelversion` | Exactly `9.2.0`. Must match `^[0-9]+\.[0-9]+\.[0-9]+$` (BZL-FLAG-01, MUST). Not the pin authority — see below. | yes |
| `MODULE.bazel` | `bazel_dep` on `rules_rust` 0.74.0, `rules_shell`, `gazelle_rust` 0.1.0 (if WP-1a is green), `buildifier_prebuilt`; `rust.toolchain(versions = ["1.95.0"])`; `crate.from_cargo(... lockfile = ...)` with an **explicit** `lockfile =` (BZL-RUST-01, MUST); one `ocx.project(ocx_toml = "//:ocx.toml", ocx_lock = "//:ocx.lock")`; `git_override(module_name = "rules_ocx", remote = …, commit = <sha>)`. | yes |
| `MODULE.bazel.lock` | Committed; regenerated only by `bazel mod deps`, never hand-edited; a JSON-aware merge driver registered (BZL-MOD-02/03/04). | yes |
| `.bazelrc` | Cache URI with an explicit `https://` scheme; `--remote_download_minimal` on CI, `toplevel` default locally (BZL-CACHE-11) **with the BZL-CACHE-12 position beside it** (§ Cache staging ruling 10); `--remote_timeout` and `--remote_retries` set explicitly (ruling 11); `--remote_instance_name` carrying the generation salt (ruling 6b); **no credential of any kind, and no `--credential_helper` line at all** (BZL-CACHE-04, MUST); `try-import %workspace%/.bazelrc.user` as the last non-comment line (BZL-FLAG-19). | yes |
| `.bazelrc.user` | Local opt-in: `--disk_cache` and local flag overrides **only**. **No `--credential_helper` of any kind** — see ruling 4a. | **gitignored** |
| `$RUNNER_TEMP/bazel-cache.rc` | The **CI-only** rc file, written by a composite action on a trusted event, carrying the one `--credential_helper=bazel-cache.ocx.sh=…` line; consumed with `--bazelrc=$RUNNER_TEMP/bazel-cache.rc` on that lane's command line; deleted in an `if: always()` step. Never tracked, never `%workspace%`-relative. | **never written to the workspace** |
| `.bazelignore` | `.agents/`, `target/`, `.tmp/`, `external/rust-oci-client/target`, `external/docker_credential/target`, `external/sigstore-rs/target`, `.agents/worktrees`. See the semantics note below. | yes |
| `crates/<name>/BUILD.bazel` × 20 | One `rust_library`; one `rust_test` for the lib's unit tests; **one further `rust_test` per `crates/<name>/tests/*.rs`** (14 across five crates — § Stage 1). `deps` mirroring `Cargo.toml`. | yes |
| `crates/TEST_TARGET_MAP.toml` | The per-target expected-count table the floor reader floors on. § Stage 2. | yes |
| `external/<name>/BUILD.bazel` × 3 | `rust_library` only. | yes |
| `website/BUILD.bazel`, `website/site.bzl` | The coarse rule. Carries the BZL-JS-01/BZL-JS-03 exemption as a named comment citing this ADR. | yes |
| `test/doc_scripts/BUILD.bazel` | **39** cast actions, one per `# cast: true` script (granularity ruled below). | yes |
| `test/BUILD.bazel` | Acceptance `sh_test`s, one per `test/tests/test_*.py`. | yes |
| `scripts/bep_to_otlp.py` | § Observability. | yes |
| `scripts/bazel_pin_check.py` | § The pin authority. | yes |
| `scripts/bazel_build_drift.py` | § Stage 1. | yes |
| `scripts/bazel_tag_guard.py` | § Cache staging ruling 1a — the query gate behind the `local`/`no-sandbox` corollary and the `rust_binary` stamping rule. | yes |

### The file set this ADR edits

The dossier enumerated these (research 1 `:143-154`); the previous draft dropped the
list, leaving `/hex-plan` no work-package surface for them. One row per surface.

| Path | Contract of the edit |
|---|---|
| `ocx.toml` / `ocx.lock` | Add `bazel = "ocx.sh/bazelbuild/bazel:9.2.0"` (exact tag, not `:9` — § pin authority) and `agg = "ocx.sh/asciinema/agg:<v>"` once the mirror package exists. Relock. |
| `taskfile.yml` | `.verify:build-test` — four of fourteen steps swapped (§ The hook point). `verify:scoped` — a Bazel-native per-crate equivalent beside `:206`, `:223`, `:225` (§ The second call site). New `bazel:pin:check`, `bazel:build:drift`, `bazel:tag:guard` wired into `.verify:lint`. |
| `taskfiles/rust.taskfile.yml` | `test:floor` / `test:ceiling` re-derived against the Bazel reader; `test:unit` gains the Bazel arm. The `:228-234` "no `sources:`" comment is **preserved and re-pointed** at the new bracket, not deleted. |
| `.github/workflows/verify-basic.yml` | `smoke` job: the nextest sub-sequence swapped; an explicit job-level `permissions: { contents: read }`; the credential supplied only through a trusted-event-conditioned expression (§ Cache staging ruling 5). |
| `.github/workflows/verify-deep.yml` | Linux matrix leg: the nextest step swapped, plus the ungated report-only parity probe (§ Stage 2 ruling 6). `--remote_upload_local_results=false` and no credential on **every** trigger including `push` to `main`. No floor/ceiling introduced (ruling 2). |
| `.github/actions/` (bazel bootstrap) | Bazel arrives through `setup-ocx` + `ocx.lock`; **no new third-party action** is introduced, and every existing one stays SHA-pinned (`subsystem-ci.md:98`). The credential-helper rc writer is a composite action here — and the ref gate is **not** placed in it (§ Cache staging ruling 5a). |
| `website/taskfile.yml`, `website/recordings.taskfile.yml` | Call `bazel build //website:site` / the cast targets; their existing `sources:` lists stay as the non-Bazel path. |
| `.claude/rules.md` | The `bazel-quality.md` auto-load row is **widened to match that rule's own `paths:` frontmatter** — claim diff 14 (`discover_bazel_full_adoption.md:37`) found the catalog row narrower, verified: `.claude/rules/bazel-quality.md:18` carries `- "**/*.scl"` among globs the row omits. A `meta-ai-config.md`-flagged drift, and it starts mattering the moment Bazel files exist. |
| `CLAUDE.md` § Build & Development | Name `bazel` as a `task`-dispatched engine, not a command anyone types. |
| `.claude/rules/subsystem-ci.md` | The cache section: lane gating, the write predicate, the BEP→OTLP push beside junit2otlp. |
| `.claude/rules/subsystem-taskfiles.md` | The `.verify:build-test` step list and the caching contract's new Bazel neighbour. |
| **New** dev-setup docs page under `website/src/docs/` | Claim diff 17 (`discover_bazel_full_adoption.md:40`) confirmed **ABSENT** — "no `contributing.md`, no `contributing/` directory" — so this is a **new page**, not an edit to one. |

Not edited by this ADR, stated so a reviewer does not look for it: any file under
`.claude/rules/bazel-quality/**` (vendored — § The BZL-JS exemption), and
`CHANGELOG.md` (generated).

**`.bazelignore` semantics are not `.gitignore` semantics.** A slash-less
`.gitignore` pattern like `target/` matches a directory of that name at any
depth, which is why `.gitignore` needs no `external/*/target` entry.
`.bazelignore` lines are workspace-relative directory paths, not depth-matching
globs, so each `external/<crate>/target` must be listed explicitly. Do not
assume parity between the two files (BZL-ARCH-26: "Bazel does not read
`.gitignore`"). Verification of the pattern language itself is a WP-1 item —
write the entries, then confirm `bazel build --nobuild //...` does not traverse
`external/*/target` by checking the directory is absent from
`bazel query 'buildfiles(//...)'`.

### The pin authority, and the drift check (Friction C — a named override)

`bazel-quality.md:50-51` § The Gate says, verbatim:

> Run it after every change, cheapest first, and through the pin — `bazelisk`
> reading `.bazelversion`, never a `bazel` on `$PATH`.

This ADR's design is the opposite: Bazel comes from
`ocx.sh/bazelbuild/bazel` through `ocx.toml`/`ocx.lock`, put on `PATH` by the
per-prompt hook. **This is a deliberate, named override of `bazel-quality.md:50-51`
§ The Gate**, taken because `ocx.lock`'s per-platform digest is a strictly stronger
pin than the semver string `.bazelversion` carries, and because the repository's own
toolchain path is the mandate of driver 5. The override and its reason are recorded;
what a reviewer concludes from them is a reviewer's call.

The mechanism is settled by a primary source (research artifact 4 § 6,
bazelisk's own README). Two non-bazelisk cases exist and they differ: an
OS-installer `bazel` is a *wrapper script* that does read `.bazelversion`; a
**raw prebuilt binary** on `PATH` ignores it entirely. OCX distributes raw
prebuilt upstream release artifacts, so the raw-binary case applies. No Bazel
startup flag validates the running version against `.bazelversion`; the only
native mechanism ever offered was `bazel_skylib`'s `versions.check()`, invoked
from `WORKSPACE`, which Bazel 9 deleted.

The property The Gate defends is **the version that actually runs equals the
pinned version**. `ocx.lock` pins a per-platform **digest**, which is a strictly
stronger pin than `.bazelversion`'s semver string — a semver can be re-tagged
upstream; a digest cannot. So:

- **`ocx.lock` is the pin authority.**
- **`ocx.toml` pins `bazel = "ocx.sh/bazelbuild/bazel:9.2.0"`, not `:9`.** The
  dossier says `:9`; a floating major means an unrelated `ocx lock` rerun can
  move Bazel silently. Exact-version tags are already the in-house convention
  (`cosign = "…:3.1.1"`, `shellcheck = "…:0.11"`), so this is not an invention.
  This is a deliberate divergence from the dossier.
- **`.bazelversion` is retained** for BZL-FLAG-01 (MUST) and for any contributor
  or tool that does reach for bazelisk.
- **A drift check makes the pair honest.**

#### Gate contract — `task bazel:pin:check` (`scripts/bazel_pin_check.py`)

| | |
|---|---|
| **Reads** | `.bazelversion` (whole file, stripped); the output of `bazel --version` from the binary the project toolchain resolves. |
| **Compares** | The `.bazelversion` string against the `X.Y.Z` parsed out of `bazel --version`'s single output line, whose literal form is `bazel 9.2.0`. Deliberately compares against the **running binary**, not against a version field in `ocx.lock` — `ocx.lock` records `name`, `group`, `repository` and per-platform digests and **carries no resolved semver anywhere** (verified: `ocx.lock:8-19`). Measuring the binary also measures the property the rule cares about, rather than a second spelling of it. |
| **Exit 0** | Strings equal. No output. |
| **Exit 1** | Mismatch. stderr: `bazel pin drift: .bazelversion says <A>, the resolved toolchain binary is <B> — ocx.lock is the pin authority; bump .bazelversion or relock` |
| **Exit 1** | `.bazelversion` absent, empty, or not `^[0-9]+\.[0-9]+\.[0-9]+$`. stderr: `.bazelversion must be an exact three-component semver (BZL-FLAG-01), got: <text>` |
| **Exit 1** | `bazel --version` non-zero or unparseable. stderr: `bazel pin check could not read a version from the toolchain binary` — a reader floor, so a missing binary is not indistinguishable from a pass. |
| **Runs in** | `.verify:lint` (parallel `deps:`, so it costs nothing); and as its own step in the `smoke` job before the Bazel lane. In `cmds:`, never `preconditions:` — `task --force` skips `preconditions:`. |
| **Red state** | In a scratch copy write `9.1.0` into `.bazelversion` while the lock resolves 9.2.0; the task must exit 1 and print the drift line. Then delete `.bazelversion` entirely; must exit 1 with the second message. Then restore and re-run: exit 0, silent. Both halves are required before the check may be cited (BZL-CORE-02). |

### Stage 1 — Rust unit tests (the pilot)

#### The pilot choice (Friction A — compliant in spirit, with a named literal deviation)

`bazel-adopt/SKILL.md:158-174` step 4 says, verbatim:

> The pilot is the smallest subtree that produces something shippable, in one
> language, with no compile-time dependency on a live service. **Not the subtree
> with the worst pain** — that one is usually the one with the non-hermetic step,
> and fixing hermeticity first is the sequencing every case study that succeeded
> actually followed.

and its "What agents get wrong" item 5 (`SKILL.md:384-386`) names "picks the
painful subtree as the pilot" as an anti-pattern, on the same ground: "It is
usually the one with the non-hermetic compile step."

The dossier picks Rust unit tests **because** they hit the pain. The claim diff
flagged this as a direct contradiction, and it is one — at the level of the
literal word. The ruling:

**Verdict: compliant in spirit, with a named literal deviation on "smallest".**

1. **Step 4's stated rationale inverts here.** The reason given for avoiding the
   painful subtree is that it is *usually the non-hermetic one*. In this
   repository the painful subtree is the **most** hermetic of the four. The
   other three each carry precisely the non-hermetic step step 4 warns about:
   casts need a PTY outside the sandbox
   ([bazel#5373](https://github.com/bazelbuild/bazel/issues/5373), closed
   `not_planned`); acceptance tests shell out to docker-compose services;
   the website build needs network for `bun install`. Choosing Rust unit tests
   *is* hermeticity-first sequencing — step 4's own criterion — reached by
   inverting its heuristic rather than ignoring it.
2. **Step 4's live-service check is empty.** Run against this tree:
   `sqlx::query!|DATABASE_URL|localstack|testcontainers` over `crates/**`
   returns **no files**. Per step 4's own text, "Empty output = no compile-time
   service dependency found in tracked source = **any subtree is eligible**, and
   you pick on size."
3. **"Smallest" is the word the choice deviates from, and it is a real
   deviation.** `crates/**` is the largest subtree in the repository by every
   measure. A literally step-4-compliant pilot would be one leaf crate —
   `ocx_exit` — which produces nothing shippable and proves nothing about
   per-crate skip, the property the whole adoption exists to establish. The
   deviation buys a pilot whose green/red answers the actual question.

**Cost of the deviation.** A **20-crate** pilot (the verified workspace-member count;
23 is the *BUILD-file* count, which adds the three `external/` packages) fails slower
and in more places than a one-crate pilot. If `gazelle_rust` reds against the pin
pair or the patched submodules, the fallback (hand-written BUILD files) is 23 files
carrying **57** targets — 20 `rust_library` + 34 `rust_test` under `crates/`, plus 3
`rust_library` under `external/` — not one file carrying two. Per § The go/no-go reading, that fallback is
the **budgeted** path rather than the contingency: the generator is rated
Experimental, and `go-no-go.md:98-100` makes hand-maintained package-per-directory
BUILD files the correct default below Production.

**Abort condition.** If WP-1a cannot produce a green `bazel build --nobuild
//crates/...` **within 3 working days** (the budget ratified in § Orchestrator rulings R2)
by either generation route, stage 1 aborts and the initiative stops at Option C.
The deviation is not re-scoped to a single crate to rescue it — a one-crate green
would be a result reported under a scope wider than the one that ran
(`quality-core.md` § "A green is only as wide as what ran").

#### Targets and pins

- **Scope and target shape — corrected, because the previous shape could not reach
  the floor it is measured against.** 20 workspace members (`Cargo.toml`
  `members = ["crates/*"]`, verified count 20), each getting:
  - one `rust_library`;
  - one `rust_test` over the lib's `#[cfg(test)]` unit tests;
  - **one further `rust_test` per `crates/<name>/tests/*.rs`**.

  The last line is the correction. `crates/NEXTEST_FLOOR` = 8213 was produced by
  `cargo nextest list --workspace` (`taskfiles/rust.taskfile.yml:607-610`), which
  sums test cases over **every** rust-suite — including the integration binaries
  under `crates/*/tests/`. There are **14** of them, verified by enumeration:
  `ocx_cli` 3 (`linux_self_contained.rs`, `macos_self_contained.rs`,
  `help_surface.rs`), `ocx_index` 3, `ocx_package` 2, `ocx_schema` 4,
  `ocx_test_support` 2 (`boundary.rs`, `workspace_structure.rs`). One `rust_test`
  per member compiles only the lib's inline tests, so a BEP-derived count would be
  short by the whole integration set **on a perfectly healthy tree** and
  `task rust:test:floor` would red forever.

  **Today's target count is therefore 20 `rust_library` + 34 `rust_test`** (20 lib +
  14 integration). That number moves whenever a `crates/*/tests/*.rs` is added or
  removed, which is why the reader floors on a committed per-target table rather
  than on a constant (§ Stage 2).

- **The three `external/` patched submodules** (`oci-client` →
  `external/rust-oci-client`, `docker_credential`, `sigstore`, all three in
  `Cargo.toml`'s `exclude` and all three in `[patch.crates-io]`) get `rust_library`
  **only**. Their own tests are not wired: `cargo nextest run --workspace` does not
  run them either, and wiring them would make the Bazel test count non-comparable to
  the floor for a reason unrelated to coverage.
- **Mode:** `crate.from_cargo`, keeping `Cargo.toml` the source of truth. Not
  `crates_vendor`.
- **Bazel 9.2.0** — the current GA (9.3.0 was at rc2 on 2026-09-21). **Do not
  write "newest 9.x"**: that is a moving target and BZL-FLAG-01 forbids it.
- **rules_rust 0.74.0** (2026-08-28).
- **Do not gate on Bazel 9.3.0.** The `CARGO_BAZEL_REPIN` crash
  ([bazel#29114](https://github.com/bazelbuild/bazel/issues/29114)) is already
  mitigated on the rules_rust side by
  [PR #3932](https://github.com/bazelbuild/rules_rust/pull/3932), merged
  2026-04-07 and present in every release from 0.71.2 onward, including 0.74.0.
  The Bazel-side hardening (PR #31091) is belt-and-braces and ships in 9.3.0,
  not yet GA.
- **Drop rules_rust #3732 as a blocker.** The maintainer scoped it to
  `crates_vendor` only: "We **do** have path dependency support for
  non-`crates_vendor`." It stays a note against a future vendoring mode.
- **The toolchain pin is doubled, and that is a named drift risk.**
  `rules_rust` ignores `rust-toolchain.toml`
  ([#2753](https://github.com/bazelbuild/rules_rust/issues/2753), still open;
  the fix, [PR #3792](https://github.com/bazelbuild/rules_rust/pull/3792), is an
  unmerged draft). So `rust.toolchain(versions = ["1.95.0"])` in `MODULE.bazel`
  is a second, parallel pin beside `rust-toolchain.toml`'s `channel = "1.95.0"`.
  **rules_rust's own default is 1.98.0**, not 1.94.0 as the 2026-09-20 lane
  stated — so the pin sits three stable releases behind the ruleset default, and
  every `rules_rust` bump widens that gap silently. **Owner: whoever bumps
  `rust-toolchain.toml` bumps `MODULE.bazel` in the same commit.** The plan owes
  a check for this pair in the same shape as the `.bazelversion` drift check;
  it is cheap (two file reads, string compare) and is listed as a plan-docket item.

#### BUILD generation and its drift gate

`gazelle_rust` (Calsign) is on BCR at `0.1.0`, was last updated 2026-09-18, and
its issue [#5](https://github.com/Calsign/gazelle_rust/issues/5) — "workspace
that uses `Cargo.toml` files as the source of truth", a crate path-depending on
a sibling, exactly this repository's shape — is **closed/completed**. WP-1a is
therefore no longer "is this viable at all" but:

> Does `gazelle_rust` 0.1.0 generate correct BUILD files against **Bazel 9.2.0 /
> rules_rust 0.74.0** and against the **three `[patch.crates-io]` submodule
> crates**?

Both halves need asking because `gazelle_rust`'s own CI pins Bazel 8.7.0 /
rules_rust 0.71.0 (its `.bazelversion` and `MODULE.bazel`), and no issue in its
tracker mentions `[patch.crates-io]` — an absence of evidence, not evidence of
absence. Its Bazel 9 compatibility is reactive: issue
[#29](https://github.com/Calsign/gazelle_rust/issues/29) ("`sh_test` no longer
shipped with Bazel 9.x") is closed, which is the same
`--incompatible_autoload_externally` breakage this repository's own BUILD files
must handle. On Bazel 9 the load paths are
`@rules_shell//shell:sh_test.bzl` and `@rules_shell//shell:sh_binary.bzl`.
Known refinement gap: issue [#15](https://github.com/Calsign/gazelle_rust/issues/15)
leaks transitive deps into generated `deps` when the
`# gazelle:rust_cargo_lockfile` directive is used.

**The expected path is hand-written BUILD files; `gazelle_rust` is the
optimisation.** `go-no-go.md:95` rates the Rust generator **Experimental** and
`:98-100` states the consequence — below Production, "coarse, package-per-directory,
hand-maintained BUILD files are the correct default". This ADR's target shape is
package-per-directory, so the design is compatible with that verdict; the polarity
matters for planning, because it makes the 23-file hand-written route the budgeted
path and a green `gazelle_rust` the saving, rather than a spike whose red is a
crisis. Either route needs the same gate.

#### Stage 1 does not depend on rules_ocx — a stated sequencing, not a weakened mandate

The previous draft made `rules_ocx` block stage 1 ("**Blocks stage 1** — it is the
only tool path"). Nothing in the stage-1 graph consumes a rules_ocx-provided tool:
stages 1 and 2 build `rust_library` + `rust_test` only (§ Cache staging ruling 3
rules out `rust_binary` entirely), rustc and cargo come from rules_rust's own
toolchain, and **Bazel itself comes from `ocx.toml`/`ocx.lock`, not from rules_ocx**.
So the pilot's critical path would have run through an out-of-tree module the claim
diff could not read, pinned at Bazel 8.7.0, consumed by bare commit SHA with no
`integrity`, for **zero stage-1 benefit** — and `agg`, a mirror package that does
not exist yet, would have compounded it.

**Ruling: rules_ocx enters at stage 3**, where `@tools//:bun`, `@tools//:uv` and
`@tools//:agg` are first actually used. Stage 1 then depends only on `bazel` (from
`ocx.lock`, digest-pinned) and BCR modules with real versions.

**The mandate is unchanged.** "rules_ocx is the sole tool path" is a *composition*
rule — no `http_file`/`http_archive` for anything ocx can package — and it binds in
full from the moment a tool is needed. What moves is only *when* that path is first
exercised. The dogfood still happens, on the same stages, with the same gap-fixing
obligation; it simply stops gating a pilot that uses none of it.

#### Gate contract — `task bazel:build:drift` (`scripts/bazel_build_drift.py`)

Modelled on `scripts/crate_map.toml` + `workspace_structure.rs::deps_direction`
— a flat declared table read by a test against `cargo metadata`.

| | |
|---|---|
| **Reads** | Every `crates/*/BUILD.bazel` and `external/*/BUILD.bazel` (the `deps` attribute of each `rust_library`/`rust_test`, via `bazel query --output=build`, not a text parse); and `cargo metadata --locked` for the same packages. |
| **Compares** | The **full** dependency set per target, not only the first-party `ocx_*` edges. Third-party edges are compared through a declared name-mapping table (`@crates//:tokio` ↔ `tokio`, and the three `[patch.crates-io]` submodule edges ↔ their `external/` labels), committed beside `scripts/crate_map.toml`. First-party-only comparison is what the previous draft specified, and it lets a generated `BUILD.bazel` that drops `@crates//:tokio` pass the drift gate and red only at compile time, after the whole pilot build — and A7's `bazel build --nobuild //...` is loading/analysis only, so it does not catch it either. The `crate_map.toml` precedent this gate is modelled on is a **full** edge table, not a subset. |
| **Exit 0** | Sets equal for every package. No output. |
| **Exit 1** | Per differing package: `BUILD drift: <pkg> — in Cargo not in BUILD: [...]; in BUILD not in Cargo: [...]` |
| **Exit 1** | Fewer than 23 packages, or fewer than **57** targets read from either side. 57 = 20 `rust_library` + 34 `rust_test` under `crates/` (§ Stage 1) **+ 3 `rust_library` under `external/`** — this gate's **Reads** row names both directories, so its floor must count both. `BUILD drift check read <n> packages / <t> targets, expected 23 / 57 — the reader stopped early` (a reader floor; without it, a query that returns nothing is indistinguishable from a clean tree). **This floor is derived from the enumerated BUILD-file set, not from `crates/TEST_TARGET_MAP.toml`** — the map counts test targets only (34 rows), so it is the wrong instrument for a reader whose universe is every target in 23 packages. The two floors measure different subjects on purpose; the invariant that keeps them consistent is narrower and is asserted separately: the `rust_test` targets this gate reads under `crates/` must number exactly the map's row count, and a mismatch reds here with `BUILD drift: read <t> crates/ rust_test targets, TEST_TARGET_MAP has <k> rows`. |
| **Exit 1** | A third-party label with no row in the name-mapping table: `BUILD drift: unmapped external label <label> — add a row to the mapping table` (an unmapped label must not be silently skipped, or the widened scope is vacuous for exactly the edges it was widened to cover). |
| **Runs in** | **Not** `.verify:lint` — that is a parallel `deps:` block that runs before anything has established the graph loads, and this gate reads through `bazel query`, which needs a loading-clean graph. It runs in `.verify:build-test` **immediately after** `bazel build --nobuild //...`, and in the `smoke` job in the same order. In `cmds:`, never `preconditions:`. |
| **Red state** | Delete one first-party `deps` entry from one generated `BUILD.bazel` in a scratch copy → exit 1 naming that package. Delete one `@crates//` entry → exit 1 naming it (**the half the narrow scope could not red**). Add a bogus entry → exit 1 in the other direction. Rename one third-party label so no mapping row matches → exit 1 on the unmapped-label message. Point the query at an empty directory → exit 1 on the reader floor. Restore → exit 0. |

**Doctests stay on cargo.** `taskfiles/rust.taskfile.yml:257-278` runs
`cargo test --doc --workspace --locked`, and that task's own summary states
nextest cannot run doctests at all — so `test:unit` never executes one and the
floor/ceiling bracket has never seen them. No Bazel lane absorbs them.

### Stage 2 — the CI lane swap, and the floor/ceiling gates

**The claim diff corrects the dossier here.** Goal G3.2 reads as if both
workflows carry the floor/ceiling bracket. They do not:

- `verify-basic.yml` job `smoke` ("Smoke (Linux)", L86-87) carries the whole
  bracket: `Unit-test floor` (L210-211, `task rust:test:floor`) → `Test`
  (L212-219, `task rust:test:unit -- --profile ci`) → `Skip ceiling` (L220-221,
  `task rust:test:ceiling`) → `Ceiling self-test` (L227-228).
- `verify-deep.yml` job `build` (3-OS matrix, L76-77) runs
  `cargo nextest run --workspace --target=… --profile ci --locked` **directly at
  L135-136, with no floor or ceiling step at all**.

#### Rulings

1. **The Bazel lane replaces the nextest sub-sequence in `verify-basic.yml`'s
   `smoke` job only**, and in `verify-deep.yml`'s **Linux** matrix leg. The
   darwin and windows legs keep `cargo nextest run` — the dossier's own
   out-of-scope list says "Windows/macOS Bazel lanes (Linux first)", so this is
   the dossier's scope, not a narrowing of it.
2. **Floor and ceiling are NOT introduced into `verify-deep.yml`.** That would
   be a net-new control, and it would put two different floor *readers* (BEP on
   the Linux leg, nextest's `Summary [...]` grammar on the other two) on one
   invariant — two readers that can disagree, with nothing announcing the split.
   If the deep legs should be gated, that is a separate change, independent of
   Bazel. Recorded so the omission is a decision rather than a gap.
3. **Both gates stay `platforms: [linux]` and stay in `cmds:`, never
   `preconditions:`** — `task --force` skips `preconditions:` on go-task 3.52,
   and the existing placement is deliberate. The Bazel re-derivation inherits
   that obligation exactly.
3a. **`verify-deep.yml`'s Linux leg is a cache *reader* on every trigger,
   including `push` to `main`.** The credential question has two spellings in this
   ADR — a *lane* ("`verify-basic.yml`, push-to-`main` only") and a *predicate*
   (`github.event_name == 'push' && github.ref == 'refs/heads/main'`) — and
   `verify-deep.yml` triggers on `push: [main]`, so the predicate alone would have
   that leg attempt a write with no credential present. Stated once, as both:
   **the write lane is `verify-basic.yml`'s `smoke` job and only that job**, and
   within it the predicate gates the credential. **Every other lane and leg,
   `verify-deep.yml`'s `push`-to-`main` Linux leg included, carries
   `--remote_upload_local_results=false` unconditionally and no credential at all** —
   BZL-CACHE-01's verification treats the *absence* of that flag in a lane as the
   finding, so "it has no credential anyway" is not the control.
4. **`crates/NEXTEST_FLOOR` = 8213** (verified current file content) and does not
   change. The file stays; only the reader changes.
5. **`$XML_OUTPUT_FILE` is the design; the BEP + libtest grammar is the fallback.**
   The previous draft rejected one hand-derived stdout grammar as a fragility class
   and adopted a different one in the same section — and walked past the escape
   hatch it had itself written down. Bazel synthesises a minimal `test.xml` with one
   `testcase` per *target* **only when the harness emits no JUnit XML**. This
   harness already emits it: `taskfiles/rust.taskfile.yml:254` sets
   `JUNIT: '{{.ROOT_DIR}}/target/nextest/default/junit.xml'`, and `telemetry:push`
   already counts its `testcase` elements. A `rust_test` wired to write JUnit XML to
   `$XML_OUTPUT_FILE` therefore gives Bazel a **real** `test.xml` with per-case
   granularity, which makes the counts native, removes the hand grammar entirely,
   keeps the junit2otlp pipeline working unchanged, and dissolves WP-1b's second
   falsifier as well — a cached target replays its declared outputs, `test.xml`
   among them, so "does a cache hit still yield a parseable log" stops being a
   question. `quality-core.md` § "Don't Own Non-Domain Code" escalates hand-owned
   parsing of an external format to Block, and its bar for owning it is "no library
   implements the requirement, **verified by searching, not assumed**" — here the
   mechanism exists, is already in the repository, and was named in this ADR's own
   falsifier clause. **WP-1b's first item is therefore: wire `rust_test` to
   `$XML_OUTPUT_FILE` and confirm `test.xml` carries per-case granularity.** Only if
   that reds does the reader fall back to BEP `TestResult` + libtest's
   `test result:` line.

#### Gate contract — `task rust:test:floor` (Bazel-backed)

| | |
|---|---|
| **Reads** | The `--build_event_json_file` emitted by `bazel test //crates/...`. For each `TestResult` event, the **`test.xml`** named in `test_action_output`, and from it the `testcase` elements — a machine-readable per-target count produced by the test binary itself. (Fallback only if WP-1b's first item reds: the `test.log` and libtest's `test result:` line.) |
| **Computes** | `per_target[label] = |testcase elements|` for every `//crates/...` test target, then `count = Σ per_target`. |
| **Exit 0** | `count >= 8213` **and** every target in `crates/TEST_TARGET_MAP.toml` is present in `per_target` **and** no target's observed count is below its recorded value. |
| **Exit 1** | `count < 8213`. stderr: `bazel test floor: <count> test cases reported, floor is 8213 (crates/NEXTEST_FLOOR) — the unit-test set shrank` |
| **Exit 1** | A target present in the map is absent from `per_target`, or its observed count is below the recorded one. stderr: `bazel test floor: <label> reported <n> cases, TEST_TARGET_MAP records <m> — tests were removed from an existing target` |
| **Exit 1** | Zero `TestResult` events, or the BEP file absent/empty, or **fewer test targets than `crates/TEST_TARGET_MAP.toml` has rows** (**34** today — the map is one row per *test* target, so its row count is the 34 of § Stage 1, never the 54 all-`//crates/...` targets or the 57 including `external/`). stderr: `bazel test floor read <n> test targets and <m> results, the map has <k> rows — the reader stopped early` (reader floor; this reader's subject *is* test targets, which is why the map is the right floor for it and not for the two gates that read other universes — see § Cache staging ruling 1a and § Observability). |
| **Red state** | Set `crates/NEXTEST_FLOOR` to `count + 1` in a scratch copy → exit 1 with the shrank message. **Delete three `#[test]` functions from inside one existing `rust_test`'s sources** → exit 1 naming that label (the per-target half; a sum-only floor of 8213 can absorb three). Point the reader at an empty BEP file → exit 1 on the reader floor. Restore → exit 0. |

**Why the per-target half exists — the fallback that was not a floor.** The previous
draft's fallback, if the log line proved unreachable, was "the target count plus a
committed table of expected test counts". **Deleting tests from inside an existing
target leaves both numbers unchanged**, so the floor passes while coverage is lost —
the precise invariant the reader exists to hold, and exactly `quality-core.md`
§ "Unchecked Green"'s shape: a check whose passing state is indistinguishable from
the check never having run. An asserted table is not a floor. The contract above
therefore requires **observed per-target counts from the test binaries' own
`test.xml`**, and the committed map is only the *reader floor* against them — it
says which targets must have been read, never how many cases they contain.

**`crates/TEST_TARGET_MAP.toml`** is one row per **test** target — the Bazel label,
the crate, and the last-recorded case count — regenerated by the same `-- --update`
convention `clippy-warn-baseline.json` already uses, and only ever rising for a
target unless the commit that lowers it says why. **34 rows today.**

**Its authority stops at that number, and this is a rule, not a caveat.** Four gates
in this ADR carry a reader floor, and each reads a *different* universe:

| Gate | Its reader's subject | Its floor |
|---|---|---|
| `rust:test:floor` / `:ceiling` | crate **test** targets | `TEST_TARGET_MAP.toml` row count — **34** |
| `bazel:build:drift` | every target in 23 `crates/` + `external/` BUILD files | the enumerated BUILD-file set — **57** |
| `bep_to_otlp` | every target the BEP announces (spans include `rust_library`) | **54** on a `//crates/...` build |
| `bazel:tag:guard` | the whole `//...` universe | `bazel query 'kind(rule, //...)'` — **≥ 274** |

`quality-core.md` § "A green is only as wide as what ran" states the obligation:
*floor a derived check on its reader, not only on its subject*. Flooring all four on
one map would make three of them pass while having read a fraction of their universe —
and one table cited as authority for four different quantities is exactly the
substitution that put `54` where `34` belonged in the draft before this one. Each
floor above names its own number and its own query.

**Until `$XML_OUTPUT_FILE` is proven (WP-1b item 1), the nextest coverage gate keeps
running.** The existing `rust:test:floor` / `rust:test:ceiling` pair stays wired to
nextest on the `smoke` job, and the Bazel reader runs beside it report-only. The
swap happens in the commit that shows the Bazel reader red on the three-test
deletion above — not before. A floor is replaced by a floor, never by a promise of
one.

#### Gate contract — `task rust:test:ceiling` (Bazel-backed)

Same reader; compares `Σ ignored` against `crates/NEXTEST_SKIP_CEILING`. Exit 1
above the ceiling, naming the count and the file. **Bazel's own target-level
skipping (a cache hit) must never be counted as a test skip** — the two are
different facts and conflating them makes the ceiling rise with cache warmth.
The `ceiling:self-test` step keeps its role: it plants a value above the ceiling
and asserts the gate reds.

#### Two mechanism assumptions, and their falsifiers (WP-1b)

Both of these are stated as contracts above and must be **verified**, not
assumed, before the lane swap lands:

1. **A `rust_test` target can be wired to write JUnit XML to `$XML_OUTPUT_FILE`,
   and the resulting `test.xml` carries one `testcase` per test case.** This is
   ruling 5's design and the first thing WP-1b establishes. Its falsifier is
   Bazel's *synthesised* `test.xml`, which carries one `testcase` per **target** —
   so a reader that cannot tell the two apart would report ~34 against a floor of
   8213 and pass nothing while looking like a count. **The discriminator, and it
   must be shown:** on a commit whose nextest count is 8213, the `test.xml` sum
   equals 8213. A sum near the target count means the synthesised form is being
   read. **Fallback if the wiring is not reachable:** BEP `TestResult` +
   `test.log` + libtest's `test result: ok. N passed; M failed; K ignored; …`
   line, with the same per-target map and the same three-test-deletion red.
2. **A cached test target still replays a parseable `test.xml`.** A cache hit
   replays the target's declared outputs, and `test.xml` is one of them — which is
   why ruling 5's design dissolves this falsifier rather than managing it. It
   survives only on the fallback path, where the count could collapse as the cache
   warms: a floor that passes cold and reds warm, or worse, the reverse.
   **Fallback-of-the-fallback:** run the floor step with `--nocache_test_results`
   on the gating lane only, accepting that the floor step does not benefit from the
   cache while the `test` step does.
3. **The BEP target count equals the `rust-suites` count from
   `cargo nextest list --workspace` on the same commit.** **34** today (§ Stage 1) —
   `rust-suites` is keyed by **test binary** (`taskfiles/rust.taskfile.yml:607-610`
   sums `len(s["testcases"])` over `json[...]["rust-suites"]`), so it counts the 20
   lib test binaries plus the 14 integration ones and can never return the 54 that
   includes `rust_library` targets. This is the item that catches a graph missing
   the integration suites, which a sum-only floor cannot distinguish from a shrunken
   test set.

#### Ruling 6 — a report-only nextest parity probe, with an end date

Test-count parity is otherwise asserted **once**, at swap time. After the swap no
independent oracle for the count survives on any gated lane: the darwin/windows legs
keep nextest but ruling 2 leaves them ungated, and WP-1b's falsifier 1 describes a
failure that only appears later. Ruling 2 correctly refuses to put two floor
*readers* on one invariant. A third shape answers the objection without creating that
problem:

**`verify-deep.yml`'s Linux leg keeps `cargo nextest run --workspace` as an
ungated, report-only parity probe.** No floor, no ceiling, no gate — one invariant
with one gate, and a second count printed beside it. It fails the job only if nextest
itself fails, exactly as today. **Removal condition, named now so this is a track
record and not a permanent second lane: after five consecutive releases in which the
Bazel-derived count and the nextest count agree on the same commit, the probe is
deleted in the release that follows.** A probe with no end date is a second lane
nobody owns, and the CI minutes it costs are the ones this change exists to reduce.

#### The other call sites the dossier misses — three, not one

`taskfile.yml`'s `verify:scoped` (`:129-251`) is the fast local loop, and it holds
**three** nextest-family invocations. The previous draft cited `L152-155`, which is
inside `summary: |` — prose, not a step. The live steps, each opened at the line:

| Line | Step | Bazel-native equivalent |
|---|---|---|
| `:206` | `cargo nextest run -p ocx_test_support --test workspace_structure --locked` — the `ROUTE_MANIFESTS` route | `bazel test //crates/ocx_test_support:workspace_structure` — one of the 14 integration targets § Stage 1 adds, and the target class the previous draft's graph did not contain at all |
| `:223` | `cargo nextest run -p {{.ITEM}} --locked --no-tests=warn`, under `for: { var: CRATES }` | `bazel test //crates/{{.ITEM}}:all` per `CRATES` entry, with `--build_tests_only`; `--no-tests=warn`'s tolerance is reproduced by accepting an empty target set rather than erroring, so a phase-1 empty shell behaves as it does today |
| `:225` | `cargo test --doc -p {{.ITEM}} --locked`, under the same loop | **unchanged** — doctests never enter the Bazel graph (§ Doctests stay on cargo) |

The contract for the scoped arm, in the same shape as the other gates: the Bazel
per-crate step **replaces `:223` only**; `:206` gains a Bazel-native spelling in the
same commit that lands the integration targets; `:225` is untouched. A crate whose
Bazel query returns no test target is a warning, not a failure, matching
`--no-tests=warn` — and the BUILD drift gate is what makes "no target" a detectable
defect rather than a silent skip.

#### The hook point

`.verify:build-test` (`taskfile.yml:288-331`) is a **14**-step sequential `cmds:`
block: `rust:license:check`, `rust:license:deps`, `rust:license:notice:check`,
`scripts:suite-census`, `scripts:dead-path-sweep`, `rust:lint:ratchet`,
`rust:doc:ratchet`, `rust:build`, `rust:test:floor`, `rust:test:unit`,
`rust:test:ceiling`, `rust:test:ceiling:self-test`, `rust:test:doc`,
`test:parallel`. The Bazel lane replaces exactly four —
`rust:test:floor → rust:test:unit → rust:test:ceiling → rust:test:ceiling:self-test`.
The other **ten** are untouched. Two gates are *inserted* rather than replacing
anything: `bazel build --nobuild //...` immediately before the swapped four, and
`bazel:build:drift` immediately after it (§ Stage 1 — the drift gate needs a
loading-clean graph, which is why neither lives in `.verify:lint`).

### Stage 3 — casts and the website

#### Casts

- **Granularity ruling: 39 cast targets, one per cast-enabled script.**
  `test/doc_scripts/` holds 72 `*.sh` files in a flat directory, but only **39** of
  them are cast-enabled: the recorder runs a script only when its header carries
  `# cast: true` (`test/recordings/test_recordings.py:1-4` — "For each .sh file in
  doc_scripts/ with cast: true"). Verified by enumeration: 39 files carry
  `# cast: true`, and `website/src/public/casts/` holds exactly **39** `*.cast`
  files — the live output set matches the header count. The remaining **33 are doc
  snippets** published to the website by `scripts:publish`; they are **inputs to
  the site rule**, not cast targets. A 72-target ruling would mint 33 targets that
  record nothing and make A3's green permanently unreachable. The claim diff never
  asserted 72 *casts* — `discover_bazel_full_adoption.md:30` said 72 *files* and
  hedged ("implies 72 individual Bazel actions/targets **if done literally
  per-script**"); the previous draft dropped the hedge.

  The 39 are enumerated from the `cast: true` header through the existing discovery
  export, `test/scripts/doc_scripts_list.py` — the same seam the website publish
  task already consumes — never from a second hand-maintained list. One target per
  script is the granularity that makes the graph worth having; grouping them into
  one action means any script edit re-records all 39.

- **Target contract, per cast target.** The previous draft gave none, which left
  A3's green ("every cast is a `local`-tagged target that records") admitting no
  failing test. Modelled on the live recorder
  (`website/recordings.taskfile.yml:52-69`), which is a pytest run over a shared
  fixture set driven by each script's `# state:` / `# cast:` / `# doc:` header, not
  a per-script command:

  | | |
  |---|---|
  | **Declared inputs** | the one `test/doc_scripts/<name>.sh`; `test/recordings/**`; `test/src/**`; `test/conftest.py`; `test/pyproject.toml`; the `ocx` binary under test (the same artifact stage 4 consumes, and **by content digest**, not by path) |
  | **Declared output** | `website/src/public/casts/<doc>/<name>.cast`, where `<doc>` is the script's own `# doc:` header value |
  | **Provisioning** | the `# state:` header's fixture state, materialised by the existing harness; the compose stack reached at `localhost:<port>` |
  | **Exit** | 0 = the `.cast` file exists and is non-empty; non-zero = the recorder failed, with the recorder's own message |
  | **Tags** | `no-sandbox`, `requires-network`, `local` |

  A3's green is then a file-existence assertion on a named path, not prose.
- **Tags: `no-sandbox`, `requires-network`, `local`, from day one.**
  [bazel#5373](https://github.com/bazelbuild/bazel/issues/5373) (PTY in
  `linux-sandbox`) is closed **`not_planned`**, and
  `test/recordings/cast_recorder.py` calls `pexpect.spawn(` at **line 377** — so
  the sandbox will fail, and no spike is needed to learn that. `no-sandbox` does
  **not** disable caching (Bazel's own Common Definitions reference:
  "it can still be cached or run remotely"); `local` is what removes these from
  the shared cache, and it does so by including `no-remote`.
- **Registry addressed as `localhost:<port>`**, never a compose service name
  (bazel#11325, #5869, #738).
- **`agg` via `@tools//:agg`**; the Nerd Font stays an `http_archive` with
  `integrity` — it is an asset, not a tool.
- **Cast byte reproducibility is a non-goal**, and the header is not the reason.
  `CastRecording.to_cast()` (`cast_recorder.py:72-86`) writes a header of
  `{"version", "width", "height", "title"}` with **no wall-clock `timestamp`
  field** — the header bytes are already reproducible. The non-determinism is
  per-event relative timing from `pexpect`, not the header. The dossier's
  framing is right; its implied reason is not.

#### Website

- **The DAG is 5 stages**, not 3: `website/taskfile.yml:45-63` runs
  `schema:default` → `scripts:publish` (the doc-scripts stage) →
  `recordings:parallel` → `sbom:generate:page` → `bunx vitepress build`.
- **Declared inputs must include `test/doc_scripts/**/*.sh`.** The dossier's
  input list (`website/**` + generated schemas/casts/SBOM) omits it, but
  `website/recordings.taskfile.yml:52-69` already declares
  `doc_scripts/**/*.sh` in its own `sources:`. A coarse rule that omits it is
  **unsound**, not merely incomplete: a doc-script edit would produce a cache hit
  on a stale site.
- **One coarse Starlark rule**, not a `genrule`: `@tools//:bun install
  --frozen-lockfile` + `bunx vitepress build`. Output: the site directory.
- **Tags: `requires-network` only** — this is the one stage of four that stays
  sandboxed and therefore the one target in the whole design eligible for the
  shared cache. See § Cache staging ruling 2 for the three-part hermeticity check
  it must pass first, and note that § Decision Outcome's `local` sentence covers
  the casts and stage 4, **not** this rule.
- **The economics of this stage are unmeasured, and that is stated rather than
  implied.** Nothing in this repository records what the `bunx vitepress build`
  chain costs or how often it runs; it is not one of `.verify:build-test`'s
  fourteen steps, so it is not in the gate at all. A named exemption from two
  MUST-severity rules (§ The BZL-JS exemption) plus a hand-written Starlark rule
  with no upstream maintainer is therefore purchased against a cost nobody has
  sized, while `subsystem-taskfiles.md` § Caching Contract already supplies the
  same coarse property. **WP-1c measures it** — wall-clock of
  `task website:build` cold and warm, and its invocation frequency over the last
  50 `deploy-website` runs — and § Acceptance A3 carries an abort condition keyed
  to that number. If the chain is minutes and runs on deploy only, the honest
  conclusion is that this stage does not pay for the exemption, and A3 aborts.
- Fix-forward, **outside this ADR's scope**: `.claude/rules/subsystem-website.md`'s
  own pipeline table lists three generators and omits the `scripts:publish`
  stage entirely. That doc is stale regardless of Bazel.

#### The BZL-JS-01 / BZL-JS-03 exemption (Friction B)

Corrected location — the dossier cites the wrong file. Both rules live in
`.claude/rules/bazel-quality/typescript.md` at lines **93** and **94**, severity
**MUST**, not in the `bazel-quality.md` index:

> **BZL-JS-01** — Confirm a real `pnpm-lock.yaml` at `lockfileVersion` 9 or
> higher exists before wiring any `npm_translate_lock`; convert the repository to
> pnpm first (`pnpm import`) rather than pointing the rule at an npm or yarn
> lockfile "for now".
>
> **BZL-JS-03** — Never point a lockfile attribute at a `bun.lock` and never
> write `bun_lock =` — the attribute does not exist; a bun-locked package
> converts fully to pnpm before rules_js can see any dependency.

**The ruling, in four parts.**

1. **Both rules are scoped to rules_js ingestion.** Their subject is
   `npm_translate_lock` and its lockfile attribute. BZL-JS-01's own text
   conditions on "before wiring any `npm_translate_lock`"; BZL-JS-03's on "before
   rules_js can see any dependency". The coarse rule never calls
   `npm_translate_lock` and never loads rules_js, so **the rules' premise does
   not obtain**. This is not a claim that the rules are wrong — they are correct
   about the thing they govern, and they would bind the moment anyone reached for
   `npm_translate_lock` here.
2. **The skill's own escape clause fits this repository exactly.**
   `bazel-adopt/references/branches-python-ts-cpp.md:108-111`:
   > **Adopting for a repository not already on pnpm is a "no" as a starting
   > move.** The cost — conversion, `hoist: false`, phantom-dependency fixes — is
   > paid before any Bazel benefit arrives. **The answer flips only when the JS
   > package is a minority slice of a polyglot migration already justified by
   > other languages.**

   The website is a minority slice; the migration is justified by Rust. This is
   the skill's own sanctioned path, not an invented carve-out. (The same file at
   `:94-98` states the harder line — "a bun-locked package converts to pnpm first
   or it does not adopt (BZL-JS-01, BZL-JS-03, BZL-JS-29)" — and that line is the
   rules_js path, which this ADR does not take.)
3. **The coarse rule is the only option, not a preference.** Research artifact 4
   § 9 checked BCR itself, not just rules_js:
   [rules_js#1258](https://github.com/aspect-build/rules_js/issues/1258) is open,
   labelled `need: funding`, untouched since 2025-08-23; and **no maintained Bun
   ruleset exists anywhere in the BCR catalogue**. Recorded as a decision, not an
   open question.
4. **The ceiling, stated honestly.** The coarse rule buys **coarse-grained
   input-hash skipping** — the site rebuilds or it does not. It buys **no
   per-file JS caching**, no `ts_project` typecheck test, no incremental
   transpile. That is the price of the exemption, and it is the reason this stage
   scores low on cache economics in § Decision Outcome.

**Where the exemption is recorded.** `bazel-quality` is vendored: `grimoire.toml:6`
pins `bazel-essentials = "ghcr.io/ocx-sh/lore/bazel-essentials:latest"`, and the
index `bazel-quality.md` carries `license: Apache-2.0` and
`repository: https://github.com/ocx-sh/grimoire-lore` in its frontmatter
(lines 21-22). (The depth file `bazel-quality/typescript.md` carries only `title`
and `summary` — the vendoring marker is on the index, and the depth files are part
of the same bundle. Correcting the brief's description of where that marker sits.)
Editing any of it is upstream work, not this repository's. So the exemption lives:

- **(a) in this ADR**, as the reasoned ruling above; and
- **(b) as a named comment beside the rule** in `website/site.bzl` when it is
  written, citing this ADR and both rule IDs verbatim.

This ADR **does not edit the vendored rule file.**

### Stage 4 — acceptance

**Stage 4 buys selection, not caching. That is the whole of its payoff, stated
first, because two separate defects in the previous draft both came from implying
otherwise.**

#### The cache unit: one `sh_test` per test module, never a `SCOPED_ROWS` row

`SCOPED_ROWS` (`test/taskfile.yml:371-414`, 20 rows) is a **selection** mechanism
with a hand-maintained fallback, not a disjoint ownership map, so it cannot define a
cache unit. Three defects in the live table:

- **The globs overlap.** `ocx_oci` and `ocx_trust` both name `tests/test_logging.py`;
  `ocx_console` and `ocx_shell` both name `tests/test_completion_ascii.py`;
  `ocx_store` and `ocx_shim` both name `tests/test_windows_shim.py`. A per-row target
  therefore duplicates execution, and "exactly that module re-runs" is false by
  construction.
- **Two rows are the literal word `escalate`** — `ocx_test_support` and `ocx` — which
  name no files and have no target shape. `ocx` is the CLI crate, i.e. the row an
  ordinary commit hits most.
- **Its own failure mode is a hand-maintained fallback**, not a partition:
  `test/taskfile.yml:427` reds with "no row for crate '<x>' — add one to
  `SCOPED_ROWS`".

**Ruling:** the cache/execution unit is **one `sh_test` per `test/tests/test_*.py`** —
the natural disjoint unit, one file one target. `SCOPED_ROWS` stays exactly what it
is: the crate → target **selection query**, unchanged in `test/taskfile.yml`, read to
turn a crate name into a set of `//test:<module>` labels. **An `escalate` row maps to
`//test:all`** — the whole suite — which is what `escalate` already means today.

#### Concurrency

> **SUPERSEDED 2026-09-23 by `adr_test_speed_tiers.md` AM-9.** The targets run
> concurrently at xdist parity behind the runner's host locks (`test/bazel.bzl`
> § Concurrency). `--local_test_jobs=$ACCEPT_JOBS` (default `min(8, nproc)`) is
> set on `task bazel:test:accept`'s command line and in no rc file; the paragraph
> below is the original ruling.

Today the suite is one pytest-xdist process against one docker-compose stack. Under
Bazel each `sh_test` spawns its own `uv run pytest`, and N of them would contend for
the registry/zot/sigstore ports. The contract: the compose-backed targets run under
**`--local_test_jobs=1`**, set in `.bazelrc` for the `//test/...` package and nowhere
else. Per-target port allocation is explicitly **not** attempted — the fixtures are
session-scoped against fixed ports, and changing that is a test-suite redesign
outside this ADR. The consequence, named: stage 4 loses today's xdist parallelism
unless and until the fixtures take a port from the environment. **That is a real
regression in wall-clock**, and A4's abort condition is keyed to it.

#### Cache-result caching is disabled, deliberately

> **AMENDED 2026-09-22 — result caching is ON.** The ruling below stands as
> written until its last paragraph, which names its own exit condition: *"If a
> future change ever wants result caching here, the precondition is a
> demonstrated red on the binary-swap case — change the binary, require a miss —
> and the `external` tag comes off in that same commit or not at all."* That is
> what happened. The owner directed the reversal; the precondition was met in the
> same change, and the amendment is recorded here rather than by rewriting the
> reasoning, because the reasoning is what makes the conditions legible.
>
> **What changed, and what it cost.**
>
> * `external` came off, and `local` with it. `local` is `no-remote` +
>   `no-sandbox`, and the `no-remote` half suppresses the `--disk_cache` hit —
>   measured on the pin, a `local`-tagged test re-runs on a warm **fresh
>   server**, which is the state every CI runner is in. `no-sandbox` buys the
>   execroot working directory the suite needs with none of that; the five-row
>   measured table is in `test/bazel.bzl`'s module docstring and is shipped as an
>   expectation by `scripts/bazel_accept_proofs.py --prove-s015`, so a Bazel
>   release moving a cell reds rather than silently invalidating this.
> * The inputs stopped being aspirational. Each target declares its own module
>   plus `//test:suite_inputs` — `conftest.py`, `pyproject.toml`, `uv.lock`,
>   `ocx.toml`/`ocx.lock`, the floor and ceiling files, `src/**`, the fixture
>   tree, `scenarios/`, `specs/`, `sigstore/`, `scripts/`, `docker/`,
>   `docker-compose.yml`, `zot-config.json` and `bin/ocx*` — with every generated
>   path excluded by a measured list rather than a guessed one.
> * The two compensating controls are live rather than promised.
>   `bazel:tag:guard` now runs at stage 4 (`//...`) and refuses any acceptance
>   target that stops declaring `//test:docker-compose.yml` and
>   `//test:suite_anchor`; `bazel_accept_proofs.py --check-s015` requires a
>   warm run in which everything cached **and** a binary-swap run in which
>   nothing did.
> * **The under-declaration that remains is named rather than closed:** the
>   compose stack's running state is not an action input, and ~20 modules read
>   `target/release/ocx_schema`, `crates/**`, `website/src/docs/**` or
>   `test/doc_scripts/**` across a package boundary. `test/bazel.bzl`'s docstring
>   carries the list and each one's stale-green risk. This is the honest price of
>   the reversal and it is not hidden behind the word "cached".
> * **The wall clock got worse, not better, and A4's abort condition is
>   breached.** See A4 below for the three measured numbers. The owner took the
>   decision with the regression named; caching is the mitigation, not a denial.

The previous draft based invalidation on `SCOPED_ROWS` plus "each test owning its own
registry state — unchanged from today's assumption". That does not make a cache key.
Nothing in it requires the **tested `ocx` binary's bytes**, the **compose
configuration**, the **service image versions** or the **shared fixtures** to enter a
target's inputs — so a changed binary could leave a cached pass valid, which is the
`#4276` shape with the poisoning done by ordinary correctness rather than by an
adversary. Owning registry state is an *isolation* property; it is not input
tracking, and the two were conflated.

**Ruling:** every `//test/...` target carries **`tags = ["external"]`** and the lane
runs `--nocache_test_results`. Per `bazel-quality/testing.md:77` (BZL-TEST-08, MUST),
`external` is test-only and makes the tagged test re-execute every invocation while
an untagged sibling reports `(cached)` — which is exactly the semantics wanted here.
Stage 4 therefore buys **selection** (a docs-only commit selects zero acceptance
modules) and **nothing else**; it buys no caching, local or remote. Said plainly so
no later reader "optimises" it back on.

The inputs are still declared honestly, because selection depends on them: the module
file, `test/src/**`, `test/conftest.py`, `test/tests/conftest.py`,
`test/pyproject.toml`, `uv.lock`, the compose file, and the `ocx` binary **by content
digest**. If a future change ever wants result caching here, the precondition is a
demonstrated red on the binary-swap case — change the binary, require a miss — and
the `external` tag comes off in that same commit or not at all.

#### The rest

- `sh_test` (loaded from `@rules_shell//shell:sh_test.bzl` — Bazel 9 removed the
  global) or a thin custom rule, shelling to `@tools//:uv run pytest`.
- Tagged **`local`** as well as `external`: `local` implies `no-remote-cache`
  (§ Cache staging ruling 1), so nothing from this stage can reach another machine.
- `uv.lock` and `conftest.py` untouched. **No rules_python.**
- Services (registry, zot, sigstore, toxiproxy) stay in the external
  docker-compose stack, addressed as `localhost:<port>` and never a compose
  service name (bazel#11325, #5869, #738).

### Cache staging, and the trust boundary

**Staging, in `bazel-adopt` step 8's order:** `--disk_cache` (local and on the
runner, with an explicit `--experimental_disk_cache_gc_max_size` or `_max_age` —
both default to `"0"`, unbounded, even on a version that supports them,
BZL-CACHE-17) → **remote cache read-only, everywhere** (explicit `https://`
scheme; both remote flags default to `grpcs` on a scheme-less URI) → **writes on
the main lane only** → **no remote execution, ever, under this ADR**.

#### Rulings the research demands

1. **`local` already implies `no-remote-cache`.** Per
   `bazel.build/reference/be/common-definitions`: `local` "precludes the action
   or test from being remotely cached, remotely executed, or run inside the
   sandbox" — it is `no-remote` + `no-sandbox` combined. So the cast and
   acceptance stages are safe by construction. **Corollary, load-bearing:** a
   later "let's cache acceptance tests too" must not drop the
   `no-remote-cache`/`local` half while keeping `no-sandbox`. `no-sandbox` alone
   is cache-key-**unsound**: the action computes a key from its declared inputs,
   silently consumes an undeclared one that differs machine to machine, and
   uploads a result wrong for every other machine matching that key
   (BZL-CACHE-34; Bazel's sandboxing docs: "A bad cache entry in a shared cache
   affects every developer on the project, and wiping the entire remote cache is
   not a feasible solution"). Any future tag trim on these targets is a
   BZL-CORE-01 violation ("never reach green by weakening the check").

   > **AMENDED 2026-09-22.** The corollary's own case arrived: the acceptance
   > stage dropped `local` and kept `no-sandbox`, which is exactly the trim this
   > paragraph forbids. It is not a BZL-CORE-01 violation because the check was
   > not weakened — it was **narrowed and given a subject**. `bazel:tag:guard`
   > advanced to stage 4 so the acceptance package is in its universe at all,
   > and it credits a `no-sandbox` acceptance target only while that target
   > declares the binary under test and the compose definition among its inputs.
   > The cast stage is untouched: its 40 genrules keep `no-remote-cache`, which
   > is a *build* action's answer and was measured separately. What this
   > paragraph gets right and keeps is that `no-sandbox` alone is cache-key
   > unsound; what the amendment adds is that a declared input set is the other
   > way to be sound, and that the gate has to be able to see it.

1a. **The corollary gets a gate, because prose is not one.** This ADR's own
   non-negotiable is that every gate it creates has a demonstrable red state, and
   the previous draft honoured that for the pin check, the drift check, the floor,
   the ceiling, the hermeticity check, the telemetry parity check and the tool-path
   grep — and left this one, the invariant whose violation is **silent and
   cross-machine**, to a sentence. `task bazel:tag:guard`
   (`scripts/bazel_tag_guard.py`):

   | | |
   |---|---|
   | **Reads** | `bazel query 'attr(tags, "no-sandbox", //...)'` and `bazel query 'attr(tags, "(no-remote-cache\|no-remote\|local)", //...)'`; and `bazel query 'kind(rust_binary, //...)'`. |
   | **Compares** | Set difference: every `no-sandbox` target must also carry one of the cache-excluding tags. Separately: every `rust_binary` must carry `no-remote-cache` or be built `--nostamp` on the cached path (§ ruling 3). |
   | **Exit 0** | Both differences empty. No output. |
   | **Exit 1** | `bazel tag guard: <label> is no-sandbox without no-remote-cache/no-remote/local — a cache-key-unsound target (BZL-CACHE-34)` |
   | **Exit 1** | `bazel tag guard: <label> is a rust_binary on the cacheable path without --nostamp or no-remote-cache (BZL-CACHE-20)` |
   | **Exit 1** | Reader floor **on this gate's own universe, not on `crates/TEST_TARGET_MAP.toml`**: `bazel query 'kind(rule, //...)' \| wc -l` — the count BZL-CI-01 names — must return at least the sum of the enumerated stage counts (274 today, § NFR Scalability), and each of the three tag queries must have parsed without error. `bazel tag guard read <n> of an expected >= <k> rule targets — the reader stopped early`. The map is the wrong floor here: it counts 34 crate test targets, so flooring a `//...` query on it would pass a reader that saw 12 % of the tree. |
   | **Runs in** | `.verify:build-test`, beside `bazel:build:drift`, after the loading check. |
   | **Red state** | Add `no-sandbox` to one cast target's tags **without** `local` → exit 1 naming it. Add a `rust_binary` with neither escape → exit 1 naming it. Point the query at an empty universe → exit 1 on the reader floor. Restore → exit 0. |

2. **The website rule is the only sandboxed cacheable target**, and therefore the
   only place an undeclared-input bug produces a cross-machine-wrong entry. It does
   not go on the shared cache until all three checks below pass, each shown red and
   green.

   **The previous draft's check was inverted and could not discriminate.** It
   required a cache **miss** after changing an *undeclared* ambient file. Bazel keys
   reuse on **declared** action inputs, so a properly isolated action is
   *unaffected* by an undeclared file and legitimately stays a hit — the demanded
   outcome is the one a correct implementation cannot produce. And a hit/miss
   observation cannot reveal whether a *forced* execution would consume an
   undeclared input either way, because on a hit no execution happens. The check as
   specified could never go red for the right reason, nor green for one.

   Split into two checks that can:

   **(a) Declared-input invalidation.** Warm a `--disk_cache` by building
   `//website:site` at checkout A. Build a byte-identical copy at a *different
   absolute path* (checkout B) against the same cache → **must be a hit** (this is
   the path-independence property BZL-CACHE-34 measures). Then change a **declared**
   input at checkout B — one `test/doc_scripts/*.sh` body, which § Stage 3 requires
   to be declared — and rebuild → **must be a miss**. Red half: if the second is
   also a hit, `test/doc_scripts/**/*.sh` is missing from declared inputs and the
   rule is unsound.

   **(b) Ambient-input isolation.** Force **uncached execution** in both runs —
   `--noremote_accept_cached` plus a cold `--disk_cache`, or any equivalent that
   defeats reuse; a run that can hit measures nothing here. Execute the rule twice
   under two **different ambient conditions** (a different `$HOME` with different
   dotfiles; a different absolute workspace path; a different system timezone) and
   **compare the output trees byte for byte** → **must be equal**. Red half:
   deliberately make the rule read one ambient value (a `date` stamp into a
   generated page) and show the two trees differ. Until that red is shown, check (b)
   is a habit, not a check — and per `quality-core.md` § Unchecked Green, a
   mutation that *fails* to produce the difference means the mutation missed, not
   that the rule is clean.

   **(c) Tool-pin participation.** The rule runs `@tools//:bun install
   --frozen-lockfile` + `vitepress build`, so its toolchain comes from the path
   whose cache-key participation is unknown from here (WP-0b's record — retired from
   the Open Questions as a lookup rather than a decision). Neither (a) nor (b) reds when
   the *tool version* changes but is absent from the key: in (a) the tree is
   identical, and in (b) the mutated thing is a source file. So: **change the `bun`
   pin in `ocx.toml`/`ocx.lock`, rebuild `//website:site` against the same warm
   cache, require a miss.** Red half: if it hits, the tool pin is outside the key
   and a `bun` bump would silently reuse a site built by the previous `bun` on every
   machine. This converts an unanswerable out-of-tree question into an in-tree
   observable, and it is the check that most directly defends the ADR's own driver 3.

   **"Require a miss" is the wrong reader against the shipped rule, and the plan's
   "green by construction" for C-023(c) is refuted.** WP-31 measured it with `uv`
   pinned 0.10.0 ↔ 0.10.1: lock bumped, plain `bazel build` → **stale**, 12
   action-cache hits, output still `uv 0.10.0`; `bazel fetch --force --repo=@probe`
   then build → one re-execution and the new output. The pin *does* participate in
   the action key — M-05's launcher-text contract holds — but the **repository** is
   stale, because `@tools`'s repo marker records no `FILE:` entry for `ocx.toml` or
   `ocx.lock`. `ocx/private/project.bzl` believes it does
   (`ctx.path(ctx.attr.ocx_lock)  # register the lock as an input`); on Bazel 9.2.0
   `ctx.path(Label)` registers no content dependency, and `ctx.watch` appears nowhere
   in that file on the pinned commit or on `main`. rules_ocx's *lazy* form carries
   the edge because `ctx.read()` auto-watches. So a single reading conflates "the
   digest is not in the key" (Block) with "the repository did not refetch"
   (owner-gated, `ctx.watch` upstream), and it reds for the second reason on a rule
   whose key is correct. Two readings are required, and
   `scripts/bazel_hermeticity_proofs.py` § "(c) Tool-pin participation" ships the
   table that separates them, plus `marker_watch_findings` — the same defect read
   statically off the repo marker with no build at all.

   **Until all three are green with both halves shown, the rule carries
   `no-remote-cache`.**
3. **`build.rs` stamping — the dossier's open question, closed.**
   `crates/ocx_cli/build.rs` reads `CI` (L40), `__OCX_BUILD_VERSION` /
   `__OCX_BUILD_CHANNEL` (L83-84), six `GITHUB_*` vars (L87-96) and `.git/` via
   `vergen_gix` (L54-66, degrading to a `cargo:warning` when absent). Under Bazel
   9's `--incompatible_strict_action_env` (on by default) **none of those reach
   the action**. The ruling:
   - **Do not add `CI` or any `GITHUB_*` to `--action_env` for any cacheable
     target.** Two hazards, and **the second is the one that makes the ban
     unconditional** — the previous draft argued only from the first, which a
     future reader could reverse on its own terms ("`CI` is constant within CI, so
     the split costs one universe"):
     - *Cache economics (the weaker hazard).* Every `--action_env` var enters the
       cache key: `GITHUB_RUN_ID` changes every run and would make the action
       permanently uncacheable; bare `CI` splits the cache into two disjoint
       universes that never share a hit.
     - *Output nondeterminism (the real hazard).* `crates/ocx_cli/build.rs:35-42`
       gates `build_timestamp(in_ci)` on the presence of `CI` (`:40`,
       `let in_ci = std::env::var_os("CI").is_some();`), and its own comment at
       `:35` states the consequence: `build_timestamp(true)` "emits the current UTC
       time every invocation". Under `--action_env=CI` that build-script action becomes
       a **remote-cacheable action with a wall-clock-varying output on the lane
       that writes the shared cache** — a `#4276`-class poisoner, not an
       inefficiency. That, not the split, is why the ban does not negotiate.
   - **The six `GITHUB_*` vars populate the optional `ci` block of
     `ocx version --format json`** — `ci.run_url`, `ci.workflow`, `ci.git_ref` and
     `ci.sha` (`crates/ocx_cli/src/app/build_info.rs:146-157`; the previous draft
     said `ci.run_url` alone, which is narrower than the code). The material claim
     is unaffected and holds: all four are optional fields reached through
     `option_env!()`, the module's own test doc comment records that each block
     "is present when its backing env vars happen to be exported at test-binary
     build time … absent locally", and their absence under Bazel matches ordinary
     non-GHA release behaviour. No test breaks; `option_env!()` resolving to `None`
     is silently correct here.
   - **Route build metadata through `--workspace_status_command`**, and keep the
     git SHA in the **volatile** half. `stable-status.txt` participates in the
     action key; `volatile-status.txt` does not — Bazel "pretends that the
     volatile file never changes". The trap runs both ways: a *volatile* SHA on a
     remote-cacheable target means commit A's CI run can serve a hit to a build
     of commit B, and the binary silently carries A's SHA (bazel#5573); a
     *stable* key holding a per-build value busts the cache every invocation
     while its name advertises the opposite (BZL-CACHE-20).
   - **Therefore: stages 1 and 2 build no `rust_binary` at all.** Only
     `rust_library` + `rust_test`. The stamped `ocx` binary keeps its existing
     cargo/`dist` path, and the acceptance stage consumes that binary as it does
     today. This removes the stamping/caching conflict from the pilot's blast
     radius entirely rather than solving it. **The standing rule, stated where the
     instruction is rather than three bullets away:** *any* `rust_binary` added to
     this graph carries `no-remote-cache` **or** is built `--nostamp` on the cached
     path, with stamping only on the `dist` profile — and ruling 1a's query gate is
     what reds when neither holds. "Keep the SHA in the volatile half" is safe only
     because no `rust_binary` exists; whoever adds the first one must carry both
     halves, so both are in the gate rather than in the prose.
4. **Credential — stronger than the dossier, on a MUST.** The dossier plans
   `--remote_header` from a secret. **BZL-CACHE-03 (MUST for a new setup)**:
   "Carry a cache-write credential through `--credential_helper`, never a bare
   `--remote_header=authorization=…` or any static bearer token in an rc file,
   gitignored or not. **pinned** — a new setup takes the helper from its first
   line." This is a new setup. **Ruling: `--credential_helper` from day one, not
   `--remote_header`.** Three independent reasons converge:
   - BZL-CACHE-03 is a MUST for new setups, unconditionally. (This is stronger
     than research artifact 3's conditional framing, which made the preference
     contingent on BEP export. The rule does not condition on that.)
   - A static token is visible in process argv and in any log that echoes the
     command line.
   - `--announce_rc` and BEP's `structured_command_line` are a designed-in,
     fully-expanded transcript of every flag value. Whether `--remote_header`
     values are masked there is **UNVERIFIED** (no source found either way; treat
     as not masked). Since this ADR's own BEP→OTLP script reads
     `--build_event_json_file` on the writing lane, `--remote_header` would put
     the credential directly in the script's input.
   - **BZL-CACHE-04 (MUST):** the helper is configured **only from an rc file that
     does not ship with the repository**, never a committed `.bazelrc`, and never
     a `%workspace%`-relative helper path — a read-only `bazel query //...` on a
     fresh clone is enough to execute it (reproduced on 9.2.0, closed by the
     vendor as intended behaviour, [bazel#30439](https://github.com/bazelbuild/bazel/issues/30439)).

#### Ruling 4a — resolving the -03/-04 tension, and killing the second writer

Two MUSTs pull in opposite directions and the previous draft's file-set table landed
between them. **BZL-CACHE-03** (MUST, *pinned*) says a new setup takes the credential
through `--credential_helper` from its first line and forbids a bare
`--remote_header`. **BZL-CACHE-04** (MUST) says the helper may be configured *only*
from an rc file that does not ship with the repository. Together those exclude the
tracked `.bazelrc` — and **`.bazelrc.user` does not satisfy -04's intent either**,
for a different reason: **BZL-CACHE-02** (MUST) is explicit that "a developer rc file
holding the write token is **the second un-gated actor** that a CI-only review never
sees", and putting a personal `--credential_helper` there — as the previous draft's
file-set table did — is that literal violation. In this design reads present no
credential at all, so a helper on a developer machine can only be a **write** helper:
an unreviewed writer to the shared Action Cache, running unsandboxed builds of
whatever branch is checked out. That is the Stripe/`#4276` threat model the ADR
accepts elsewhere only on the strength of "writes on one main-only lane".

**The resolved shape, stated once:**

- The credential lives in a **CI-only rc file written by a composite action on a
  trusted event** — `$RUNNER_TEMP/bazel-cache.rc`, carrying the single line
  `build --credential_helper=bazel-cache.ocx.sh=$RUNNER_TEMP/bazel-cache-helper`,
  consumed with `--bazelrc=$RUNNER_TEMP/bazel-cache.rc`. Outside the workspace, not
  tracked, not `%workspace%`-relative: -03 and -04 both satisfied.
- **The tracked `.bazelrc` carries no `--credential_helper` line of any kind**, so
  the -04 sweep (`git ls-files | grep -E '\.bazelrc' | xargs -r grep -n credential_helper`)
  is empty by construction.
- **`.bazelrc.user` carries no `--credential_helper` either.** Struck from the
  file-set table. Note the reason changed when ruling R1 landed and reads stopped
  being anonymous: it is **not** "reads present no credential" any more — a developer
  does hold a read credential now (ruling 6a). It is that -02's subject is the
  **write** credential, and -02's enumeration must return exactly one un-gated actor
  for *writes*. A read helper on a developer machine would add a second actor to the
  wrong side of the asymmetry -02 exists to keep, so the read credential stays a
  `--remote_header` in gitignored `.bazelrc.user` and the helper stays CI-only.
- **Server-side, so "who can write" is answerable from the server rather than from
  trust:** bazel-remote's htpasswd realm holds exactly **one** account, rotated on
  the schedule ruling 6b names.

#### Ruling 4b — how the helper is provisioned, in four lines with a red state

"Written at job setup" was the whole attack surface and one clause long. Bazel spawns
this program **before the sandbox, with the full client environment**, on every
invocation on that lane — the property BZL-CACHE-04 is quoted for two bullets above
and which the previous draft did not carry into the spec. The four lines:

1. **The secret reaches the step through `env:`, never through `${{ secrets.X }}`
   inside a `run:` body.** GitHub materialises step scripts to a file under
   `/home/runner/work/_temp/`, so the `${{ }}` spelling writes the credential to a
   second on-disk location and into the expression-expansion path.
2. **The helper is created private and stays private** — `umask 077` before the
   heredoc, or `install -m 600` then `chmod 700`. `$RUNNER_TEMP` is not private by
   construction, and the helper's whole job is to print a JSON object containing the
   credential on stdout. The repository already knows the right shape and this ADR
   cites it two sections later: `~/.config/ocx-telemetry/env`, **mode 600**
   (§ Observability).
3. **Both files are deleted in an `if: always()` step.**
4. **No upload step may take a path under `$RUNNER_TEMP`** — `verify-basic.yml`'s
   `smoke` job already runs `actions/upload-artifact` (`:178-186`), and
   `bep_to_otlp.py`'s inputs must be written elsewhere.

**Red state** (the ADR demands one of every other gate): a job step that asserts
`stat -c '%a' "$RUNNER_TEMP/bazel-cache-helper"` is `700`, and that
`grep -rl "$BAZEL_CACHE_WRITE" "$GITHUB_WORKSPACE"` returns nothing. Show it red by
creating the helper with `chmod 644` in a scratch run, and by planting the credential
in a workspace file; restore; show green.
5. **Lane gating — and the fork-only premise the previous draft reasoned from is
   false for this repository.** The old text concluded "the write secret is never
   in scope for a PR-triggered job" from GitHub's documentation that fork PRs get
   no secrets. GitHub withholds secrets from **fork** PRs only. **A pull request
   opened from a branch in the same repository receives the full `secrets`
   context** — and same-repo is the only PR shape this project uses (`goat`,
   `evelynn`, `sion`, `soraka`, `feat/*`). BZL-CACHE-01 (MUST) says so in as many
   words, verbatim:

   > Every CI lane an untrusted contributor can trigger — **any pull request, fork
   > or same-repo** — runs the cache read-only, with the write credential
   > **absent from that lane's environment, not merely unused**.

   The precedent this ADR points implementers at makes the failure concrete:
   `verify-basic.yml:92-100` is a **job-level `env:` block** carrying
   `AWS_SECRET_ACCESS_KEY: ${{ secrets.SCCACHE_AWS_SECRET_ACCESS_KEY }}`, and its own
   comment at `:90-91` says "a **fork** PR has no secrets" — i.e. a same-repo PR does,
   and gets a live write credential for the existing shared compile cache today.
   Copying that **placement** for the bazel-cache credential would put the secret in
   the environment of every same-repo PR run of the very job that holds it, leaving a
   flag value as the only thing between an untrusted contributor and the shared
   Action Cache. `--remote_upload_local_results=false` is the rule's *verification*
   column, not its control.

   **The mechanism, which is the rule's own named portable shape** — "a composite
   action that appends the authorization header only when its secret input is
   non-empty … with the workflow supplying that input only on a trusted event":

   ```yaml
   # verify-basic.yml, job `smoke`
   env:
     BAZEL_CACHE_WRITE: ${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')
                            && secrets.BAZEL_CACHE_WRITE || '' }}
   ```

   The credential is **never** placed in an unconditional job-level `env:` block. On
   any other event the variable is the empty string, the composite action writes no
   rc file, and the credential is absent from the environment rather than merely
   unused.

   Everything else stands: PR lanes stay on `pull_request` (**never**
   `pull_request_target`); every non-main lane carries
   `--remote_upload_local_results=false` in its effective flags, since BZL-CACHE-01's
   verification treats that flag's *absence* in an untrusted lane as the finding;
   write happens only under the predicate above. **This is new plumbing, not a
   copy:** no job in either workflow is ref-gated at job level today — the only ref
   gating that exists is `verify-basic.yml:123`'s
   `save-if: github.ref == 'refs/heads/main'` on a cache step.

   A2's write-isolation test (run the PR lane with the credential deliberately
   present, confirm zero `PUT` lines) is **retained as defence in depth** and
   relabelled as such — it deliberately constructs the state BZL-CACHE-01 forbids, so
   it cannot be the control. Its true counterpart is added: **on a same-repo PR run,
   assert `BAZEL_CACHE_WRITE` is empty.**

5b. **The read credential — new, because reads are 401 (§ Context).** The design
   assumed anonymous reads; measurement says otherwise, so a read path has to be
   built rather than inherited. **Orchestrator ruling, pending owner ratification.**

   **Server side (a sibling `server-hetzner1` PR, which the goal already allows — the
   Grafana dashboard lands there too).** Split the vhost's single realm in two so
   reads and writes take different credential files:

   ```nginx
   location / {
       auth_basic "bazel-cache";
       auth_basic_user_file /data/auth/bazel-cache-readers.htpasswd;  # readers
       limit_except GET HEAD {
           auth_basic_user_file /data/auth/bazel-cache.htpasswd;      # writers, unchanged
       }
   }
   ```

   Bazel presents one credential per build, so a writer must also exist in the
   readers file or its own GETs return 401 (measured on the real stack,
   server-hetzner1 branch `bazel-cache-reader-realm`, 2026-09-21).

   — i.e. the reader realm is the default and `limit_except GET HEAD` re-asserts the
   writers file for every other method. One `bazel-cache-readers.htpasswd` account for
   reads, the existing account for writes. `/srv/sh.ocx/bazel-cache/README.md:7-8`
   ("reads are anonymous") is corrected in the same PR, since leaving it is how this
   premise got into a ratified decision in the first place.

   **Client side.** `BAZEL_CACHE_READ_AUTH` on same-repo PR lanes and the `main`
   lane; `BAZEL_CACHE_WRITE_AUTH` on the `main`-push lane only, under ruling 5's
   trusted-event expression. Both injected by the workflow, neither committed. Local
   developers put the **read** header in the gitignored `.bazelrc.user`.
   **Fork PRs carry neither and get `--disk_cache` only** — that is a real capability
   difference for outside contributors and it belongs in the new dev-setup docs page,
   not only here.

   **Two rule reconciliations, written out so a later reviewer does not re-raise a
   closed finding:**

   - **This is not F2 returning.** F2 struck a *write* helper from `.bazelrc.user`
     because BZL-CACHE-02 (MUST) forbids a second un-gated **writer**. A **read**
     credential there is the opposite case: the rule's own text is "keep write access
     **strictly narrower than read access** — never symmetric", so broad read plus
     one gated writer is the shape the rule asks for, not a violation of it. The
     -02 enumeration still returns exactly **one** un-gated writer. Re-raising F2
     against the read header would be reading the rule by the file it lives in rather
     than by what the credential can do.
   - **BZL-CACHE-03's "never a bare `--remote_header`" governs the write
     credential.** Its rationale is entirely about a *write* token being "long-lived,
     unscoped, readable by anything that reads the file, and visible in process
     argv", and its verification greps rc files for the credential that can poison the
     cache. A read header on a public-source repository grants the ability to fetch
     blobs whose keys are already derivable from a public tree (ruling 6a), so the
     blast radius it protects is absent. **Reading:** the write credential keeps
     `--credential_helper` from the CI-only rc file (ruling 4a), unchanged; the read
     credential may be a `--remote_header` in `.bazelrc.user` or a workflow `env:`.
     If the owner prefers symmetry, a second `--credential_helper` entry for the read
     host is a drop-in and costs one more rc line — stated so the choice is visible
     rather than assumed away.

   **Red state**, same standard as every other gate here: on a fork-PR lane, assert
   both `BAZEL_CACHE_READ_AUTH` and `BAZEL_CACHE_WRITE_AUTH` are empty **and** that
   the build still exits 0 with `WARNING: Remote Cache:` and zero remote hits. Show
   it red by supplying the read credential to that lane and observing hits.

5a. **Where the gate may be placed, and two triggers that do not write.**
   - **`workflow_call` inverts the gate's context.** `verify-deep.yml:5-10` is a
     reusable workflow composed into `release-readiness.yml`, and its own comment at
     `:77-79` records that "Under `workflow_call` the `github` context is the
     CALLER's". The predicate `github.event_name == 'push' && github.ref ==
     'refs/heads/main'` therefore evaluates against the *caller* wherever it sits in
     a reusable workflow or a composite action. It is safe today only because the
     write lives in `verify-basic.yml`, which `verify-basic.yml:18-20` records as
     neither reusable nor composed — **an accident of placement, now a rule**: the
     ref gate is placed **only in a non-reusable workflow**, never in a composite
     action under `.github/actions/**` and never in a `workflow_call` target.
   - **`merge_group` and `workflow_dispatch` do not write, and that is intended.**
     Both workflows carry `merge_group: {}` (`verify-basic.yml:11`,
     `verify-deep.yml:28`) and `verify-deep.yml` carries `workflow_dispatch:`
     (`:4`); neither satisfies `event_name == 'push'`. The consequence, said out
     loud: **the merge-queue run that immediately precedes a merge never warms the
     cache**, so the first `main` push after a merge is a cold-ish write. Accepted —
     a queue run is triggerable by anyone who can open a PR.
   - **`permissions:`.** The `smoke` job has no job-level override today and
     inherits `contents: read`, `checks: write`, `pull-requests: write`
     (`verify-basic.yml:13-16`). A job holding a cache-write credential does not also
     need write on checks and pull requests: **the `smoke` job gets an explicit
     job-level `permissions: { contents: read }`**, per `subsystem-ci.md:100`
     ("declare at workflow level, elevate per-job"). No new third-party action is
     introduced, and `ocx-sh/setup-ocx` stays SHA-pinned (`verify-basic.yml:58`).
6. **The residual risk, named — and what it operates on.** With no remote
   execution, the write path is "main-branch CI runs `bazel test` on a
   GitHub-hosted runner, then uploads". That is the "CI is the trusted uploader"
   model, and [bazel#4276](https://github.com/bazelbuild/bazel/issues/4276) is the
   production incident where it failed with **no adversary**: a CI instance low on
   disk produced a malformed-but-exit-0 result that poisoned every downstream
   consumer. PRs cannot be the *source* of poisoning here; they can be a *victim*
   of it. This is accepted, not mitigated, and it is why the website rule's
   hermeticity checks are a precondition rather than a follow-up.

   **The asymmetry the risk operates on, which the previous draft never wrote
   down.** The remote cache is two stores and they have opposite trust properties:

   - The **CAS** (content-addressed store) is **self-verifying**: Bazel rehashes
     every downloaded blob against the digest it asked for, so a wrong blob is
     detected by the client.
   - The **Action Cache** is **input-addressed and not verifiable from its key**.
     Research 3 finding 6 states it plainly: "there is no way to verify the validity
     of an AC entry based on its key." bazel-remote's `--disable_http_ac_validation`
     toggles a **proto well-formedness** check only — never whether the referenced
     blobs are the honest output of that action.

   So the attack, and the accident, are the same shape: **upload an ordinary blob to
   CAS, then repoint one AC entry at it.** *Who was allowed to PUT* is the entire
   control. Everything in rulings 4a, 4b and 5 exists to keep that set at one actor.

6a. **Read as a content channel — the anonymous half is now moot, the invariant is
   not.** § Context measured reads at **401**, so there is no *anonymous* content
   channel to analyse: the exposure is bounded to whoever holds
   `BAZEL_CACHE_READ_AUTH` (ruling 5b), which is CI lanes and developers. The
   analysis below is retained in full anyway, for two reasons — it is the correct
   analysis if the owner ever opens the read side (§ Context branch (c)), and its
   **standing invariant is independent of who can read** and is the part that gates.

   Under stage 1 every `rust_test` target's outputs — `test.xml` and `test.log`
   among them — are uploaded to the CAS from the credential-holding main lane and
   are readable by anyone who computes the key. **Keys are not guessed**: they are
   deterministic functions of a public tree, a public `BUILD.bazel` and a pinned
   toolchain, so any reader with the same checkout derives them (research 3 finding
   19: "the real exfiltration path is **prediction, not brute force** … **except**
   where an action's inputs or its output embed a value that is not meant to be
   public"). **This repository is public, so the compiled form of readable code plus
   its test logs is not a disclosure.** Said once, and moved past.

   What it *does* establish is a standing invariant, which is the part worth
   keeping: **no cacheable action on the writing lane may read an ambient secret.**
   Concretely — no `--action_env` for a secret-bearing variable, and no target
   tagged `no-sandbox` without a cache-excluding tag — because if one ever does, the
   world-readable CAS is the recovery channel. That invariant has a gate already:
   ruling 1a's query, plus the `--action_env` ban in ruling 3. The `smoke` job's
   job-level `env:` today carries `AWS_SECRET_ACCESS_KEY` (`verify-basic.yml:100`)
   and `OTEL_OTLP_AUTH` (`:105`); `--incompatible_strict_action_env` keeps both out
   of actions, and this ruling makes that a **standing invariant of the writing
   lane** rather than a default nobody is watching. `--remote_cache` traffic is TLS
   to the explicit `https://` scheme the `.bazelrc` pins — which is also what stops
   an on-path reader seeing CAS contents in clear.

6b. **Detection, recovery and rotation — three lines that cost nothing now and are
   unrecoverable later.** The previous draft accepted the residual risk with no exit,
   while quoting Bazel's own "wiping the entire remote cache is not a feasible
   solution".

   1. **A cache generation salt.** Run the remote cache under
      `--remote_instance_name=v1` (or a namespaced URI prefix). Abandoning a
      suspected-poisoned generation then costs **one string bump in `.bazelrc`**
      rather than a 50 GB wipe and a coordination exercise. This is the single
      cheapest line in the section and it is only cheap before the cache is warm.
   2. **Detection.** Alert on any `PUT` to the cache host outside a `main`-lane run
      window. bazel-remote already exports the counters and Prometheus already
      scrapes it (§ Context), so this is a Grafana alert rule, not new
      infrastructure. Its red state: a deliberate authenticated `PUT` from a
      workstation must fire the alert.
   3. **Rotation.** The htpasswd realm holds exactly one account (ruling 4a). It is
      rotated **on adoption, then every 90 days, and immediately on any detection
      hit**, owned by the repository owner. A schedule with no owner is not a
      schedule.
7. **Keep the nginx read/write split.** Do not move to bazel-remote's native
   `--allow_unauthenticated_reads` without first ruling out
   [buchgr/bazel-remote#468](https://github.com/buchgr/bazel-remote/issues/468)
   (`.htpasswd` returning 401 on PUT in some config combinations) against the
   pinned v2.6.2. The nginx split sidesteps that bug entirely and needs no change.
8. **Confirm the metrics surface is not on the anonymous listener.**
   bazel-remote's `/status` and `--enable_endpoint_metrics` expose aggregate
   size/count/hit-rate — not content, but a real information-disclosure and
   DoS-targeting surface. Whether hetzner1's nginx already separates them is
   **UNVERIFIED**; it is a config-review item for the server repo, not a code
   change here.
9. **`git_override` carries no `integrity` hash**, unlike a BCR release —
   verification is delegated entirely to git's commit hash, with no independent
   second attestation and no registry-level review. **Obligation:** every
   `rules_ocx` commit-pin bump is treated as a reviewable supply-chain event —
   the PR diff shows old SHA → new SHA and names what changed upstream.
   **Exit:** the override is removed in the same commit that bumps to the first
   BCR release of rules_ocx. Whether `MODULE.bazel.lock` records a re-diffable
   content hash for a `git_override` is **UNVERIFIED** — treat the lockfile as
   reproducibility tooling, not a security control.

   **What the commit-pin review is actually reviewing.** The analysis above stops at
   *content integrity*, and that is the smaller half. **A Bazel module extension
   runs arbitrary code at analysis time, outside the sandbox, with the full client
   environment**, and `ocx.project()` additionally shells out to a binary that
   performs network pulls. After this lands, `git clone && bazel query //...` on
   this repository executes an unreviewed branch of a second repository plus network
   fetches — **before any `test`, `build` or human read**. This is the same property
   the ADR establishes four rulings earlier for credential helpers, citing the
   vendor's own verdict that it is intended behaviour (bazel#30439); it transfers
   here unchanged. So the pin-bump review is reviewing **code that will run on every
   contributor's machine**, not a version string. Three consequences:

   - **The pinned SHA must be on a protected branch of `rules_ocx` with force-push
     disabled**, so the pin target cannot be made unreachable and history cannot be
     rewritten under it.
   - **Availability, which the NFR table did not carry:** with rules_ocx on the
     tool path, an unreachable git remote **fails the build closed** — unlike the
     cache outage, which degrades to a slow green. The two are not the same
     posture and must not be summarised as one.
   - **If `rules_ocx` is private**, a git credential becomes a prerequisite for
     `task verify` on a fresh clone. Its visibility is owner-attested and not
     verifiable from here; WP-0b records it, because the answer changes the
     contributor onboarding story.

10. **BZL-CACHE-12 — the rule paired with the download mode this ADR chose, and the
    previous draft cited only its sibling.** `.bazelrc` takes
    `--remote_download_minimal` on CI per BZL-CACHE-11, and BZL-CACHE-12 (SHOULD)
    measures exactly that shape (`bazel-quality/caching.md:117`): "a `toplevel` (or
    **`minimal`**) cache hit leaves intermediates as CAS references and a later
    locally-executing action needs an evicted blob — but the caller sees exit 0
    (Bazel retries the whole build under a fresh invocation ID and re-executes
    locally) or, at `retries=0`, a **generic exit 1 indistinguishable from a compile
    error**." Selecting `minimal` maximises exposure to that class, so the paired
    position is stated rather than omitted:
    - Leave `--experimental_remote_cache_eviction_retries` at its **default 5**.
      Never `0`, and never `0` with `--rewind_lost_inputs` believing that covers it.
    - **Never key CI retry or alerting logic on exit code 39.** Bare 39 never
      reached the caller in the rule's ten measured invocations.
    - The durable signals are the **error strings**: `lost inputs with digests:`,
      `Found transient remote cache error, retrying the build...`,
      `Lost inputs no longer available remotely:`, `lost input too many times (#21)`.
    - `--experimental_remote_cache_ttl` stays unset (measured default 3h) unless the
      server's real eviction window is known and shorter (BZL-CACHE-13).

    **Capacity, which nothing in the previous draft sized.** The cache is **50 GB**,
    LRU, holding release-profile Rust artefacts (`test:unit` runs
    `cargo nextest run --workspace --release`) for a 23-package workspace, on every
    `main` commit. Eviction is not hypothetical at that shape. **WP-1c measures the
    working set** — the CAS bytes one cold `bazel test //crates/...` uploads, and
    the marginal bytes a one-crate change adds — and the answer decides whether 50 GB
    is sized for this workload or whether the cap moves. § NFR Cost's "already
    provisioned" is about the *host*, not about the *size*, and the two were
    conflated.

11. **The degraded cache, which is the common case and worse than the dead one.**
    NFR Availability rules on the *outage* case and rules correctly (BZL-CACHE-26).
    The outage is the easy case: it is loud and it is green. The common case is a
    **slow** single Hetzner box with no failover, serving thousands of AC lookups per
    build from GitHub-hosted runners over a link nobody has measured.
    `--remote_timeout` defaults to 60 s **per operation**, so a cache that is slow
    rather than refusing connections makes the build slower than no cache at all,
    green, with nothing in the design noticing. The positions, in `.bazelrc`:
    - **`--remote_timeout=30s`** — below the default, because on this shape a
      per-operation wait longer than the action it is trying to avoid is never a
      win.
    - **`--remote_retries=2`** — stated explicitly rather than inherited, so the
      worst case per operation is bounded and readable.
    - **The circuit breaker is WP-1c's threshold, not a flag:** if the measured
      median RTT or the warm-cache wall-clock fails R2's ratified number
      (§ Orchestrator rulings), the remote cache is dropped from the lane in favour
      of `--disk_cache` + `actions/cache`. There is no in-band Bazel mechanism for
      "this cache is too slow to be worth it"; the decision is made once, from a
      measurement.

12. **Resource envelope on the development host — stated as numbers, because the
    defaults do not fit it.** Measured 2026-09-21: **32 GB RAM** (11 GB used, 3 GB
    swap), **32 cores**, `/tmp` tmpfs **76 % full**, and **8 live `rust-analyzer`
    processes**. Bazel's defaults are hostile to exactly this shape — the JVM sizes
    its heap from total RAM, and `--jobs` defaults to the core count, so an
    unconfigured `bazel test //...` would ask for ~32 concurrent actions on a host
    already carrying eight LSP servers and (per the project's build-slot discipline)
    at most two build-capable agent workers. The `.bazelrc` values, each with the
    constraint it is sized against:

    | Flag | Value | Why it fits |
    |---|---|---|
    | `--host_jvm_args=-Xmx2g` | 2 GB | Bazel's server is a scheduler, not a compiler; 2 GB is ample for a 274-target graph and leaves the 21 GB headroom for `rustc` and the eight `rust-analyzer` processes that are already resident. Default heap sizing off 32 GB total would reserve multiples of this for no benefit. |
    | `--jobs=12` | 12 | Matches the repository's existing `cargo build` job cap, which `project_cargo_jobs_temporarily_lowered` records as **a RAM cap, not a speed choice** — the same constraint binds Bazel, and using a different number for the same machine would be a second, contradictory answer to one question. |
    | `--local_test_jobs=1` | 1 | Not a resource choice — stage 4's compose stack is on fixed ports (§ Stage 4). Stated here so a later reader tuning `--jobs` upward does not read it as one and raise it. *(Superseded by `adr_test_speed_tiers.md` AM-9: `--local_test_jobs=$ACCEPT_JOBS`, default `min(8, nproc)`, on `task bazel:test:accept`'s command line only.)* |
    | `--disk_cache` location | **under `$OCX_HOME`, never `/tmp`** | `/tmp` is a tmpfs at 76 % that this host's reaper also sweeps, and a tmpfs disk cache is RAM. Sized with `--experimental_disk_cache_gc_max_size` (BZL-CACHE-17: it defaults to `"0"` = unbounded even where supported). |

    **CI runners are the opposite shape** — 2–4 cores, no LSP, ephemeral — so these
    are **local** values in `.bazelrc.user` or a host-conditioned block, and the CI
    lane keeps Bazel's own defaults. Writing one number for both would mis-size both.

### Observability

A bespoke post-build script, `scripts/bep_to_otlp.py`, modelled on
`taskfiles/telemetry.taskfile.yml`'s junit2otlp push. Verified properties of that
push, which the new script copies: gRPC **only**;
`OTEL_EXPORTER_OTLP_ENDPOINT` + `OTEL_EXPORTER_OTLP_HEADERS=Authorization=Basic …`
sourced from `~/.config/ocx-telemetry/env` (mode 600); a silent no-op when
unconfigured (`[ -n "${OTEL_EXPORTER_OTLP_ENDPOINT:-}" ] || exit 0` at L50-51).

**Two corrections to the dossier's design:**

1. **`TargetComplete` has no timing field.** It carries `success`,
   `output_group`, `tag` and `test_timeout` only. Per-target wall time must come
   from:
   - **test targets:** `TestResult.test_attempt_duration`
     (`google.protobuf.Duration`), from the BEP;
   - **non-test targets:** a `SpawnMetrics`-based aggregation
     (`total_time` / `execution_wall_time` in `spawn.proto`) keyed by
     `target_label`, from the execution log. There is no BEP-level wall-clock
     field for a `rust_library` with no test.
2. **The flag is `--execution_log_compact_file`**, no `experimental_` prefix —
   confirmed current at the 9.2.0 tag (`@Option(name =
   "execution_log_compact_file", oldName = "experimental_execution_log_compact_file")`).
   The old spelling is a backward-compat alias only.

**Cache hit/miss** comes from `ExecLogEntry.Spawn.runner` (string) +
`ExecLogEntry.Spawn.cache_hit` (bool). Documented semantics: a cache hit renders
as `"disk cache hit"` or `"remote cache hit"`; **spawns whose owning action hits
the persistent local action cache are never reported at all** — an action's
*absence* from the log is the positive signal for a local hit, not a bug
(BZL-CACHE-15). Bazel ships a reference parser at `//src/tools/execlog`.

**Budget for the failure class this pipeline already has.**
`.claude/rules/subsystem-ci.md` records **two silent-failure classes found in
this exact telemetry pipeline**: a 1 MB-per-line stdin scanner that lost whole
reports, and a 2,048-span default export queue that silently dropped spans. The
new script budgets for both from day one — an unbounded (or explicitly sized past
the target count) export queue, and no single-line ceiling on its input — rather
than rediscovering them.

#### Gate contract — `scripts/bep_to_otlp.py`

The only new component the previous draft left with prose and no contract — no exit
codes, no failure text, no reader floor, no span schema — in a document that gives
every other new script a five-row table. A tester could not write a failing test from
it, and A5's "Tempo span count == BEP target count" was an assertion about a live
Grafana round trip rather than something the script could be tested against offline.

| | |
|---|---|
| **Reads** | `--build_event_json_file` (the BEP) and `--execution_log_compact_file`. **Only these message types**: `TestResult`, `TargetComplete`, `ExecLogEntry.Spawn`. See the allowlist below. |
| **Emits** | One span per **target**, plus one child span per spawn. Span name = `target_label`. Attributes: `ocx.bazel.target` (label), `ocx.bazel.runner` (`ExecLogEntry.Spawn.runner`), `ocx.bazel.cache_hit` (bool), `ocx.bazel.mnemonic`, `ocx.build.id` (the BEP's invocation id), `ocx.bazel.duration_ms` (`TestResult.test_attempt_duration` for tests; `SpawnMetrics.total_time` aggregated by `target_label` otherwise — `TargetComplete` has no timing field). Trace id = the invocation id, so one build is one trace. |
| **Exit 0** | Spans successfully exported == targets read, **and** `OTEL_EXPORTER_OTLP_ENDPOINT` was set. |
| **Exit 0 (no-op)** | `OTEL_EXPORTER_OTLP_ENDPOINT` unset → exit 0 silently, matching `taskfiles/telemetry.taskfile.yml:50-51`'s existing behaviour. This is the **only** sanctioned silent exit. |
| **Exit 1** | Exported count != targets read. stderr: `bep_to_otlp exported <s> spans for <n> targets — the export dropped spans` (this is the queue-drop class, and it is the failure the pipeline has already shipped once). |
| **Exit 1** | Reader floor **on this reader's own subject**: BEP absent or empty; zero `TestResult` **and** zero `TargetComplete` events; or, on a `//crates/...` build, fewer than **54** targets — this script emits one span per *target* including `rust_library`, so its universe is 20 `rust_library` + 34 `rust_test`, **not** the 34 rows of `crates/TEST_TARGET_MAP.toml`. Flooring it on the map would pass a reader that saw only the test half. stderr: `bep_to_otlp read <n> targets and <m> spawns, expected >= <k> for this invocation's pattern — the reader stopped early`. Without it, a reader that parsed nothing is indistinguishable from a clean build. |
| **Exit 1** | A BEP field outside the allowlist reached the span builder. stderr: `bep_to_otlp: refused field <name> — not in the read allowlist`. |
| **Red state** | (a) Set the OTLP export queue below the span count → exit 1 with the drop message; restore → exit 0 and equality. (b) Point it at an empty BEP → exit 1 on the reader floor. (c) Unset `OTEL_EXPORTER_OTLP_ENDPOINT` → exit 0, silent, zero spans. (d) Feed a fixture BEP carrying a planted `--remote_header=Authorization=secret` in `structured_command_line` → the emitted spans must not contain the string, asserted by the unit test below. |

**Field allowlist, and why it exists even though the current exposure is closed.**
`--credential_helper` means no secret is ever a flag value, so the credential never
enters the BEP at all and the `structured_command_line` scrub research 3 asked for is
not needed **today**. That closes the current exposure, not the class: this script
runs on the one lane that holds a credential, and its input is a designed-in,
fully-expanded transcript of every flag plus every action environment. **The script
reads only `TestResult`, `TargetComplete` and `ExecLogEntry.Spawn`, and never
`structured_command_line`, `unstructured_command_line`, `OptionsParsed` or
`WorkspaceStatus`.** Asserted by a unit test over a fixture BEP with a planted
`--remote_header=Authorization=<secret>` line: the emitted spans must not contain it,
and the test is shown red by removing the allowlist.

**Reuse before writing.** Bazel ships a reference parser for the execution log at
`//src/tools/execlog`. WP-1's first question for this script is whether that parser
can be shelled to for the compact-log half, leaving only the BEP JSON and the OTLP
emit as hand-written — `quality-core.md` § "Don't Own Non-Domain Code" asks this of
any new format reader, and a bespoke protobuf reader for a format upstream already
parses is the shape the rule names. If it cannot, the ADR's answer is the allowlist
above plus the fixture test, and the reason is recorded in the plan rather than
discovered later.

**Acceptance check, retained from G3.5: Tempo span count == BEP target count for
one build.** `subsystem-ci.md` states this check was the only thing that caught
the queue-drop bug. Its **red state** is row (a) above, and it is now reproducible
offline against the script rather than only through a Grafana session.

Dashboard on server-hetzner1 beside the provisioned `cache.json` and
`test-time.json`. No BuildBuddy, no BES backend, no new service.

### rules_ocx as the sole tool path

`MODULE.bazel` carries exactly one tool-provisioning call:
`ocx.project(ocx_toml = "//:ocx.toml", ocx_lock = "//:ocx.lock")`, yielding
`@tools//:bun`, `@tools//:uv`, `@tools//:lychee`, `@tools//:agg`, and so on.
**No `http_file` or `http_archive` for anything ocx can package.** Only
rustc/cargo stay on rules_rust's own toolchain; the Nerd Font stays an
`http_archive` with `integrity` because it is an asset, not a tool.

`ocx.toml`'s nine current tools, verified: `actionlint`, `bun`, `cosign`,
`git-cliff`, `go-task`, `lychee`, `shellcheck`, `shfmt`, `uv`. The identifier
convention is `ocx.sh/<org>/<name>[:version]` — e.g. `ocx.sh/oven-sh/bun:1`,
`ocx.sh/astral-sh/uv:0`, `ocx.sh/sigstore/cosign:3.1.1`. Two entries are added:

| Entry | Status | Consequence |
|---|---|---|
| `bazel = "ocx.sh/bazelbuild/bazel:9.2.0"` | **Published and enumerated — measured 2026-09-21, no longer a WP-0 assumption.** `ocx --remote index list` returns 9.x tags **9.0.0, 9.0.1, 9.0.2, 9.1.0, 9.1.1, 9.2.0** plus the rolling `9`, `9.0`, `9.1`, `9.2`. `ocx --remote package inspect ocx.sh/bazelbuild/bazel:9` resolves `sha256:252fc12c…` with candidates darwin/amd64, darwin/arm64, **linux/amd64+libc.glibc**, **linux/arm64+libc.glibc**, windows/amd64. | `9.2.0` is the newest GA in the mirror and resolves for linux/amd64 — the WP-0 escape ("pick the nearest resolvable 9.x") is **discharged, not carried**. Two consequences worth naming: (i) the exact-tag decision is now **evidenced** — `:9` and `:9.2` both exist as rolling tags, so pinning `:9.2.0` is a live choice against two available floating alternatives, not a hypothetical; (ii) **there is no linux/arm64 musl candidate**, so an arm64 Alpine runner or container has no Bazel from this path. Out of scope today (every lane is glibc), and a hard stop the day one is introduced. |
| `agg = "ocx.sh/asciinema/agg:<v>"` | **Does NOT exist, and there is no sibling to add it to.** Not published under `ocx.sh`, and the `../mirror-*` siblings are **astral-sh, bazelbuild, kitware, pypi** — **no `mirror-asciinema`** (verified from this session: no such directory beside the others). | So the prerequisite is **create a new mirror repository**, not add a tag to an existing one — a materially larger work package than the previous draft implied, and it stays on **stage 3's critical path**. Pattern to follow: `../mirror-bazelbuild/bazel/mirror.yml` — a `tag_pattern` (`"^(?P<version>\\d+\\.\\d+\\.\\d+)$"`) over upstream GitHub releases, with a `mirror-base.yml` beside it and a per-tool workflow under `.github/workflows/`. Size WP accordingly: new repo + base spec + `agg` spec + CI + first publication, not a one-line tag bump. |

**Provenance of the two new packages, stated because they are build-critical.** One
of them is the **build engine itself**, which is the highest-value supply-chain
target in this design. The digest pin in `ocx.lock` gives *integrity* — the same
bytes as last time, and the `:9.2.0`-not-`:9` correction above strengthens it — but a
digest attests nothing about *provenance*: it does not say the bytes are the ones
upstream published. The posture:

- **Today's posture is inherited, not a regression:** `ocx.toml` declares no
  `[[trust.policy]]` and `ocx.lock` carries per-platform digests only; the nine
  existing tools are digest-pinned and unsigned. The two new entries widen that set
  rather than changing its shape.
- **Obligation on the two new entries:** the mirror publication records the
  **upstream release checksum** it was built from — upstream publishes SHA-256 for
  every Bazel binary — so the chain terminates at an upstream attestation rather
  than at the mirror author. `agg` does not exist yet, which makes its **first**
  publication the one moment when pinning provenance is free.
- **`ocx package sign` on both is the cheapest possible dogfood of this product's
  own differentiator #12** (keyless Sigstore signing, identity-pinned
  `[[trust.policy]]`), applied to the two packages that most warrant it. Recorded as
  a plan-docket item rather than a decision, because it touches the mirror repos.

**rules_ocx is at v0.4.0, not 0.1.0 — a correction, and it resizes the work
package.** Measured 2026-09-21 and verified from this session against the working
copy: `../rules_ocx` clean on `main` @ `825f20b`; `MODULE.bazel:10` declares
`version = "0.4.0"`; `.bazelversion` reads `8.7.0`; and the `ocx.project()` module
extension **already exists and works** — `ocx/extensions.bzl` defines the `ocx`
`module_extension` with a `project` tag class carrying `ocx_toml` / `ocx_lock` label
attributes, a root-module-only guard ("`ocx.project()` may only be used by the root
module"), and it dogfoods its own `ocx.toml`/`ocx.lock`.

The dossier said "API 0.1.0" and earlier drafts of this ADR repeated it. **Both were
wrong**, and the error mattered: it made rules_ocx read as an unbuilt dependency with
"missing API", which is what justified treating it as a blocker. What "outdated"
actually means here is narrow and specific:

- **the 8.7.0 `.bazelversion` pin**, and
- **being unverified on Bazel 9.x**

— *not* missing features. So WP-0b is **a Bazel-9 verification and version bump on an
existing, working extension**, not a build. Resize it accordingly.

This also weakens Q9's coupling objection in this ADR's favour, and that is recorded
rather than quietly banked: the review seat's case against making stage 1 depend on
rules_ocx rested partly on it being an unread, feature-incomplete module. It is
neither. The **sequencing** ruling stands anyway — stage 1 consumes no rules_ocx tool,
so the dependency buys nothing there (§ Stage 1 sequencing) — but it now rests on
"zero benefit at that stage" alone, which is the honest ground, rather than on risk
that measurement has removed.

Its state remains **owner-attested for anything beyond the working copy** (what is
published, what CI does). Work happens in
`../rules_ocx/.agents/worktrees/<slug>` on its own branch, consumed here via
`git_override(module_name = "rules_ocx", remote = …, commit = <sha>)`. Per the
owner's mandate, every gap the migration exposes (Bazel 9 compatibility, missing
`binaries` claims, CI bootstrap through `setup-ocx`) is fixed in rules_ocx or in
the mirror package — never worked around here. The migration is rules_ocx's first
real consumer, which is the point.

**It enters at stage 3, not stage 1** (§ Stage 1 sequencing): the mandate is a
composition rule about *what may provide a tool*, and it binds in full the moment a
tool is needed. It does not also fix *when* the path is first exercised, and the
previous draft read it as if it did — routing the pilot's critical path through an
out-of-tree Bazel-8.7.0 module for zero stage-1 benefit. Recorded explicitly so a
later reader does not read the sequencing as the mandate being weakened.

### Non-functional requirements

| NFR | Position |
|---|---|
| **Scalability** | **Target count, re-derived against the corrected shapes — and it is roughly double what the previous draft estimated.** 23 `rust_library` (20 members + 3 `external/`) + **34** `rust_test` (§ Stage 1) + **39** cast (§ Stage 3) + **172** acceptance `sh_test` (one per `test/tests/test_*.py`, enumerated; the previous draft's "~15" counted `SCOPED_ROWS` rows, which § Stage 4 no longer uses as the unit) + ~6 website/schema ≈ **274 rule targets**. **BZL-CI-01 (MUST, pinned) is quoted rather than paraphrased, because its own verification names a bad paraphrase as the finding**: "Cross into 'consider target selection' **only when** the whole-repo job's median wall-clock on the widest CI runner is consistently past **~40 minutes**; treat **~300 rule targets** as the tripwire that says **measure the wall-clock now, not as the trigger itself**. Present both numbers as a derived tripwire, never as a published threshold." So ~300 is **not half of a trigger** — it is the signal that says *go measure*, and 274 crosses it. The switch point is the wall-clock, and only the wall-clock. Consequences: whole-repo `bazel test //...` stays the default, bazel-diff and target-determinator stay out of scope, and **what 274 obliges is the measurement WP-1c already owes**, read off `bazel query 'kind(rule, //...)' \| wc -l` beside the median. Adopting a selection tool below that switch point is what the rule calls premature adoption, and citing 274 as a reason to would be the "industry threshold" misreading its verification catches. (The previous draft's "both are below" was true only of a target set that undercounted stage 4 by an order of magnitude; stage 4's 172 targets are also why `--local_test_jobs=1` (§ Stage 4) is a real wall-clock cost rather than a footnote.) |
| **Availability** | The cache is a single Hetzner host with no failover. **BZL-CACHE-26 records, measured on 8.7.0 and 9.2.0 against an HTTP cache on Linux** (not against `bazel-cache.ocx.sh`, which this repository has never run Bazel against): with the endpoint refusing connections, `WARNING: Remote Cache: Connection refused`, every action executed locally, **exit 0**; all four combinations of `--remote_local_fallback` and `--incompatible_remote_local_fallback_for_remote_cache` produce the identical outcome, because with no `--remote_executor` there is nothing to fall back *from* (MUST: do not cite those flags as an outage policy). **Ruling: a cache outage degrades to a full local build, green and slow. Accepted; no pre-flight reachability probe.** `--remote_upload_local_results=false` is the *untrusted-lane write* control, not an outage control. **The degraded case is different and is ruled separately** (§ Cache staging ruling 11) — a slow cache is worse than a dead one and has no Bazel-side signal. **And the tool path does not share this posture:** with rules_ocx as the sole tool path from stage 3, an unreachable git remote **fails the build closed** (ruling 9). |
| **Latency** | **Unmeasured, and treated as such.** Hetzner ↔ GitHub-hosted runners at thousands of AC lookups per build is a WP-1c measurement, never an assumption. Measurement shape: median wall-clock of `bazel test //crates/...` from a GH-hosted runner with a warm remote cache, against the current `cargo nextest run --workspace` baseline on the same commit; the observed per-lookup RTT; the CAS working-set size per cold build and per one-crate change (ruling 10); and the `bunx vitepress build` chain's cold/warm wall-clock and invocation frequency (§ Stage 3). The `--disk_cache` layering question is decided by the same measurement. **The threshold is a decision, not a measurement, and it is written in this ADR before the numbers arrive** — Open Question #2 carries a proposed number for the owner to ratify or override. |
| **Security** | § Cache staging, in full. Summary: writes on one main-only lane, credential supplied only through a trusted-event-conditioned expression and carried by `--credential_helper` from a **CI-only rc file outside the workspace** (rulings 4a/4b/5); **no developer-machine write helper** (ruling 4a); reads are **401 — measured, not assumed** — so a read-only credential is supplied to same-repo lanes and the local loop while fork PRs get `--disk_cache` only (§ Context; ruling 5b, which also records why that read credential is not the BZL-CACHE-02 violation F2 closed); no `pull_request_target`; no remote execution; the AC/CAS asymmetry named with a generation salt, a detection alert and a rotation cadence (ruling 6b); `git_override` bumps reviewed as analysis-time code, on a protected branch (ruling 9). Residual risk (a flaky trusted uploader) accepted and named. |
| **Cost** | No new service, no new host, no new vendor. The 50 GB cache is already provisioned and already scraped — **that is about the host, not about the size**; whether 50 GB fits this working set is a WP-1c measurement (ruling 10). Incremental infrastructure cost ≈ bandwidth + one Grafana dashboard + one alert rule. The real cost is **maintenance**: `BUILD.bazel` as a second source of truth beside `Cargo.toml` (mitigated by the drift gate), a hand-written Starlark rule for the website, a second Rust-version pin, and four bespoke scripts (`bep_to_otlp`, `bazel_pin_check`, `bazel_build_drift`, `bazel_tag_guard` — the first of them a BEP reader with no upstream maintainer) — four surfaces that did not exist before. The **delivery** cost is priced in the trade-off matrix's own row rather than left out of it. |
| **Operability** | `task` stays the only entry point anyone is expected to type; `task verify` stays the gate. A contributor who never runs `bazel` is unaffected. Local opt-outs live in the gitignored `.bazelrc.user` (`try-import`, last line). Rollback is `git rm` of the file set plus restoring four Taskfile steps — the artifacts are deletable; what is not cheaply reversible is rules_ocx having become a real consumer and the CI lane swap. |

## Evidence and attestation status

Every claim in this ADR traces to a file:line in one of the **five** input
artifacts numbered in § Industry Context, to a file in this repository opened at the
cited line, or is marked below. Two inherited numbers (278,777 LOC, 63 module
cycles) come from `adr_crate_split_workspace.md` and are marked as inherited at both
use sites; the LOC figure is additionally marked stale.

**Owner-attested, not independently verified from this repository:**

1. **The hetzner1 server inventory** — bazel-remote v2.6.2, HTTP-only,
   `--max_size=50`, `--enable_endpoint_metrics`, nginx htpasswd on writes,
   no gRPC port, Prometheus scraping, Grafana provisioning `cache.json` and
   `test-time.json`. **This is the single largest unverified input to the plan**,
   and the entire cache-reuse decision rests on it. No file in this repository
   encodes any of it.

1a. **The read posture is not merely unverified — it is contradicted**, by the
   immediately preceding decision in the same series and by `hex.md`, both of which
   say reads are 401 (§ Context). An unverified input with a conflicting, dated prior
   is a different object from an unverified input with none, and it is the one item
   in this list that a WP-0 precondition resolves rather than carries.
2. ~~**rules_ocx's state**~~ — **no longer owner-attested; measured 2026-09-21.**
   v0.4.0 @ `825f20b`, `.bazelversion` 8.7.0, `ocx.project()` implemented in
   `ocx/extensions.bzl`. The earlier "API 0.1.0" was wrong and is corrected at
   § rules_ocx. What is still owner-attested is only what lives outside the working
   copy: what rules_ocx publishes, and its CI.
3. **`../mirror-bazelbuild`'s contents** — that it publishes bazel, bazelisk,
   buildifier, buildozer.

**UNVERIFIED, carried as such (no source found either way):**

4. Whether `--remote_header` values are masked in BEP JSON's
   `structured_command_line`. Treated as **not** masked (the safer assumption),
   which is one of four reasons this ADR chooses `--credential_helper`.
5. Whether `MODULE.bazel.lock` records a re-diffable content hash for a
   `git_override`. Treated as reproducibility tooling, not a security control.

**Corrections applied (claim diff and 2026-09-21 research win):**

| Dossier said | Corrected to | Source |
|---|---|---|
| BZL-JS-01/03 live in `bazel-quality.md` | `bazel-quality/typescript.md:93-94` | claim diff 1a (**WRONG**) |
| Pilot is step-4-compliant | Step 4 explicitly rejects "worst pain"; ruled a named deviation | claim diff 2c |
| The coarse-rule carve-out is unsupported | It is `branches-python-ts-cpp.md:108-111`'s own escape clause | claim diff 2d + this ADR |
| Both workflows carry floor/ceiling | Only `verify-basic.yml`'s `smoke` does | claim diff 4b |
| rules_rust default Rust = 1.94.0 | **1.98.0** | research 4 § 2 |
| rules_rust #3732 blocks us | `crates_vendor`-only; `from_cargo` unaffected | research 4 § 1 |
| Pin "the newest 9.x" | **9.2.0**; 9.3.0 is rc-only and unneeded | research 4 § 3, § 5 |
| gazelle_rust viability is open | On BCR 0.1.0; issue #5 closed; spike narrows to our pin pair + `[patch]` | research 4 § 4 |
| `agg` mirror is a contingency | Hard prerequisite, confirmed absent from the index | research 4 § 7 |
| Per-target time from `TargetComplete` | `TargetComplete` has **no timing field** | research 4 § 8 |
| `--experimental_execution_log_compact_file` | `--execution_log_compact_file` | research 4 § 8 |
| Bun ruleset might exist | None on BCR; coarse rule is the only option | research 4 § 9 |
| Write credential via `--remote_header` | `--credential_helper` (BZL-CACHE-03 MUST, new setup) | this ADR |
| `ocx.toml` pins `bazelbuild/bazel:9` | `:9.2.0` — a floating major lets an unrelated relock move Bazel | this ADR |
| Website inputs = `website/**` + generated | must also declare `test/doc_scripts/**/*.sh` | claim diff 8 |
| 72 cast targets, one per `doc_scripts/*.sh` | **39** — only the `# cast: true` set records; the other 33 are site-rule inputs | this ADR (enumerated) |
| One `rust_test` per member (20) | 20 lib + **14** integration = **34** test targets; 20 could not reach the 8213 floor | this ADR (enumerated) |
| `SCOPED_ROWS` defines the acceptance cache unit | It is not a partition (overlapping globs, two `escalate` rows); the unit is one target per test module, and stage 4 caches nothing | this ADR |
| Stage 4 cache validity rests on registry-state ownership | That is isolation, not input tracking; result caching is **disabled** (`external` + `--nocache_test_results`) | this ADR |
| Anonymous reads on `bazel-cache.ocx.sh` | **False — measured 401.** `ocx-sh-10-bazel-cache.conf:29-33` has `auth_basic` with no `limit_except`. The dossier's premise traced to a stale `bazel-cache/README.md:7-8` | measured 2026-09-21 |
| `rules_ocx` is "API 0.1.0" / missing features | **v0.4.0** @ `825f20b`, `ocx.project()` already implemented in `ocx/extensions.bzl`; "outdated" = the 8.7.0 pin + unverified on 9.x | measured 2026-09-21 |
| `ocx.sh/bazelbuild/bazel:9.2.0` resolution is a WP-0 assumption | **Published and enumerated** — 9.0.0…9.2.0 plus rolling `9`/`9.0`/`9.1`/`9.2`; no linux/arm64 **musl** candidate | measured 2026-09-21 |
| The `agg` mirror is a tag added to an existing sibling | **No `mirror-asciinema` exists** — it is a new mirror repository, patterned on `mirror-bazelbuild/bazel/mirror.yml` | measured 2026-09-21 |
| `verify-deep` median is 3347 s | **3407 s** (and verify-basic 1731 s), last 50 successful runs; the 3347 s predates `task test:parallel` | measured 2026-09-21 |
| The 2026-09-20 dossier chose a no-go | It recorded a **sequencing** decision whose step (3) is "re-measure, then run the gate" | research 5 `:17` |
| `rust:test:unit` has never had a `sources:` guard tried | Its absence is a documented decision with a stated mechanism | `taskfiles/rust.taskfile.yml:228-234` |
| Fork PRs get no secrets, so PR lanes are safe | Same-repo PRs **do** get secrets, and every PR here is same-repo (BZL-CACHE-01, MUST) | this ADR |
| A `.bazelrc.user` `--credential_helper` is fine because the file is gitignored | It is BZL-CACHE-02's named shape — the second un-gated writer | this ADR |
| The floor reader parses libtest's stdout `test result:` line | `$XML_OUTPUT_FILE` is the design; the harness already emits JUnit XML | this ADR |
| The hermeticity check requires a **miss** on an undeclared-input change | Inverted — a correct action is unaffected; split into declared-input invalidation, ambient-input isolation and tool-pin participation | this ADR |
| ~135–150 rule targets, well below BZL-CI-01's ~300 | **~270–280** — stage 4 was undercounted by an order of magnitude | this ADR (enumerated) |
| `.verify:build-test` is 13 steps, nine untouched | **14** steps, **ten** untouched | this ADR (enumerated) |
| `verify:scoped`'s per-crate nextest is at `taskfile.yml:152-155` | `:152-155` is `summary:` prose; the steps are `:206`, `:223`, `:225` | this ADR |
| The six `GITHUB_*` vars populate `ci.run_url` only | They populate `ci.run_url`, `ci.workflow`, `ci.git_ref`, `ci.sha` (`build_info.rs:146-157`); the conclusion is unaffected | this ADR |

## Open questions from the dossier — disposition

Nine entries: the dossier's single `## Open questions` bullet (research 1 `:169-171`,
`build.rs` under strict action env) plus the eight unresolved items at the tail of
`## Verification` (`:187-201`). The previous draft cited "lines 167-201", which also
sweeps in six `## Verification` bullets (`:175-185`) this table does not disposition —
those map to § Acceptance A1–A7 instead, so they are addressed, not skipped. Nothing
is left dangling.

| Dossier question | Disposition | Where it is settled / what the spike must produce |
|---|---|---|
| `crates/ocx_cli/build.rs` under `--incompatible_strict_action_env` | **DECISION — closed** | § Cache staging ruling 3. No `--action_env` for `CI`/`GITHUB_*`; metadata through `--workspace_status_command`, volatile half; **stages 1 and 2 build no `rust_binary` at all**, which removes the conflict from the pilot rather than solving it. The six `GITHUB_*` vars populate the optional `ci` block — `ci.run_url`, `ci.workflow`, `ci.git_ref`, `ci.sha` (`crates/ocx_cli/src/app/build_info.rs:146-157`) — every one reached through `option_env!()`, so their absence under Bazel matches non-GHA release behaviour and breaks no test. |
| gazelle_rust on Bazel 9 / rules_rust 0.74 with `[patch]`ed path deps | **SPIKE — WP-1a** | Must produce: generated `BUILD.bazel` for all 20 members **and** the 3 patched submodules, under Bazel 9.2.0 / rules_rust 0.74.0, with `bazel build --nobuild //...` green and a written verdict on `[patch.crates-io]`. Fallback on red: hand-written BUILD files, same drift gate. |
| bazel#29114 (repin crash on Bazel 9.0.1) | **DECISION — closed** | Mitigated on the rules_rust side since PR #3932 (in 0.74.0). Pin 9.2.0; do not wait for 9.3.0 GA. WP-1a still runs `CARGO_BAZEL_REPIN=1 bazel fetch --repo=@<repo>` once as a smoke (BZL-RUST-03's spelling; `bazel sync` was deleted at 9.0.0). |
| rules_ocx on 9.x (pinned 8.7.0 today) | **PLAN DOCKET — WP-0b, resized** | **A version bump and a Bazel-9 verification, not a build**: rules_ocx is **v0.4.0** @ `825f20b` with `ocx.project()` already implemented (§ rules_ocx — the dossier's "API 0.1.0" was wrong). Work in `../rules_ocx/.agents/worktrees/<slug>`, own branch, consumed via `git_override`. **Blocks stage 3, not stage 1** — stages 1 and 2 consume no rules_ocx-provided tool (§ Stage 1 sequencing). WP-0b also records whether the repository is public (ruling 9) and how it pins the `ocx` binary (docket below). |
| How `bazel test` reproduces the 8213 floor and the skip ceiling | **DECISION (contract) + SPIKE — WP-1b** | Contract in § Stage 2, including both exit codes, both messages and the reader floor. The two mechanism assumptions (libtest's `test result:` line reachable via `TestResult.test_action_output`; a cached target still yielding a parseable log) are WP-1b verification items, each with a named fallback. |
| Which CI lane holds the bazel-cache write credential | **DECISION — closed** | `verify-basic.yml`, push-to-`main` only (`github.event_name == 'push' && github.ref == 'refs/heads/main'`), carried by `--credential_helper` from `$RUNNER_TEMP` — never `--remote_header`, never a committed `.bazelrc` (BZL-CACHE-01/02/03/04). Every other lane carries `--remote_upload_local_results=false` and no credential at all. New plumbing; no existing job-level ref gate to copy. |
| `.bazelversion` ↔ `ocx.lock` drift check | **DECISION — closed** | § The pin authority. Compares `.bazelversion` against `bazel --version` from the resolved binary (`ocx.lock` carries **no** semver field — verified), with three distinct exit-1 messages including a reader floor, and both red halves demonstrated before the check may be cited. |
| Cache latency Hetzner ↔ GH runners; is `--disk_cache` layering worth it | **SPIKE — WP-1c** | Must produce: median wall-clock of `bazel test //crates/...` from a GH-hosted runner with a warm remote cache vs. the `cargo nextest run --workspace` baseline on the same commit; observed per-lookup RTT; CAS working-set bytes per cold build and per one-crate change (ruling 10); the `bunx vitepress build` chain's cold/warm wall-clock and invocation frequency (§ Stage 3); a yes/no on `--disk_cache` layering. Its **threshold** is Open Question **#2**, which carries a proposed number rather than asking whether a number is needed. |
| pexpect under this host's sandbox | **DECISION — closed, no spike** | Expected red; [bazel#5373](https://github.com/bazelbuild/bazel/issues/5373) is closed `not_planned`. Casts are tagged `no-sandbox, requires-network, local` from day one, so the answer gates nothing. Spiking it would spend a work package to confirm a closed upstream issue. |

Four further dockets this ADR opens, listed here because the first draft named two of
them in prose and docketed neither, and a later one spent a capped marker slot on the
fourth:

| Item | Disposition | What it owes |
|---|---|---|
| ~~**Anonymous cache read — contradicted**~~ | **CLOSED by measurement, 2026-09-21** | Reads are **401** — `ocx-sh-10-bazel-cache.conf:29-33` has no `limit_except` (§ Context). The docket becomes a **sibling `server-hetzner1` PR**: split the vhost realm, add `bazel-cache-readers.htpasswd`, correct the stale `bazel-cache/README.md:7-8`. Client half in § Cache staging ruling 5b; the branch choice is § Orchestrator rulings R1. |
| **`rust-toolchain.toml` ↔ `MODULE.bazel` pin drift** | **PLAN DOCKET — WP-1a** | A check in the same shape as `bazel:pin:check`: read `channel` from `rust-toolchain.toml` and the version from `rust.toolchain(versions = [...])`, string-compare, exit 1 naming both on mismatch, with a reader floor for either file being absent or unparseable, and both halves shown red. Cheap (two file reads) and it is the only guard on a pin pair `rules_rust` will not reconcile while [#2753](https://github.com/bazelbuild/rules_rust/issues/2753) is open. |
| **`ocx package sign` on the two new mirror packages** | **PLAN DOCKET — mirror repos** | Provenance for `bazelbuild/bazel` and `asciinema/agg` (§ rules_ocx). Touches `../mirror-*`, so it is docketed rather than decided here. |
| **How `rules_ocx` pins the `ocx` binary, and whether that pin enters module resolution / the action key** | **PLAN DOCKET — WP-0b** (retired from the Open Questions) | Read it off the module extension and record it beside rules_ocx's other owner-attested state. A tool-provisioning change invisible to the key would let two runs with different `ocx` versions share entries on the one shared-cache-writing target — but § Cache staging ruling 2(c) already gates that target on an in-tree observable, so this is a lookup that explains the check's result, not a decision the site rule waits on. |

## Acceptance — the four-stage definition of done

The dossier's G3, restated as testable criteria. Each names its **scope** and a
**reachable red state**, per `quality-core.md` § Unchecked Green and § "A green
is only as wide as what ran". A criterion whose red half has not been shown is
not met.

### A1 — Stage 1: per-crate skip (the pilot's proof)

**Scope:** `//crates/...` on Linux only — the **34** `rust_test` targets of § Stage 1
(20 lib + 14 integration). Not the `external/` libraries, not doctests, not
darwin/windows.

**Green:** on a clean checkout of `main`, with `--disk_cache` empty and
`--remote_cache=https://bazel-cache.ocx.sh`, run `bazel test //crates/...`
twice. Run 2 reports **every** test target as cached. Evidence is the BEP, not
the terminal summary line: every `TestResult` carries `cached_locally` or a
remote-cache runner in the execution log, and the count of cached targets equals
the count of test targets.

**Red — both halves required:**

1. **The skip is real.** Touch one **leaf** crate, rerun. Exactly
   that crate's `rust_test` and the `rust_test`s of its dependents re-run;
   every other target is still cached. The dependent set is read from
   `bazel query 'rdeps(//crates/..., //crates/<leaf>:<leaf>)'`, not guessed —
   if the re-run set is larger than the query's answer, the graph is wrong.
2. **The check can fail.** Touch a **hub** crate (`ocx_util`), rerun. A large
   dependent set re-runs. If touching a leaf and touching a hub produce the same
   re-run set, the graph is not per-crate and A1 is **not met**, whatever run 2
   reported — a universal cache hit and a universally-invalidating graph are
   indistinguishable from the summary line alone.

**The pair this ADR originally named cannot carry clause 2, and the correction
is measured, not argued (DX-18).** `ocx_exit` was picked as the leaf because it
is a leaf of the *forward* graph; clause 2 reads the *reverse* one, where it is
not. On this tree today, against the 34 `rust_test` targets of `//crates/...`:

| touched crate | `rust_test`s in its reverse closure | share |
|---|---|---|
| `ocx_exit` (the named "leaf") | 27 | 79 % |
| `ocx_util` (the named "hub") | 29 | 85 % |
| `ocx_script` | 10 | 29 % |
| `ocx_setup` | 10 | 29 % |

`ocx_exit` and `ocx_util` share **26** of those targets and differ by four
labels in total (`bazel query "kind('rust_test rule', rdeps(//crates/...,
//crates/<c>:<c>))"`). A correct per-crate graph therefore separates them by two
net targets, which is inside the noise clause 2 is supposed to sit outside of —
the check nearly fires its own abort condition on a graph that is right. **Use
`ocx_script` (or `ocx_setup`) as the leaf against `ocx_util` as the hub**: 10
against 29 is a spread a wrong graph cannot fake. Re-measure the table before
relying on it; these are reverse closures and they move with every new edge.

**And the touch must change *emitted* code (DX-37).** The natural probe — append
an unused `pub const` to the leaf's `lib.rs` — is dead-stripped before it reaches
the test binary, so both probes produce identical re-run sets and A1 reports
`a1-probes-alike` **on a correct graph**. That is a false Block: the finding
names the graph and the defect is in the probe. Change something the compiler
must emit and a test must observe — a function body the crate's own `rust_test`
calls — or the two halves are measuring the optimiser.

**Abort condition:** WP-1a cannot produce a green `bazel build --nobuild
//crates/...` by either generation route within budget → stage 1 aborts, the
initiative stops at Option C, whose rung 1 (re-measure) has already run by then and
whose rung 3 (move the floor/ceiling bracket onto `target/nextest/default/junit.xml`,
then add the `sources:` guard) is built instead. **Not** a bare `sources:` guard on
`rust:test:unit` — `taskfiles/rust.taskfile.yml:228-234` documents why that breaks
the bracket.

### A2 — Stage 2: the lane swap and its gates

**Scope:** `verify-basic.yml`'s `smoke` job and `verify-deep.yml`'s Linux matrix
leg. Explicitly **not** the darwin/windows legs, which keep `cargo nextest run`
and stay ungated as they are today.

**Green:** `task verify` is green end to end with the Bazel lane in
`.verify:build-test`; the BEP-derived floor reports `>= 8213` on the same commit
nextest counts 8213 (**test-count parity**, on the same commit, stated as such), and
its target count equals `cargo nextest list --workspace`'s `rust-suites` count (**34**);
a PR lane **presenting no credential** still reads the cache **if and only if the
WP-0 probe returned 404/200** (§ Context — on 401 this clause is struck and Open
Question #1's branch applies); a `main` run increments bazel-remote's `http_cache`
write metric, **read through Grafana**, which is already provisioned and already
authenticated — not through the cache host, whose listener separation is itself
UNVERIFIED (ruling 8), and whose config review is sequenced **before** this check
rather than beside it.

**Red — each gate shown red before it is cited (BZL-CORE-02):**

- Floor: set `crates/NEXTEST_FLOOR` to `count + 1` → exit 1 with the shrank
  message. **Delete three `#[test]` functions from inside one existing target** →
  exit 1 on the per-target row. Point the reader at an empty BEP → exit 1 on the
  reader floor.
- Ceiling: the existing `ceiling:self-test` step plants a value above the
  ceiling and asserts a red.
- Pin drift: the three cases in § The pin authority.
- BUILD drift: the five cases in § Stage 1.
- Tag guard: the three cases in § Cache staging ruling 1a.
- **Credential absence (the control).** On a **same-repo** PR run, assert the job's
  `BAZEL_CACHE_WRITE` is the empty string and that no rc file was written under
  `$RUNNER_TEMP`. Its red half: remove the trusted-event condition from the `env:`
  expression in a scratch workflow and show the assertion fail. This is the
  BZL-CACHE-01 control; the next bullet is not.
- **Write isolation (defence in depth, explicitly not the control).** Run the
  PR-lane workflow with the write credential deliberately present and confirm
  `--remote_upload_local_results=false` still produces zero `PUT` lines. A green
  here that was never run with the credential present proves nothing about
  isolation — and a green here **does not** discharge BZL-CACHE-01, because this
  test constructs the very state that rule forbids.
- **Cache reachability, both outcomes.** For the anonymous-read green: point a PR
  lane at an unreachable endpoint and show the build still exits 0 with
  `WARNING: Remote Cache: Connection refused` and **zero** cache hits — which is
  also the NFR Availability claim, otherwise asserted with no demonstration. For
  the write-metric green: run the same `main` job with
  `--remote_upload_local_results=false` and show the metric does **not** move.
  Without these, a misconfigured endpoint and a working one are indistinguishable.
- **Helper hygiene:** the two assertions in § Cache staging ruling 4b, each shown
  red.

**Abort condition:** WP-1c's measured median fails the threshold of Open Question
**#2** → **the lane swap does not land**; the terminal state is B-without-the-swap
or C, and `.verify:build-test` keeps its fourteen steps unchanged.

### A3 — Stage 3: casts and the website

**Scope:** the **39** cast-enabled `test/doc_scripts/*.sh` as cast targets — the
`# cast: true` set, enumerated through `test/scripts/doc_scripts_list.py`, not the
72 files in the directory — and the single coarse site rule with its five upstream
stages as declared inputs. The other 33 scripts are **inputs to the site rule**, not
targets.

**Green:** `deploy-website` builds the site through the coarse rule; a second build
with no input change performs no work; **each of the 39 cast targets produces its
declared output** at `website/src/public/casts/<doc>/<name>.cast`, non-empty, and
`bazel query 'kind(cast_recording, //test/doc_scripts:all)'` returns exactly 39
labels. That is a file-existence assertion on named paths plus a cardinality check,
not prose — and the cardinality half is what catches a target set that silently
shrank.

**Red — three halves:**

1. Edit one `test/doc_scripts/*.sh` body → the site rule **must** rebuild. If it
   does not, `test/doc_scripts/**/*.sh` is missing from declared inputs and the
   rule is unsound.
2. Delete one `.cast` output and rerun → exactly that cast target re-runs and the
   file returns. Point the query at an empty package → the cardinality check reds
   on 0 ≠ 39 rather than reporting a clean tree.
3. The § Cache staging ruling 2 checks (a), (b) and (c), each with the red half its
   contract names. Until all three are shown, the site rule carries
   `no-remote-cache`.

**Abort condition — and it aborts.** Two triggers, either one sufficient:

- **Check (b) or (c) cannot be made green** → the site rule is **not written**.
  Stage 3 ships the 39 cast targets only, and the website chain stays on Taskfile.
  (The previous draft's abort was "the rule stays `no-remote-cache` permanently;
  stage 3 still completes" — that is a *degrade*, not an abort, and it meant no
  condition existed under which stage 3 did not land. A rule that can never reach
  the shared cache buys coarse input-hash skipping that `website/taskfile.yml`'s
  own `sources:` already provides, so writing it anyway buys a MUST exemption and a
  hand-written Starlark rule for nothing.)
- **WP-1c measures the `bunx vitepress build` chain at under 3 minutes cold, or at
  fewer than 5 invocations per week** → the site rule is not written, for the same
  reason: § Stage 3 says the exemption is purchased against an unmeasured cost, and
  those numbers are what make the purchase a bad one. The number is proposed here
  so the measurement cannot be negotiated after the fact; the owner may move it.

### A4 — Stage 4: acceptance

> **AMENDED 2026-09-22 — the lane exists, its results are cached, and the abort
> condition below is breached.** Halves 1 and 3 stand; half 2 is inverted (see
> the § Stage 4 amendment note). The lane is `task bazel:test:accept`, it runs in
> `task verify` phase 2 and in `verify-deep.yml`'s acceptance job, and it is the
> first thing that has ever executed these targets — both callers ran
> `task test:parallel`, i.e. pytest directly, so the 181 targets existed and
> nothing observed them.
>
> **The three measured wall clocks** — `task test:parallel`, the cold
> `bazel test //test:all --local_test_jobs=1`, and the same run warm — are in
> `plan_bazel_build_adoption.md`'s **DX-97** row with the arithmetic. **The 50 %
> ceiling below is breached by a wide margin on the cold run**, for exactly the
> reason this section predicted: 181 serial `uv run pytest` processes against one
> xdist run. The owner directed the change with that named, and caching is the
> mitigation — the *second* run is where the lane pays, and a docs-only commit
> executes zero targets. The abort condition is recorded as **breached and
> overruled**, not as met.
>
> **Scope, as built:** `//test:all` rather than `//test/...`. `bazel test` builds
> the non-test targets a pattern expands to, and `//test/...` reaches
> `//test/doc_scripts` — 79 genrules recording 39 casts and rendering 39 GIFs,
> plus `:gif_check`, whose deps are all 39. That is stage 3's subject and its own
> lane's cost; pulling it in would make this lane's wall clock a statement about
> cast recording.

**Scope:** **one `sh_test` per `test/tests/test_*.py`** — 181 targets today
(the "172" this line carried was stale; the count is the `glob`'s, and
`scripts/bazel_gate_proofs.ACCEPTANCE_MODULE_TARGETS` is now its one home),
`no-sandbox` + `exclusive`-tagged, `--local_test_jobs=1` on the command line,
against the existing docker-compose services. *(Superseded by `adr_test_speed_tiers.md`
AM-9: `exclusive` dropped, `--local_test_jobs=$ACCEPT_JOBS` (default `min(8, nproc)`), host locks in the runner.)* `SCOPED_ROWS` is the crate →
target **selection query**, not the unit (§ Stage 4).

**Green:** the suite runs as `sh_test`s shelling to `@tools//:uv run pytest`; a
docs-only commit **selects zero** acceptance targets; `bazel test //test:all` on a
clean tree reports the same pass/fail verdict as `task test:parallel` on the same
commit, and the `sh_test` count equals the `test/tests/test_*.py` file count.

**Red — three halves:**

1. **Selection is real.** Touch `crates/ocx_setup` — a crate whose `SCOPED_ROWS` row
   names a **non-overlapping** module set (`tests/test_self_*.py`,
   `tests/test_session_path.py`, `tests/test_update_check_throttle.py`; deliberately
   not `ocx_oci`/`ocx_trust`, which share `tests/test_logging.py`, nor `ocx` or
   `ocx_test_support`, whose rows read `escalate`) → exactly those targets are
   selected. Touch `crates/ocx` → the `escalate` row maps to `//test:all` and the
   whole suite is selected. **If a docs-only commit and a crate-touching commit
   select the same set, the selection is not wired and A4 is not met.**
2. **Caching is off, and that is asserted rather than assumed.** *(INVERTED
   2026-09-22.)* Run `bazel test //test:all` twice with no source change →
   **every** target reports `(cached)`; `Executed 0 out of 181 tests` is the
   line. A target that re-executes on an unchanged tree is the finding now, and
   its usual cause is a generated file inside one of `//test:suite_inputs`'
   globs. The untagged-sibling control went with the contract it served: under
   caching-on, "everything cached" is also what a reader crediting key
   *presence* rather than truth answers on any BEP, so the control moved to
   half 3, which must red on its own.
3. **The binary is not silently stale.** Change the `ocx` binary under test
   without touching any `test/**` file → every target re-executes **and** the
   declared input digest for the binary changes. *(2026-09-22: no longer defence
   in depth. It is the control, exactly as this sentence said it would become —
   `scripts/bazel_accept_proofs.py --check-s015` takes the warm BEP and the
   binary-swap BEP and refuses a run given only one of them.)*

**Abort condition** *(breached and overruled — see the amendment note at the head
of this section)*: stage 4's wall-clock under `--local_test_jobs=1` exceeds
`task test:parallel`'s measured wall-clock on the same commit by **more than 50 %**
→ stage 4 does not land, and the acceptance suite stays on Taskfile. Stage 4 buys
selection only (§ Stage 4); selection that costs half again as much as the parallel
run it replaces is a net loss, and the serial-vs-xdist regression is a foreseeable
way to get there rather than a surprise. The percentage is proposed here so the
measurement cannot be renegotiated afterwards; the owner may move it.

### A5 — Telemetry

**Green:** Grafana shows per-build, per-target wall time and cache hit/miss from
the BEP→OTLP push, from CI and from a workstation. **Tempo span count == BEP
target count for one build.**

**Red:** the five rows of § Observability's gate contract, each reproducible
**offline against the script** rather than only through a Grafana session — (a) the
queue below the span count → exit 1 and diverging counts, restore → equality;
(b) empty BEP → the reader floor; (c) unset endpoint → silent exit 0 with zero
spans; (d) the planted `--remote_header` fixture → no secret in the emitted spans,
shown red by removing the allowlist. `subsystem-ci.md` records that the count-parity
check was the only thing that caught the queue-drop defect in the junit2otlp
pipeline; (b) and (d) are the two the previous draft had no way to run at all.

### A6 — rules_ocx is the sole tool path

**Green:**

```sh
git ls-files -z 'MODULE.bazel' '*.bzl' | tee >(tr -cd '\0' | wc -c >&2) \
  | xargs -0 grep -n 'http_file\|http_archive'
```

returns exactly **one** hit — the Nerd Font, with an `integrity` attribute and a
comment naming it an asset — **and** the file count on stderr is at least 3
(`MODULE.bazel`, `website/site.bzl`, and whatever else the tree holds). Every other
tool resolves through `@tools//:`.

**Red — and the floor is on the file count, not only the hit count.** The previous
spelling was `grep -rn '…' MODULE.bazel *.bzl **/*.bzl`, which is itself an
unchecked green: `**/*.bzl` expands recursively only with `globstar`, off by default
in non-interactive bash and absent in POSIX `sh` and Taskfile's default shell, and
`*.bzl` at the repo root matches nothing and is passed to `grep` literally when
unmatched. That command can therefore read **only `MODULE.bazel`** and still return
exactly one hit, because the one expected hit lives there — a check that read one
file is indistinguishable from one that read the tree, which is the precise failure
this section's preamble forbids. So: zero hits means it did not read the files (the
Nerd Font entry must be there); **a file count below 3 means the reader stopped
early**; two or more hits means a tool bypassed rules_ocx. All three are findings,
and the middle one is the half that did not exist before.

### A7 — the bazel-quality gate set

Chained into one named target that CI invokes, with each step's exit code
propagated — never `|| true`, never a stdout scrape (BZL-LARK-01, BZL-CI-07):

```
bazel build --nobuild //...            # loading + analysis
bazel run //:buildifier.check          # format + lint, gate on nonzero
bazel mod deps --lockfile_mode=error   # lockfile fresh
bazel test //...                       # the CI verb
```

**Red:** each of the four shown red once against a deliberately planted
violation — an undefined name in a dead `.bzl` branch for line 1, a misformatted
BUILD file for line 2, a hand-edited `MODULE.bazel.lock` for line 3, a failing
test for line 4.

## Orchestrator rulings pending owner ratification

**Two decisions in this ADR were taken by the orchestration chain in autonomous mode,
not by the owner.** They are recorded here, separately and by name, so that ratifying
this ADR is not mistaken for having made them — and so that an owner who disagrees
can find both in one place rather than by reading the sections they landed in.

**The ADR's Status stays `Proposed`.** Flipping it to Accepted is the owner's step,
and doing so ratifies these two along with everything else.

### R1 — the read-credential branch (replaces the former open question)

**Decided:** branch **(a)** — a read-only credential for same-repo lanes and the
local loop; fork PRs get `--disk_cache` only.

**Evidence it rests on:** `nginx/config/main/conf.d/ocx-sh-10-bazel-cache.conf:29-33`
on hetzner1 — `location /` carries `auth_basic` + `auth_basic_user_file` with no
`limit_except`, so anonymous `GET` is 401 (§ Context, measured 2026-09-21). The
dossier's "anonymous reads" traced to a stale
`/srv/sh.ocx/bazel-cache/README.md:7-8`.

**What it commits:** a sibling `server-hetzner1` PR splitting the vhost realm, a
second htpasswd account, and two workflow-injected variables (§ Cache staging
ruling 5b, which also reconciles this against BZL-CACHE-02 and -03).

**Owner's levers:** reject the server PR → fall back to branch **(b)**, no cache
reach on any PR lane, the 43 % payoff landing only on `main` and the local loop.
Or take branch **(c)** — reopen the read side for `GET`/`HEAD` — which is a broader
server change and a different security posture (ruling 6a analyses it).

### R2 — the performance threshold and the pilot budgets (replaces the former open question)

**Ratified as proposed:** a warm-cache `bazel test //crates/...` on a GitHub-hosted
runner must beat `cargo nextest run --workspace --profile ci` on the same commit by
**≥ 4 minutes median over 5 runs**, with per-lookup RTT **< 150 ms p50**. Otherwise
the remote stage falls back to `--disk_cache` + `actions/cache` and **A2's lane swap
does not land**. Budgets: **WP-1a 3 working days** to a green
`bazel build --nobuild //crates/...` by either generation route; **WP-1c 1 working
day** of measurement.

**One obligation attached:** **WP-1c re-measures the nextest baseline before
comparing anything to it** — against the measured 1731 s / 3407 s, never the stale
3347 s (§ The go/no-go reading). A threshold compared against a stale baseline is a
number with no meaning, which is the failure this threshold exists to prevent.

**Evidence it rests on:** the 4-minute figure is a **judgement, not a measurement** —
the honest baseline for it does not exist until WP-1c's first step runs. It is
written down now precisely so a disappointing result is not renegotiable afterwards
(`quality-core.md` § Verification Honesty).

**Owner's lever:** move any of the four numbers. Move them *now*, not after the
measurement arrives.

## Open Questions

**None. Zero clarification markers remain** — stated explicitly, because an empty
section with no sentence reads like an omission rather than a result. The token
itself is deliberately not written anywhere in this file, so a mechanical grep for it
returns **0** rather than matching this sentence.

Five questions earlier drafts carried are all closed. Three were closed by this ADR's
own reasoning or by a docket; two were closed by measurement on 2026-09-21 and are
recorded above as orchestrator rulings:

| Former question | How it closed |
|---|---|
| Is the `go` conditional on WP-0? | Answered by the ADR in its own voice — § The go/no-go reading, § Decision Outcome |
| Can the 8213 floor be reached? | Ruled in § Stage 1 (34 test targets); verified by WP-1b item 3 |
| How does rules_ocx pin the `ocx` binary, and does that pin enter the action key? | **Docketed to WP-0b** — a fact about an out-of-tree repo, not a decision, and § Cache staging ruling 2(c) already gates the site rule on an in-tree observable |
| Which branch if anonymous reads are 401? | **Measured** → reads *are* 401 → **R1** above |
| What threshold makes WP-1c a no-go? | **Ratified** → **R2** above |

Three was a cap, never a quota; the count reaching zero is the evidence arriving, not
the questions being quietly dropped.

## Consequences

**Positive**

- Per-crate test invalidation becomes expressible, which is what the crate split
  was for and what no cargo-side fix provides.
- The 43 % of commits that touch no Rust stop running the whole unit suite.
- The schema → doc-scripts → recordings → SBOM → site chain becomes one declared
  graph rather than five sequential Taskfile steps whose real inputs are implicit.
- rules_ocx acquires a consumer that finds its gaps, and `agg` acquires a mirror
  package.
- One `build.rs`-under-Bazel question and one drift-check question that the
  dossier left open are closed here rather than carried into the plan.

**Negative**

- `BUILD.bazel` is a second source of truth beside `Cargo.toml`, permanently,
  mitigated but not removed by a drift gate.
- Two Rust version pins (`rust-toolchain.toml` and `MODULE.bazel`) that
  rules_rust will not reconcile while #2753 is open, currently three releases
  behind the ruleset default.
- A hand-written Starlark rule for the website that buys coarse skipping only,
  and a bespoke BEP reader with no upstream maintainer.
- Casts get **no** shared-cache reach — `local` implies `no-remote-cache` — and
  their payoff is local input-hash skipping that `website/recordings.taskfile.yml`
  already provides through its own `sources:`.
- **Acceptance gets no caching at all**, local or remote: stage 4 is
  `external`-tagged and runs `--nocache_test_results`, because its real inputs
  (a compose stack, service images, the binary under test) cannot be represented.
  It buys selection only, and it **loses today's xdist parallelism** to
  `--local_test_jobs=1`. *(Superseded: result caching is on since the
  2026-09-22 Stage 4 amendment, and the targets run concurrently since
  `adr_test_speed_tiers.md` AM-9.)*
- The target count lands at 274, just under BZL-CI-01's ~300 tripwire — which is
  itself only the signal that says **measure the whole-repo median now**, never a
  trigger for target selection. Arriving within ~10 % of it on day one means the
  measurement is owed immediately; the switch point stays the wall-clock, and WP-1c
  already owes it.
- One innovation token spent (`quality-core.md` § Choose Boring Technology).
- **The chosen scope is the one the ADR's own matrix ranks third of four** (A 97
  against B 114 and C 110), carried on an owner scope decision rather than on the
  analysis. Recorded here so the disagreement stays visible instead of being
  rediscovered.

**Risks**

- *A flaky trusted uploader poisons the shared cache* (bazel#4276 — no adversary
  needed). Mitigation: no remote execution; the only sandboxed cacheable stage
  gated on a hermeticity check with both halves shown; writes restricted to one
  main-only lane.
- *The website rule's declared inputs are incomplete.* Mitigation: the
  cross-path + mutated-input check is a precondition, not a follow-up; until it
  is green the rule carries `no-remote-cache`.
- *The floor reader measures the wrong thing.* Bazel's **synthesised** `test.xml`
  reports one case per target, so a reader that cannot tell it from a
  harness-written one would report ~34 against a floor of 8213 — or, if the
  comparison were inverted, pass forever. Mitigation: the `$XML_OUTPUT_FILE`
  design, the per-target map, the reader floor derived from that map, and WP-1b's
  three named falsifiers with their fallbacks.
- *The floor passes while coverage is lost.* A floor that gates on target count
  plus an **asserted** table of expected counts does not move when tests are
  deleted from inside an existing target — the exact invariant it exists to hold.
  Mitigation: per-target counts come from the test binaries' own `test.xml`, the
  committed map is only the reader floor, and the three-test-deletion red is shown
  before the nextest gate is retired.
- *A same-repo PR carries the write credential.* GitHub withholds secrets from
  fork PRs only, and every PR here is same-repo. Mitigation: the credential is
  supplied through a trusted-event-conditioned expression so it is **absent**, not
  merely unused, and A2's control asserts that on a real PR run.
- *A poisoned AC entry has no exit.* Wiping a 50 GB shared cache is not feasible.
  Mitigation: a generation salt in `--remote_instance_name`, so abandoning a
  suspected generation is a one-string bump; plus a PUT-outside-the-window alert
  and a named rotation cadence.
- *Stage 4 serialises what is parallel today.* Mitigation: A4's abort condition is
  a measured 50 % wall-clock ceiling against `task test:parallel`, written before
  the measurement.
- *gazelle_rust reds against the pin pair or the patched submodules.*
  Mitigation: hand-written BUILD files behind the same drift gate — 23 files, 57
  targets, which the generator's **Experimental** rating makes the budgeted path
  rather than a surprise.
- *The decision rests on one measured number and one contradicted premise.*
  Mitigation: the WP-0 decision file can return no-go, that terminus is named as a
  *successful* end state of the autonomous chain (§ Decision Outcome), the read
  posture is probed before the verdict line is written, and Option C's rung 1 runs
  before any go.

## Links

- [`adr_crate_split_workspace.md`](./adr_crate_split_workspace.md) — amended at line 19 by this ADR
- [`discover_bazel_full_adoption.md`](./discover_bazel_full_adoption.md) — claim diff
- [`research_bazel_cache_trust_boundary.md`](./research_bazel_cache_trust_boundary.md)
- [`research_bazel_toolchain_verification.md`](./research_bazel_toolchain_verification.md)
- `.agents/discussions/bazel-full-adoption.md` — ratified dossier (research 1)
- `/home/mherwig/dev/ocx-evelynn/.agents/discussions/bazel-adoption-timing.md` — the predecessor decision (research 5): the measured CI cost split, the agreed order, and the 401 read record
- `.claude/skills/bazel-adopt/references/go-no-go.md` — the gate applied in § The go/no-go reading
- `.agents/research/bazel_*.md` — six 2026-09-20 lanes (context; superseded where they disagree), incl. `research_graph_tools_middle_ground.md` (the Buck2 lane, § Option D)
- `.claude/rules/bazel-quality.md` + `bazel-quality/{flags,caching,ci,rust,typescript,testing,hermeticity,bzlmod}.md` — vendored, not edited by this ADR
- `.claude/skills/bazel-adopt/SKILL.md` + `references/` — the 11-step procedure, WP-0

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-09-21 | Architect (`/hex-architect high`) | Initial draft. Reopens `adr_crate_split_workspace.md:19`. Rules the three bazel-adopt/bazel-quality frictions; closes six of the dossier's nine open questions; carries three open questions for the owner. |
| 2026-09-21 | Architect (`/hex-architect high`, Round 1 fix pass) | Revised against a four-seat review panel (three opus seats plus a cross-model adversary). Material changes: the anonymous-read premise is withdrawn and replaced by a stated contradiction plus a hard WP-0 probe with a branch table; the `go-no-go.md` reading table is applied rather than sequenced, and the disposition recorded as the owner's scope decision; Option C rebuilt as the predecessor's measured ladder; Buck2 added to Option D; a delivery-cost row and a criterion-1 range added to the matrix, with the A-vs-B disagreement stated rather than weighted away; the Rust target shape corrected to 34 test targets; the floor reader moved to `$XML_OUTPUT_FILE` with observed per-target counts; the website hermeticity check split into declared-input invalidation, ambient-input isolation and tool-pin participation; stage 4's cache unit changed to one target per test module with result caching disabled; the credential mechanism resolved to a CI-only rc file with a trusted-event-conditioned secret and no developer write helper; the AC/CAS asymmetry, a generation salt, detection and rotation added; a tag-guard gate, a BEP→OTLP gate contract and an edited-file-set table added; 72 cast targets corrected to 39; A3 and A4 given abort conditions that abort. |
| 2026-09-21 | Architect (`/hex-architect high`, measured-facts pass) | Five measurements arrived and closed both remaining open questions, taking the marker count to **zero**. Anonymous reads measured **401** (`ocx-sh-10-bazel-cache.conf:29-33`, no `limit_except`) — the dossier's premise traced to a stale `bazel-cache/README.md`, the second ratified decision in this run to rest on a stale record; branch (a), a read-only credential, ruled in. CI medians measured (verify-basic **1731 s**, verify-deep **3407 s**), superseding `hex.md`'s 3347 s; the go/no-go reading rewritten against them — reading 3's "cheaper fix named as insufficient" clause is now **evidenced as no demonstrated movement** (not as a regression: different measurement windows), and the reading still lands on the fallthrough because 3407 s is a **total, not a decomposition**, which is now the single remaining empty signal row. `rules_ocx` corrected from "API 0.1.0" to **v0.4.0** with `ocx.project()` already implemented — WP-0b resized from a build to a version bump, and Q9's coupling objection correspondingly weakened. `bazel` mirror confirmed published and enumerated (no linux/arm64 musl candidate). `agg` confirmed to need a **new mirror repository**, not a tag. Host envelope added as ruling 12 (`-Xmx2g`, `--jobs=12`, disk cache off `/tmp`). New § Orchestrator rulings pending owner ratification records R1 and R2 as chain decisions, not owner ones; Status stays **Proposed**. |
| 2026-09-22 | Owner decision, executed | **Stage 4's "cache-result caching is disabled, deliberately" ruling reversed.** The acceptance suite runs as `task bazel:test:accept` (`bazel test //test:all --local_test_jobs=1`) in `task verify` phase 2 and in `verify-deep.yml`'s acceptance job, **with results cached**. The ruling's own exit condition was the precondition and it is met in the same change: `external` and `local` both off (measured — `local`'s `no-remote` half suppresses the disk-cache hit on a fresh server), `no-sandbox` in their place, the real input set declared through `//test:suite_inputs`, `bazel:tag:guard` advanced to stage 4 and narrowed so it refuses an acceptance target that stops declaring the binary and the compose definition, and `--check-s015` rebuilt around the binary-swap control. A4's red half 2 is inverted, half 3 is promoted from defence in depth to the control, and A4's 50 % wall-clock abort condition is recorded as **breached and overruled** with its three numbers in `plan_bazel_build_adoption.md`'s DX-97 row. The residual under-declaration (the live compose stack, and ~20 modules reading across a package boundary) is named in `test/bazel.bzl`'s docstring rather than closed. |
| 2026-09-21 | Architect (`/hex-architect high`, R2 fix pass) | Closes the R2 residual, which was one systematic substitution the previous pass introduced. `54` (all `//crates/...` targets) replaced by **34** (test targets) at the five sites whose subject is test targets, `rust-suites` or `TEST_TARGET_MAP` rows, and by **57** at the two whose subject is every target in the 23 Rust BUILD files; the drift gate's false "the two floors cannot disagree" claim replaced by four floors each named on its own reader's universe (34 / 57 / 54 / ≥ 274); "23-crate pilot" corrected to 20; the `GITHUB_*` enumeration mirrored into the disposition table; BZL-CI-01 quoted instead of paraphrased, so ~300 reads as the signal to measure rather than half a trigger; the decision file's empty-cell count taken from the quoted check (three rows) instead of by eye, and the `Build owner` Answer cell filled; `build.rs` cited `:35-42`. NC#3 retired to WP-0b as a lookup rather than a decision, leaving **two** markers — three is a cap, not a quota. Every `hex.md` citation converted from a line number to a quoted phrase, that file being appended to at the top. |
