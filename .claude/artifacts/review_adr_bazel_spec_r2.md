# Review R2 — `adr_bazel_build_adoption.md`, spec focus: closure + overshoot

**Pass:** re-validation of the Round 1 fix round (1279 → 2453 lines).
**Scope of this pass:** Q-A closure of B1–B3, H1–H5, Q1–Q12, F1–F7, N1–N5; Q-B
overshoot hunt (matrix drift, re-derived numbers, new quotes, new gate contracts,
internal consistency, marker hygiene, the crate-split amendment).
**Not re-reviewed:** the ADR from scratch. Warn/Note findings of Round 1 (spec W1–W9,
N1–N4; quality Q13–Q18; security F8–F12) were not in this pass's scope.

**Verification done at source, not from the ADR:** 22 citations opened at the cited
line (10 required). Every quote checked matched verbatim. Two number families did not.

---

## 1. Closure table

### Spec seat (B1–B3, H1–H5)

| ID | Verdict | Evidence |
|---|---|---|
| **B1** 72 → 39 cast targets | **closed** | `:1123` rules 39 via `# cast: true`, cites `test_recordings.py:1-4`, names the other 33 as site-rule inputs, enumerates through `test/scripts/doc_scripts_list.py`; `:638` and A3 `:2188` both say 39. Verified on disk: 39 headers, 39 `.cast` files, 72 `*.sh`. |
| **B2** 20 → 34 test targets | **closed (fix landed) — but see O-B1** | `:811` "20 `rust_library` + **34** `rust_test` (20 lib + 14 integration)"; per-target map replaces the bare `>= 20` floor; WP-1b item 3 added at `:1063`. Verified on disk: 20 `crates/*/Cargo.toml`, 14 `crates/*/tests/*.rs` with the exact per-crate distribution the ADR claims. **The derivation is right; four derived sites then say 54.** |
| **B3** website cache-eligibility contradiction | **closed** | `:540-549` now reads "The casts half of stage 3 and all of stage 4 are `local`-tagged" + "The website rule is the one sandboxed, shared-cache-eligible target … A planner must not read the `local` sentence above as covering it". Sibling sweep clean: `:324`, `:1192-1195`, `:1401`, A3 `:2211`, Consequences `:2374`. |
| **H1** `SCOPED_ROWS` is not a partition | **closed** | `:1283-1305` rules one `sh_test` per `test/tests/test_*.py`, keeps `SCOPED_ROWS` as the selection query, maps `escalate` → `//test:all`; `:1307-1317` adds the `--local_test_jobs=1` concurrency contract and names the xdist regression; A4 red 1 names `ocx_setup` (the non-overlapping row). |
| **H2** `bep_to_otlp.py` had no gate contract | **closed** | `:1883-1892` — Reads / Emits (span schema) / Exit 0 / Exit 0 no-op / three Exit 1s with stderr text / four red states. A5 `:2278` now reproducible offline. |
| **H3** call sites `:206`/`:223`/`:225` | **closed** | `:1092-1096` table + `:1098-1103` contract. All three opened: `taskfile.yml:206` = `cargo nextest run -p ocx_test_support --test workspace_structure --locked`; `:223` = `cargo nextest run -p {{.ITEM}} --locked --no-tests=warn`; `:225` = `cargo test --doc -p {{.ITEM}} --locked`. Exact. |
| **H4** no edited-file-set table | **closed** | `:645-667`, 13 rows, carrying both claim-diff consequences (the `.claude/rules.md` glob widening — `bazel-quality.md:18` `- "**/*.scl"` verified — and the docs page as **new**). |
| **H5** marker numbering + a spent slot | **closed (argued variance)** | Exactly 3 `[NEEDS CLARIFICATION #N]` tokens, numbered in the marker text; all 6 `Open Question #N` cross-references resolve. Marker 1 retired as H5 asked. The freed slot went to the rules_ocx pin question rather than floor-reachability, with `:2340-2343` arguing floor-reachability is ruled in § Stage 1 and verified in WP-1b — argued, not silent. The `rust-toolchain.toml` ↔ `MODULE.bazel` docket row was added (`:2088`). |

