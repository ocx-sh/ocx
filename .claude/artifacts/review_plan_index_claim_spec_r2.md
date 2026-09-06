# Spec re-validation (round 2) — `plan_index_claim_command.md`

**Reviewer:** reviewer (focus: spec), Opus 5 — **re-validation pass**
**Target:** `.claude/artifacts/plan_index_claim_command.md` (revised: 19 WPs, 75 contracts, 40 scenarios)
**Checklist:** the 8 Block + 18 High findings across
`review_plan_index_claim_spec.md` (4B/9H), `review_plan_index_claim_quality.md` (1B/4H),
`review_plan_index_claim_security.md` (3B/5H), `review_plan_index_claim_sota.md` (0B/0H)
**Sources of truth:** `adr_index_claim_command.md` (Accepted), `system_design_index_claim_command.md`,
`meta-ai-config.md` § Plan Status Protocol, `~/.claude/skills/hex-core/references/protocol.md`, `CLAUDE.md`
**Date:** 2026-09-05

Every "closed" below quotes the plan's new text. Every source claim was ruled by opening the file.

---

## Part 1 — Fix verification

### Spec review (4 Block, 9 High)

| Finding | Verdict | Evidence |
|---|---|---|
| **F-01 · Block** · preflight readable-`false` lands on 77 instead of 86 | **closed** | C-029 now: "**Three outcomes, not two:** a field that reads `true` passes; a field that reads **`false` returns `ForgeError::WriteCapabilityUnavailable` (86) from `ensure_push_access` before any push**, with the Settings → CI/CD → Job token permissions message and the project path, and an allowlist that does not contain the publishing project does the same naming both paths; only an **unreadable** field yields `unknown`-and-proceed. The readable-`false` path never reaches the stderr classifier, which is why C-044's promotion covers only the `unknown` path." C-044 adds "**The promotion is driven by the preflight, never by the phrase.**" Tests landed: WP-8 `preflight_readable_false_errs_86`, `preflight_allowlist_miss_errs_86`, `preflight_unknown_field_does_not_fail`; WP-17 `::test_job_token_push_disabled_86_before_any_push`, `::test_allowlist_miss_86_names_both_projects`. S-015/S-016 both now read "before any push". |
| **F-02 · Block** · recording `git` shim has no WP, file or test | **closed** | WP-4 renamed "git-over-HTTP fixture **and the recording `git` shim**", Expected files gain `test/tests/git_shim.py` (new), scope widened to `C-030, C-033…C-045, C-074; S-013…S-033, S-039, S-040` (so C-034/C-035 and S-030…S-033 are cited), size stays `L`. Test inventory WP-4 gains `::test_shim_records_argv_and_env_then_delegates`. |
| **F-03 · Block** · request title/body fixed-template invariant absent | **closed** | New **C-067**: "the claim and announce **request title and body are one fixed template over structured values only**… **No operator-supplied string is interpolated** — in particular `--upstream-disclaimer`, `--upstream-repository-url` and `--upstream-org` reach the **root file only**… Owners render as bare `login:id`, **never `@login`**". Owned by WP-9. Tests: WP-9 `request_body_is_a_fixed_template` (with `[x](https://evil)` and `@alice`), `owner_logins_render_without_at_sign`; WP-16 `::test_disclaimer_reaches_root_not_request_body`. New scenario S-038. Edge-case hunt § Input names "a disclaimer containing markdown and an `@mention`". |
| **F-04 · Block** · three files in two WPs' Expected files vs the absolute ownership claim | **closed** | New § preamble: "**File sets are disjoint within a wave**… Exactly three files are written twice **across** waves — `crates/ocx_lib/src/forge/github.rs`, `crates/ocx_lib/src/forge/gitlab.rs` and `crates/ocx_lib/src/forge/git_workspace.rs` — first by WP-5… and then by WP-7 / WP-8 / WP-13… The merge-time predicate is therefore: **every file in a WP's actual diff must appear in that WP's declared set; a file also claimed by another WP is permitted only when that other WP is a declared ancestor.**" WP-5's cell now spells all three paths fully. (The "exactly three" count does not survive Part 2 check 3 — see F-03 below; the ownership sentence and predicate themselves are fixed.) |
| **F-05 · High** · DV-4 names the wrong drift gate; `doc_binding.py` unowned | **closed** | DV-4 now: "joins the transcluded walkthrough set via **`WALKTHROUGH_PAGES` in `test/src/doc_binding.py`**… The anti-drift gate is **not** `test_doc_scripts_one_tree.py` (glob-driven, carries no page list and would need no edit); it is the `WALKTHROUGH_PAGES` tuple". `test/src/doc_binding.py` is in WP-18's Expected files; the one-tree file is gone. Test row: "`WALKTHROUGH_PAGES` gains `user-guide/claiming-a-namespace.md` and its NC1–NC3 checks run against it". Red/green table: "Add an untranscluded `ocx` fence to the new page; NC1 reds." Verified `WALKTHROUGH_PAGES` is a six-entry tuple at `test/src/doc_binding.py:50-57`, and `test/tests/test_doc_binding.py` exists. |
| **F-06 · High** · `http.followRedirects=false` scoped to clone hygiene | **closed** | New **C-068**: "**every** git invocation carries `-c http.followRedirects=false`, without exception — the fetch, the retry fetch and the push alike. Git's default is `followRedirects=initial`, so the initial request of the push *is* followed… The credential pair and `-c credential.helper=` keep the narrower scope of C-034…; the redirect flag does not." The flag is gone from C-033's list. Tests: WP-13 `every_git_invocation_carries_no_redirects`; WP-17 `::test_push_does_not_follow_a_redirect`. New scenario S-039 + a Risks row. |
| **F-07 · High** · no test over the `ForgeError` → exit-code mapping (C-018) | **closed** | C-018 now ends: "`GitCommandFailed` and `GitPushFailed` are **deliberately unclassified** and must map to `None`, asserted as `None` rather than omitted, so a later accidental classification also reds." WP-5 test row gains `forge_error_exit_code_table` "(all ten variants, with the two unclassified asserted as `None`)". |
| **F-08 · High** · `credential_kind`/`push_credential_kind` value vocabularies missing | **closed** | C-060 now: "**The value vocabularies are contracted, not only the keys**: `status` is `"unchanged"` \| `"updated"` — and claim compares against the **open claim branch**, so a claim `--out` run always reports `updated`…; `credential_kind` is `"job-token"` \| `"token"` \| `"none"` and **may not report a kind ocx cannot observe** (no `pat`, no `deploy-token`, no `oauth`); `push_credential_kind` is `"job-token"` \| `"token"` \| `"git-helper"` \| `null`, `null` under `api`." C-061 "with C-060's value vocabularies". Tests: WP-14 `credential_kind_wire_spellings`, `push_credential_kind_wire_spellings`, `claim_out_status_is_always_updated`; WP-15 `announce_report_gains_six_keys` "(with C-060's value sets)". |
| **F-09 · High** · WP-16 Scope says three cross-repo issues, table lists six | **closed** | WP-19 Scope cell now reads "**six** cross-repository issues, the two issue-body posts"; the closeout header reads "**Six issues and two posts**" and adds "The count in WP-19's Scope cell, this header and this table must agree — an earlier draft said three, which would have silently dropped three Handoff-decision rows that land only here." Closing condition added: "WP-19 completes only when all six rows above have a recorded outcome." Rows 1 and 6 are marked release-blocking, and Release gate **6** now reads "Closeout rows 1 and 6 filed / verified". |
| **F-10 · High** · C-066's checklist split across two WPs two waves apart | **closed** | C-066 is now "a **four-edit change landing in one work package**: the constant itself; `env.rs`'s own **'Known non-members'** doc block…; `.claude/rules/subsystem-cli.md`'s credential-exemption table; and `website/src/docs/reference/environment.md`." All four sites are in WP-14's Expected files (`crates/ocx_lib/src/env.rs`, `.claude/rules/subsystem-cli.md`, `website/src/docs/reference/environment.md`); the Documentation-surfaces table assigns both doc rows to **WP-14**; WP-18 no longer lists them. Test: `credential_keys_contains_git_token_not_username` (both memberships). WP-14 per-package note: "It owns all four edits of C-066 so the constant, its own doc block, the exemption table and the reference page never disagree." |
| **F-11 · High** · 89 `--package` invocations in six unowned acceptance modules | **partial** | The six modules landed in WP-15's Expected files, `Verify: full`, plus `no_package_flag_invocation_survives_outside_the_deprecation_test` and the acceptance note "the six migrated modules must stay green under the positional form". I re-counted the tree: `test_announce.py` 48, `test_announce_gitlab.py` 33, `test_tag_reserved.py` 4, `test_exit_codes.py` 2, `test_package_cascade.py` 1, `test_announce_push_file.py` 1 = **89**, exactly WP-15's set, and `announce_helpers.py` has none. **What remains:** C-062's new wording generalises to "**Every in-repository invocation moves to the positional form in the same change**", and a seventh invocation exists outside `test/` — `.github/workflows/oci-publish.yml:276` (`ocx package announce \ --package "${repo_path}" \`) — in no WP's Expected files. See F-05 below. |
| **F-12 · High** · WP-6 `light` on the file that decides `ForgeIdentity::bot` | **closed** | The package renumbered to **WP-7** and its `Review` cell is now `panel`: "`**WP-7** \| GitHub REST identity + preflight. C-023…C-025 \| crates/ocx_lib/src/forge/github.rs \| M \| 3 \| WP-5 \| panel \| scoped`" — symmetric with WP-8 (GitLab). |
| **F-13 · High** · C-054 branch name and S-035 mitigation untested | **partial** | **C-054 closed**: contract gains "The name is asserted directly, not inferred from idempotence"; tests `claim_branch_name_is_distinct_from_announce` (WP-9) and `::test_claim_branch_name` (WP-16). **S-035 partial**: both requested tests landed in WP-14 (`index_claim_suggests_package_claim`, `index_group_help_states_no_forge_write`), but the file the second one must change is `crates/ocx_cli/src/command.rs:110` — the `ocx index` group's doc comment (`/// Operations related to the package index`, verified in place) — and that file is in **no** work package's Expected files. See F-01 below. |

