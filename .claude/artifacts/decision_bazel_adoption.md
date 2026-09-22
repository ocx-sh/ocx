# Bazel adoption decision

Repository: ocx · Decided: 2026-09-21 · Decided by: Michael Herwig (owner)

Satisfies **C-001** of [`plan_bazel_build_adoption.md`](./plan_bazel_build_adoption.md).
The binding design is [`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md); the
procedure is [`bazel-adopt`](../skills/bazel-adopt/SKILL.md) and its
[`go-no-go.md`](../skills/bazel-adopt/references/go-no-go.md). Every value below is a
command's output, recorded in the plan's § "Measured facts — 2026-09-21, this run"
(M-01 … M-08) or in `.agents/memory/hex.md`. **WP-00 re-measured nothing and invented
nothing**; where a value is owed rather than held, the row says so in the cell rather
than in a footnote.

### Signals

| Signal | Measured value | Command | Answer |
|---|---|---|---|
| Cheaper fix tried and named | The 2026-09-20 narrower fix, **landed and never re-measured**: parallel acceptance in `verify-deep` (`task test:parallel`), `CARGO_BUILD_TARGET` so schema-generate reuses the build dir, `sccache-action` with org secrets, and commit `91dea8ac` removing the four 30 s timeouts. The median after it landed is the row below and shows **no demonstrated movement**. The controlled before/after on one commit **is still owed** — WP-30 | `gh run list --workflow=verify-basic.yml --limit 50 --json startedAt,updatedAt,conclusion --jq '[.[]\|select(.conclusion=="success")\|((.updatedAt\|fromdate)-(.startedAt\|fromdate))]\|sort\|.[length/2\|floor]'`, and the same with `--workflow=verify-deep.yml` | **yes — landed, never re-measured.** Tried is not the same as exhausted, and unmeasured is not insufficient |
| Median whole-repo CI wall-clock | **verify-basic 1731 s, verify-deep 3407 s** — medians over the last 50 successful runs, measured 2026-09-21. The **3347 s** carried by the older records is **stale**: it predates `task test:parallel` on `verify-deep`. The two figures span different measurement windows over a moving commit stream, so the 60 s delta is noise, not a regression | `gh run list --workflow=verify-basic.yml --limit 50 --json startedAt,updatedAt,conclusion --jq '[.[]\|select(.conclusion=="success")\|((.updatedAt\|fromdate)-(.startedAt\|fromdate))]\|sort\|.[length/2\|floor]'`, and the same with `--workflow=verify-deep.yml` | **measured** — and it supersedes the stale 3347 s wherever that number is still carried |
| Largest CI job is a build job | In all five most recent successful runs of the two gating workflows (M-01): `Build & Unit Test (Windows)` at **1250 / 1456 / 1487 s** in the three `verify-deep` runs, `Smoke (Linux)` at **1195 / 1263 s** in the two `verify-basic` runs. `Build & Unit Test (Linux)` for comparison: **641 / 683 / 930 s**. This supersedes the ADR's inherited decomposition (research 5 `:15`), which said acceptance dominates — it does not; it is consistently second | `gh run view <run-id> --json jobs --jq '.jobs[]\|{name,secs:((.completedAt\|fromdate)-(.startedAt\|fromdate))}'` over run ids 35539665987, 35538021043, 35534195939 (verify-deep) and 35539665948, 35538021028 (verify-basic) | **yes** — in all five runs. Read the caveat under the verdict before spending this answer: largest is not compile-dominated, and the Windows job is a surface the Bazel lane never touches |
| Generator maturity per language | **Rust: Experimental.** `gazelle_rust` publishes exactly one version, `v0.1.0` (BCR `versions` is `["0.1.0"]`, re-read 2026-09-21), which pins Bazel 8.4.2 / rules_rust 0.67.0 with no version matrix, so that pin pair is exercised nowhere (corroborated 2026-09-21: its BCR `MODULE.bazel` at 0.1.0 declares `bazel_dep(name = "rules_rust", version = "0.67.0")`, and `.bazelversion` at tag `v0.1.0` is `8.4.2`); issue [#15](https://github.com/Calsign/gazelle_rust/issues/15) — "Transitive dependencies are considered when using `Cargo.lock`" — is **open**, verified 2026-09-21; there is **zero** handling of `[patch.crates-io]`, path deps or excluded members in its code or issues; and its default mode names every target `lib`, which breaks cross-package `deps` the moment two members exist — this workspace has 20. Python and TypeScript carry CI cost but neither is a wave-1 subject | `curl -s https://bcr.bazel.build/modules/gazelle_rust/metadata.json \| jq -r '.versions[-1]'` | **Experimental** → per `go-no-go.md:98-100` **hand-written BUILD files are the correct default**, which is plan ruling P3. The generator is the optimisation, not the path |
| Cross-repo coupling to model | **Three git submodules, all mode `160000`** (M-08): `external/rust-oci-client`, `external/docker_credential`, `external/sigstore-rs`. Each is also a `[patch.crates-io]` build dependency and each sits in `Cargo.toml`'s `exclude`. **None of the three declares a `MODULE.bazel`** | `git config -f .gitmodules --get-regexp path`, then `git ls-files -s external/` for the mode | **yes — three, and unmodelled.** Per BZL-ARCH-28 a submodule that is also a build dependency is an **open Bzlmod question, not a settled pattern**: Bzlmod has no submodule-equivalent primitive, and the nearest analogue needs the vendored fork to declare a module itself. WP-11 owns the route (C-007) |
| Build owner after adoption | Michael Herwig, **sole maintainer, no rotation**. Starlark is read by the owner — who already runs Bazel elsewhere (`rules_ocx`, `mirror-bazelbuild`), so tool novelty is low — and by the agent chain | (owner statement) | **Michael Herwig.** `go-no-go.md:138`'s documented failure mode, "a migration whose only Starlark reader is an agent has no reviewer for the diffs it produces", does not obtain |

### Verdict

go (owner-directed under the /goal of 2026-09-21; procedure signal 4 not established), because the decision is the owner's scope call and not a procedure output.

**This verdict is not a procedure output, and the file says so rather than dressing it
up as one.** `go-no-go.md`'s § "Reading the answers" offers three decidable readings and
a fourth line. None of the three fires here, so the disposition is the fourth line —
*"Anything else is a decision the owner takes, not one this procedure takes."* That is
plan ruling **P4**, and it is the reading this file stands on.

**Reading 3 clause 3 is not satisfied.** Its words are "the cheaper fix is in place
**and named as insufficient**". The cheaper fix is in place; it has **never been
re-measured**, and the controlled before/after is still owed. **Unmeasured is not
insufficient** — the clause asks for a measurement that has not been taken, and a row
answering *"yes — landed, never re-measured"* does not answer it. Reading 3 clause 4,
*"the build is the measured cost"*, is likewise not established: a largest-job name is
not a decomposition (see the caveat immediately below). The authority for the `go` is
the owner's `/goal` of 2026-09-21, which ratified the dossier with a definition of done
of all four stages. **The procedure is not the authority, and no tie-break was
invented.**

M-01's caveat, reproduced verbatim from
[`plan_bazel_build_adoption.md`](./plan_bazel_build_adoption.md) § M-01, because it is
what bounds the size of any win this decision can be read as promising:

> **What it does NOT establish, stated here and not in a footnote [R1].** The ADR's signal
> row asks two things — *"which job is the largest, **and how much of it is compile**"*.
> Only the first was measured. A job's wall-clock does not separate cacheable compilation
> and test execution from checkout, toolchain setup, downloads and the ten untouched
> `.verify:build-test` steps. **The second half is WP-30's job**, and until WP-30 runs, no
> claim about the size of the win is available. See P4.

> **And the arithmetic the caveat implies [R1].** `verify-deep`'s build stage is a
> three-OS matrix, so its duration is `max(windows, macos, linux)`. Bazel takes only the
> Linux leg (ADR § Stage 2 ruling 1). Taking Linux to zero shortens the matrix stage by
> **nothing**, because the workflow still waits for Windows at 1250–1487 s. **Stage 2's
> contribution to `verify-deep`'s 3407 s median is structurally zero.** The only surface
> where a win can land is `Smoke (Linux)` (1195–1263 s), minus its lint, build and
> non-test portions — the subtraction WP-30 owes. WP-00 writes this paragraph into the
> decision file verbatim.

### What would change it

- **WP-30's step-level cold/warm split of `Smoke (Linux)`** — reopens the *Largest CI job
  is a build job* row. If compile dominates that job, `go-no-go.md` reading 3 clause 4
  fires on a surface Bazel **actually touches**, reading 3 becomes a procedure `go`, and
  **this verdict can be amended in that commit**. If it does not dominate, the lane swap
  does not land (WP-30's own no-go terminus) and this verdict stays exactly as written.
- **The controlled before/after of the 2026-09-20 narrower fix on a single commit** —
  reopens the *Cheaper fix tried and named* row. Until it runs, "insufficient" is not a
  claim anyone here can make, and only "never re-measured" is true.
- **A BCR release of `gazelle_rust` carrying the Bazel-9 fix `d1ae032` plus a documented
  `[patch.crates-io]` story** — reopens the *Generator maturity per language* row and with
  it plan ruling P3; the 22 hand-written BUILD files become the fallback rather than the
  path.
- **A `MODULE.bazel` landing in any of the three `external/` forks** — reopens the
  *Cross-repo coupling to model* row, because BZL-ARCH-28's precondition is exactly that
  file and the coupling stays invisible until it exists.

### Explicitly not decided here

- **Target selection: deferred, see BZL-CI-01.** The ~40 min median with ~300 rule targets
  is a derived tripwire from one team's stated pre-adoption context, about target
  selection and never about adoption — it is not a published threshold and is not
  repurposed as one here.
- **Remote execution: deferred, see BZL-CACHE-07.** M-05 additionally measured that
  `rules_ocx` launchers exec an **absolute** `$OCX_HOME/packages/…/content/<bin>` path,
  which cannot work on a remote executor — so RBE is *structurally* out of reach, not
  merely unscheduled. Remote **caching** is unaffected.
- **Which `external/` route models the submodule coupling** — WP-11's call under C-007
  (M-08 names the two routes; this file pre-decides neither).
