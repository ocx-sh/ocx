# Review: `plan_crate_split_workspace.md` — spec compliance (post-stub, plan-artifact)

**Reviewer:** reviewer:spec (opus) · **Tier:** xhigh · **Base:** `evelynn` @ `d8750fd7` · **Date:** 2026-09-16
**Target:** `.claude/artifacts/plan_crate_split_workspace.md` (read in full)
**Sources:** ADR (full), design addendum (full, D-001…D-066), system design § 3 / § 12, the four discover artifacts, `hex-execute/SKILL.md` § Argument syntax / § Work packages / § Schedule, `hex-core/references/{decompose,verify,worktree}.md`, quality-core § Unchecked Green.

Method: the C-/S-/D-ID sets, the WP Scope cells, the Testing rows, the Depends-on column, the mermaid edges and the Size histogram were extracted by script and compared mechanically; every wildcard file cell in waves 7–9 was resolved against the tree with `command grep`.

## 1. Mechanical coverage

91 IDs defined (C-001…C-076, S-001…S-015). Testing strategy: 91/91 covered. WP Scope cells: 90/91 — **S-012 is in no WP**. No Scope or Testing row cites an undefined ID. Every D-ID is cited somewhere in the plan; D-023 and D-035 are not cited by any C-row (carried textually by C-036…C-040 and C-056 — cite them).

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 1 | **Block** | § UX scenarios S-012 / § Work packages | S-012 ("full acceptance suite passes unmodified at every commit; goldens unchanged") — the owner's central constraint — appears in no WP's Scope cell, so no WP owns it and no merge predicate asserts it. | Add `S-012` to every WP's Scope cell (or a one-line table preamble: "S-012 and DEC-10 are implicit members of every WP's scope and of the merge predicate"). See #2 for the predicate. |

## 2. Testability