### Quality review (1 Block, 4 High)

| Finding | Verdict | Evidence |
|---|---|---|
| **F-01 · Block** · `ClaimError` never reaches the classifier | **closed** | New **C-071**: "`ClaimError` is registered in `cli::classify`'s `try_downcast!` ladder, beside `AnnounceError` (`classify.rs:181`) and `ForgeError` (`:180`)… **Without the row, `classify_error` falls through to `ExitCode::Failure`**… The test drives `classify_error` over a boxed `ClaimError`, never `ClaimError::classify` directly." `crates/ocx_lib/src/cli/classify.rs` added to WP-9's Expected files and C-071 to its Scope; test `claim_error_reaches_classify_error` "(over a boxed `ClaimError`, asserting 65/79/64)"; Risks row flipped to "~~Medium~~ **closed by C-071**". **Verified in source:** `classify.rs` doc comment reads "Add a new `try_downcast!` entry here whenever a new top-level error type gains a `ClassifyExitCode` impl"; `try_downcast!(ForgeError);` and `try_downcast!(AnnounceError);` are both present; `classify_error` ends `ExitCode::Failure`. |
| **F-02 · High** · C-041 mints a second backoff schedule | **closed** | C-041 now: "confirmed by a bounded poll driven by the **existing** `forge::poll::backoff_delays` with `PollSchedule { initial_interval: 1s, max_interval: 30s, deadline: 30s, .. }`, which yields `1, 2, 4, 8, 15`". Recorded as **DV-7**. Test `poll_uses_forge_poll_backoff_delays` "(asserting the configuration yields `1,2,4,8,15`)". **Verified by hand-running `backoff_delays` (`forge/poll.rs`) with those parameters:** 1, 2, 4, 8, then the fifth delay clamped to the remaining 15 → `[1,2,4,8,15]`, cumulative 30s. Correct. |
| **F-03 · High** · `CREDENTIAL_KEYS` breaks the ocx-mirror plugin path | **closed** | C-066: "Both the constant edit and the two documentation edits **must record the asymmetry**: `OCX_ANNOUNCE_GIT_TOKEN` is scrubbed from plugin child environments while its sibling `OCX_ANNOUNCE_TOKEN` deliberately is not, so a plugin-dispatched `ocx-mirror` inherits the API half and not the push half. That is benign today only because `AnnounceConfig` carries no `transport` field, which is why the WP-19 ocx-mirror issue must say that any transport wiring there passes the push credential explicitly rather than relying on inheritance." Closeout row 2 carries the same sentence with "**Must state**". |
| **F-04 · High** · git-version gate's accept boundary untested; `Version::Ord` inverts it | **closed** | C-013 now: "`GitVersion` is a **local `(major, minor, patch)` tuple newtype**, not `crate::package::version::Version` (see C-075)." New **C-075** states the rolling-parent hazard and adds "The gate's **accept** side is tested, not only its refusals: a gate that refuses everything passes every refusal test." Tests: `probe_git_binary_accepts_the_boundary_release` (2.31, 2.31.0, `2.54.0.windows.1` accepted; 2.30.9 refused), `git_version_compare_is_a_plain_tuple`; Red/green mutation "Swap the tuple compare for `package::version::Version`; 2.31.0 is then refused." S-021 adds "git 2.31.0 exactly is **accepted**". **Verified in source:** `package/version.rs` `impl Ord` returns `Ordering::Greater` at the patch step when `lhs` has no patch and `rhs` does — so `2.31 > 2.31.0`. C-075's premise holds. |
| **F-05 · High** · `capability_checks_never_empty` is a self-consistent green | **partial** | New **C-069**: "`PushAccess` is constructed through `PushAccess::skipped_all()`, which seeds one row per `CapabilityName` at `CheckStatus::Skipped`; implementations **upgrade** rows and never build the vector from empty, and the struct exposes no constructor that can produce an empty `checks`." Test moved to the producer side (`push_access_skipped_all_seeds_every_name`, WP-5), the missing GitLab twin `gitlab_ensure_push_access_emits_rows` added to WP-8, and `capability_checks_never_empty` is gone from WP-14's list. **What remains:** C-011 still spells the type as `PushAccess { checks: Vec<CapabilityCheck> }` — a struct-literal form with a public field, which is exactly the empty construction C-069 says is unrepresentable. See F-07 below. |