### Quality seat (Q1–Q12)

| ID | Verdict | Evidence |
|---|---|---|
| **Q1** go/no-go reading | **closed per the orchestrator's ruling** | `:225-283`. Does **not** say "No-go, and stop". All three readings quoted verbatim — checked against `go-no-go.md:158-168`, exact. Reading 1 dismissed on the "**tried**" clause (`test:parallel`, `91dea8ac`); reading 2 on multi-language + wall-clock-is-the-complaint; reading 3 on "the build is the **measured** cost" being unmeasured. Lands on the table's fallthrough, quoted verbatim, and names the owner (dossier ratification + the `/goal`). |
| **Q2** A3/A4 abortability | **closed** | A3 gets two abort triggers (`:2216-2228`), A4 a 50 % wall-clock ceiling (`:2264`). Both carry numbers. |
| **Q3** Option C ≠ a `sources:` guard | **closed** | `:341-361` quotes `taskfiles/rust.taskfile.yml:228-234` verbatim (opened, exact) and rebuilds C as a bracket redesign onto `junit.xml` (`:254` verified). |
| **Q4** predecessor mis-cited as a no-go | **closed** | `:363-383` quotes research 5 `:15`, `:17`, `:57`. All three opened at `/home/mherwig/dev/ocx-evelynn/.agents/discussions/bazel-adoption-timing.md` — verbatim. Corrections row `:2050`. |
| **Q5** anonymous read | **closed** | § Context `:76-105`: the claim is withdrawn, the contradiction stated with both counter-sources quoted, one `curl` made a hard WP-0 precondition, three branches costed. |
| **Q6** matrix | **closed** | Delivery-cost row added at weight 3; criterion 1 scored as a range `5 (2–5)`; the pessimistic collapse computed as its own row. See § 2 item 1 — **zero unexplained drift**. |
| **Q7** go-no-go decision file | **closed (see O-W3)** | `:285-302` prints the table with its empty cells and quotes `go-no-go.md:210-211` (opened, exact). |
| **Q8** `$XML_OUTPUT_FILE` | **closed** | Ruling 5 `:971-991` + WP-1b item 1 `:988`. Cites `rust.taskfile.yml:254` (verified) and reaches `quality-core.md` § "Don't Own Non-Domain Code" for the hand-grammar ban. |
| **Q9** rules_ocx blocking stage 1 | **closed** | `:884-904` rules entry at stage 3; `:1973-1978` restates the mandate as unchanged in composition, moved only in sequencing. |
| **Q10** tool-pin in the cache key | **closed** | Ruling 2(c) `:1438-1447` — change the `bun` pin, require a miss; red half named. |
| **Q11** BZL-CACHE-12 + capacity | **closed** | Ruling 10 `:1786-1814`. Quote checked against `bazel-quality/caching.md:117` — verbatim. 50 GB capacity handed to WP-1c; "already provisioned is about the host, not the size" reconciled with NFR Cost `:1988`. |
| **Q12** four-stage scope | **closed per the orchestrator's ruling** | Scope stays. `:513` records the matrix favours B (114) and C (110) over A (97); `:515-517` records the panel's independent B recommendation; `:519-521` records four stages as an owner scope decision taken outside the matrix; `:523-524` states no weight was adjusted. Confirmed by arithmetic — see § 2 item 1. |

### Security seat (F1–F7)

