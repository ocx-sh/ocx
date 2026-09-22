# Review — `adr_bazel_build_adoption.md`, reviewer/quality (Round 1, adversarial)

Reviewer: `reviewer` focus **quality** · Model: opus · Date: 2026-09-21
Subject: `.claude/artifacts/adr_bazel_build_adoption.md` (1279 lines, Status Proposed)
Posture: read-only. The ADR was not edited. Nothing was committed.

**Verdict: the recommendation is not earned as argued.** The ADR is unusually
honest — it names its own thin evidence, its deviations and its unverified
inputs — but three of its load-bearing structural claims are contradicted by
its own § Acceptance, by the repository's own source comments, and by the
predecessor record it claims to be reversing.

Counts: **Block 5 · High 6 · Warn 4 · Note 2**

---

## Q1 | **Block** | § Considered Options → Option C / § Open Questions

**Anchor.** "The narrower fix here is **not exhausted**: `rust:test:unit`
(`taskfiles/rust.taskfile.yml:226-255`) carries no `sources:` or `status:` field
at all, so the cheapest possible whole-suite skip has never been tried." And
[NEEDS CLARIFICATION #1]: "That number is still unmeasured".

**Counter-case at its strongest.** `bazel-adopt/references/go-no-go.md:155-167`
§ "Reading the answers" gives three decidable readings. The first, verbatim:

> **No-go, and stop.** No cheaper fix has been tried; or the largest CI job is
> not a build job; or no successful CI history exists to measure.

The ADR affirms clause 1 ("not exhausted… never been tried") and clause 3 (the
median is unmeasured) **in its own text**. It quotes the skill's step 1 and step
2 at length, and then never quotes the table that turns those two facts into a
verdict. By the skill's own procedure this evidence base produces *No-go, and
stop* — or, on the most generous reading available, the second reading ("No-go
for now, with a named condition"). It does not produce a `go`, and the ADR's
title, Status and Decision Outcome all assert one.

**Ruling: sustained.** Nothing in the ADR answers this. The nearest thing —
"This ADR's `go` is conditional on WP-0's decision file … which retains the
authority to return no-go" — is itself contradicted (see Q2 and Q18) and is a
claim about a *future* file, not an application of the gate to present evidence.

**Fix.** Quote `go-no-go.md` § "Reading the answers" in § Considered Options and
apply it. Restate the Decision Outcome as **"no-go for now, with a named
condition"** — the reading the evidence supports — with the condition being
WP-1c's measured median plus the post-`test:parallel` re-measurement (Q4). The
four-stage design in § Technical Details survives unchanged as the plan that
executes *if* the condition clears; only the verdict line changes.

---

## Q2 | **Block** | § Decision Outcome vs § Acceptance A3/A4

**Anchor.** § Decision Outcome: "A is chosen because … stages 3 and 4 are
separable, **individually abortable** increments rather than a single
commitment" and "**Every stage carries an abort condition** (§ Acceptance). An
aborted stage leaves the previous stages intact".

**Counter-case at its strongest.** This is the single claim that makes the
recommendation "A, sequenced as B, gated" rather than plain A — the
lowest-scoring option in the ADR's own matrix (94 of 130, against B's 108 and
C's 98). Read § Acceptance:

- **A3's abort condition does not abort.** Lines 1154-1156: "the hermeticity
  check cannot be made green → the site rule stays `no-remote-cache`
  permanently. **Stage 3 still completes**; it simply buys local skipping only".
  That is a degrade, not an abort. There is no condition under which stage 3
  does not land.
- **A4 has no abort condition at all.** A1, A2 and A3 each end with an
  "**Abort condition:**" line. A4 (lines 1158-1168) has Scope, Green and Red —
  and stops. Stage 4 is unconditional.

So of the two stages the Decision Outcome justifies by their abortability,
one cannot be aborted and the other has no abort clause. The gating is
decorative, and with it removed the recommendation collapses to plain A.

**Ruling: sustained.** The ADR states the opposite of what its own § Acceptance
says, on the claim that carries the difference between the chosen option and
the highest-scoring one.

**Fix.** Either (a) give A4 a real abort condition with a number and rewrite
A3's so it can abort stage 3 (e.g. "hermeticity check red → the site rule is not
written; stage 3 ships the 72 cast targets only"), or (b) delete the
abortability argument and own the recommendation as plain A, which then has to
beat B on the matrix on some other ground. Option (c) — Q12's — is cleaner
still.

---

## Q3 | **Block** | § Considered Options → Option C

**Anchor.** "`rust:test:unit` (`taskfiles/rust.taskfile.yml:226-255`) carries no
`sources:` or `status:` field at all, so the cheapest possible whole-suite skip
has **never been tried**" and "Option C's `sources:` guard is built regardless
as the fallback" and the pros row "The cheapest unexplored win (`sources:`
guard) is ~5 lines".

**Counter-case at its strongest.** `taskfiles/rust.taskfile.yml:228-234` reads,
verbatim (cross-checked with `sed`, inside the range the ADR itself cites):

> No `sources:`. The bracket around this run — `test:floor` deletes the log,
> `test:ceiling` reads it — needs the run to happen every time: a
> fingerprint-cached skip would leave the ceiling no log and red an unchanged
> tree, and a subset run (`-- <filter>`) would otherwise stamp the full run up
> to date.

The absence of `sources:` is a documented decision with a stated mechanism, not
an unexplored option. Two consequences, both load-bearing:

1. **Option C's steelman is built on a misread.** Its one named "cheapest
   unexplored win" was explored and rejected, on the record, in the file the ADR
   cites by line range. C is not weaker for it — C's real content is the
   predecessor's ladder (Q4) — but the ADR's version of C is not the option.
2. **The declared fallback cannot be built as specified.** The conditional-go
   rests on "the initiative stops at Option C, and the `sources:` guard is built
   instead" (A1 abort condition, line 1105) and on "Option C's cheap half … is
   built regardless, as the fallback". Adding `sources:` to `test:unit` breaks
   the floor/ceiling bracket — the repo says so. The safety net under the whole
   conditional structure is a known-unsound change.

**Ruling: sustained.** Not answered anywhere; the ADR never engages the comment.

**Fix.** Read `taskfiles/rust.taskfile.yml:228-234` and rewrite Option C. A
`sources:` guard is still reachable, but only by first moving the bracket so
floor/ceiling read a **persisted** artifact rather than a freshly-tee'd log —
the repo already writes one (`target/nextest/default/junit.xml`, see Q8). That
is real design work; the "~5 lines" claim must go.

---

## Q4 | **Block** | § Considered Options → Option C, "Steelmanned"

**Anchor.** "This is the option the 2026-09-20 `bazel-adoption-timing` dossier
chose, and the case for reversing it is thin" and Decision Driver 6: "The
measured basis for reversing the 2026-09-20 no-go is one number (43 %)".

**Counter-case at its strongest.** `bazel-adoption-timing.md` (at
`/home/mherwig/dev/ocx-evelynn/.agents/discussions/bazel-adoption-timing.md`)
records **no no-go**. Line 17, verbatim:

> Order agreed in principle: (1) no-infra CI fixes, (2) sccache → existing cache
> server, local + CI, (3) **re-measure, run bazel-adopt gate, Rust-only pilot if
> warranted**. Nothing in (2) is discarded by (3): same server.

That is a *sequencing* decision whose step (3) is "re-measure, **then** run the
gate". And line 15 is the measurement the ADR's Option C is missing entirely:

> CI cost is ~half compilation, ~half non-compile: acceptance runs serial
> (`task test`, 22 min; `task test:parallel` exists), schema-generate recompiles
> without `--target=` (5.8 min), one 210 s unit test + four 30 s timeouts.

Its line 57 enumerates what was still owed before the gate: "verify-deep `task
test:parallel`; schema `--target=`; sccache-action + multilevel config …; 210 s
/ 30 s tests; **re-measure; bazel-adopt gate**."

Since then those fixes have been landing: `verify-deep.yml` now runs `task
test:parallel` with the comment "the serial run took 21:41 for 3819 tests … on a
runner whose cores sat idle", and `91dea8ac` removed the 30 s timeouts. So the
narrower fix is not "not exhausted" in the sense the ADR means — it is
**mid-flight, materially complete on its largest item, and never re-measured**.

This is the strongest form of Option C and the ADR does not contain it. The
ADR's C is `nextest -p` + wider sccache + a five-line `sources:` guard; the
predecessor's C is a measured ladder whose biggest rung (22 min → parallel) has
already landed and whose next step is literally "re-measure, run the gate".

Compounding: `go-no-go.md` signal 2 asks whether the largest CI job is a build
job — the predecessor measured **~half non-compile**, with the acceptance suite
the largest single chunk, and Bazel's stage 4 is `local`-tagged and gets no
cache reach at all. Bazel does not address the half the predecessor measured as
dominant.

**Ruling: sustained.** The predecessor is cited as authority for a decision it
did not make, and its measured content — the part that would actually contest
the recommendation — is absent.

**Fix.** Quote `bazel-adoption-timing.md:15` and `:17` in § Considered Options.
Rebuild Option C as that ladder. Re-measure `verify-deep` median **after**
`test:parallel` landed, before any go — which is the predecessor's own step (3)
and `bazel-adopt` step 2's lead signal in one action.

---

## Q5 | **Block** | § Context / § Acceptance A2

**Anchor.** § Context: "`bazel-cache.ocx.sh` … HTTP only, 50 GB cap, **anonymous
reads**, nginx htpasswd on writes". A2 Green: "a PR lane with no secret still
reads the cache anonymously".

**Counter-case at its strongest.** `bazel-adoption-timing.md:50` — dated
2026-09-20, describing work *landed* on the server (commits `c0504b1`,
`f0b5446`), one day before the new dossier — reads:

> Anonymous 403; **bazel-cache reads now 401.**

If that is current, anonymous reads do not work, fork-PR lanes get zero cache
reach, and the CI half of the payoff evaporates — while the ADR's § Cache
staging design ("remote cache read-only, **everywhere**") and A2's green
criterion both assume otherwise. The ADR flags the hetzner1 inventory as
"owner-attested, not independently verified" and calls it "the single largest
unverified input to the plan" — correct, and good — but it does not reconcile
this **specific, dated, contradicting record from the immediately preceding
decision in the same series**. An unverified input with a conflicting prior is
a different object from an unverified input with none.

**Ruling: sustained.** The ADR's general caveat does not cover a named
contradiction it never surfaces.

**Fix.** WP-0 gets one command as a hard precondition, not a note:
`curl -sI https://bazel-cache.ocx.sh/ac/<any 64-hex>` from an unauthenticated
client, expecting 404 (absent) or 200, and **not** 401. Record the result in the
decision file. If it is 401, the PR-lane read story and the matrix's criterion-2
score both change before the verdict is written.

---

## Q6 | **High** | § Trade-off matrix

**Anchor.** Criterion "Per-crate skip on the 57 % touching Rust", **Weight 6**
(the highest), A/B = 5, C/D = 1. Decision Drivers 1: "This is the requirement
that discriminates between the options".

**Counter-case at its strongest.** The arithmetic is clean — I recomputed all
four columns (94 / 108 / 98 / 60 against a 130 max, weights summing to 26). No
row is fudged. The problem is upstream of the arithmetic:

1. **The heaviest criterion is the one whose magnitude the ADR says is
   unmeasured.** § Context: "the post-split per-crate payoff is **not** measured
   by that number. The post-split sample is 15 commits, 2 touching Rust". A
   weight-6 row scored 5-vs-1 on an unquantified benefit is 30 of 130 points —
   23 % of the total scale — allocated to something the document declines to
   size. The ADR names the falsifier ("if … most Rust commits touch
   `ocx_util`/`ocx_exit` … C wins on every remaining criterion") and then does
   not discount the score for it.
2. **A criterion chosen *because* it discriminates, then weighted highest, is
   the definitional shape of a reverse-engineered matrix.** Driver 1 says so in
   as many words. That is not dishonest — it is a legitimate way to state a
   requirement — but it means the matrix records the requirement rather than
   testing it.
3. **There is no delivery-cost row.** C is small (even after Q3 corrects "~5
   lines"). A is 23 `BUILD.bazel` files, three bespoke scripts, a hand-written
   Starlark rule, a `rules_ocx` Bazel-9 bump in another repo, and a new mirror
   package that does not exist yet. "Maintenance surface" (weight 4) covers the
   steady state, not the build-out. Its absence systematically favours A and B.

**Ruling: sustained**, partially answered. Answered: "What would falsify the
choice" is a genuine, well-written hedge. Sustained: the hedge sits outside the
matrix and changes no score.

**Fix.** Add a "Delivery cost / time to first benefit" row at weight 3-4. Score
criterion 1 as a range (e.g. 2-5 for A/B) pending the hub-churn re-measurement
at ~100 post-split commits, and state the matrix's verdict as conditional on
that range collapsing.

---

## Q7 | **High** | § Considered Options / whole document — `go-no-go.md` coverage

**Anchor.** The ADR engages `bazel-adopt` step 1 and step 2 at length and
`references/go-no-go.md` not at all beyond them (grep for "largest CI job",
"Team shape", "Starlark reader", "Signal" over the ADR: **zero hits**).

**Counter-case at its strongest.** The decision file `go-no-go.md:183-190`
mandates six rows, every one carrying a measured value, and states the check:
"Empty output = every signal row carries a measured value … the verdict is not
writable yet." Three rows are unaddressed by this ADR:

- **Signal 2 — "Largest CI job is a build job".** Never mentioned. The skill's
  own text: "A run whose largest job is install, provisioning, container pull or
  a deploy step **is not a build-time problem**, and Bazel does not address it."
  The predecessor measured ~half non-compile with acceptance the largest chunk
  (Q4). Unaddressed on the one signal whose "no" reading is *No-go, and stop*.
- **Signal 5 — Team shape.** Two owner questions, "recorded verbatim in the
  decision file": *who owns the build after adoption*, and *does anyone on the
  team already read Starlark* — the latter with the skill's own gloss, "A
  migration whose only Starlark reader is an agent has no reviewer for the diffs
  it produces." This ADR proposes a hand-written Starlark rule plus three
  bespoke scripts on a single-owner repository driven by an autonomous agent
  chain. The question is answerable and favourable — `bazel-adoption-timing.md:14`
  records "Owner already runs Bazel elsewhere (`rules_ocx`,
  `mirror-bazelbuild`) — tool novelty is low" — and the ADR carries neither the
  question nor that answer.
- **Signal 3 — Generator maturity.** The skill's own table (`go-no-go.md:95`)
  rates the Rust generator "an independent single-maintainer plugin, one tag,
  `0.1.0`" → **Experimental**, with the consequence spelled out: "Anything below
  [Production] means coarse, package-per-directory, hand-maintained BUILD files
  are the correct default." The ADR's § BUILD generation instead argues from
  gazelle_rust's issue tracker that "WP-1a is therefore no longer 'is this
  viable at all'". Per-crate *is* package-per-directory so the design is
  compatible with the verdict — but the ADR never states the verdict, and the
  verdict is what makes the hand-written fallback the expected path rather than
  the contingency.

**Ruling: sustained.** The ADR treats the skill as a procedure to sequence
(WP-0) rather than a gate to pass, and Q1 is what that produces.

**Fix.** Put the six-row decision-file table in the ADR with what is known
today, empty cells and all. That is the artefact `go-no-go.md:207-211` says makes
a verdict writable, and printing it with three empty cells makes Q1's ruling
self-evident to any reader.

---

## Q8 | **High** | § Stage 2, ruling 5 + Gate contract `rust:test:floor`

**Anchor.** Ruling 5: "The existing ceiling parses a tee'd log's `Summary [...]`
line against a hand-derived nextest-0.9.144 grammar regex. Bazel produces no
equivalent single-summary artifact, so a port of that grep is not available and
**would be the same fragility class anyway**." Then the floor contract's Reads
row: "from each log, libtest's `test result: ok. N passed; M failed; K ignored;
…` line."

**Counter-case at its strongest.** The ADR rejects one hand-derived stdout
grammar as a fragility class and adopts a different hand-derived stdout grammar
in the same section. Worse, it states the escape hatch and walks past it. WP-1b
assumption 1: "Bazel synthesises a minimal `test.xml` with one `testcase` per
*target* **when the harness emits no JUnit XML**."

The harness here already emits JUnit XML. Verified in this tree:

- `taskfiles/rust.taskfile.yml:254` — `JUNIT:
  '{{.ROOT_DIR}}/target/nextest/default/junit.xml'`, pushed to `otel.ocx.sh` by
  `telemetry:push`, which counts `testcase` elements to do it.
- `verify-deep.yml` (the `test:parallel` step comment) — "the task writes
  `test/results/junit.xml` on every run, local ones included … one flag in one
  place (`test/taskfile.yml` `JUNIT_XML`)."

Bazel's own contract for this is `$XML_OUTPUT_FILE`: a test target that writes
JUnit XML there gives Bazel a **real** `test.xml` with 8213 `testcase`
elements, which makes `bazel test` counts native, removes the hand grammar
entirely, keeps the existing junit2otlp pipeline working unchanged, and
dissolves WP-1b's *second* falsifier too (a cached target replays its declared
outputs, `test.xml` among them — no "does a cache hit still yield a parseable
log" question).

`quality-core.md` § "Don't Own Non-Domain Code" escalates hand-owned parsing of
an external wire format to **Block**, and its bar for owning it is "No library
implements the requirement, **verified by searching, not assumed**". Here a
mechanism exists, is already in the repository, and is named in the ADR's own
falsifier clause.

**Ruling: sustained.** Not answered — the conditional in WP-1b assumption 1 is
the answer and is not followed.

**Fix.** Make WP-1b's first item "wire `rust_test` to emit JUnit XML to
`$XML_OUTPUT_FILE`; confirm `test.xml` carries per-case granularity". The BEP +
libtest-grammar reader becomes the fallback, not the design. The same change
kills the "fewer than 20 test targets" reader floor's ambiguity, because
`test.xml` case counts are directly comparable to `NEXTEST_FLOOR`.

*(Same rule, second instance, noted not filed separately: `scripts/bep_to_otlp.py`
is bespoke while the ADR itself records at § Observability that "Bazel ships a
reference parser at `//src/tools/execlog`". Use it or state why not.)*

---

## Q9 | **High** | § rules_ocx as the sole tool path / § Open questions disposition

**Anchor.** Disposition table, rules_ocx row: "**Blocks stage 1** — it is the
only tool path." § rules_ocx: "Only rustc/cargo stay on rules_rust's own
toolchain."

**Counter-case at its strongest.** Stages 1 and 2 build `rust_library` +
`rust_test` and, by the ADR's own § Cache staging ruling 3, "**stages 1 and 2
build no `rust_binary` at all**". Nothing in that graph consumes `@tools//:bun`,
`@tools//:uv`, `@tools//:agg` or `@tools//:lychee`. Bazel itself comes from
`ocx.toml`, not from rules_ocx. So the pilot's critical path is routed through:

- an out-of-tree module this repository's claim diff could not read at all
  ("OUT-OF-REPO — not read"), state owner-attested only;
- pinned at Bazel **8.7.0** and API 0.1.0, i.e. requiring a Bazel-9 bump in
  another repo before anything here compiles;
- consumed by bare commit SHA via `git_override` with **no `integrity`**
  (research 3 finding 21: `git_override` has no such field);
- maintained by the same person as the consumer, so the supply-chain review the
  ADR promises ("every commit-pin bump is treated as a reviewable
  supply-chain event") has the author and reviewer in one seat;

— for **zero stage-1 benefit**. And `agg` compounds it: `ocx.sh/asciinema/agg`
is confirmed absent from the live index, making a new mirror package "a hard
prerequisite on the critical path".

The staged alternative is obvious and the ADR never argues against it: rules_ocx
enters at **stage 3**, where its tools are first actually used. The dogfood
mandate is preserved in full; only its position in the DAG moves. The ADR treats
"rules_ocx is the sole tool path" (a *composition* rule, correctly owner-set) as
if it also fixed *when* that path is first exercised, which it does not.

**Ruling: sustained.** No sentence in the ADR argues why the pilot must depend
on it. The one that comes closest — "the migration doubles as rules_ocx's first
real consumer" — is a reason to do it, not a reason to do it first.

**Fix.** Move the rules_ocx dependency to stage 3's entry condition. Stage 1
then depends only on `bazel` (from `ocx.toml`, which the ADR already rules pins
`:9.2.0`) and rules_rust 0.74.0 from BCR — both with real pins and no
cross-repo blocker. Record the sequencing explicitly so a future reader does not
read it as the mandate being weakened.

---

## Q10 | **High** | § Stage 3 → Website + § Cache staging ruling 2 + [NEEDS CLARIFICATION #3]

**Anchor.** [NEEDS CLARIFICATION #3]: "how does rules_ocx pin the `ocx` binary
its module extension shells out to, and does that pin enter module resolution /
the action key? If it does not, a tool-provisioning change is invisible to the
cache — two runs with different `ocx` versions would share cache entries." And §
Stage 3: "Tags: `requires-network` only — this is **the one stage of four that
stays sandboxed** and therefore the one stage eligible for the shared cache."

**Counter-case at its strongest.** These two facts intersect and the ADR does
not join them. The website rule is the **only** shared-cache writer in the whole
design, and it runs `@tools//:bun install --frozen-lockfile` + `vitepress build`
— i.e. its toolchain comes from the exact path whose cache-key participation is
an open question. Decision Driver 3 calls a wrong shared entry "invisible and
reaches every reader"; this is a named mechanism for producing one.

The declared mitigation does not cover it. The § Cache staging hermeticity check
has two halves: (a) build the same tree at a different absolute path → expect a
**hit**; (b) mutate one undeclared candidate input → expect a **miss**. Neither
half reds when the *tool version* changes but is absent from the key: the tree
is identical in half (a) and the mutated input in half (b) is a source file, not
a tool. A `bun` bump would silently reuse a site built by the previous `bun`, on
every machine, until someone noticed a rendering difference.

**Ruling: sustained.** The ADR is right to carry #3 as unverifiable from this
repository, but it files it as a general observability caveat rather than as a
precondition on the only remote-cacheable target it has.

**Fix.** Add a **third** half to the § Cache staging hermeticity gate: change
the `bun` pin in `ocx.toml`/`ocx.lock`, rebuild `//website:site` against the same
warm cache, require a **miss**. Until that half is shown green, the site rule
carries `no-remote-cache` — the same posture the ADR already applies to the
other two halves. This also converts #3 from an unanswerable out-of-tree
question into an in-tree observable.

---

## Q11 | **High** | § The file set this ADR creates → `.bazelrc` row; § NFR Scalability / Cost

**Anchor.** "`--remote_download_minimal` on CI, `toplevel` default locally
(BZL-CACHE-11)". § Cost: "The 50 GB cache is already provisioned and already
scraped."

**Counter-case at its strongest.** `bazel-quality/caching.md:117` (BZL-CACHE-12,
SHOULD) measures exactly the shape this ADR chooses:

> a `toplevel` (or **`minimal`**) cache hit leaves intermediates as CAS
> references and a later locally-executing action needs an evicted blob — but
> the caller sees exit 0 (Bazel retries the whole build under a fresh invocation
> ID and re-executes locally) or, at `retries=0`, a **generic exit 1
> indistinguishable from a compile error**.

The ADR cites BZL-CACHE-11 for the download mode and never mentions
BZL-CACHE-12, exit 39, `--experimental_remote_cache_eviction_retries`, lost
inputs, or the error strings that are the only durable signal. It selects the
mode that maximises exposure to the class and omits the paired rule.

Eviction is not hypothetical here: a **50 GB** LRU cache holding release-profile
Rust artefacts (`test:unit` runs `cargo nextest run --workspace --release`) for a
278k-LOC workspace across 23 packages, every commit, will churn. No capacity
estimate, retention policy or working-set sizing appears anywhere. § NFR
Scalability sizes the **target count** (135-150) against BZL-CI-01's tripwire —
a different quantity entirely — and § Cost treats the 50 GB as settled.

**Ruling: sustained.** BZL-CACHE-12 is in the vendored rule set the ADR
otherwise cites rule-by-rule; this is the one it skips, and it is the one that
bites the configuration it chose.

**Fix.** In § NFR Scalability, add a cache-capacity paragraph: estimated
working-set size per commit, bazel-remote's eviction behaviour at the 50 GB cap,
and whether 50 GB is sized for this workload. In the `.bazelrc` row, add an
explicit BZL-CACHE-12 position: leave `--experimental_remote_cache_eviction_retries`
at its default 5, never key retry logic on exit 39, match `lost inputs with
digests:` / `Found transient remote cache error` instead.

---

## Q12 | **High** | § Decision Outcome — steelman against dossier decision "highest degree, all four stages"

**Anchor.** § Decision Outcome, third bullet: "Stages 3 and 4 have materially
weaker measured justification. Both are `local`-tagged … they get **no
shared-cache reach at all**. Their payoff is local input-hash skipping, which
`subsystem-taskfiles.md` § 'Caching Contract' already provides through
`sources:`/`status:`. They are justified by **graph unification and by the
rules_ocx dogfood mandate**, not by cache economics."

**Counter-case at its strongest.** Take that sentence at face value and it
argues against itself. Two stages of permanent maintenance are justified by (a)
an aesthetic property and (b) a *different repository's* testing need. That is
`quality-core.md` § YAGNI and § KISS in one move, and it is precisely "scope
bought with the Rust stage's case".

The case is stronger than the ADR's own framing, because the substitute is not
merely available — it is **already running**:

- Casts already have input-hash skipping. The ADR records it itself:
  "`website/recordings.taskfile.yml:52-69` already declares `doc_scripts/**/*.sh`
  in its own `sources:`". Stage 3's cast half therefore reimplements a working
  mechanism as 72 new Bazel targets.
- The website DAG already exists: "`website/taskfile.yml:45-63` runs
  `schema:default` → `scripts:publish` → `recordings:parallel` →
  `sbom:generate:page` → `bunx vitepress build`". "Graph unification" means
  re-expressing a declared five-step DAG in a second language.
- Stage 4's skipping rests on an assumption the ADR flags as inherited, not
  established: "Cache validity rests on each test owning its own registry state
  — **unchanged from today's assumption**." That assumption is load-bearing for
  skipping acceptance modules and is nowhere verified. The `local` tag protects
  other machines from it; it does not protect *this* machine from a false skip.

And stage 4 is where `go-no-go.md` signal 2 lands hardest: acceptance is the
largest measured CI chunk (Q4), and `local` means Bazel caches none of it
across machines.

**Ruling: sustained.** The ADR's own text is the counter-case; what it lacks is
the conclusion that follows from it. The dossier decision "highest degree, all
four stages" reached the ADR as owner-ratified prose and was ranked, not
attacked.

**Fix.** Stages 3 and 4 leave this ADR. They become a follow-on decision, gated
on (a) stage 2's measured result and (b) rules_ocx reaching a BCR release so the
`git_override` is gone before a second consumer depends on it. This is Option B
as the ADR's own matrix scores it highest, with A retained as the stated
direction rather than the decision — and it makes Q2's abortability problem
disappear rather than needing a fix.

---

## Q13 | **Warn** | § Stage 3 → Website / the BZL-JS-01 / BZL-JS-03 exemption

**Anchor.** Ruling part 4: "The coarse rule buys **coarse-grained input-hash
skipping** — the site rebuilds or it does not. It buys **no per-file JS
caching**, no `ts_project` typecheck test, no incremental transpile."

**Counter-case at its strongest.** The ADR never states what the website build
costs today. Not a wall-clock, not a frequency, not a share of any lane. It is
not in `.verify:build-test`'s 13 steps (the ADR lists all of them). So a named
exemption from two **MUST**-severity rules, plus a hand-written Starlark rule
with no upstream maintainer and no per-file caching, is purchased against an
unmeasured cost — while `quality-core.md` § "Choose Boring Technology" and
§ KISS both point the other way and `subsystem-taskfiles.md` § Caching Contract
already supplies the same coarse property.

**Ruling: partially answered, sustained on the economics.** Answered, and well:
the *legal* reasoning is sound — the rules' premise genuinely does not obtain
(neither rule's subject, `npm_translate_lock`, is ever called), and
`branches-python-ts-cpp.md:108-111`'s "minority slice of a polyglot migration"
clause is a real, quoted escape hatch, not an invented carve-out. Sustained: the
reasoning establishes the exemption is *permitted*, never that the trade is
*positive*.

**Fix.** One number in § Stage 3: the current `bunx vitepress build` chain's
wall-clock and how often it runs, with the share of it the coarse rule would
skip. If the chain is minutes and runs on deploy only, the honest conclusion is
that this stage does not pay for a MUST exemption — which folds into Q12.

---

## Q14 | **Warn** | § Stage 2, ruling 1 — steelman against "the Bazel lane replaces nextest"

**Anchor.** "The Bazel lane replaces the nextest sub-sequence in
`verify-basic.yml`'s `smoke` job only, and in `verify-deep.yml`'s **Linux**
matrix leg."

**Counter-case at its strongest (run both).** Test-count parity is asserted
**once**, at swap time: "the BEP-derived floor reports `>= 8213` on the same
commit nextest counts 8213". After the swap no independent oracle for the count
survives on any gated lane. The ADR's *own* WP-1b falsifier 2 describes a
failure that only appears later: "If a remote cache hit yields a `TestResult`
without a readable log, the count collapses as the cache warms — a floor that
passes on a cold run and reds on a warm one, **or worse, the reverse**." Its
fallback, `--nocache_test_results` on the gating lane, removes the cache from
the lane the exercise exists to accelerate. The darwin/windows legs keep nextest
but ruling 2 explicitly leaves them **ungated**, so they are not an oracle
either.

**Counter-case against running both (the ADR's implicit position).** Ruling 2
refuses exactly this shape for a good reason: "it would put two different floor
*readers* … on one invariant — two readers that can disagree, with nothing
announcing the split." And a doubled gating lane doubles the CI minutes the
change is meant to reduce.

**Ruling: sustained, narrowly.** The ADR picks deliberately and correctly
between "one reader" and "two readers on one gate". It never considers the third
shape, which is the one that answers the objection without creating the problem
ruling 2 names: nextest retained on the Linux deep leg as an **ungated,
report-only parity probe** — no floor, no ceiling, one invariant with one gate,
and a second count printed beside it.

**Fix.** Add that probe to § Stage 2 with an explicit removal condition ("after
N consecutive green releases with parity, or at release X") so it is a track
record with an end date rather than a permanent second lane.

---

## Q15 | **Warn** | § NFR Availability / Latency — steelman against "no RBE, reuse the existing cache"

**Anchor.** Availability: "a cache outage degrades to a full local build, green
and slow. That is accepted; no pre-flight reachability probe is added."
Latency: "**Unmeasured, and treated as such.**"

**Ruling on the outage half: answered, and well.** BZL-CACHE-26 requires exactly
this — do not cite the fallback flags, and *state* the lane's outage policy. The
ADR does both, and its MUST-half compliance is real. Cited rather than
re-litigated.

**Counter-case at its strongest (what is missing).** The outage case is the easy
one. The common one is **degraded**: one Hetzner box, no failover, serving
thousands of AC lookups per build from GitHub-hosted runners over a link the ADR
says nobody has measured. `--remote_timeout` defaults to 60 s per operation; a
cache that is slow rather than refusing connections makes the build *slower than
no cache at all*, green, with nothing in the design noticing. No
`--remote_timeout` position, no `--remote_retries` position, no circuit-breaker,
no "if median RTT exceeds X, drop the remote cache" rule.

And the deferral is not fully honest: latency goes to WP-1c, but WP-1c's
threshold is itself [NEEDS CLARIFICATION #2], whose own text states the
principle it violates — "Without a number written **before** the measurement, a
disappointing result is negotiable after the fact." A threshold is a decision,
not a measurement; it belongs in this ADR.

**Ruling: partially answered; sustained on the degraded case and the threshold.**

**Fix.** (a) Name WP-1c's threshold as a number here. (b) Add a
`--remote_timeout` ruling to the `.bazelrc` row, with the reasoning that a
single-host cache's degraded mode is slower than its dead mode.

---

## Q16 | **Warn** | § Considered Options → Option D

**Anchor.** "### Option D — A different graph tool (Nx, Turborepo, Pants,
Moon)" … "None of the four has an established Rust story comparable to
`rules_rust` 0.74.0 + `gazelle_rust` on BCR".

**Counter-case at its strongest.** The strongest member of that family is
absent. `bazel-adoption-timing.md:9` names the prior research lane by title:
`.agents/research/research_graph_tools_middle_ground.md` — "**moon/Buck2**/Pants/Nx
lane". Buck2 is the one graph tool outside Bazel with a first-class Rust story
(reindeer, the prelude's Rust rules, Meta's own Rust monorepo) and is therefore
the only Option-D candidate that could contest criterion 1 — the weight-6 row
that decides the matrix. Excluding it, unstated, from a row scored **60 of 130**
is a strawman in the position where a strawman inflates the winner most.

The three remaining dismissals are themselves uneven: Nx/Turborepo and Pants are
dispatched with a sourced quote from `bazel-adopt` step 1, Moon with a bare
assertion ("Moon has no per-crate Rust test-target model either") and no source.

**Ruling: sustained.** Not answered; Buck2 appears nowhere in the ADR.

**Fix.** Name Buck2 in Option D and dispatch it on evidence. The honest
dismissal is available and short — no BCR-equivalent module ecosystem, a
markedly smaller external-ruleset surface for Python/TS, and no `rules_ocx`
analogue for the owner's tool-path mandate — and it costs two sentences. Cite
the prior lane's finding if it already made the argument.

---

## Q17 | **Note** | § NFR Availability

**Anchor.** "**Measured behaviour on 8.7.0 and 9.2.0** with the endpoint
refusing connections: `WARNING: Remote Cache: Connection refused`, every action
executed locally, **exit 0**."

**Counter-case.** Those numbers are faithfully carried from
`bazel-quality/caching.md:116` — BZL-CACHE-26's own evidence column, upstream's
measurement against a generic HTTP cache on Linux. Nothing was measured against
`bazel-cache.ocx.sh`, and this repository has no Bazel at all. Read cold, the
row's "Measured behaviour" attaches to *this* cache. `quality-core.md`
§ Verification Honesty: "state what was checked and what the result was";
"Stating 'verified' without citing evidence: **Block-tier**".

**Ruling: sustained, cosmetic.** The substance is correct and correctly applied;
only the attribution is missing. Filed as Note because no decision changes.

**Fix.** "BZL-CACHE-26 records, measured on 8.7.0 and 9.2.0 against an HTTP
cache on Linux: …". Same sentence, sourced.

*(Scan result, for the record: the ADR contains **no** instances of "should
work", "probably", "likely", "seems to", "presumably" or "ought to". On the
banned-phrase half of § Verification Honesty it is clean, and unusually so.)*

---

## Q18 | **Note** | § Considered Options vs § Open Questions — one question, two answers

**Anchor.** § Considered Options, Option C: "**This ADR's `go` is conditional on
WP-0's decision file, which cannot be written until the median is measured and
which retains the authority to return no-go.**" § Open Questions, line 1209:
"[NEEDS CLARIFICATION: … Is this ADR's 'go' conditional on WP-0's decision file
— with that file retaining the authority to return no-go … — or is adoption
already decided and WP-0 documentation-only?]"

**Counter-case.** The document asserts the answer in bold in one section and
asks it as an open question in another. Worse for the decorative/load-bearing
test the brief poses: the ratified dossier this ADR implements states the
autonomous stop condition as *"G3 is the definition of done"* — all four stages
green — with the owner letting the chain run unattended through `/hex-plan` →
`/hex-execute` → `/hex-finalize` (`bazel-full-adoption.md:203-205`). A WP-0
no-go is structurally unable to fire inside a chain whose completion criterion
is the thing WP-0 would refuse.

**Ruling: sustained.** The no-go authority is decorative as the surrounding
machinery is configured, not because the ADR is insincere.

**Fix.** Merges with Q1's. If the go is genuinely conditional, the Status line
must say so (`Proposed — blocked on WP-0 decision file`), the Decision Outcome
must read as the conditional, and the autonomous goal's stop condition has to
admit "WP-0 returned no-go; Option C built; done" as a *successful* terminus.
Then delete line 1209, since the document would answer it.

---

## What would make the recommendation earned

Three things, none of them large:

1. **Measure the lead signal**, post-`test:parallel`, and write the six-row
   decision file with real values (Q1, Q4, Q7). One `gh run list` and an hour.
2. **Verify anonymous reads on `bazel-cache.ocx.sh`** (Q5). One `curl`.
3. **Drop stages 3 and 4 to a follow-on decision** (Q12). This is Option B —
   which the ADR's own matrix already scores highest — and it dissolves Q2, most
   of Q9 and most of Q10 as side effects.

With those, the case for the Rust stage is genuinely strong: the 43 % figure is
real, the crate split really is the precondition that reopened this, and § Cache
staging, § Stage 2's gate contracts and § Acceptance's red-state discipline are
the best-argued parts of the document. The recommendation that is not earned is
the **scope**, not the direction.