### Security review (3 Block, 5 High)

| Finding | Verdict | Evidence |
|---|---|---|
| **F-01 · Block** · request title/body has no contract, scenario or test | **closed** | Same fix as spec F-03: **C-067**, WP-9 scope + tests, WP-16 acceptance, scenario **S-038** ("A claim whose `--upstream-disclaimer` contains markdown and an `@mention` \| The disclaimer reaches the **root file only**; the request title and body contain neither the disclaimer text nor any `@`"), and a Risks row "A free-text flag reaches the request body a human merges under G-04". |
| **F-02 · Block** · push-option assertion is count-only | **closed** | C-039 now: "asserted as **the sorted key set, not merely a count of four**: swapping `.description` for `merge_request.merge_when_pipeline_succeeds` keeps the count at four while auto-merging the claim and defeating G-04… `merge_request.merge_when_pipeline_succeeds` and `merge_request.remove_source_branch` are forbidden **by name**, with a test that reds when either is sent." Tests renamed and added at all three levels: WP-4 `::test_push_delivers_exactly_the_four_option_keys` + `::test_merge_when_pipeline_succeeds_is_rejected_by_the_hook`; WP-11 `push_options_render_exactly_the_four_keys`, `merge_when_pipeline_succeeds_is_never_rendered`, `remove_source_branch_is_never_rendered`; WP-17 `::test_merge_when_pipeline_succeeds_is_never_sent`. Red/green row: "Substitute `merge_request.merge_when_pipeline_succeeds` for `.description` — the count stays 4." S-013 now says "one push whose option **key set** is exactly the four". SOTA F-01's delimiter half also landed: C-039 adds "any of the delimiter characters a server-side push-option parser is known to split on — CVE-2026-3854 was a delimiter injection through printable text", with `delimiter_character_in_option_value_is_refused` and the edge-case-hunt line "control characters **and printable delimiters**". |
| **F-03 · Block** · `followRedirects` scoped to the clone | **closed** | Same fix as spec F-06 — **C-068**, plus the Risks row "The push follows a redirect and hands the credential to another host … C-068 puts `-c http.followRedirects=false` on **every** invocation, asserted over each rendered argv and end to end by a fixture that answers the receive-pack POST with a 302". |
| **F-04 · High** · no test asserts the child env block against the allowlist | **closed** | C-035 now holds the allowlist "as **data tables, one per platform** (`PASSTHROUGH` / `SET` / `NEVER`)" and ends "The invariant is caller-enforced rather than structural, so the child's actual environment block is asserted against the tables by name, not merely seeded from `Env::clean()`." C-035 moved into **WP-6**'s Scope — the package that owns `forge/git_command.rs`, the code that builds the environment. Tests: WP-6 `git_child_env_allowlist_tables_are_complete` (both platforms, by `#[cfg]`); WP-17 `::test_child_env_matches_the_allowlist` "(asserting `OCX_ANNOUNCE_TOKEN`, `OCX_ANNOUNCE_GIT_TOKEN`, `CI_JOB_TOKEN`, `GIT_CURL_VERBOSE`, `GIT_ASKPASS`, `SSH_ASKPASS` absent while set in the parent)". Red/green row: "`git_child_env_is_built_from_clean` (C-019) \| Construct with `Env::new()` instead of `Env::clean()`." **Verified in source:** `env.rs` `Env::new()` collects `std::env::vars_os()`, `Default` delegates to it, `Env::clean()` starts with `HashMap::new()` — C-019's claim is exact. |
| **F-05 · High** · C-035 omits `GIT_AUTHOR_*` / `GIT_COMMITTER_*` | **closed** | C-035's "**Set by ocx:**" clause now enumerates them explicitly: "…`GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`, **`GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`**, `LC_ALL=C`, `LANGUAGE=`." C-045 cross-references it and adds the discriminating fixture: "The assertion runs with a `~/.gitconfig` carrying a *different* identity, so it discriminates between 'ocx set it' and 'git read it from `HOME`'." Test row: `commit_identity_is_fixed` "(run with a differing `~/.gitconfig`)". |
| **F-06 · High** · three different cross-repo issue counts | **closed** | Same fix as spec F-09; both flagged rows are marked release-blocking in the closeout table ("**yes, to file**" for corporate-CA; "**yes, to verify**" for the reviewer checklist) and Release gate 6 was added so they inherit the pre-0.6.1 bar. |
| **F-07 · High** · `credential.helper=` negative case can pass vacuously | **closed** | C-034 now: "**Both halves are proved from one fixture `HOME` carrying a recording `credential.helper`**: without a helper configured, 'no helper was invoked' is true in every state of the code, including with the reset deleted." WP-17 test row: "`::test_no_helper_invoked_on_injecting_run` and `::test_helper_invoked_on_step_three_run` (one shared fixture `HOME` with a recording helper; the second also asserts no `extraHeader` and `push_credential_kind: "git-helper"`)". Red/green row: "Delete `-c credential.helper=` from the argv, with a helper configured in the shared fixture `HOME`." Edge-case hunt § Environment: "a credential helper present with a run that must not invoke one **and** its complement that must, from one fixture `HOME`". |
| **F-08 · High** · `SPAWN_ALLOWED` widening rejects a strawman alternative | **closed** | Option (b) taken in full. The Constitution-deviations row now names the loss in the firewall's own words — "moving the file that spawns the credential-bearing `git` from **module privacy** to a **source-text search**. `launch.rs:40-46` is explicit that the searches 'are not proofs' and that '**privacy is what actually holds**', so this is a real weakening and not a formality" — and evaluates the real alternative: "**Adding a capturing primitive to the private `launch/child_process.rs` and exporting a narrow non-recording `launch::capture`** preserves privacy and needs no allowlist row — it is the stronger option on security alone, and it is rejected because it makes `launch` own a forge concern, invents a shared seam nothing else uses, and departs from the eleven-precedent pattern". The residual is closed by C-021: "`forge/git_command.rs` is additionally the **only** file permitted to name a `Command` on this path, and it must not export a type alias, re-export or wrapper that would let a sibling file spawn without naming one of `SPAWN_TOKENS` — the one evasion a source-text search structurally cannot see," with a matching Risks row. C-021 is also restated as a mutation, not a state (security F-09's Warn, folded in). |