| ID | Verdict | Evidence |
|---|---|---|
| **F1** same-repo PRs get secrets | **closed** | Ruling 5 `:1588-1641`. BZL-CACHE-01 quoted verbatim (checked at `caching.md:69`). The trusted-event `env:` expression given as YAML; `verify-basic.yml:90-91`/`:100` opened — the comment does say "a **fork** PR has no secrets" and `AWS_SECRET_ACCESS_KEY` is at `:100`, job-level. A2 gains the real control ("assert `BAZEL_CACHE_WRITE` is empty on a same-repo PR") and relabels the old test as defence in depth. |
| **F2** `.bazelrc.user` second writer | **closed** | Ruling 4a `:1526-1559`. Struck from the file-set table (`:631` now reads "**No `--credential_helper` of any kind**"); sweep of all 7 `.bazelrc.user` mentions finds no surviving credential. |
| **F3** helper provisioning | **closed** | Ruling 4b `:1561-1587` — four lines, mode 600/700, `if: always()` deletion, the `$RUNNER_TEMP` upload-artifact hazard (`verify-basic.yml:178-186`), and a red state with both halves. |
| **F4** AC/CAS asymmetry, detection, rotation | **closed** | Ruling 6 `:1678-1692` (the asymmetry, with research 3 finding 6 quoted) + ruling 6b `:1722-1740` (generation salt, PUT-outside-window alert with its own red state, 90-day rotation with a named owner). |
| **F5** `git_override` = analysis-time code | **closed** | Ruling 9 `:1762-1784` — protected branch, fail-closed availability posture carried into NFR Availability `:1985`, and the private-repo onboarding consequence docketed to WP-0b. |
| **F6** anonymous read as content channel | **closed** | Ruling 6a `:1694-1720`, explicitly conditioned on the probe, and converted into the standing "no cacheable action on the writing lane reads an ambient secret" invariant with ruling 1a as its gate. |
| **F7** the tag corollary had no gate | **closed** | Ruling 1a `:1382-1399` — `task bazel:tag:guard`, full five-row contract, four red states. `bazel-quality/testing.md:77` (BZL-TEST-08, MUST) verified. |

### Cross-model adversary (N1–N5)

| ID | Verdict | Evidence |
|---|---|---|
| **N1** ADR directed its own reviewers | **closed** | Both sites rewritten. `:692-693` now ends "The override and its reason are recorded; **what a reviewer concludes from them is a reviewer's call**." § Relationship `:170-176` states the `Superseded By:` override with its reason and stops. Sweep of every `reviewer` occurrence: the only two left are that deferral and a neutral "stated so a reviewer does not look for it" (`:665`). |
| **N2** asserted vs observed coverage | **closed** | `:999-1003` requires **observed** per-target `testcase` counts from the binaries' own `test.xml`; `:1005-1014` names the old shape as `quality-core.md` § Unchecked Green and states "An asserted table is not a floor"; the committed map is demoted to the *reader* floor. Red state includes the three-`#[test]`-deletion. |
| **N3** inverted hermeticity gate | **closed — and the replacement discriminates** | `:1406-1413` names the inversion ("the demanded outcome is the one a correct implementation cannot produce"). Split into (a) declared-input invalidation — hit across paths, miss on a declared change, with the hit-on-change red half; (b) ambient-input isolation — **forced uncached execution** in both runs ("a run that can hit measures nothing here"), byte-compare, with a planted `date` as the red half and an explicit "a mutation that *fails* to produce the difference means the mutation missed"; (c) tool-pin participation. Each can reach both states. The defect is not reproduced in any of the four new tables. |
| **N4** stage-4 cache key | **closed** | `:1319-1343` — "Owning registry state is an *isolation* property; it is not input tracking, and the two were conflated." Result caching disabled (`external` + `--nocache_test_results`); inputs still declared for selection, binary **by content digest**; a `(cached)` acceptance target declared Block-tier. Sibling sweep: every stage-4 caching statement in the document (`:406`, `:542`, `:1279`, `:2047`, `:2048`, `:2377`) says the same thing. A4 red half 2 demonstrates both states (tagged → no `(cached)`, untagged sibling → `(cached)`). |
| **N5** no executable threshold | **closed** | NC#2 `:2347` carries ≥ 4 min median over 5 runs, RTT < 150 ms p50, WP-1a = 3 working days, WP-1c = 1 working day. A3 abort: < 3 min cold **or** < 5 invocations/week. A4 abort: > 50 %. Each states "proposed here so the measurement cannot be renegotiated afterwards; the owner may move it." |