Most contracts name a red state. Exceptions:

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 3 | High | C-053 `ssrf_guard_on_every_client_construction` | The test as worded ("every `reqwest::Client`/`ClientBuilder` construction … seeds a `GuardedResolver` or is preceded by `guard_destination`") is **red on the current tree**: `forge/http.rs:40`, `oci/index/ocx_index.rs:257,277` (production) and ~10 test-site constructions are unguarded — the pre-existing opt-in gap the ADR rules out of scope (#409). A structural-only WP cannot turn it green. | Specify the test as a **ratchet with a committed allowlist** (`crates/ocx_cli/tests/fixtures/ssrf_unguarded_baseline.txt` naming today's sites, test-module sites excluded via `#[cfg(test)]` stripping); red state = one *new* unguarded construction; the allowlist only shrinks (#409 empties it). |
| 4 | High | C-007 / C-010 / DEC-7; WP-09, WP-10, WP-12, WP-37 | Standing guards that must outlive `ocx_lib` (`no_log_shim`, `no_classification_in_libraries`, `overrides_are_test_only`, E3) take their witness fixtures from `crates/ocx_lib/tests/fixtures/boundaries/` (`log_shim.rs.txt`, `classify_impl.rs.txt`, `env_accessor_reach.rs.txt`). WP-37 deletes `crates/ocx_lib/**` → property 3 (witness) fails, or someone drops the witness and the guard goes vacuous. | Standing-guard fixtures live under `crates/ocx_cli/tests/fixtures/boundaries/` from WP-02 on; phase-1-only fixtures stay under `ocx_lib` and die with their tests (C-049 ii). State the split in C-007. |
| 5 | Medium | C-048 / D-034 `test_no_crate_path_assertions.py` | (a) "no string literal in `test/tests/*.py` contains `ocx_lib::`" is red **today**: module docstrings are string literals (`test_config_setup.py:8`, `test_self_setup.py:502`, `test_package_create_extract.py:45` comment) — WP-20's file list has no docstring edits. (b) The scanner's own needle literal `ocx_lib::` matches itself (quality-core self-matching detector). | Scan with `ast`, skip docstrings and the scanner's own file; build the needle by concatenation. Or list the DEC-10(e) docstring re-spells in WP-20's Expected files. |
| 6 | Medium | C-075 vs Testing row C-071…C-075 | The only named test asserts no `crates/ocx_lib` under `.claude/**` and `CLAUDE.md`; `.agents/memory/hex.md`, the two website pages and `product-context.md` Technical Overview are outside that sweep — their green is "never ran". | Extend the sweep's path list to every file C-075 names. |
| 7 | Medium | C-029, C-033, C-040 | "Emitted log lines are byte-identical", "rendered strings identical", "preserved call-for-call" — no named oracle. S-013 pins one library `debug!` line, not byte-identity; C-033 names no acceptance file; C-040's push order is only implied by push/cascade acceptance tests. | C-029: drop "byte-identical" or pin it with a golden of one full `-vv` run under `OCX_LOG`; C-033: name the `test:scoped` files (`test_inspect.py`, `test_deps.py`); C-040: name the acceptance files as the oracle in the contract, not only in the B3 step text. |
| 8 | Low | C-014(a) | The `exec` candidate's literal `"exec"` lives in the helper `_run_run`, not the marked test's body; a body-only scan reds falsely. | Scan module-reachable literals, or take the verb as a marker arg (`@pytest.mark.smoke("exec")`) — exact and cheaper. |
| 9 | Low | C-049 (ii) | Contains an inline self-correction ("re-declared via `#[path]`? — **no**: …") — editorial noise in a contract builders read. | Delete the aside; keep "the move commit rewrites `mod` declarations so it compiles; nothing else". |

## 3. Owner constraints as acceptance criteria; ADR feature list

Every item in the owner's feature list is present: 17 crates (C-001), `ocx_test_support` (C-025), unit-test split + floor (C-070, C-049), boundary tests (C-006/007), `rust:deps:direction` (C-009), `verify:scoped` ≤ 5 min (C-019), smoke ≤ 60 s + three anti-rot + budget (C-012…C-014), deep triggers (C-016), `.verify:mark` scope (C-021), READMEs (C-001), globs/catalog/CLAUDE.md/arch-principles/test_ai_config (C-071…C-073, C-024), `ocx_exit` seam (C-050), sealed `OciTransport` (C-053), tiering adversarial test (C-054), SSRF boundary test (C-053), four integration tests (C-057, C-059), two workflow `paths:` (C-056, C-061, C-065), four CLAUDE.md (C-074), `unreachable_pub` ratchet (C-011), per-rule globs (C-024), E1 (C-068), satellite job + window (C-017/018, DEC-4), satellite specs (C-076). **No feature cut.**

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 2 | **Block** | § Work packages (every row) | The owner's structural-change-only rule is encoded once (DEC-10, S-012, C-069) — not on every WP, and nothing mechanical enforces "no assertion changed / no skip added": `SUITE_FLOOR` counts tests, it cannot see a weakened assertion. | Add to the § Parallelization merge predicate: "`git diff --name-status <base>..HEAD -- test/` shows no `D`; every `M` hunk under `test/tests/*.py` adds only `@pytest.mark.smoke` lines, new test functions or docstring text and removes only docstring text — checked by `scripts/test_diff_guard.py --self-test` (stdlib, WP-03) and quoted in § Schedule log". Cite S-012 per WP (#1). |
| 10 | Medium | ADR § Phase 1 verification / § Validation vs C-008, C-047 | ADR requires the reciprocated-pair count recorded per phase-1 commit and monotonically decreasing; the plan records it once (WP-20) and does not gate it. DEC-12's "70 reciprocated module pairs" has no source — no discover artifact carries 70 (file map cites the ADR's 63). | Either add `reciprocated_pairs` to `edge_inventory.baseline.json` with the same only-decreases rule, or state in C-047 that the pair count is review-only. Cite the run that produced 70 or drop the number. |
| 11 | Medium | Testing row C-016/C-017 · § Manual checks | "One draft→ready PR observed in CI at batch B1's end" needs a push; the project rule is *never push — the human decides*. As written it is a batch-end gate no agent can clear. | Mark it an owner action in the B1 handoff (`## Actions`), not a gate; S-008's automated half is `test_workflows.py`. |

## 4. Consistency with the ADR and addendum