### SOTA review (0 Block, 0 High)

Its single Warn (F-01, CVE-2026-3854 delimiter injection) landed anyway — see security F-02's row.

**Part 1 tally: 23 closed / 3 partial / 0 not closed / 0 regressed.**

---

## Part 2 — Mechanical checks, re-run on the restructured plan

| # | Check | Result |
|---|---|---|
| 1 | **Traceability — Scope cells** | 75 `C-001…C-075` + 40 `S-001…S-040` defined, contiguous, no duplicates, no gaps. **115/115 appear in at least one work-package `Scope` cell.** Pass. |
| 1b | **Traceability — named tests** | **115/115** now carry a named test or the doc-only marker. The round-1 gap of nine is fully closed: `C-003` and `C-032` carry "**Doc-only: reviewed, not tested**" with the reviewing package named (WP-1, WP-8); `C-014` → `write_transport_default_is_api` + `write_transport_value_spellings`; `C-018` → `forge_error_exit_code_table`; `C-054` → `claim_branch_name_is_distinct_from_announce` + `::test_claim_branch_name`; `C-066` → `credential_keys_contains_git_token_not_username`; `S-005` → `::test_announce_unclaimed_namespace_exits_79` plus the cited existing guard; `S-015` → `::test_job_token_push_disabled_86_before_any_push`; `S-035` → `index_claim_suggests_package_claim` + `index_group_help_states_no_forge_write`. Pass, with four soft spots recorded as F-09 (test named in a package other than the one whose Scope claims the ID). |
| 2 | **Reverse coverage** | Every ID cited in a `Scope` cell exists; the highest cited are `C-075` and `S-040`, both defined. **0 dangling citations.** Pass. |
| 3 | **File-set disjointness** | **(a) Same-wave: PASS.** All 19 sets normalised and intersected pairwise; wave 1 {WP-1,2,3,4}, wave 3 {WP-6,7,8,9,10,11,12}, wave 4 {WP-13,14}, wave 5 {WP-15,16}, wave 6 {WP-17,18} are each fully disjoint. **(b) Cross-wave: FAIL — three collisions beyond the three named, none of them declared.** The three named exceptions check out (WP-5∩WP-7 `forge/github.rs`; WP-5∩WP-8 `forge/gitlab.rs`; WP-5∩WP-13 `forge/git_workspace.rs`), each serialised by a declared dependency edge. But four new files are declared "(new)" under WP-6 (`forge/git_command.rs`, `forge/credentials.rs`), WP-11 (`forge/git_push_options.rs`) and WP-12 (`forge/git_stderr.rs`), and Rust requires a `mod` declaration for each in `crates/ocx_lib/src/forge.rs` — a file only WP-5 declares (verified: `forge.rs:28-35` is a hand-written `mod api; mod error; mod github; mod gitlab; mod http; mod identity; mod kind; mod poll;` block). Either WP-6/11/12 each write `forge.rs` (a file absent from their own declared sets → first half of the predicate fails) or WP-5 pre-declares and pre-creates all four (→ seven cross-wave doubles, not three). See **F-03**. Two further files no package declares at all: `crates/ocx_cli/src/command.rs` and `crates/ocx_cli/src/api/data.rs` (**F-01**, **F-02**), plus `website/.vitepress/config.mts` (**F-04**) and `.github/workflows/oci-publish.yml` (**F-05**). |
| 4 | **DAG soundness** | Pass. All **23** declared edges are wave-monotone (dependency wave strictly less than dependent wave in every case). The mermaid graph carries exactly 23 edges and reproduces the `Depends on` column with no edge in one and not the other. **Critical path recomputed independently:** the longest chain is 7 nodes, and `WP-1 → WP-5 → WP-9 → WP-14 → WP-15 → WP-17 → WP-19` is one of two (the other, `WP-1 → WP-5 → WP-6 → WP-14 → …`, is the same length). The next-longest, via WP-13, is 6. Sizes on the claimed path are S, L, L, L, L, L, M — "**five of them large**" is correct. |
| 5 | **ADR coverage after the restructure** | Pass on all three. **Implementation Plan 10/10** (1 → WP-1/WP-5/WP-7/WP-8/WP-14/WP-18; 2 → WP-5/7/8; 3 → WP-6/8/14; 4 → WP-2/WP-9; 5 → WP-10/14/15; 6 → WP-10; 7 → WP-6/11/12/13; 8 → WP-5/7/8/14/15; 9 → WP-4/16/17; 10 → WP-3/18/19). **Handoff decisions 7/7** (1 → Overview "Release vehicle" + § Shippable after wave 5; 2 → gate 1; 3 → closeout row 6; 4 → C-062/DV-3/WP-15; 5 → gate 5; 6 → closeout row 4; 7 → closeout row 5). **Validation:** the four round-1 homeless items all landed — "every new exit-code mapping" → `forge_error_exit_code_table`; the two `AuthError`/80 arms → `::test_no_credential_exits_80` (WP-16) and `::test_transport_git_outside_job_with_no_variable_exits_80` (WP-17); "the website build" → a Verification-commands row plus phase-pattern step 5 "**`task website:build` for WP-18**"; "the claim branch" → `::test_claim_branch_name`. Two items remain covered on one side only — the self-managed-GitLab partial-clone proof (**F-11**) and the canonical-login override (**F-12**). |
| 6 | **Review / Verify budgets** | Pass against `protocol.md` § "Every WP declares its review budget". No under-budget row survives: the two round-1 complaints are fixed (WP-1 `light` → `panel`; old WP-6 `light` → new WP-7 `panel`), and WP-3's sub-overhead isolation now carries the required one-line justification in § Under-parallelization ("**WP-3 stays isolated below the overhead floor**… because it is the amendment that makes the whole design constitutionally legal"). `Verify` is `full` on exactly four rows (WP-5, WP-15, WP-17, WP-19), each justified in the paragraph that follows the table, matching the plan's own "declared on four packages". The two budget columns are adjacent with `Verify` immediately after `Review`, as C-302 requires, and `Verify-default: scoped` is in the Status block. |