**Closure result: 30 of 30 findings closed.** None refused silently; the one variance (H5's slot choice) is argued in the ADR's own text.

---

## 2. Overshoot findings

### Item 1 — matrix drift: **none.** Reconstructed and accounted for.

Round 1 recorded **A 94 / B 108 / C 98 / D 60** against max 130, weights summing to 26
(`review_adr_bazel_quality.md` Q6: "I recomputed all four columns (94 / 108 / 98 / 60
against a 130 max, weights summing to 26)"). The ADR now records **A 97 / B 114 /
C 110 / D 63** against max 145, weights summing to 29.

Exactly one row was added — `**Delivery cost / time to first benefit (5 = cheapest)**`,
weight 3, scored A 1 / B 2 / C 4 / D 1. Subtracting it:

| | A | B | C | D |
|---|---|---|---|---|
| Current total | 97 | 114 | 110 | 63 |
| less delivery row (`score × 3`) | −3 | −6 | −12 | −3 |
| **= Round 1** | **94** | **108** | **98** | **60** |

Every column reconciles exactly. **No criterion weight changed and no per-option score
changed.** Max moved 130 → 145 because weights moved 26 → 29 (3 × 5 = 15). The
pessimistic row is arithmetically right too (`6 × 3 = 18` off A and B only; 79 / 96 /
110 / 63). The added row is the one Q6 asked for, at the weight Q6 proposed, and the ADR
accounts for it in its own text (`:477-479`). The criterion-1 range `5 (2–5)` is Q6's
other fix and does not move the optimistic totals.

**Nothing moved that the ADR does not explain.**

### Findings

| ID | Severity | § anchor + quoted phrase | What is newly wrong | Evidence | Fix |
|---|---|---|---|---|---|
| **O-B1** | **Block** | § Stage 2 floor contract `:1002` — "fewer test targets than `crates/TEST_TARGET_MAP.toml` has rows **(54 today)**"; § Stage 2 WP-1b `:1064` — "the `rust-suites` count from `cargo nextest list --workspace` … **54 today** (§ Stage 1)"; § Acceptance A2 `:2141` — "its target count equals `cargo nextest list --workspace`'s `rust-suites` count **(54)**"; § Risks `:2402` and `:1048` — "would report **~54** against a floor of 8213" | The fix round substituted the **all-targets** count (54 = 20 `rust_library` + 34 `rust_test`) for the **test-target** count (34) at every site whose subject is test targets, `rust-suites`, or `TEST_TARGET_MAP` rows. This turns B2's and N2's own fix into an unsatisfiable green: `:1016` defines the map as "one row per **test** target", so it has 34 rows; a map built to the stated 54 carries 20 `rust_library` rows that emit no `test.xml`, and the floor's own Exit-0 clause ("every target in `crates/TEST_TARGET_MAP.toml` is present in `per_target`") then reds on every run, forever. A2's green can never pass, and WP-1b item 3's verification compares against a number `cargo nextest list` cannot return. | ADR's own derivation, `:811`: "Today's target count is therefore 20 `rust_library` + **34** `rust_test** (20 lib + 14 integration)". ADR `:1016`: "`crates/TEST_TARGET_MAP.toml` is one row per test target". Verified on disk: `ls crates/*/Cargo.toml \| wc -l` → **20**; `ls crates/*/tests/*.rs \| wc -l` → **14** (distribution exactly as `:803-806` claims). `taskfiles/rust.taskfile.yml:607-610` opened: `count=$(cargo nextest list --workspace … suites = json.load(sys.stdin)["rust-suites"]; print(sum(len(s["testcases"]) for s in suites.values())))` — `rust-suites` is keyed by **test binary**, so it returns 34, never 54. The ADR itself says so at `:2342`: "the floor-reachability question is ruled in § Stage 1 (**34** targets)". | Replace 54 → 34 at `:1002`, `:1048`, `:1064`, `:2141`, `:2402`. Keep 54 only where the quantity is all `//crates/...` targets. |
| **O-B2** | **High** | § Stage 1 drift-gate contract `:917` — "Fewer than **23 packages**, or fewer than **54 targets** (20 `rust_library` + 34 `rust_test`, § Stage 1) read from either side. `BUILD drift check read <n> packages / <t> targets, **expected 23 / 54**` … **The target floor is derived from the same `crates/TEST_TARGET_MAP.toml` the floor reader uses, so the two cannot disagree.**" | Two new defects in one row. (i) The gate's own **Reads** row is "Every `crates/*/BUILD.bazel` **and `external/*/BUILD.bazel`**", and `:636` gives each of the 3 `external/` packages a `rust_library`. Reading 23 packages therefore yields **57** targets, not 54 — the floor understates its own reader by 3 and its error message names a wrong expectation. (ii) The closing sentence is impossible as written: a map of test targets (34 rows) cannot derive a floor of 54 *or* 57, so the two gates can and will disagree — the opposite of what the sentence promises. | ADR `:636` "`external/<name>/BUILD.bazel` × 3 \| `rust_library` only"; ADR `:913` **Reads** names both directories; ADR `:1016` defines the map as test-target rows only. Verified on disk: `ls -d external/*/` → 3. | State `expected 23 / 57`. Derive the target floor from the enumerated BUILD-file set, not from `TEST_TARGET_MAP.toml`, and delete the "cannot disagree" sentence or scope it to the test-target half. |
| **O-H1** | **High** | § Open questions disposition `:2072` — "The six `GITHUB_*` vars **only populate `ci.run_url`** and their absence matches non-GHA release behaviour." | The fix landed at the ruling and at the corrections table, and not at the third site. The ADR's own corrections table lists this exact sentence as a **corrected error** — and the uncorrected form is still live three sections away, inside the table a planner reads to learn what is already decided. A document that ships its own listed error is the Q-B failure shape in its purest form. | ADR `:1473-1476` (ruling 3): "The six `GITHUB_*` vars populate the optional `ci` block … `ci.run_url`, `ci.workflow`, `ci.git_ref` and `ci.sha` (`crates/ocx_cli/src/app/build_info.rs:146-157`; **the previous draft said `ci.run_url` alone, which is narrower than the code**)". ADR `:2059` (corrections table): "\| The six `GITHUB_*` vars populate `ci.run_url` only \| They populate `ci.run_url`, `ci.workflow`, `ci.git_ref`, `ci.sha` \|". Source opened — `crates/ocx_cli/src/app/build_info.rs:146-157` is `fn ci_info()` returning `run_url`, `workflow: option_env!("GITHUB_WORKFLOW")`, `git_ref: option_env!("GITHUB_REF")`, `sha: option_env!("GITHUB_SHA")`. Ruling 3 is right; `:2072` is wrong. | Mirror ruling 3's wording into `:2072`. |
| **O-W1** | Warn | § Stage 1 `:776` — "A **23-crate** pilot fails slower and in more places than a one-crate pilot"; `:778` and `:2424` — "the fallback … is **23 files carrying 54 targets**" | The BUILD-**file** count (20 `crates/` + 3 `external/`) is used as a **crate** count, in the same document that verifies the crate count as 20 nine lines later. And 23 files carry 57 targets, not 54 (same off-by-3 as O-B2). Cosmetic against the decision, but it is a new number the previous draft did not carry, and it is the sentence a planner sizes the hand-written fallback from. | ADR `:795`: "20 workspace members (`Cargo.toml` `members = ["crates/*"]`, **verified count 20**)". Verified on disk: 20 `crates/*/Cargo.toml`; `Cargo.toml` `members = ["crates/*"]`, `exclude = [3 external]`. | "A 20-crate pilot"; "23 files carrying 57 targets". |
| **O-W2** | Warn | § NFR Scalability `:1984` — "BZL-CI-01's tripwire for adopting target selection is ~300 rule targets **and** a median wall-clock past ~40 min. **The target half of that tripwire is therefore all but reached on day one**"; § Consequences `:2382` — "close enough to BZL-CI-01's ~300 tripwire **that target selection becomes a live question within one release**" | The rule says the opposite of "half of a conjunctive trigger". `~300` is not a trigger at all — it is the signal that says *measure the wall-clock*, and the rule states that in the same sentence. The ADR then reads 274 targets as making target selection "a live question", which is the reading BZL-CI-01 exists to forbid. (The ADR's own remedy at `:1984` — "re-read the moment WP-1c's median exists" — is the correct behaviour; the framing around it is not.) | `.claude/rules/bazel-quality/ci.md:117` opened, verbatim: "Cross into 'consider target selection' **only when** the whole-repo job's median wall-clock on the widest CI runner is consistently past **~40 minutes**; treat **~300 rule targets** as the tripwire that says **measure the wall-clock now, not as the trigger itself**." Same file `:183` lists "quoting '~300 targets' as an industry threshold rather than a derived tripwire" under What Agents Get Wrong. The target arithmetic itself is correct: 23 + 34 + 39 + 172 + 6 = **274**, and the 172 is verified (`ls test/tests/test_*.py \| wc -l` → 172). | Restate as: 274 targets crosses the signal that says measure; the switch point is the wall-clock, which WP-1c produces. Drop "live question within one release". |
| **O-W3** | Warn | § The decision file `:298` — "**Two empty cells** and one 'not established'. **Both empty cells are one command each and both belong to WP-0**" | The ADR quotes `go-no-go.md`'s mechanical empty-cell check two lines above and then reports a count that check does not produce. Running it over the table returns **four** rows, not two: `Median whole-repo CI wall-clock`, `Largest CI job is a build job`, **`Build owner after adoption`** (its Answer cell is empty) and `Anonymous cache read`. The `Build owner` row belongs to no work package and needs no command, so "both belong to WP-0" is false of the set the quoted check returns. Same shape the ADR polices elsewhere: cite a mechanical check, report a number from eyeballing. | ADR `:280-282` quotes `go-no-go.md:210-211` (opened, verbatim): "**Empty output = every signal row carries a measured value.** Any line printed is a row with an empty cell, and the verdict is not writable yet." Applying `grep -nE '\|[[:space:]]*\|'` to `:287-297`, filtered of the separator, returns 4 rows including `\| Build owner after adoption \| Michael Herwig (sole owner) \| owner statement \| \|`. | Fill the `Build owner` Answer cell (it is "Michael Herwig") so the check returns the set the prose describes, or restate the count as what the check returns. |
| **O-N1** | Note | § Observability `:1890` and § Cache staging ruling 1a `:1397` — both reader floors derived from `crates/TEST_TARGET_MAP.toml` | Three different gates now floor on one map that counts one thing. `bep_to_otlp` emits "one span per **target**" including `rust_library`; the tag guard queries the `//...` universe (~274 targets). Neither floor measures its own reader's subject, so both are far weaker than they read — and one map cited as authority for three different quantities is the mechanism by which O-B1 got in. | ADR `:1886` (one span per target), `:1392` (universe query `//...`), `:1016` (map = one row per test target). | Floor each reader on its own subject; keep the map's authority to the crate test-target count. |
| **O-N2** | Note | § Cache staging ruling 3 `:1466-1468` — "`crates/ocx_cli/build.rs:36-42` … its own comment states the consequence: `build_timestamp(true)` 'emits the current UTC time every invocation'" | The quoted sentence is at `build.rs:35`, one line above the cited range. The gating line the ruling depends on (`let in_ci = std::env::var_os("CI").is_some();`) **is** in range, at `:40`. | `crates/ocx_cli/build.rs:35` opened: "// build_timestamp(true) emits the current UTC time every invocation,". Verbatim match, wrong range. | Cite `:35-42`. |
| **O-N3** | Note | § Open Questions `:2349` — NC#3, "the question is **no longer load-bearing for correctness**" | A capped owner-decision slot is spent on a question the ADR itself declares non-load-bearing, and which is a fact about an out-of-tree repository rather than a decision — one WP-0b already owes (`:2075`). The weakened residue of H5's original objection: not settled by the ADR, but not an owner's to settle either. | ADR `:2349` and `:2075`. | Optional. If the slot is wanted back, fold NC#3 into WP-0b's record and leave two markers. |

---

## 3. Verdict

**Not ready for `/hex-plan` — 1 Block, 2 High, 3 Warn, 3 Note open (9 total).**
All 30 Round 1 findings closed, the matrix moved only by the delivery-cost row Q6
demanded, and every one of 22 spot-checked citations matched at the cited line. The
entire residual is one systematic substitution — **54 where the ADR's own derivation
says 34** — plus its two off-by-3 relatives and one un-mirrored correction. O-B1 alone
is a one-line-per-site edit; after it, this is plannable.