No WP or contract contradicts the facade, cycle-tool, `codesign`, `ocx_setup`, `resolve_tiered`, `layer_layout`, exit/console or OQ1 rulings. Every D-ID has a carrier. Deviations the plan records honestly: DEC-1 (`log::` not `tracing::`), DEC-2, DEC-4 (satellite job non-blocking for all of phase 2 — the ecosystem tier's lockstep obligation is suspended until the mirror follow-up; stated), DEC-6 (`/hex-finalize` full-mark reinterpreted as `release:`/`main`), DEC-11.

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 12 | Low | C-061 vs C-074 | C-074 says the four CLAUDE.md files carry "exactly the ADR § AI-config texts"; the ADR's `ocx_shell` text says `crate::project`, C-061 says `ocx_project` (correct post-split). | C-074: "the ADR texts with crate paths re-spelled post-split". |
| 13 | Low | C-052 vs DEC-5 / C-058 | C-052 "only `ocx_oci`, `ocx_shell`, `ocx_package_manager` list it" vs DEC-5's five `ocx_script` deps including `ocx_console` (unused today — 0 `cli::` refs in `script/`). | C-052: "…and `ocx_script` may; today none other lists it". |
| 14 | Low | Addendum § B row 1.17 | Still says "sed `crate::log::X!` → `tracing::X!`"; the plan (DEC-1, C-029) overrides. Plan wins; the addendum row is stale. | Note in DEC-1 that B-1.17's mechanism cell is superseded. |

## 5. Parallelization table

Waves 1, 2, 3, 7, 28: file-disjoint (wildcards resolved: WP-13's "≤ 4 further `.ink(`/`.of(` sites" = `api/data/install.rs` only; WP-15's 8 + 5 CLI files do not touch `app.rs`, `app/context.rs`, `command/version.rs`). Mermaid edges = Depends-on column exactly. Histogram `medium 2 · high 8 · xhigh 29` = S 2, M 10 (−2 promoted), L 27 (+2) — arithmetic correct. Every shared-file claim in the prose has a sufficient (transitive) edge. Defects:

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 15 | **Block** | WP-18 vs WP-19 (wave 9, no edge) | Both declare `package_manager/managed_config/publish.rs` (WP-18: `key_ref` use line; WP-19: `tls` use line). Same wave, neither an ancestor → merge predicate fails on whichever lands second. | Add `WP-18` to WP-19's Depends-on (WP-19 is M-sized; the serialization costs one wave), or move the `publish.rs` use-line edit of WP-19 into WP-18. |
| 16 | High | WP-17 (wave 8) | Depends on WP-16 (wave 8) — a same-wave edge; the graph, the B3 width-3 claim and "WP-14b, WP-16, WP-17 (parallel)" contradict it. If parallel, `utility/fs.rs` (WP-16 removes `mod assemble`; WP-17 must add `mod symlink`) collides and WP-17 omits it. | Decide: (a) WP-17 depends on WP-14a only, owns `utility/fs.rs`, and WP-16 does *not* touch `utility/fs.rs` (the `assemble` mod line moves to WP-17) → real wave 8; or (b) keep the edge and set WP-17 wave 9, B4 width 3. Either way add `utility/fs.rs` to WP-17. |
| 17 | High | WP-14b vs WP-16 (wave 8) | `publisher.rs:16` re-exports `LayerRef` today; consumers outside WP-14b's set: `managed_config/publish.rs:42` (WP-16 `git mv`s it in the same wave), `setup/version_spec.rs`, `package_manager/tasks/patch_test.rs:257`, and 5 (not 3) CLI files (`build_receipt.rs`, `conventions.rs`, `command/{package_push,package_test,patch_test}.rs`). If WP-14b hard-cuts the re-export, it collides with WP-16 and under-declares 7 files. | State in C-040/WP-14b: `publisher.rs` keeps `pub use crate::oci::layer_ref::{ArchiveMediaType, LayerRef, LayerRefParseError}` through phase 1 (a re-export inside one crate is not the extraction-time shim C-049 forbids); WP-30 hard-cuts it. Add `oci.rs` (`pub mod layer_ref`) to WP-14b's set. |
| 18 | Medium | WP-17 wildcard "~20 `crate::symlink` callers" | Actual set is 26 files and includes `project/registry.rs`, `record/execution_record.rs`, `script/guard.rs`, `shim.rs`, `hardlink.rs`, `reference_manager.rs` — none named by the cell's sub-globs. | Enumerate the 26 (`command grep -rlE 'crate::symlink' crates/ocx_lib/src`). |
| 19 | Medium | WP-18 "≤ 2 CLI files"; `key_ref` users | CLI: 7 files name `KeyRef`/signing state (`command/{package_attest,package_sbom,package_sign,package_sign_common,package_verify}.rs`, `error_envelope.rs`, `options/key.rs`); lib: `oci/sign/{bundle,error,key_signer}.rs`, `oci/verify/error.rs`, `package_manager/tasks/{attest,sign}.rs` are absent from the set. | Enumerate; no wave-9 collision with WP-19 results. |
| 20 | Medium | WP-10 Expected files | "the 51 impl-holding files of `discover_crate_split_edge_inventory.md` § 7" — § 7 says the ADR's 51 is stale (`config.rs` new, 64 impls) and lists no file set. | Take the file list from `edge_inventory.json`'s § 7 census (declare at launch, as WP-20 does). |
| 21 | Medium | WP-08 (S, wave 1, isolated) | ~20-line single concern below the WP overhead floor; no justification line (decompose.md counterweight). WP-39 (spec artifact) has an implicit reason but no line either. | Fold WP-08 into WP-03 as a sequential step (drop the WP-05 → WP-08 edge), or add the one-line justification. Add WP-39's line ("out-of-repo spec; no code"). |
| 22 | Low | WP-01 Expected files | C-003 (`--locked` on the fast `check`) is a root `taskfile.yml` edit; the cell says "include line only". | Widen the parenthetical. |

## 6. Batches and invocation

`hex-execute` computes a **ready-set** from the table's `Depends on` + `Status` and "launches eligible WPs immediately … recomputing on every merge" (SKILL.md § Work packages, § Schedule; decompose.md "Launch on dependency-ready; waves are a derived reporting view"). Nothing reads a `Batch` column or an `Active-batch` line; resume reads only the `Status` column. **On the first invocation the framework runs WP-01 → WP-39 in one go**: after WP-09 merges, WP-10 is eligible, and so on to WP-39. The batch-end `/hex-review` gates (B4 "extraction licence", B6/B9 security) never fire.

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 23 | **Block** | § Status `Active-batch`, DEC-9, § Execution batches, `Batch` column | The batch convention is not executable by `/hex-execute` as written; DEC-9's owner directive (one context per wave set, review per batch) would be silently bypassed. | Smallest fail-closed mechanism: put a **non-WP token** in the Depends-on cell of every batch's first WP(s) — `WP-10: WP-09, B1-review`; `WP-13/14a/15: WP-12, B2-review`; … `WP-38/39: WP-37, B11-review` (11 tokens). The ready-set can never resolve it to `merged`, so nothing launches past the boundary; the batch-advance step (after `/hex-review`) deletes the token. Draw the tokens as gate nodes in the mermaid graph. Move the `Batch` column out of the table (membership already lives in § Execution batches) so the table keeps the nine-column shape the skill names. |

## 7. Measured numbers

Checked against the discover artifacts: 3,618 collected ✓ (`discover_tests` § 1); 8,049 nextest ✓ (§ 4); 64 + 4 impls ✓ (`edge_inventory` § 7); 22 visible verbs and the 21 candidates ✓ (`discover_tests` § 2–3; `launcher`/`run` hidden); 14 rule files ✓ (`ai_config_satellites` § 1); 388 files / 348,648 LOC ✓; 59 `try_downcast!` ✓; 179 mirror items ✓; 86 + 25 log files ✓ (addendum A.1); 16 A.5(v) files ✓; `test_trampoline_exec.py:852` ✓ (the `crates/ocx_lib/src/shims` path). **Not verifiable:** "70 reciprocated module pairs" (#10); "51 impl-holding files" (#20).

## 8. Gate cost

| # | Severity | Location | Finding | Fix |
|---|---|---|---|---|
| 24 | High | DEC-15, `Verify` column, C-019, C-022, § Manual checks, B5–B11 gate note | The plan says scoped almost everywhere; the mechanics say full almost everywhere. (a) C-019 step 2 escalates on any non-member path: every B1 WP and **every extraction WP** (they all edit `.claude/rules.md`, `CLAUDE.md`, `test/tests/test_logging.py`) runs the full `task verify` — the B5–B11 note attributes escalation to hub/ecosystem crates only. (b) In phase 1 the only member with content is `ocx_lib`, whose `test:scoped` row is "every verb" (C-022) → the whole acceptance suite → ≈ the full gate for WP-10…WP-19 too. (c) The framework's high-risk checkpoint fires on any file that appears in another WP's Expected Files (verify.md) — `lib.rs`, `crates/ocx_cli/Cargo.toml`, `config.rs`, `workspace_structure.rs`, `test_logging.py` … — so a full documented run follows nearly every merge regardless. (d) The "≤ 5 min on a one-file change in `ocx_setup` — WP-04" measurement is unreachable: at WP-04 `ocx_setup` is an empty shell and C-022 makes `test:scoped -- ocx_setup` exit non-zero (no table row). The first honest non-hub measurement is after WP-29 (`ocx_script`). | Be explicit: "expected full-gate count ≈ 30 of 39 WPs; phase-1 scoped runs still execute the full acceptance suite". Then buy back what is cheap: (i) route `.claude/**`, `CLAUDE.md`, `crates/*/README.md` to `task claude:tests` and `test/tests/test_logging.py` to itself instead of escalating (the ADR's escalation rationale — lock/fork/taskfile changes — does not cover AI-config paths); (ii) give `test:scoped` a per-module row set for `ocx_lib` during phase 1 (module → verb, from the file map) so B2–B4 get a real subset; (iii) move the ≤ 5 min measurement to WP-29 and say the WP-04 number is the empty-shell floor, not evidence. |
| 25 | High | C-018 / WP-06 | `rsync` of this tree into `<mirror>/external/ocx` with the default `OCX_MIRROR_DIR=../ocx-mirror` writes into the owner's live `ocx-mirror` checkout (clean today, submodule at v0.6.2) — dirtying a shared worktree another session may be using. | Overlay into a disposable copy: `git -C $OCX_MIRROR_DIR worktree add <repo>/.tmp/satellite-verify HEAD`, rsync into *that* `external/ocx`, build, `worktree remove --force` after (task `cmds:`, never `preconditions:`). CI's fresh checkout is unaffected. |
| 26 | Medium | C-070 vs C-049 (ii) | Extraction deletes each crate's phase-1 boundary tests (≈ 20 nextest cases) while `NEXTEST_FLOOR` "never drops"; if the floor is raised at WP-20 ("record counts") to include them, WP-21…WP-36 red the floor by design. | State the raise policy: the floor is 8,049 until WP-37 and re-baselined there; phase-1-only tests are excluded from any raise. Pin the check to the Linux job (Windows runs raw `cargo nextest`, counts differ by `cfg`). |
| 27 | Low | C-049 (v) / C-019 step 5 | `cargo doc … -D rustdoc::broken_intra_doc_links` (`rust:doc:check`, WP-20) is named "in the gate" but `verify:scoped` step 5 does not list it. | Add `rust:doc:check` to step 5 (per changed crate). |

## Verdict

`needs-fix (4 Block, 6 High)` — Blocks: #1 S-012 owner-less, #2 no per-WP structural-change predicate, #15 WP-18/WP-19 same-wave collision on `package_manager/managed_config/publish.rs`, #23 batch convention not executable by `/hex-execute`. Highs: #3, #4, #16, #17, #24, #25. All fixes are plan edits; none reopens an ADR ruling.

---

## Round 2 — spec re-validation (2026-09-16, reviewer:spec, opus)

**Verdict on the round-2 revision:** FAIL → all findings applied in place → **PASS** (orchestrator, same day).

Carriers confirmed for all 4 Block / 8 High panel findings and all 22 adversary rows. Residuals found and applied:

| # | Finding | Applied as |
|---|---|---|
| 1 | Gate tokens `B5…B10-review` absent from the mermaid graph | `G5…G10` nodes on the extraction chain |
| 2 | Addendum A.1 / D-001 still carried the `tracing_subscriber` rewrite of `CapturingLogger` and the span-context rationale | A.1 and D-001 reworded per DEC-1 |
| 3 | Addendum D-060 still said "compiled unconditionally" | D-060 reworded per DEC-16 |
| 4 | `app/context.rs` chain WP-14a → WP-13 → WP-19 had no WP-13 edge; `file_structure.rs` chain listed out of wave order | WP-19 depends on WP-13; chain reordered WP-11 → WP-12 → WP-16 |
| 5 | "26 `crate::symlink` callers" — 25 files + `lib.rs:74` | C-032, WP-11 corrected |
| 6 | C-006 property (4) undefined | (4) = the C-007 negative-fixture property |
| 7 | WP-04's ≤ 5 min number presented as evidence on an empty-shell workspace | C-019/S-001 label WP-04 as the floor; post-WP-36 run is the evidence |
| N1 (High) | Fourth out-of-crate `impl OciTransport` — `SbomTransport` at `oci/verify/pipeline.rs:6397` | DEC-14, C-053, WP-24 list four doubles; `ocx_oci::testing::{RecordingTransport, SbomTransport}` |
| N2 (Block) | WP-16 must edit `oci/index.rs:17` (`DEFAULT_INDEX_BASE_URL` re-export) but neither owned it nor depended on WP-14a | WP-16 depends on WP-14a; `oci/index.rs` in its set; chain WP-14a → WP-16 → WP-18 |
| N3 (High) | WP-17's `utility_imports_nothing` scans `compression.rs`, which reaches `crate::MEDIA_TYPE_*` until WP-14a lands | WP-17 depends on WP-14a; `compression.rs` in its set |
| S | S-012 in no WP Scope cell; histogram summed to 37 | S-012 on WP-03 (guard) and WP-20 (suite); histogram `medium 1 · high 7 · xhigh 30` |

Structural checks (a)–(g): all OK after the edits above. Code-claim spot checks: 7/8 correct on first pass (the symlink count), the SbomTransport miss surfaced by the eighth.