**Status block** re-checked: carries all four Plan Status Protocol fields (`Plan`, `Active phase`, `Step`, `Last update`) and all four hex fields (`State`, `Tier`, `Updated`, `Next`), plus `Verify-default`. `Step: /hex-plan → plan-approved` is a valid hex-family spelling.

---

## Part 3 — New findings from the fix round

### F-01 · Block · `crates/ocx_cli/src/command.rs` is owned by no work package, yet WP-14 must add a module row to it and S-035's named test asserts a sentence that lives in it

**Where:** Work packages, WP-14 Expected files; UX scenarios `S-035`; Test inventory, WP-14 (`index_group_help_states_no_forge_write`); § Documentation surfaces

**Problem:** Two independent edits land in one unowned file.

1. WP-14 declares `crates/ocx_cli/src/command/package_claim.rs` **(new)**. Rust needs a declaration for it, and `crates/ocx_cli/src/command.rs` is where every sibling is declared — a hand-written block beginning `pub mod about; pub mod add; pub mod clean; …` (verified, `command.rs:9-…`). Without `pub mod package_claim;` the crate does not compile.
2. `S-035`'s second half — "the `ocx` index group's help states that **no index subcommand writes to a forge**" — is a change to the doc comment at `command.rs:110`, which today reads exactly `/// Operations related to the package index` above `#[command(subcommand)] Index(index::Index)`. The plan gives that half a named test, `index_group_help_states_no_forge_write` (WP-14), so the check exists and the file it checks is unowned.

`command.rs` appears in **no** `Expected files` cell across all 19 packages, so the plan's own merge predicate — "every file in a WP's actual diff must appear in that WP's declared set … Anything else blocks the merge" — rejects WP-14's merge. The ADR is explicit that this is contract text, not decoration: "Help text is contract text under `quality-cli-help.md`, so shipping the old sentence would ship a documented lie a user could act on."

This is the same defect class as round-1 spec F-05 (`test/src/doc_binding.py`), which the fix round closed for the docs surface and did not sweep for the CLI surface.

**Fix:** Add `crates/ocx_cli/src/command.rs` to WP-14's Expected files and add `S-035` to WP-14's Scope cell (it currently sits only in WP-16's, which owns no Rust). State in WP-14's per-package notes that the edit is two lines — the `pub mod package_claim;` row and the `Index` variant's doc comment — so a reviewer can tell the two apart.

---

### F-02 · Block · `crates/ocx_cli/src/api/data.rs` is owned by no work package, yet WP-14 adds `api/data/claim.rs` under it

**Where:** Work packages, WP-14 Expected files

**Problem:** WP-14 declares `crates/ocx_cli/src/api/data/claim.rs` **(new)**. Its `pub mod claim;` row belongs in `crates/ocx_cli/src/api/data.rs`, a hand-written declaration block (verified: `pub mod about; pub mod announce; pub mod attestation; …`). That file is in no package's Expected files.

The plan got the analogous cases right elsewhere and this one is the residue: WP-9 owns `crates/ocx_lib/src/lib.rs` precisely so its new `claim.rs` can be declared, and WP-14 owns `crates/ocx_cli/src/options.rs` precisely so `options/forge_write.rs` can be. The CLI's two other declaration hubs were missed.

**Fix:** Add `crates/ocx_cli/src/api/data.rs` to WP-14's Expected files.

---

### F-03 · Block · Four new `forge/` submodules need a `mod` row in `forge.rs`, which only WP-5 declares — so "exactly three files are written twice" is false and three wave-3 merges are blocked

**Where:** § Parallelization preamble ("File-set rule, stated precisely"); Work packages WP-5, WP-6, WP-11, WP-12; § Merge plan

**Claim:** "Exactly three files are written twice **across** waves — `crates/ocx_lib/src/forge/github.rs`, `crates/ocx_lib/src/forge/gitlab.rs` and `crates/ocx_lib/src/forge/git_workspace.rs`."

**Problem:** Four more files are created "(new)" inside `crates/ocx_lib/src/forge/` by packages that do not own `forge.rs`:

| File | Declared new by | Wave |
|---|---|---|
| `crates/ocx_lib/src/forge/git_command.rs` | WP-6 | 3 |
| `crates/ocx_lib/src/forge/credentials.rs` | WP-6 | 3 |
| `crates/ocx_lib/src/forge/git_push_options.rs` | WP-11 | 3 |
| `crates/ocx_lib/src/forge/git_stderr.rs` | WP-12 | 3 |

`crates/ocx_lib/src/forge.rs` carries a hand-written declaration block (`forge.rs:28-35`: `mod api; mod error; mod github; mod gitlab; mod http; mod identity; mod kind; mod poll;` followed by a `pub use` block at `:37-41`), and only WP-5 declares it. Both readings break something:

- **WP-6/11/12 each edit `forge.rs`.** `forge.rs` is absent from their declared sets, so the predicate's first half — "every file in a WP's actual diff must appear in that WP's declared set" — fails and three of the seven wave-3 merges are blocked. It also puts three concurrent writers on one file inside a single wave, which is the collision the same-wave rule exists to prevent.
- **WP-5 pre-declares all four.** Then WP-5 must also create the four files (a `mod` row for a file that does not exist fails WP-5's own `cargo check -p <crate> --all-targets --locked` gate), so the cross-wave doubled set is **seven**, not three, and WP-6/11/12's "(new)" annotations are wrong.

The same question is unanswered for `pub use` re-exports: `ForgeIdentity`, `WriteTransport`, `PushAccess`, `CapabilityCheck`, `CapabilityName` and `CheckStatus` (C-008, C-011, C-012, C-014) all have to reach `crate::forge`'s public surface through `forge.rs:37-41`, which WP-5 owns — fine — but `GitBinary`, `ForgeCredentials` and the redactor (C-013, C-015, C-019, C-022) are produced in WP-6 and consumed by WP-13 and WP-14, so their re-export rows land after WP-5 too.

**Fix:** Pick the second reading and say so: WP-5's scope note already promises "the whole forge public surface plus stub bodies" — extend it to "**and an empty stub file plus its `mod` row for every submodule a later wave fills**: `git_command.rs`, `credentials.rs`, `git_push_options.rs`, `git_stderr.rs`, `git_workspace.rs`", add those paths to WP-5's Expected files, change the four "(new)" annotations to "(implementation)", and correct the preamble's count to seven with the pairs listed. That keeps the ancestor rule intact (WP-6/11/12/13 all descend from WP-5) and keeps `forge.rs` single-writer.

---

### F-04 · Block · The new user-guide page has no sidebar entry, and `website/.vitepress/config.mts` is owned by no work package

**Where:** Work packages, WP-18 Expected files; § Documentation surfaces, row 3

**Problem:** WP-18 adds `website/src/docs/user-guide/claiming-a-namespace.md` (**new**). VitePress's sidebar in this repository is **hand-maintained**, not filesystem-globbed — `website/.vitepress/config.mts` carries explicit entries such as `{ text: "Attestations", link: "/docs/user-guide/attestations" }` and `{ text: "Promoting", link: "/docs/user-guide/promoting-packages" }` (verified at `config.mts:69-76`). A page added without an entry builds green and is unreachable from the navigation — the "quietly does less" shape.

`website/.vitepress/config.mts` is in no package's Expected files, so the edit that fixes it is blocked by the merge predicate, and WP-18's `task website:build` gate (correctly added this round) does not catch it: an orphan page is a valid build.

**Fix:** Add `website/.vitepress/config.mts` to WP-18's Expected files and one Documentation-surfaces row naming the sidebar entry. If a test asserting every `website/src/docs/**.md` page is reachable from the sidebar does not already exist, WP-18 is the cheap place to add it.

---

### F-05 · High · C-062 now claims "every in-repository invocation", but a seventh `announce --package` call site lives in an unowned CI workflow

**Where:** Component contracts § CLI, `C-062`; Work packages, WP-15; Test inventory, WP-15 (`no_package_flag_invocation_survives_outside_the_deprecation_test`)

**Claim:** "**Every in-repository invocation moves to the positional form in the same change**; exactly one dedicated test retains the hidden `--package` form, and a structural check asserts no other `--package` invocation survives."

**Problem:** The fix round generalised the contract from "89 invocations in six acceptance modules" to "every in-repository invocation", and the work package did not follow. I swept the tree: outside `test/tests/`, one live invocation remains, in `.github/workflows/oci-publish.yml:276`:

```
ocx package announce \
  --package "${repo_path}" \
  --tags-file "$tags_file" \
  --index-repo ocx-sh/index
```

That file is in no package's Expected files. Two outcomes, both bad: if `no_package_flag_invocation_survives_outside_the_deprecation_test` is scoped to `crates/` and `test/`, it reports green while the repository's own publishing workflow keeps the deprecated form until it hard-fails at 0.7 — the exact "89 simultaneous failures in a repository that has forgotten why" the WP-15 note argues against; if the check is scoped repo-wide, it reds and the fixing edit blocks the merge.

**Fix:** Add `.github/workflows/oci-publish.yml` to WP-15's Expected files, name it in the WP-15 per-package note beside the six pytest modules, and state the structural check's scope explicitly (repo-wide, excluding `.claude/artifacts/**` and `.claude/state/**`, which are historical records and must not be rewritten).

---

### F-06 · High · The documentation sweep does not name the `--package` → positional rewrite on the two surfaces that document announce's grammar

**Where:** § Documentation surfaces, rows for `website/src/docs/reference/command-line.md` (owner WP-18) and `.claude/rules/subsystem-cli-commands.md` (owner WP-18); `C-062`

**Problem:** WP-15 carries a `!` commit subject — "`feat(announce)!: take the package as a positional, deprecating --package until 0.7`" — and the Documentation-surfaces row for `command-line.md` describes only three changes: a new `#### claim` block, "`--transport` added to the `#package-announce` block", and 86 added to two exit-code tables. It does not name the announce grammar rewrite. The page carries `--package` in eight places (verified):

- `:2696` the Usage line — `ocx package announce --package <NAMESPACE>/<NAME> (--tags … )`
- `:2703` the Options table row — "`--package <NAMESPACE>/<NAME>` \| Package to announce, e.g. `acme/widget` (required)."
- `:2757`, `:2763`, `:2770`, `:2778`, `:2881` five example invocations
- `:2884` prose ("the follow-up `announce --package` names a single package")

`.claude/rules/subsystem-cli-commands.md:82` has the same problem: its `package announce` row lists `--package` first in the flags column, and WP-18's row for that file says only "New command row".

So the plan ships a breaking CLI grammar change whose reference page still documents the removed-by-0.7 form as required, on a page WP-18 does own — which means the omission is a description gap, not an ownership gap, and would pass every merge check silently. The owner's standing rule is that plans enumerate documentation surfaces; here the surface is enumerated and the change to it is not.

**Fix:** Extend the `command-line.md` row: "the `#package-announce` Usage line, its Options table row and its five example invocations move to the positional form, with the hidden `--package` recorded once as deprecated-until-0.7". Extend the `subsystem-cli-commands.md` row the same way. Consider moving both to WP-15 so the grammar change and its documentation land in one commit — the plan already applies exactly that argument to C-066's four edits.

---

### F-07 · Warn · C-011's struct-literal spelling still permits the empty `checks` vector C-069 declares unrepresentable

**Where:** Component contracts § Forge public surface, `C-011` and `C-069`

**Problem:** C-069 was added to make emptiness unrepresentable rather than asserted: "the struct exposes no constructor that can produce an empty `checks`." C-011 was not updated to match and still reads `PushAccess { checks: Vec<CapabilityCheck> }` — a struct-literal spelling with a public field. A public `Vec` field is not a constructor, so `PushAccess { checks: Vec::new() }` compiles, from any file in the crate, and the property returns to being a convention. The implementation most likely to reach for it is the one C-069 was written for: the `--out` / no-credential early return (S-011).

**Fix:** One clause in C-011: "`checks` is private; the only ways to build a `PushAccess` are `skipped_all()` and the row-upgrade methods, and reads go through an accessor." Cite it from C-069.

---

### F-08 · Warn · DV-2 still asserts the absolute file-disjointness the Parallelization preamble now corrects

**Where:** § Deviations from the ADR, `DV-2`; § Parallelization preamble

**Problem:** The preamble was rewritten this round to "File sets are **disjoint within a wave**", with three named cross-wave exceptions. `DV-2` still reads "The work is decomposed into **19 file-disjoint work packages** across 7 waves". A reader who reaches the deviations table first — it is 500 lines earlier — takes the absolute, which is what produced round-1 spec F-04.

**Fix:** `DV-2`: "…19 work packages across 7 waves, file-disjoint within each wave (see § Parallelization for the named cross-wave exceptions)".

---

### F-09 · Warn · Four IDs in WP-16's Scope have no test in WP-16's own list

**Where:** Work packages, WP-16 Scope (`S-001…S-012, S-035…S-038`); Test inventory, WP-16 and WP-14

**Problem:** Traceability passes globally — every ID has a test somewhere — but four of WP-16's own scoped scenarios are proved by tests in a different package, so WP-16 cannot demonstrate its declared scope at its own merge gate:

| ID | What it is | Nearest named test | Package that owns the test |
|---|---|---|---|
| `S-002` | `--format json` → `ClaimReport` with `capability_checks` non-empty and `owner_identity_source: "resolved"` | `claim_report_plain_is_five_columns` (plain, not JSON), `push_access_skipped_all_seeds_every_name` | WP-14, WP-5 |
| `S-011` | `--out` with **no credential** proceeds unauthenticated, `push-access: skipped` | `::test_out_renders_and_opens_nothing` (does not exercise the no-credential arm) | WP-16 |
| `S-035` | `ocx index claim` hint + group help | `index_claim_suggests_package_claim`, `index_group_help_states_no_forge_write` | WP-14 |
| `S-036` | `capability_checks` non-empty, **in execution order**, `skipped` rows present | `push_access_skipped_all_seeds_every_name` (asserts seeding, not execution order) | WP-5 |

Note also the asymmetry the fix round left behind: announce's report gets a key-set assertion (`announce_report_gains_six_keys`) and claim's sixteen-key `ClaimReport` (C-060) gets none.

**Fix:** Move `S-035` from WP-16's Scope to WP-14's (where its tests are). Add `::test_claim_json_report_key_set` and `::test_out_without_credential_reports_push_access_skipped` to WP-16. Either assert execution order in `push_access_skipped_all_seeds_every_name` or drop the phrase from `S-036`.

---

### F-10 · Warn · The recording `git` shim has a file but no placement constraint

**Where:** Work packages, WP-4 (`test/tests/git_shim.py`); § Per-package notes

**Problem:** The shim now has an owner, a file and a self-test (spec F-02 closed), but nothing says **where the executable it generates is written**. This repository already stages `test/bin/ocx` and puts `~/.ocx/**` on `PATH` through direnv; a `git` shim dropped into either shadows the real `git` for every other acceptance module, for `task verify`, and for the developer's own shell. The per-package notes section covers WP-5, 6, 11/12, 13, 14, 15 and 18 and says nothing about WP-4.

**Fix:** One WP-4 note: "the shim writes its executable into a per-test `tmp_path` directory and prepends **only that directory** to the child `PATH`; it is never written under `test/bin/`, `~/.ocx/`, or any path that outlives the test." Assert the placement in `::test_shim_records_argv_and_env_then_delegates`.

---

### F-11 · Warn · The ADR's self-managed-GitLab partial-clone item is acknowledged as uncovered and then not routed anywhere

**Where:** § Release gates (WP-19), gate 2; `DV-5`; ADR § Validation

**Problem:** The ADR asks for "**Partial clone against a self-managed GitLab, not only gitlab.com**", and gives the reason: "no source pins down whether every currently-supported self-managed version enables filtering over HTTPS." Gate 2 measures against `ocx-sh/index` — a GitHub repository — and closes with the honest note "**Note that measuring against `ocx-sh/index` on GitHub does not prove GitLab behaviour.**" Gate 4 does reach a self-managed GitLab, but is scoped to "**the authoritative source for the two server-side refusal texts C-044 matches**" and says nothing about the filter. So the plan states the gap and leaves it with no owner — which reads at execution time as "already considered, nothing to do".

**Fix:** Fold the filter proof into gate 4's scope: "the same run captures `git rev-list --objects --missing=print` and a `GIT_TRACE_PACKET=1` capability trace against the reporter's instance, against a full-clone negative control on the same host" — the mechanism DV-5 already names, applied to the host the ADR asks about.

---

### F-12 · Warn · The canonical-login override in C-048 has no named test

**Where:** Component contracts § Claim library, `C-048`; UX scenarios `S-007`; Test inventory WP-9, WP-16

**Problem:** C-048 now says the users API "resolve[s] every login and take[s] the server's `id`, `bot` and **canonical login spelling**", and `S-007` makes it the expected outcome: "`GITHUB_ACTOR`/`GITHUB_ACTOR_ID` seed the list, then the users API confirms and **overrides with the canonical login spelling**; source `resolved`." The ADR asks for it directly — "a resolvable users API **overrides** a CI-environment id and takes the forge's canonical login". The inventory's nearest tests are `owner_ladder_ci_environment` and `::test_owner_ci_environment`, which exercise the *unconfirmed* branch — the opposite arm. Nothing asserts that a CI-seeded list whose users API **is** reachable ends up `resolved` with the server's spelling, so a build that skips the override and reports `ci-environment` passes.

**Fix:** Add `owner_ladder_ci_seed_is_overridden_by_the_users_api` to WP-9 (asserting the source flips to `resolved` and the login spelling changes) and `::test_owner_ci_environment_overridden_when_users_api_reachable` to WP-16.

---

### F-13 · Suggest · WP-15's row title claims `--transport` on announce, but its Scope cites no contract for it

**Where:** Work packages, WP-15

**Problem:** WP-15 is titled "CLI announce: **`--transport`**, report keys, pre-existing-token warning, `--package` → positional" and its Scope cell is `C-061, C-062, C-064; S-027, S-034` — none of which is about `--transport`. The flag reaches announce through `C-059` (the flattened `ForgeWriteOptions`, owned by WP-14) and `C-014` (the enum, WP-5), so the contract is covered, but the package whose file actually gains the flatten declares neither. A per-WP reviewer reading only the Scope cell has no contract to check the flatten against.

**Fix:** Add `C-059` to WP-15's Scope with the note that WP-14 declares the struct and WP-15 flattens it into `package_announce.rs`.

---

### F-14 · Suggest · C-069's fixed seeding order and S-036's "execution order" are two different orders

**Where:** `C-069`; `S-036`

**Problem:** C-069 seeds "one row per `CapabilityName`", which fixes the order to the enum's declaration order. `S-036` promises a pipeline author the rows arrive "in execution order". Those coincide only by accident, and a preflight that reads `job-token-allowlist` before `job-token-push` (C-029 makes the allowlist read conditional on the push field) would diverge from either.

**Fix:** Pick one. Declaration order is the cheaper promise and the one C-069 delivers; restate `S-036` as "in a stable, contracted order, inapplicable rows present as `skipped`".

---

## Summary

| ID | Severity | Title |
|---|---|---|
| F-01 | Block | `crates/ocx_cli/src/command.rs` unowned — WP-14's module row and S-035's group-help sentence both land in it |
| F-02 | Block | `crates/ocx_cli/src/api/data.rs` unowned — WP-14's new `api/data/claim.rs` needs its declaration |
| F-03 | Block | Four new `forge/` submodules need `mod` rows in `forge.rs`; "exactly three files written twice" is false |
| F-04 | Block | New user-guide page has no sidebar entry; `website/.vitepress/config.mts` unowned |
| F-05 | High | C-062's "every in-repository invocation" misses `.github/workflows/oci-publish.yml:276` |
| F-06 | High | The docs sweep never names announce's `--package` → positional rewrite on `command-line.md` or `subsystem-cli-commands.md` |
| F-07 | Warn | C-011's public `checks` field permits the empty vector C-069 forbids |
| F-08 | Warn | DV-2 still says "19 file-disjoint work packages" |
| F-09 | Warn | S-002, S-011, S-035, S-036 sit in WP-16's Scope with no WP-16 test |
| F-10 | Warn | The `git` shim has a file but no PATH-placement constraint |
| F-11 | Warn | Self-managed-GitLab partial-clone proof acknowledged as uncovered, not routed |
| F-12 | Warn | The canonical-login override (C-048 / S-007) has no named test |
| F-13 | Suggest | WP-15's title claims `--transport`; its Scope cites no contract for it |
| F-14 | Suggest | C-069's seeding order and S-036's "execution order" are different orders |

**4 Block · 2 High · 6 Warn · 2 Suggest**

All four Blocks are one defect class: **a file the change must edit that no work package declares**, which the plan's own merge predicate turns into a blocked merge. Round 1 found one instance (`test/src/doc_binding.py`); the fix round closed that one without sweeping the rest of the declaration-site and navigation surfaces.
